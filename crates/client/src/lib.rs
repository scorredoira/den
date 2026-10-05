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
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
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

/// Keeps a watcher (`Client::watch_scoped`, `Client::watch_fs`) registered;
/// dropping it unregisters it.
pub struct Watch {
    client: Weak<Client>,
    id: u64,
    /// The folder it has the agent watch (`watch_fs`), if any.
    root: Option<PathBuf>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(client) = self.client.upgrade() {
            client.unwatch(self.id, self.root.as_deref());
        }
    }
}

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
    /// Closed here while still open in the agent: what it still sends for
    /// them is dropped, until its `RelayClosed`.
    closed: HashSet<u64>,
}

/// Lines kept for a relay nobody follows yet.
const EARLY_LINES: usize = 1000;

pub struct Client {
    process: Mutex<Option<std::process::Child>>,
    close_stream: Mutex<Option<CloseStream>>,
    /// The connected agent is from a different build than the one that would be launched now.
    outdated: std::sync::atomic::AtomicBool,
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// What goes to the agent, written by a thread of its own: sending never
    /// waits for the connection, and keys get ahead of large requests.
    outgoing: Arc<Outgoing>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Callback>>>,
    /// Several per terminal (two windows may show the same one), each with
    /// the id `subscribe` returned.
    subscribers: Arc<Mutex<HashMap<TermId, Vec<(u64, Subscriber)>>>>,
    /// Receive the events not tied to a specific terminal (activity), each
    /// with its id.
    watchers: Arc<Mutex<Vec<(u64, Watcher)>>>,
    /// How many watchers want each folder's changes: the agent is told to stop
    /// (`Request::Unwatch`) when the last one goes.
    watched: Mutex<HashMap<PathBuf, usize>>,
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
    let name = format!("den-agent-{PROTOCOL}-{modified}{}", std::env::consts::EXE_SUFFIX);
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

/// The frames waiting to be written.
#[derive(Default)]
struct Outgoing {
    queues: Mutex<Queues>,
    ready: std::sync::Condvar,
}

#[derive(Default)]
struct Queues {
    /// A terminal's keys and size: what someone is waiting to see.
    urgent: std::collections::VecDeque<Vec<u8>>,
    normal: std::collections::VecDeque<Vec<u8>>,
    closed: bool,
}

impl Outgoing {
    fn push(&self, frame: Vec<u8>, urgent: bool) {
        let mut queues = self.queues.lock().unwrap();
        // Disconnected: nobody would write it.
        if queues.closed {
            return;
        }
        match urgent {
            true => queues.urgent.push_back(frame),
            false => queues.normal.push_back(frame),
        }
        self.ready.notify_one();
    }

    fn close(&self) {
        self.queues.lock().unwrap().closed = true;
        self.ready.notify_one();
    }

    /// The next frame to write, the urgent ones first; `None` once closed.
    fn next(&self) -> Option<Vec<u8>> {
        let mut queues = self.queues.lock().unwrap();
        loop {
            if queues.closed {
                return None;
            }
            if let Some(frame) = queues.urgent.pop_front().or_else(|| queues.normal.pop_front()) {
                return Some(frame);
            }
            queues = self.ready.wait(queues).unwrap();
        }
    }
}

impl Client {
    /// Close only this connection. The agent keeps its terminals, and the UI
    /// can reattach from snapshots after a transport or output overflow.
    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.outgoing.close();
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
            outgoing: Arc::default(),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            subscribers: Arc::default(),
            watchers: Arc::default(),
            watched: Mutex::default(),
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
        let outgoing = client.outgoing.clone();
        let this = Arc::downgrade(&client);
        std::thread::spawn(move || {
            let mut writer = writer;
            while let Some(frame) = outgoing.next() {
                if writer.write_all(&frame).and_then(|_| writer.flush()).is_err() {
                    // The reader finds out too, and fails what's pending.
                    if let Some(client) = this.upgrade() {
                        client.disconnect();
                    }
                    break;
                }
            }
        });
        let this = Arc::downgrade(&client);
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
                        for (_, watcher) in watchers.lock().unwrap().iter() {
                            watcher(&event);
                        }
                    }
                    ServerMessage::Event(Event::RelayLine { relay, line }) => {
                        let relays = &mut *relays.lock().unwrap();
                        match relays.subscribers.get_mut(&relay) {
                            Some(subscriber) => subscriber(RelayUpdate::Line(line)),
                            None if relays.closed.contains(&relay) => {}
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
                            None if relays.closed.remove(&relay) => {}
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
                        if let Some(((_, last), others)) = subscribers.get(&term).and_then(|list| list.split_last()) {
                            for (_, subscriber) in others {
                                subscriber(TermUpdate::Event(copy_term_event(&event)));
                            }
                            last(TermUpdate::Event(event));
                        }
                        if exit {
                            subscribers.remove(&term);
                        }
                    }
                }
            }
            // Connection lost: requests fail and terminals become disconnected
            // (they stay alive in the agent). The reader may have stopped on its
            // own (EOF, an I/O error, a bad frame): the transport closes too, or
            // the socket stays half open and `ssh … bridge` lingers.
            connected.store(false, Ordering::Relaxed);
            if let Some(client) = this.upgrade() {
                client.disconnect();
            }
            for (_, callback) in pending.lock().unwrap().drain() {
                callback(Err(anyhow!("lost the connection to the agent")));
            }
            for (_, subscriber) in subscribers.lock().unwrap().drain().flat_map(|(_, list)| list) {
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
    /// The port of an `http(s)` URL of the loopback (`localhost`,
    /// `127.0.0.1`…); `None` for any other URL.
    pub fn loopback_port(url: &str) -> Option<u16> {
        ssh::LoopbackUrl::parse(url).map(|url| url.port)
    }

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
        let urgent = matches!(request, Request::TermInput { .. } | Request::TermResize { .. });
        let frame = proto::encode_frame(&ClientMessage { id, request })?;
        self.outgoing.push(frame, urgent);
        Ok(())
    }

    /// Sends a request and calls `callback` with the response (from another thread).
    pub fn request_with(&self, request: Request, callback: impl FnOnce(Result<Response>) + Send + 'static) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.pending.lock().unwrap().insert(id, Box::new(callback));
        // Checked after inserting: once the reader has failed what was
        // pending, nobody else would answer it.
        let result = match self.is_connected() {
            true => self.send(Some(id), request),
            false => Err(anyhow!("lost the connection to the agent")),
        };
        if let Err(err) = result
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

    /// Returns the id to `unsubscribe` with. Others following the same
    /// terminal keep receiving its events.
    pub fn subscribe(&self, term: TermId, subscriber: impl Fn(TermUpdate) + Send + 'static) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.subscribers.lock().unwrap().entry(term).or_default().push((id, Box::new(subscriber)));
        id
    }

    /// Ends a subscription; the agent stops sending the terminal's output
    /// when it was the last one.
    pub fn unsubscribe(&self, term: TermId, subscription: u64) {
        let mut subscribers = self.subscribers.lock().unwrap();
        let Some(list) = subscribers.get_mut(&term) else { return };
        list.retain(|(id, _)| *id != subscription);
        if list.is_empty() {
            subscribers.remove(&term);
            drop(subscribers);
            self.notify(Request::TermDetach { term });
        }
    }

    /// Receives (from another thread) the events not tied to a specific
    /// terminal, for as long as the client lives.
    pub fn watch(&self, watcher: impl Fn(&Event) + Send + 'static) {
        self.add_watcher(Box::new(watcher));
    }

    /// Like `watch`, until the returned `Watch` is dropped.
    pub fn watch_scoped(self: &Arc<Self>, watcher: impl Fn(&Event) + Send + 'static) -> Watch {
        let id = self.add_watcher(Box::new(watcher));
        Watch { client: Arc::downgrade(self), id, root: None }
    }

    /// Has the agent report changes inside `root`, and receives (from another
    /// thread) the events not tied to a specific terminal, until the returned
    /// `Watch` is dropped.
    pub fn watch_fs(self: &Arc<Self>, root: &Path, watcher: impl Fn(&Event) + Send + 'static) -> Watch {
        let id = self.add_watcher(Box::new(watcher));
        let first = {
            let mut watched = self.watched.lock().unwrap();
            let count = watched.entry(root.to_path_buf()).or_default();
            *count += 1;
            *count == 1
        };
        if first {
            self.notify(Request::Watch { path: root.to_path_buf() });
        }
        Watch { client: Arc::downgrade(self), id, root: Some(root.to_path_buf()) }
    }

    fn add_watcher(&self, watcher: Watcher) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.watchers.lock().unwrap().push((id, watcher));
        id
    }

    /// Undoes `watch_scoped` or `watch_fs`. Another watcher of the same folder
    /// keeps the agent watching it.
    fn unwatch(&self, id: u64, root: Option<&Path>) {
        self.watchers.lock().unwrap().retain(|(watcher, _)| *watcher != id);
        let Some(root) = root else { return };
        let mut watched = self.watched.lock().unwrap();
        let Some(count) = watched.get_mut(root) else { return };
        *count -= 1;
        if *count == 0 {
            watched.remove(root);
            drop(watched);
            self.notify(Request::Unwatch { path: root.to_path_buf() });
        }
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
        // Still open in the agent: it may send more before its `RelayClosed`.
        if relays.subscribers.remove(&relay).is_some() {
            relays.closed.insert(relay);
        }
        relays.early.remove(&relay);
        drop(relays);
        self.notify(Request::RelayClose { relay });
    }
}

/// A terminal's event for one more of its subscribers (`Event` isn't `Clone`).
fn copy_term_event(event: &Event) -> Event {
    match event {
        Event::TermOutput { term, data } => Event::TermOutput { term: *term, data: data.clone() },
        Event::TermTitle { term, title } => Event::TermTitle { term: *term, title: title.clone() },
        Event::TermExit { term } => Event::TermExit { term: *term },
        _ => unreachable!("not a terminal's event"),
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

    fn pair() -> (Arc<Client>, UnixStream) {
        let (stream, peer) = UnixStream::pair().unwrap();
        let (reader, writer, close) = platform::split(stream).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        (Client::from_stream(reader, writer, None, Some(close), None), peer)
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn a_reader_that_stops_closes_the_transport_and_fails_later_requests() {
        let (client, mut peer) = pair();
        // A frame header bigger than any frame stops the reader on its own.
        std::io::Write::write_all(&mut peer, &u32::MAX.to_le_bytes()).unwrap();
        wait_until("the reader to stop", || !client.is_connected());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0, "the agent must see EOF");
        let (tx, rx) = mpsc::channel();
        client.request_with(Request::TaskList, move |result| tx.send(result.is_err()).unwrap());
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "a request after the loss must fail, not hang");
    }

    #[test]
    fn several_subscribers_follow_one_terminal() {
        let (client, mut peer) = pair();
        let (tx, rx) = mpsc::channel();
        let first_tx = tx.clone();
        let first = client.subscribe(7, move |update| {
            if let TermUpdate::Event(Event::TermOutput { data, .. }) = update {
                first_tx.send(("first", data)).unwrap();
            }
        });
        let second = client.subscribe(7, move |update| {
            if let TermUpdate::Event(Event::TermOutput { data, .. }) = update {
                tx.send(("second", data)).unwrap();
            }
        });
        let output = |peer: &mut UnixStream, data: &[u8]| {
            proto::write_frame(peer, &ServerMessage::Event(Event::TermOutput { term: 7, data: data.to_vec() })).unwrap();
        };
        output(&mut peer, b"a");
        let mut got: Vec<_> = (0..2).map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        got.sort();
        assert_eq!(got, vec![("first", b"a".to_vec()), ("second", b"a".to_vec())]);

        // The first going away leaves the second attached.
        client.unsubscribe(7, first);
        output(&mut peer, b"b");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), ("second", b"b".to_vec()));
        client.unsubscribe(7, second);
        assert!(matches!(proto::read_frame::<ClientMessage>(&mut peer).unwrap(),
            Some(ClientMessage { request: Request::TermDetach { term: 7 }, .. })), "detaches only after the last one");
    }

    #[test]
    fn a_folder_stays_watched_until_its_last_watcher_goes() {
        let (client, mut peer) = pair();
        let root = Path::new("/project");
        let first = client.watch_fs(root, |_| {});
        let second = client.watch_fs(root, |_| {});
        assert!(matches!(proto::read_frame::<ClientMessage>(&mut peer).unwrap(),
            Some(ClientMessage { request: Request::Watch { .. }, .. })));
        drop(first);
        assert_eq!(client.watchers.lock().unwrap().len(), 1);
        drop(second);
        assert!(client.watchers.lock().unwrap().is_empty());
        // Next on the wire: no second `Watch`, and `Unwatch` only now.
        assert!(matches!(proto::read_frame::<ClientMessage>(&mut peer).unwrap(),
            Some(ClientMessage { request: Request::Unwatch { .. }, .. })));
    }

    #[test]
    fn what_arrives_for_a_closed_relay_is_dropped() {
        let (client, mut peer) = pair();
        let relay = client.connect_relay(9000, |_| {});
        let Some(ClientMessage { id: Some(id), .. }) = proto::read_frame::<ClientMessage>(&mut peer).unwrap() else {
            panic!("no RelayConnect");
        };
        proto::write_frame(&mut peer, &ServerMessage::Response { id, result: Ok(Response::Relay(3)) }).unwrap();
        assert_eq!(smol::block_on(relay).unwrap(), 3);
        client.close_relay(3);
        for line in ["late", "lines"] {
            proto::write_frame(&mut peer, &ServerMessage::Event(Event::RelayLine { relay: 3, line: line.into() })).unwrap();
        }
        proto::write_frame(&mut peer, &ServerMessage::Event(Event::RelayClosed { relay: 3 })).unwrap();
        wait_until("the agent's RelayClosed", || client.relays.lock().unwrap().closed.is_empty());
        assert!(client.relays.lock().unwrap().early.is_empty());
    }
}
