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
/// A follower the reader calls without holding the list it's in: following
/// may add or drop followers, which locks that list.
type Shared<T> = Arc<Mutex<T>>;
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
/// Each terminal's followers, and the watchers, with their ids.
type Subscribers = HashMap<TermId, Vec<(u64, Shared<Subscriber>)>>;
type Watchers = Vec<(u64, Shared<Watcher>)>;

/// Keeps a watcher (`Client::watch_scoped`, `Client::watch_fs`) registered;
/// dropping it unregisters it (an event already being delivered still is).
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
    subscribers: HashMap<u64, Shared<RelaySubscriber>>,
    early: HashMap<u64, Vec<RelayUpdate>>,
    /// Closed here while still open in the agent: what it still sends for
    /// them is dropped, until its `RelayClosed`.
    closed: HashSet<u64>,
}

/// Lines kept for a relay nobody follows yet.
const EARLY_LINES: usize = 1000;

/// How long connecting waits for each of the agent's answers: a hung agent,
/// or an `ssh` stuck on the way, would otherwise keep it waiting forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Client {
    process: Mutex<Option<std::process::Child>>,
    close_stream: Mutex<Option<CloseStream>>,
    /// The connected agent is from a different build than the one that would be launched now.
    outdated: std::sync::atomic::AtomicBool,
    /// When outdated, the new agent's binary on the agent's machine, for it
    /// to replace itself with.
    replacement: Mutex<Option<PathBuf>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    /// What goes to the agent, written by a thread of its own: sending never
    /// waits for the connection, and keys get ahead of large requests.
    outgoing: Arc<Outgoing>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Callback>>>,
    /// Several per terminal (two windows may show the same one), each with
    /// the id `subscribe` returned.
    subscribers: Arc<Mutex<Subscribers>>,
    /// Receive the events not tied to a specific terminal (activity), each
    /// with its id.
    watchers: Arc<Mutex<Watchers>>,
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
/// `target/`. The copy's name includes the binary's fingerprint, so a new
/// build gets a new copy, which `den` links to; older ones are deleted (an
/// old agent still running doesn't notice: its file keeps existing for it
/// until it exits).
fn stable_copy(agent_bin: &Path, state_dir: &Path) -> Result<std::path::PathBuf> {
    let dir = state_dir.join("agents");
    std::fs::create_dir_all(&dir)?;
    let copy = copy_agent(agent_bin, &dir)?;
    // The `den` links would point to a deleted copy until the agent restarts.
    if let Err(err) = proto::link_den(&copy) {
        eprintln!("could not link `den` to {}: {err:#}", copy.display());
    }
    remove_other_copies(&dir, &copy)?;
    Ok(copy)
}

/// What a copy is called while it's being made.
const PARTIAL: &str = ".part";

/// The copy of `agent_bin` in `dir`, made if it isn't there.
fn copy_agent(agent_bin: &Path, dir: &Path) -> Result<PathBuf> {
    let id = fingerprint(agent_bin)?;
    let name = format!("den-agent-{PROTOCOL}-{id}{}", std::env::consts::EXE_SUFFIX);
    let copy = dir.join(&name);
    if !copy.exists() {
        // Each instance of the app copies to a file of its own: two may
        // start at once.
        let partial = dir.join(format!("{name}.{}{PARTIAL}", std::process::id()));
        std::fs::copy(agent_bin, &partial)?;
        // The other one's may be there already, and running (Windows can't
        // replace it then): it's the same.
        if let Err(err) = std::fs::rename(&partial, &copy) {
            let _ = std::fs::remove_file(&partial);
            if !copy.exists() {
                return Err(err.into());
            }
        }
    }
    Ok(copy)
}

/// A binary's fingerprint (`proto::build_id`), remembered while its size and
/// modification time stay the same: every connection needs the agent's, and
/// it's megabytes to read and hash.
fn fingerprint(path: &Path) -> Result<String> {
    /// By path: its size, modification time and fingerprint.
    type Known = HashMap<PathBuf, (u64, std::time::SystemTime, String)>;
    static KNOWN: std::sync::LazyLock<Mutex<Known>> = std::sync::LazyLock::new(Mutex::default);
    let metadata = std::fs::metadata(path)?;
    let stamp = (metadata.len(), metadata.modified()?);
    if let Some((len, modified, id)) = KNOWN.lock().unwrap().get(path)
        && (*len, *modified) == stamp
    {
        return Ok(id.clone());
    }
    let id = proto::build_id(&std::fs::read(path)?);
    KNOWN.lock().unwrap().insert(path.to_path_buf(), (stamp.0, stamp.1, id.clone()));
    Ok(id)
}

/// Deletes the copies of other builds, but not one another instance of the
/// app is making: a recent `.part` (an old one was left by one that died).
fn remove_other_copies(dir: &Path, copy: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let copying = entry.file_name().to_string_lossy().ends_with(PARTIAL)
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified.elapsed().unwrap_or_default() < Duration::from_secs(600));
        if entry.path() != copy && !copying {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
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
    /// What a terminal's view sends (`urgent`): what someone is waiting to see.
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
            match client.request_within(Request::Hello { protocol: PROTOCOL }, ANSWER_TIMEOUT)? {
                Response::Hello { protocol, .. } if protocol == PROTOCOL => {
                    client.check_version(agent_bin, || {
                        let state_dir = proto::state_dir().ok()?;
                        stable_copy(agent_bin, &state_dir).ok()
                    })?;
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
            replacement: Mutex::new(None),
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
                        let callback = pending.lock().unwrap().remove(&id);
                        if let Some(callback) = callback {
                            callback(result.map_err(|err| anyhow!(err)));
                        }
                    }
                    ServerMessage::Event(event @ (Event::Activity { .. } | Event::Blocked { .. } | Event::OpenTask { .. } | Event::FsChanged { .. } | Event::Open { .. } | Event::Command { .. } | Event::Agents { .. })) => {
                        let watchers: Vec<_> = watchers.lock().unwrap().iter().map(|(_, watcher)| watcher.clone()).collect();
                        for watcher in watchers {
                            (*watcher.lock().unwrap())(&event);
                        }
                    }
                    ServerMessage::Event(Event::RelayLine { relay, line }) => {
                        let mut relays = relays.lock().unwrap();
                        match relays.subscribers.get(&relay).cloned() {
                            Some(subscriber) => {
                                drop(relays);
                                (*subscriber.lock().unwrap())(RelayUpdate::Line(line));
                            }
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
                            Some(subscriber) => {
                                drop(relays);
                                (*subscriber.lock().unwrap())(RelayUpdate::Closed);
                            }
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
                        let list: Vec<_> = {
                            let mut subscribers = subscribers.lock().unwrap();
                            let list = subscribers.get(&term).into_iter().flatten().map(|(_, subscriber)| subscriber.clone()).collect();
                            if exit {
                                subscribers.remove(&term);
                            }
                            list
                        };
                        if let Some((last, others)) = list.split_last() {
                            for subscriber in others {
                                (*subscriber.lock().unwrap())(TermUpdate::Event(copy_term_event(&event)));
                            }
                            (*last.lock().unwrap())(TermUpdate::Event(event));
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
            // Each list is taken before calling what's in it, which may lock it.
            let callbacks = std::mem::take(&mut *pending.lock().unwrap());
            for (_, callback) in callbacks {
                callback(Err(anyhow!("lost the connection to the agent")));
            }
            let terminals = std::mem::take(&mut *subscribers.lock().unwrap());
            for (_, subscriber) in terminals.into_values().flatten() {
                (*subscriber.lock().unwrap())(TermUpdate::Disconnected);
            }
            let followers = std::mem::take(&mut relays.lock().unwrap().subscribers);
            for (_, subscriber) in followers {
                (*subscriber.lock().unwrap())(RelayUpdate::Closed);
            }
            let callbacks = std::mem::take(&mut *on_disconnect.lock().unwrap());
            for callback in callbacks {
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
        if let Some(forward) = self.forwards.lock().unwrap().get(&target.port) {
            return Ok(target.with_port(forward.port));
        }
        // Not holding the lock while `ssh` runs: other URLs needn't wait.
        let forward = ssh::forward(destination, &target)?;
        // Another call may have forwarded it meanwhile: the first one stays,
        // and dropping this one cancels it.
        let local = self.forwards.lock().unwrap().entry(target.port).or_insert(forward).port;
        Ok(target.with_port(local))
    }

    /// Checks whether the connected agent is the `expected` binary (its fingerprint);
    /// one that doesn't know the request predates it, so it's also old.
    /// `replacement`: where the agent finds that binary, if it's old.
    /// Fails only when the agent doesn't answer.
    fn check_version(&self, expected: &Path, replacement: impl FnOnce() -> Option<PathBuf>) -> Result<()> {
        let Ok(expected) = fingerprint(expected) else {
            return Ok(());
        };
        let current = match self.request_within(Request::Version, ANSWER_TIMEOUT) {
            Ok(Response::Text(id)) => id,
            Err(err) if err.is::<NoAnswer>() => return Err(err),
            _ => String::new(),
        };
        let outdated = current != expected;
        self.outdated.store(outdated, Ordering::Relaxed);
        if outdated {
            *self.replacement.lock().unwrap() = replacement();
        }
        Ok(())
    }

    /// The connected agent is from an older build: `restart` starts the new one.
    pub fn outdated(&self) -> bool {
        self.outdated.load(Ordering::Relaxed)
    }

    /// Restarts the agent with the new build: it replaces itself, keeping
    /// its terminals running. One that can't (older than that, on Windows)
    /// shuts down, and the next to start reopens them.
    pub fn restart(self: &Arc<Self>) {
        let Some(exe) = self.replacement.lock().unwrap().clone() else {
            return self.notify(Request::Shutdown);
        };
        let client = Arc::downgrade(self);
        self.request_with(Request::ReplaceAgent { exe }, move |result| {
            // Replaced, the connection closes; still open, the agent refused.
            if result.is_err()
                && let Some(client) = client.upgrade()
                && client.is_connected()
            {
                client.notify(Request::Shutdown);
            }
        });
    }

    /// Whether the connection to the agent is still alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn send(&self, id: Option<u64>, request: Request) -> Result<()> {
        let urgent = urgent(&request);
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
        if let Err(err) = result {
            let callback = self.pending.lock().unwrap().remove(&id);
            if let Some(callback) = callback {
                callback(Err(err));
            }
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

    /// Sends a request and blocks until the response, for at most `timeout`:
    /// an agent that doesn't answer by then is disconnected.
    fn request_within(&self, request: Request, timeout: Duration) -> Result<Response> {
        let response = smol::block_on(smol::future::or(async { Some(self.request(request).await) }, async {
            smol::Timer::after(timeout).await;
            None
        }));
        response.unwrap_or_else(|| {
            self.disconnect();
            Err(NoAnswer.into())
        })
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
        let subscriber: Subscriber = Box::new(subscriber);
        self.subscribers.lock().unwrap().entry(term).or_default().push((id, Arc::new(Mutex::new(subscriber))));
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
        self.watchers.lock().unwrap().push((id, Arc::new(Mutex::new(watcher))));
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
                    // Nothing new arrives for it meanwhile: this is the reader.
                    let early = relays.lock().unwrap().early.remove(&relay).unwrap_or_default();
                    let mut closed = false;
                    for update in early {
                        closed |= matches!(update, RelayUpdate::Closed);
                        subscriber(update);
                    }
                    if !closed {
                        let subscriber: RelaySubscriber = Box::new(subscriber);
                        relays.lock().unwrap().subscribers.insert(relay, Arc::new(Mutex::new(subscriber)));
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

/// Whether a request goes ahead of the others: a terminal's keys and size,
/// and with them whatever else its view sends, which they must not overtake
/// (keys typed after a clear go to the cleared screen, a detach stays after
/// its attach).
fn urgent(request: &Request) -> bool {
    matches!(
        request,
        Request::TermInput { .. }
            | Request::TermResize { .. }
            | Request::TermAttach { .. }
            | Request::TermDetach { .. }
            | Request::TermClear { .. }
            | Request::TermKill { .. }
    )
}

/// The agent didn't answer in time.
#[derive(Debug)]
struct NoAnswer;

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the agent did not answer")
    }
}

impl std::error::Error for NoAnswer {}

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
    fn an_agent_that_does_not_answer_is_given_up_on() {
        let (client, mut peer) = pair();
        let result = client.request_within(Request::Hello { protocol: PROTOCOL }, Duration::from_millis(100));
        assert!(result.unwrap_err().is::<NoAnswer>());
        assert!(!client.is_connected());
        assert!(proto::read_frame::<ClientMessage>(&mut peer).unwrap().is_some());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0, "the agent must see EOF");
    }

    #[test]
    fn a_callback_may_use_the_client() {
        let (client, mut peer) = pair();
        let (tx, rx) = mpsc::channel();
        let this = Arc::downgrade(&client);
        let first_tx = tx.clone();
        client.request_with(Request::TaskList, move |_| {
            let client = this.upgrade().unwrap();
            client.request_with(Request::TaskList, |_| {});
            first_tx.send("answered").unwrap();
            client.on_disconnect(move || first_tx.send("disconnect").unwrap());
        });
        let Some(ClientMessage { id: Some(id), .. }) = proto::read_frame::<ClientMessage>(&mut peer).unwrap() else {
            panic!("no request");
        };
        proto::write_frame(&mut peer, &ServerMessage::Response { id, result: Ok(Response::Ok) }).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "answered");
        assert!(proto::read_frame::<ClientMessage>(&mut peer).unwrap().is_some(), "the request made in the callback");
        let this = Arc::downgrade(&client);
        client.on_disconnect(move || {
            this.upgrade().unwrap().on_disconnect(move || tx.send("late").unwrap());
        });
        drop(peer);
        let mut got: Vec<_> = (0..2).map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap()).collect();
        got.sort();
        assert_eq!(got, vec!["disconnect", "late"]);
    }

    #[test]
    fn a_build_gets_a_copy_of_its_own() {
        let dir = std::env::temp_dir().join(format!("den-stable-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let agents = dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let bin = dir.join("den-agent");
        std::fs::write(&bin, "first").unwrap();
        let in_flight = agents.join("den-agent-x.1.part");
        std::fs::write(&in_flight, "another instance's").unwrap();
        let first = copy_agent(&bin, &agents).unwrap();
        // Rebuilt within the same second: still a new copy.
        std::fs::write(&bin, "second").unwrap();
        let second = copy_agent(&bin, &agents).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        remove_other_copies(&agents, &second).unwrap();
        assert!(!first.exists(), "the old copy is deleted");
        assert!(in_flight.exists(), "another instance's copy in progress is left alone");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_stay_after_what_the_terminal_sent_before() {
        let outgoing = Outgoing::default();
        let push = |request: Request| {
            let urgent = urgent(&request);
            outgoing.push(proto::encode_frame(&ClientMessage { id: None, request }).unwrap(), urgent);
        };
        push(Request::TaskList);
        push(Request::TermClear { term: 1 });
        push(Request::TermInput { term: 1, data: b"ls".to_vec() });
        push(Request::TermDetach { term: 1 });
        let mut order = Vec::new();
        for _ in 0..4 {
            let frame = outgoing.next().unwrap();
            let Some(message) = proto::read_frame::<ClientMessage>(&mut frame.as_slice()).unwrap() else { panic!("no frame") };
            order.push(match message.request {
                Request::TaskList => "tasks",
                Request::TermClear { .. } => "clear",
                Request::TermInput { .. } => "keys",
                Request::TermDetach { .. } => "detach",
                _ => "other",
            });
        }
        assert_eq!(order, vec!["clear", "keys", "detach", "tasks"]);
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
