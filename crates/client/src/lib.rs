//! Connection from the UI to an agent. A thread reads the agent's messages
//! and dispatches responses and events; writing is a direct call.

mod platform;
mod ssh;

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

/// What a terminal's follower receives.
pub enum TermUpdate {
    Event(Event),
    /// The connection to the agent was lost: the terminal is still alive there
    /// and can be reattached on reconnect.
    Disconnected,
}
type Watcher = Box<dyn Fn(&Event) + Send>;

pub struct Client {
    process: Mutex<Option<std::process::Child>>,
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
    /// The server's `ssh` destination, for connections over SSH.
    destination: Option<String>,
    /// Ports of the server forwarded to this machine: remote → local.
    forwards: Mutex<HashMap<u16, u16>>,
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
    let name = format!("{}-{PROTOCOL}-{modified}", proto::AGENT_BIN);
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
        let (reader, writer) = platform::connect(socket)?;
        Ok(Self::from_stream(reader, writer, None, None))
    }

    /// A client over any stream (the local socket or `ssh … bridge`).
    /// `process` is the process backing it, if any: it dies with the client.
    fn from_stream(
        reader: Box<dyn std::io::Read + Send>,
        writer: Box<dyn Write + Send>,
        process: Option<std::process::Child>,
        destination: Option<String>,
    ) -> Arc<Self> {
        let client = Arc::new(Self {
            process: Mutex::new(process),
            outdated: std::sync::atomic::AtomicBool::new(false),
            connected: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            subscribers: Arc::default(),
            watchers: Arc::default(),
            on_disconnect: Arc::default(),
            destination,
            forwards: Mutex::default(),
        });
        let pending = client.pending.clone();
        let subscribers = client.subscribers.clone();
        let watchers = client.watchers.clone();
        let connected = client.connected.clone();
        let on_disconnect = client.on_disconnect.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            while let Ok(Some(decoded)) = proto::read_message::<ServerMessage, ServerEnvelope>(&mut reader) {
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
                    ServerMessage::Event(event @ (Event::Activity { .. } | Event::Blocked { .. } | Event::OpenTask { .. } | Event::FsChanged { .. })) => {
                        for watcher in watchers.lock().unwrap().iter() {
                            watcher(&event);
                        }
                    }
                    ServerMessage::Event(event) => {
                        let term = match &event {
                            Event::TermOutput { term, .. }
                            | Event::TermTitle { term, .. }
                            | Event::TermExit { term } => *term,
                            Event::Activity { .. } | Event::Blocked { .. } | Event::OpenTask { .. } | Event::FsChanged { .. } => unreachable!(),
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
            Some(local) => *local,
            None => {
                let local = ssh::forward(destination, &target)?;
                forwards.insert(target.port, local);
                local
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

    pub fn unsubscribe(&self, term: TermId) {
        self.subscribers.lock().unwrap().remove(&term);
        self.notify(Request::TermDetach { term });
    }
}
