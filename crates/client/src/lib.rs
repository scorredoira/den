//! Connection from the UI to an agent. A thread reads the agent's messages
//! and dispatches responses and events; writing is a direct call.

mod platform;
mod ssh;

#[cfg(windows)]
pub mod windows_pipe;
#[cfg(windows)]
pub mod wsl;

pub use ssh::{AGENT_LINUX_X86_64, connect_ssh};

use std::{
    collections::HashMap,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use proto::{ClientMessage, Decoded, Event, PROTOCOL, Request, Response, ServerEnvelope, ServerMessage, TermId};

type Callback = Box<dyn FnOnce(Result<Response>) + Send>;
type Subscriber = Box<dyn Fn(TermUpdate) + Send>;
type OnDisconnect = Box<dyn FnOnce() + Send>;
type CloseStream = Box<dyn FnOnce() + Send + Sync>;

/// What a terminal's follower receives.
pub enum TermUpdate {
    Event(Event),
    /// The connection to the agent was lost: the terminal is still alive there
    /// and can be reattached on reconnect.
    Disconnected,
}
type Watcher = Box<dyn Fn(&Event) + Send>;

/// What the follower of a relay (`Client::connect_relay`) receives.
pub enum RelayUpdate {
    Line(String),
    /// The other end closed it, or the connection to the agent was lost.
    Closed,
}
type RelaySubscriber = Box<dyn FnMut(RelayUpdate) + Send>;

/// The followers of relays, and what arrived for a relay before its
/// follower: the agent may read the program's first lines before the
/// response that names the relay gets here.
#[derive(Default)]
struct Relays {
    subscribers: HashMap<u64, RelaySubscriber>,
    early: HashMap<u64, Vec<RelayUpdate>>,
}

/// Lines kept for a relay nobody follows yet.
const EARLY_LINES: usize = 1000;

pub struct Client {
    process: Mutex<Option<std::process::Child>>,
    close_stream: Mutex<Option<CloseStream>>,
    /// The connected agent is from a different build than the one that would be launched now.
    outdated: std::sync::atomic::AtomicBool,
    connected: Arc<std::sync::atomic::AtomicBool>,
    writer: Mutex<Box<dyn Write + Send>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Callback>>>,
    subscribers: Arc<Mutex<HashMap<TermId, Subscriber>>>,
    /// Receive the events not tied to a specific terminal (activity).
    watchers: Arc<Mutex<Vec<Watcher>>>,
    on_disconnect: Arc<Mutex<Vec<OnDisconnect>>>,
    relays: Arc<Mutex<Relays>>,
    /// The server's `ssh` destination (or `wsl:<distro>`), for remote connections.
    destination: Option<String>,
    /// Ports of the server forwarded to this machine: remote → local.
    forwards: Mutex<HashMap<u16, ssh::Forward>>,
}

/// Copy of the agent binary to start the daemon from. On macOS, rebuilding a
/// running binary can kill the process (the system detects that its code
/// changed): the daemon, which holds the terminals, can't run from
/// `target/`. The copy's name includes the binary's timestamp, so a new build
/// gets a new copy; older ones are deleted (an old agent still running
/// doesn't notice: its file keeps existing for it until it exits).
fn stable_copy(agent_bin: &Path, state_dir: &Path) -> Result<std::path::PathBuf> {
    let dir = state_dir.join("agents");
    std::fs::create_dir_all(&dir)?;
    let modified = std::fs::metadata(agent_bin)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = format!("sik-agent-{PROTOCOL}-{modified}{}", std::env::consts::EXE_SUFFIX);
    let copy = dir.join(&name);
    if !copy.exists() {
        let partial = dir.join(format!("{name}.part"));
        std::fs::copy(agent_bin, &partial)?;
        std::fs::rename(&partial, &copy)?;
    }
    for entry in std::fs::read_dir(&dir)?.flatten() {
        if entry.file_name() != name.as_str() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(copy)
}

impl Drop for Client {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl Client {
    /// Close only this connection. The agent keeps its terminals, and the UI
    /// can reattach from snapshots after a transport or output overflow.
    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::Relaxed);
        // Closing just the writer leaves the reader's cloned socket alive.
        // Shut down both directions to wake the reader and notify the agent.
        if let Some(close) = self.close_stream.lock().unwrap().take() {
            close();
        }
        if let Some(mut process) = self.process.lock().unwrap().take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

impl Client {
    /// Connects to the local agent for this protocol; if none is alive, starts
    /// `agent_bin daemon` and waits for it to listen.
    pub fn connect_local(agent_bin: &Path) -> Result<Arc<Self>> {
        let socket = proto::socket_path()?;
        if let Ok(client) = Self::connect(&socket) {
            match smol::block_on(client.request(Request::Hello { protocol: PROTOCOL }))? {
                Response::Hello { protocol, .. } if protocol == PROTOCOL => {
                    client.check_version(agent_bin);
                    return Ok(client);
                }
                other => bail!("the agent at {} does not speak protocol {PROTOCOL}: {other:?}", socket.display()),
            }
        }

        let state_dir = proto::state_dir()?;
        std::fs::create_dir_all(&state_dir)?;
        let agent = stable_copy(agent_bin, &state_dir)
            .with_context(|| format!("could not copy {}", agent_bin.display()))?;
        platform::spawn_daemon(&agent, &state_dir.join("agent.log"))
            .with_context(|| format!("could not start {}", agent.display()))?;
        for _ in 0..250 {
            std::thread::sleep(Duration::from_millis(20));
            if let Ok(client) = Self::connect(&socket) {
                return Ok(client);
            }
        }
        bail!("the agent did not start; see {}", state_dir.join("agent.log").display())
    }

    fn connect(socket: &Path) -> Result<Arc<Self>> {
        let (reader, writer, close) = platform::connect(socket)?;
        Ok(Self::from_stream(reader, writer, None, Some(close), None))
    }

    /// A client over any stream (the local socket or `ssh … bridge`).
    /// `process` is the process backing it, if any: it dies with the client.
    fn from_stream(
        reader: Box<dyn std::io::Read + Send>,
        writer: Box<dyn Write + Send>,
        process: Option<std::process::Child>,
        close_stream: Option<CloseStream>,
        destination: Option<String>,
    ) -> Arc<Self> {
        let client = Arc::new(Self {
            process: Mutex::new(process),
            close_stream: Mutex::new(close_stream),
            outdated: std::sync::atomic::AtomicBool::new(false),
            connected: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            subscribers: Arc::default(),
            watchers: Arc::default(),
            on_disconnect: Arc::default(),
            relays: Arc::default(),
            destination,
            forwards: Mutex::default(),
        });
        let pending = client.pending.clone();
        let subscribers = client.subscribers.clone();
        let watchers = client.watchers.clone();
        let connected = client.connected.clone();
        let on_disconnect = client.on_disconnect.clone();
        let relays = client.relays.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            while let Ok(Some(decoded)) = proto::read_message::<ServerMessage, ServerEnvelope>(&mut reader) {
                if !connected.load(Ordering::Relaxed) {
                    break;
                }
                let message = match decoded {
                    Decoded::Known(message) => message,
                    // From a newer agent: its response fails, its events are ignored.
                    Decoded::Unknown { id } => {
                        if let Some(callback) = id.and_then(|id| pending.lock().unwrap().remove(&id)) {
                            callback(Err(anyhow!("agent response this app does not understand")));
                        }
                        continue;
                    }
                };
                match message {
                    ServerMessage::Response { id, result } => {
                        if let Some(callback) = pending.lock().unwrap().remove(&id) {
                            callback(result.map_err(|err| anyhow!(err)));
                        }
                    }
                    ServerMessage::Event(event @ (Event::Activity { .. } | Event::Blocked { .. } | Event::OpenTask { .. } | Event::FsChanged { .. } | Event::Open { .. } | Event::Command { .. } | Event::Agents { .. })) => {
                        for watcher in watchers.lock().unwrap().iter() {
                            watcher(&event);
                        }
                    }
                    ServerMessage::Event(Event::RelayLine { relay, line }) => {
                        let mut relays = relays.lock().unwrap();
                        match relays.subscribers.get_mut(&relay) {
                            Some(subscriber) => subscriber(RelayUpdate::Line(line)),
                            None => {
                                let early = relays.early.entry(relay).or_default();
                                if early.len() < EARLY_LINES {
                                    early.push(RelayUpdate::Line(line));
                                }
                            }
                        }
                    }
                    ServerMessage::Event(Event::RelayClosed { relay }) => {
                        let mut relays = relays.lock().unwrap();
                        match relays.subscribers.remove(&relay) {
                            Some(mut subscriber) => subscriber(RelayUpdate::Closed),
                            None => relays.early.entry(relay).or_default().push(RelayUpdate::Closed),
                        }
                    }
                    ServerMessage::Event(event) => {
                        let term = match &event {
                            Event::TermOutput { term, .. }
                            | Event::TermTitle { term, .. }
                            | Event::TermExit { term } => *term,
                            Event::Activity { .. }
                            | Event::Blocked { .. }
                            | Event::OpenTask { .. }
                            | Event::FsChanged { .. }
                            | Event::Open { .. }
                            | Event::Command { .. }
                            | Event::Agents { .. }
                            | Event::RelayLine { .. }
                            | Event::RelayClosed { .. } => unreachable!(),
                        };
                        let exit = matches!(event, Event::TermExit { .. });
                        let mut subscribers = subscribers.lock().unwrap();
                        if let Some(subscriber) = subscribers.get(&term) {
                            subscriber(TermUpdate::Event(event));
                        }
                        if exit {
                            subscribers.remove(&term);
                        }
                    }
                }
            }
            // Connection lost: requests fail and terminals become disconnected
            // (they stay alive in the agent).
            connected.store(false, Ordering::Relaxed);
            for (_, callback) in pending.lock().unwrap().drain() {
                callback(Err(anyhow!("lost the connection to the agent")));
            }
            for (_, subscriber) in subscribers.lock().unwrap().drain() {
                subscriber(TermUpdate::Disconnected);
            }
            for (_, mut subscriber) in relays.lock().unwrap().subscribers.drain() {
                subscriber(RelayUpdate::Closed);
            }
            for callback in on_disconnect.lock().unwrap().drain(..) {
                callback();
            }
        });
        client
    }

    /// The URL to open on this machine for `url`, seen from the agent's
    /// machine. On a server, a URL pointing at the server itself
    /// (`localhost:5173`) gets its port forwarded over SSH; other URLs come
    /// back unchanged. Blocks while `ssh` runs.
    pub fn local_url(&self, url: &str) -> Result<String> {
        let Some(destination) = &self.destination else {
            return Ok(url.to_string());
        };
        let Some(target) = ssh::LoopbackUrl::parse(url) else {
            return Ok(url.to_string());
        };
        let mut forwards = self.forwards.lock().unwrap();
        let local = match forwards.get(&target.port) {
            Some(forward) => forward.port,
            None => {
                let forward = ssh::forward(destination, &target)?;
                let port = forward.port;
                forwards.insert(target.port, forward);
                port
            }
        };
        Ok(target.with_port(local))
    }

    /// Checks whether the connected agent is the `expected` binary (its fingerprint);
    /// one that doesn't know the request predates it, so it's also old.
    fn check_version(&self, expected: &Path) {
        let Ok(bytes) = std::fs::read(expected) else {
            return;
        };
        let current = match smol::block_on(self.request(Request::Version)) {
            Ok(Response::Text(id)) => id,
            _ => String::new(),
        };
        self.outdated
            .store(current != proto::build_id(&bytes), Ordering::Relaxed);
    }

    /// The connected agent is from an older build: restarting it (with
    /// `Shutdown`) starts the new one, at the cost of closing its terminals.
    pub fn outdated(&self) -> bool {
        self.outdated.load(Ordering::Relaxed)
    }

    /// Whether the connection to the agent is still alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn send(&self, id: Option<u64>, request: Request) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        proto::write_frame(&mut *writer, &ClientMessage { id, request })
    }

    /// Sends a request and calls `callback` with the response (from another thread).
    pub fn request_with(&self, request: Request, callback: impl FnOnce(Result<Response>) + Send + 'static) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(id, Box::new(callback));
        if let Err(err) = self.send(Some(id), request)
            && let Some(callback) = self.pending.lock().unwrap().remove(&id)
        {
            callback(Err(err));
        }
    }

    /// Sends a request and waits for the response.
    pub fn request(&self, request: Request) -> impl Future<Output = Result<Response>> + use<> {
        let (tx, rx) = smol::channel::bounded(1);
        self.request_with(request, move |result| {
            let _ = tx.try_send(result);
        });
        async move { rx.recv().await.map_err(|_| anyhow!("no response from the agent"))? }
    }

    /// Sends a request without waiting for a response.
    pub fn notify(&self, request: Request) {
        let _ = self.send(None, request);
    }

    /// Receives a terminal's events (from another thread) until it exits.
    /// Calls `callback` (from another thread) when the connection is lost; if it
    /// already was, right away.
    pub fn on_disconnect(&self, callback: impl FnOnce() + Send + 'static) {
        let mut callbacks = self.on_disconnect.lock().unwrap();
        if self.is_connected() {
            callbacks.push(Box::new(callback));
        } else {
            drop(callbacks);
            callback();
        }
    }

    pub fn subscribe(&self, term: TermId, subscriber: impl Fn(TermUpdate) + Send + 'static) {
        self.subscribers.lock().unwrap().insert(term, Box::new(subscriber));
    }

    /// Receives (from another thread) the events not tied to a specific terminal.
    pub fn watch(&self, watcher: impl Fn(&Event) + Send + 'static) {
        self.watchers.lock().unwrap().push(Box::new(watcher));
    }

    /// Connects a relay to `port` on the agent's machine (`Request::RelayConnect`)
    /// and calls `subscriber` (from another thread) with each line it reads.
    /// It gets every line, also those the agent read before the response
    /// that names the relay arrived.
    pub fn connect_relay<S: FnMut(RelayUpdate) + Send + 'static>(
        &self,
        port: u16,
        mut subscriber: S,
    ) -> impl Future<Output = Result<u64>> + use<S> {
        let relays = self.relays.clone();
        let (tx, rx) = smol::channel::bounded(1);
        // Runs in the reader thread before it reads the relay's first line.
        self.request_with(Request::RelayConnect { port }, move |result| {
            let result = match result {
                Ok(Response::Relay(relay)) => {
                    let mut relays = relays.lock().unwrap();
                    let mut closed = false;
                    for update in relays.early.remove(&relay).unwrap_or_default() {
                        closed |= matches!(update, RelayUpdate::Closed);
                        subscriber(update);
                    }
                    if !closed {
                        relays.subscribers.insert(relay, Box::new(subscriber));
                    }
                    Ok(relay)
                }
                Ok(other) => Err(anyhow!("unexpected response {other:?}")),
                Err(err) => Err(err),
            };
            let _ = tx.try_send(result);
        });
        async move { rx.recv().await.map_err(|_| anyhow!("no response from the agent"))? }
    }

    /// Writes a line to a relay.
    pub fn relay_send(&self, relay: u64, line: String) {
        self.notify(Request::RelaySend { relay, line });
    }

    pub fn close_relay(&self, relay: u64) {
        let mut relays = self.relays.lock().unwrap();
        relays.subscribers.remove(&relay);
        relays.early.remove(&relay);
        drop(relays);
        self.notify(Request::RelayClose { relay });
    }

    pub fn unsubscribe(&self, term: TermId) {
        self.subscribers.lock().unwrap().remove(&term);
        self.notify(Request::TermDetach { term });
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{io::Read as _, os::unix::net::UnixStream, sync::mpsc};

    #[test]
    fn dropping_local_client_closes_socket_and_notifies_listeners() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (reader, writer, close) = platform::split(stream).unwrap();
        let client = Client::from_stream(reader, writer, None, Some(close), None);
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        let (tx, rx) = mpsc::channel();
        let request_tx = tx.clone();
        client.request_with(Request::TaskList, move |result| {
            assert!(result.is_err());
            request_tx.send("request").unwrap();
        });
        let terminal_tx = tx.clone();
        client.subscribe(1, move |update| {
            assert!(matches!(update, TermUpdate::Disconnected));
            terminal_tx.send("terminal").unwrap();
        });
        client.on_disconnect(move || tx.send("disconnect").unwrap());
        assert!(matches!(proto::read_frame::<ClientMessage>(&mut peer).unwrap(),
            Some(ClientMessage { request: Request::TaskList, .. })));

        drop(client);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0, "the agent must see EOF");
        let mut notifications: Vec<_> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        notifications.sort();
        assert_eq!(notifications, vec!["disconnect", "request", "terminal"]);
    }

    #[test]
    fn explicit_disconnect_fails_pending_requests_and_notifies_the_ui() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let (reader, writer, close) = platform::split(stream).unwrap();
        let client = Client::from_stream(reader, writer, None, Some(close), None);
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (tx, rx) = mpsc::channel();
        client.request_with(Request::TaskList, move |result| { tx.send(result.is_err()).unwrap(); });
        assert!(proto::read_frame::<ClientMessage>(&mut peer).unwrap().is_some());
        let (tx, disconnected) = mpsc::channel();
        client.on_disconnect(move || { tx.send(()).unwrap(); });
        client.disconnect();
        assert!(!client.is_connected());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        disconnected.recv_timeout(Duration::from_secs(2)).unwrap();
        client.disconnect(); // idempotent; the Arc can remain alive in views.
    }
}
