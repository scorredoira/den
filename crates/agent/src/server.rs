//! The agent: owner of the terminals. Each connection has one thread reading
//! requests and another writing; each terminal, a thread reading its pty.

use std::{
    collections::{HashMap, HashSet},
    io::Read as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc, atomic::{AtomicBool, AtomicUsize, Ordering}},
    time::{Duration, Instant},
};

use alacritty_terminal::{
    Term,
    event::{Event as AlacEvent, EventListener},
    grid::Dimensions as _,
    index::{Column, Line},
    term::{Config, test::TermSize},
    vte::ansi::Processor,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use proto::{
    AgentInfo, ClientEnvelope, ClientMessage, Decoded, Event, PROTOCOL, Request, Response, ServerMessage, TermId, TermInfo,
};

use crate::{
    platform::{self, Listener, Stream},
    pty::Pty,
    blocked, format, fs, git, lsp, ports, search,
    snapshot::{self, snapshot},
    tasks,
    handover::Handover,
};

/// With no terminals and no connected UIs, the agent exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);

/// A task is working while its terminals produced output less than this long ago.
const WORKING_WINDOW: Duration = Duration::from_secs(2);

/// After this long without output, check whether the screen is asking for an answer.
const QUIET: Duration = Duration::from_millis(800);

/// Output counts as work once it keeps coming (no pause of `QUIET`) for this
/// long: a redraw or a stray line from an idle terminal is not work, and
/// would mark the task as finished again after it was seen.
const WORK_BURST: Duration = Duration::from_secs(1);

type ConnId = u64;

/// What limits a slow connection is `MAX_PENDING_OUTPUT`: a burst of small
/// reads from a terminal is many messages but few bytes.
const MAX_PENDING_MESSAGES: usize = 8192;
/// Output of one terminal waiting together is sent in one message up to this.
const MAX_JOINED_OUTPUT: usize = 256 * 1024;
const MAX_PENDING_OUTPUT: usize = 8 * 1024 * 1024;

/// Never wait for a slow connection while holding the agent's state lock.
/// Closing it lets the UI reconnect from a snapshot; the terminals live on.
#[derive(Clone)]
struct Outbox {
    sender: mpsc::SyncSender<QueuedMessage>,
    output_bytes: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    close: Arc<dyn Fn() + Send + Sync>,
}

struct QueuedMessage {
    message: ServerMessage,
    bytes: usize,
    output_bytes: Arc<AtomicUsize>,
}

impl Drop for QueuedMessage {
    fn drop(&mut self) {
        self.output_bytes.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl Outbox {
    fn new(close: Arc<dyn Fn() + Send + Sync>) -> (Self, mpsc::Receiver<QueuedMessage>) {
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_MESSAGES);
        (Self { sender, output_bytes: Arc::default(), closed: Arc::default(), close }, receiver)
    }

    fn disconnect(&self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            (self.close)();
        }
    }

    fn send(&self, message: ServerMessage) -> Result<(), ()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(());
        }
        let bytes = match &message {
            ServerMessage::Event(Event::TermOutput { data, .. }) => data.len(),
            _ => 0,
        };
        if self.output_bytes.try_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
            pending.checked_add(bytes).filter(|bytes| *bytes <= MAX_PENDING_OUTPUT)
        }).is_err() {
            self.disconnect();
            return Err(());
        }
        let message = QueuedMessage { message, bytes, output_bytes: self.output_bytes.clone() };
        if self.sender.try_send(message).is_err() {
            self.disconnect();
            return Err(());
        }
        Ok(())
    }
}

/// Collects the events of the agent's emulator.
#[derive(Clone, Default)]
struct Listener_(Arc<Mutex<Vec<AlacEvent>>>);

impl EventListener for Listener_ {
    fn send_event(&self, event: AlacEvent) {
        self.0.locked().push(event);
    }
}

struct AgentTerm {
    group: String,
    pty: Pty,
    cwd: crate::shell_cwd::ShellCwd,
    emulator: Term<Listener_>,
    parser: Processor,
    events: Listener_,
    title: Option<String>,
    /// The connections attached to it, each with its outbox: its output goes
    /// to them without the agent's lock.
    subscribers: HashMap<ConnId, Outbox>,
    last_output: Instant,
    /// When the output that keeps coming up to `last_output` began.
    burst_start: Instant,
    /// Its first quiet moment has passed: what a shell or a resumed session
    /// prints on starting up is not work, so until then output doesn't mark
    /// the task as working (on an agent restart every task would end up
    /// "finished").
    settled: bool,
    /// Whether the screen (no output since `last_output`) asks for an answer,
    /// and up to which output it was checked.
    blocked: bool,
    checked: Instant,
    /// Its foreground process when last looked at, and the coding agent it
    /// is (`claude`, `codex`…), if one, with its command line.
    foreground: Option<u32>,
    agent: Option<String>,
    agent_args: Option<String>,
}

impl AgentTerm {
    /// Whether it's doing something. Claude Code says so in its title (see
    /// `claude_working`): its redraws (on a resize, a reconnect, a status
    /// line) are not work. Anything else works while its output keeps
    /// coming, with no pause of `QUIET`, for `WORK_BURST`, past what it
    /// printed on starting up.
    fn working(&self) -> bool {
        if let Some(working) = self.claude_title().and_then(claude_working) {
            return working;
        }
        self.settled
            && self.last_output.elapsed() < WORKING_WINDOW
            && self.last_output.duration_since(self.burst_start) >= WORK_BURST
    }

    /// Its title, if Claude Code's and Claude Code still runs: one left by
    /// a Claude Code that died (a spinner, say) is not its. Where the
    /// foreground process can't be read (Windows), the title is all there is.
    fn claude_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .filter(|title| claude_title(title))
            .filter(|_| cfg!(windows) || self.agent.as_deref() == Some("claude"))
    }

    fn broadcast(&self, make: impl Fn() -> Event) {
        for outbox in self.subscribers.values() {
            let _ = outbox.send(ServerMessage::Event(make()));
        }
    }

    /// Notes output, and whether that makes it work.
    fn note_output(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_output) >= QUIET {
            self.burst_start = now;
        }
        self.last_output = now;
        self.blocked = false;
        self.working()
    }
}

/// A terminal has a lock of its own, so one that prints a lot holds up
/// only itself. Whoever takes both takes the agent's first: never the
/// agent's while holding a terminal's.
type SharedTerm = Arc<Mutex<AgentTerm>>;

#[derive(Default)]
struct State {
    terms: HashMap<TermId, SharedTerm>,
    clients: HashMap<ConnId, Outbox>,
    next_term: TermId,
    next_conn: ConnId,
    idle_since: Option<Instant>,
    /// Groups (tasks) with some terminal producing output.
    working: HashSet<String>,
    /// Groups with some terminal waiting for an answer.
    blocked: HashSet<String>,
    /// Folders each connection is watching.
    watchers: HashMap<ConnId, HashMap<PathBuf, notify::RecommendedWatcher>>,
    /// The relays each connection opened.
    relays: HashMap<ConnId, HashMap<u64, crate::relay::Relay>>,
    next_relay: u64,
    /// Connections of apps, which run `den` commands: the one showing the
    /// terminal a command ran in or, if none does, the last.
    apps: Vec<ConnId>,
    /// `den` commands an app is running: the app, and who asked.
    commands: HashMap<u64, (ConnId, Asker)>,
    next_command: u64,
    /// The terminals running an agent, as last sent.
    agents: Vec<AgentInfo>,
    /// The socket's descriptor, for the agent that replaces this one.
    #[cfg(unix)]
    listener: std::os::fd::RawFd,
}

type Shared = Arc<Mutex<State>>;

/// A lock whose holder panicked is taken all the same: one request that
/// fails that way mustn't leave every terminal unreachable, with the agent
/// still holding the socket so that no other can take its place.
trait Lock<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> Lock<T> for Mutex<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Who waits for a `den` command the app runs.
enum Asker {
    /// A connection's request (`den` in a terminal).
    Request(ConnId, Option<u64>),
    /// Claude Code's IDE side (`ide.rs`), on a thread of its own.
    Ide(std::sync::mpsc::Sender<Result<String, String>>),
}

impl Asker {
    fn answer(self, state: &State, result: Result<String, String>) {
        match self {
            Asker::Request(conn, Some(id)) => state.send(conn, ServerMessage::Response { id, result: result.map(Response::Text) }),
            Asker::Request(_, None) => {}
            Asker::Ide(sender) => {
                let _ = sender.send(result);
            }
        }
    }
}

/// Claude Code's IDE side reaches the terminals and the apps through this.
struct IdeHost(Shared);

impl crate::ide::Host for IdeHost {
    fn terminal_of(&self, pid: u32) -> Option<(TermId, String)> {
        let shells: HashMap<u32, TermId> = {
            let state = self.0.locked();
            state.terms.iter().filter_map(|(term, entry)| Some((entry.locked().pty.pid()?, *term))).collect()
        };
        let term = crate::ports::shell_of(pid, &shells)?;
        let group = self.0.locked().terms.get(&term)?.locked().group.clone();
        Some((term, group))
    }

    fn ask_app(&self, term: TermId, group: &str, args: Vec<String>) -> Result<String, String> {
        let (sender, answer) = std::sync::mpsc::channel();
        send_command(&self.0, Asker::Ide(sender), args, PathBuf::from(group), Some(term)).map_err(|err| format!("{err:#}"))?;
        answer
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "den didn't answer".to_string())?
    }
}

impl State {
    fn send(&self, conn: ConnId, message: ServerMessage) {
        if let Some(client) = self.clients.get(&conn) {
            let _ = client.send(message);
        }
    }

    fn broadcast(&self, term: TermId, make: impl Fn() -> Event) {
        if let Some(entry) = self.terms.get(&term) {
            entry.locked().broadcast(make);
        }
    }

    /// The terminal `term`, to use without the agent's lock.
    fn term(&self, term: TermId) -> Result<SharedTerm> {
        self.terms.get(&term).cloned().ok_or_else(|| gone(term))
    }

    fn broadcast_all(&self, event: impl Fn() -> Event) {
        for client in self.clients.values() {
            let _ = client.send(ServerMessage::Event(event()));
        }
    }

    /// Marks a task whose terminal produced output as working.
    fn mark_working(&mut self, group: String) {
        if self.working.insert(group.clone()) {
            self.broadcast_all(|| Event::Activity {
                group: group.clone(),
                working: true,
            });
        }
    }

    /// Considers tasks without a working terminal as stopped.
    fn expire_activity(&mut self) {
        let mut working = HashSet::new();
        for entry in self.terms.values() {
            let mut entry = entry.locked();
            if !entry.settled && entry.last_output.elapsed() >= WORKING_WINDOW {
                entry.settled = true;
            }
            if entry.working() {
                working.insert(entry.group.clone());
            }
        }
        let stopped: Vec<String> = self.working.difference(&working).cloned().collect();
        for group in stopped {
            self.working.remove(&group);
            self.broadcast_all(|| Event::Activity {
                group: group.clone(),
                working: false,
            });
        }
        self.check_blocked();
    }

    /// Checks the screen of terminals that just went quiet (and weren't checked
    /// since their last output) and notifies about tasks that start waiting
    /// for an answer, or stop doing so.
    fn check_blocked(&mut self) {
        let mut now = HashSet::new();
        for entry in self.terms.values() {
            let mut entry = entry.locked();
            if entry.checked != entry.last_output && entry.last_output.elapsed() >= QUIET {
                entry.checked = entry.last_output;
                entry.blocked = blocked::is_blocked(&bottom_lines(&entry.emulator));
            }
            if entry.blocked {
                now.insert(entry.group.clone());
            }
        }
        let changed: Vec<(String, bool)> = now
            .difference(&self.blocked)
            .map(|group| (group.clone(), true))
            .chain(self.blocked.difference(&now).map(|group| (group.clone(), false)))
            .collect();
        self.blocked = now;
        for (group, blocked) in changed {
            self.broadcast_all(|| Event::Blocked { group: group.clone(), blocked });
        }
    }

    /// The terminals whose foreground process changed since last looked at,
    /// with the new one: whether it's an agent is read off the lock.
    fn foregrounds_changed(&mut self) -> Vec<(TermId, Option<u32>)> {
        let mut changed = Vec::new();
        for (term, entry) in &self.terms {
            let mut entry = entry.locked();
            let foreground = entry.pty.foreground_pid();
            if foreground != entry.foreground {
                entry.foreground = foreground;
                changed.push((*term, foreground));
            }
        }
        changed
    }

    /// The terminals running an agent: those whose foreground process is
    /// one or, where that can't be read (Windows), whose title is Claude
    /// Code's.
    fn agent_list(&self) -> Vec<AgentInfo> {
        let mut agents: Vec<AgentInfo> = self
            .terms
            .iter()
            .filter_map(|(term, entry)| {
                let entry = entry.locked();
                let name = entry.agent.clone().or_else(|| entry.claude_title().map(|_| "claude".to_string()))?;
                Some(AgentInfo {
                    term: *term,
                    group: entry.group.clone(),
                    name,
                    title: entry.title.clone(),
                    working: entry.working(),
                    blocked: entry.blocked,
                })
            })
            .collect();
        agents.sort_by_key(|agent| agent.term);
        agents
    }

    /// Tells every connection the agents, if they changed.
    fn send_agents(&mut self) {
        let agents = self.agent_list();
        if agents != self.agents {
            self.agents = agents.clone();
            self.broadcast_all(|| Event::Agents { agents: agents.clone() });
        }
    }

    fn update_idle(&mut self) {
        self.idle_since = (self.terms.is_empty() && self.clients.is_empty()).then(Instant::now);
    }
}

/// `handover`: what the agent this process was before left running.
pub fn run(listener: Listener, mut handover: Option<Handover>) -> Result<()> {
    let state: Shared = Arc::default();
    #[cfg(unix)]
    {
        state.locked().listener = listener.raw_fd();
    }
    // Before any terminal starts: they're told where it listens.
    if let Err(err) = crate::ide::start(Arc::new(IdeHost(state.clone())), handover.as_mut().and_then(handed_ide)) {
        eprintln!("no IDE integration for Claude Code: {err:#}");
    }
    if let Some(handover) = handover {
        take_over(&state, handover);
    }
    let older = shut_down_older_agents();
    let mut resumed = HashSet::new();
    if let Ok(path) = restart_file() {
        restore_after_restart(&state, &path, &mut resumed);
    }
    for path in older {
        restore_after_restart(&state, &path, &mut resumed);
    }
    state.locked().update_idle();

    std::thread::spawn({
        let state = state.clone();
        let mut saved = Vec::new();
        move || for tick in 0u64.. {
            std::thread::sleep(Duration::from_millis(500));
            if tick % SAVE_TICKS == 0 {
                lsp::stop_idle();
            }
            let changed = state.locked().foregrounds_changed();
            // Reading a command line runs `ps`: not while holding the lock.
            let agents: Vec<(TermId, Option<String>)> = changed
                .into_iter()
                .map(|(term, pid)| (term, pid.and_then(platform::process_args).filter(|args| agent_name(args).is_some())))
                .collect();
            let mut state = state.locked();
            for (term, args) in agents {
                if let Some(entry) = state.terms.get(&term) {
                    let mut entry = entry.locked();
                    entry.agent = args.as_deref().and_then(agent_name);
                    entry.agent_args = args;
                }
            }
            state.expire_activity();
            state.send_agents();
            if tick % SAVE_TICKS == 0 {
                save_if_changed(&state, &mut saved);
            }
        }
    });

    std::thread::spawn({
        let state = state.clone();
        move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let idle = state.locked().idle_since;
            if idle.is_some_and(|since| since.elapsed() >= IDLE_EXIT) {
                eprintln!("no terminals or connections: exiting");
                crate::ide::stop();
                std::process::exit(0);
            }
        }
    });

    loop {
        match listener.accept() {
            Ok(stream) => {
                let state = state.clone();
                std::thread::spawn(move || {
                    if let Err(err) = serve(stream, state) {
                        eprintln!("connection closed with error: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("accept failed: {err:#}"),
        }
    }
}

fn serve(mut stream: Box<dyn Stream>, state: Shared) -> Result<()> {
    let mut writer = stream.try_clone_stream()?;
    let (tx, rx) = Outbox::new(stream.close_handle()?);
    let writer_closed = tx.closed.clone();
    let close_writer = tx.close.clone();
    let conn = {
        let mut state = state.locked();
        let conn = state.next_conn;
        state.next_conn += 1;
        state.clients.insert(conn, tx);
        state.update_idle();
        conn
    };

    std::thread::spawn(move || {
        let mut next = None;
        loop {
            let mut message = match next.take() {
                Some(message) => message,
                None => match rx.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
            };
            // A terminal's output with more of it already waiting behind:
            // one message, one write, one wake of the UI.
            if let ServerMessage::Event(Event::TermOutput { term, data }) = &mut message.message {
                while data.len() < MAX_JOINED_OUTPUT {
                    let Ok(mut more) = rx.try_recv() else {
                        break;
                    };
                    match &mut more.message {
                        ServerMessage::Event(Event::TermOutput { term: other, data: more_data }) if other == term => {
                            data.append(more_data);
                        }
                        _ => {
                            next = Some(more);
                            break;
                        }
                    }
                }
            }
            if write_message(&mut writer, &message.message).is_err() {
                if !writer_closed.swap(true, Ordering::Relaxed) {
                    close_writer();
                }
                break;
            }
        }
    });

    let result = (|| -> Result<()> {
        while let Some(decoded) = proto::read_message::<ClientMessage, ClientEnvelope>(&mut stream)? {
            let mut message = match decoded {
                Decoded::Known(message) => message,
                // From a newer UI: rejected without dropping the connection.
                Decoded::Unknown { id } => {
                    if let Some(id) = id {
                        let result = Err("this agent does not know the request: close it so a new one starts".into());
                        state.locked().send(conn, ServerMessage::Response { id, result });
                    }
                    continue;
                }
            };
            let id = message.id;
            own_separators(&mut message.request);
            // Answered when the app is done, not now.
            if let Request::Command { args, cwd, term } = message.request {
                if let Err(err) = send_command(&state, Asker::Request(conn, id), args, cwd, term) {
                    reply_to(&state, conn, id, Err(err));
                }
                continue;
            }
            match caught(|| handle(&state, conn, message.request)) {
                Ok(Reply::Now(response)) => reply_to(&state, conn, id, Ok(response)),
                Ok(Reply::Later(work)) => {
                    let state = state.clone();
                    std::thread::spawn(move || reply_to(&state, conn, id, caught(work)));
                }
                Err(err) => reply_to(&state, conn, id, Err(err)),
            }
        }
        Ok(())
    })();

    let mut state = state.locked();
    if let Some(client) = state.clients.remove(&conn) {
        client.disconnect();
    }
    state.watchers.remove(&conn);
    state.relays.remove(&conn);
    state.apps.retain(|app| *app != conn);
    // What that app was running won't be answered.
    let lost: Vec<_> = state.commands.extract_if(|_, (app, _)| *app == conn).collect();
    for (_, (_, asker)) in lost {
        asker.answer(&state, Err("the app closed before answering".into()));
    }
    for entry in state.terms.values() {
        entry.locked().subscribers.remove(&conn);
    }
    state.update_idle();
    result
}

/// Sends a `den` command, with the workspace of the terminal it ran in, to
/// the app showing that terminal (an app can have several windows, each
/// with its own connection) or else to the one that last said it runs them.
fn send_command(
    state: &Shared,
    asker: Asker,
    args: Vec<String>,
    cwd: PathBuf,
    term: Option<TermId>,
) -> Result<()> {
    let mut state = state.locked();
    let entry = term.and_then(|term| state.terms.get(&term)).map(|entry| entry.locked());
    let app = *state
        .apps
        .iter()
        .rev()
        .find(|app| entry.as_ref().is_some_and(|entry| entry.subscribers.contains_key(app)))
        .or(state.apps.last())
        .ok_or_else(|| anyhow::anyhow!("no den app is connected to this machine"))?;
    let group = entry.map(|entry| entry.group.clone());
    let term = term.filter(|_| group.is_some());
    state.next_command += 1;
    let command = state.next_command;
    state.commands.insert(command, (app, asker));
    state.send(app, ServerMessage::Event(Event::Command { command, args, cwd, term, group }));
    Ok(())
}

/// The last `count` lines of a terminal's history and screen as text, with
/// the rows a long line wrapped into joined again. Blank lines at the end
/// (the empty screen below the prompt) don't count. Read from the bottom,
/// only as far as needed: the agent's lock is held meanwhile.
fn last_lines<T: EventListener>(term: &Term<T>, count: usize) -> Vec<String> {
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let last = Column(grid.columns() - 1);
    let top = -(grid.history_size() as i32);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut line = grid.screen_lines() as i32 - 1;
    while line >= top && lines.len() < count {
        let row = &grid[Line(line)];
        let text: String = (0..grid.columns())
            .map(|col| &row[Column(col)])
            .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|cell| cell.c)
            .collect();
        current.insert_str(0, &text);
        // A row starts its line unless the one above wrapped into it.
        let continues = line > top && grid[Line(line - 1)][last].flags.contains(Flags::WRAPLINE);
        if !continues {
            let text = std::mem::take(&mut current).trim_end().to_string();
            if !(lines.is_empty() && text.is_empty()) {
                lines.push(text);
            }
        }
        line -= 1;
    }
    lines.reverse();
    lines
}

fn reply_to(state: &Shared, conn: ConnId, id: Option<u64>, reply: Result<Response>) {
    if let Some(id) = id {
        let result = reply.map_err(|err| format!("{err:#}"));
        state.locked().send(conn, ServerMessage::Response { id, result });
    }
}

/// An oversized response fails its request without disconnecting all terminals.
fn write_message(writer: &mut impl std::io::Write, message: &ServerMessage) -> Result<()> {
    match proto::write_frame(writer, message) {
        Err(err) if err.is::<proto::FrameTooLarge>() => {
            if let ServerMessage::Response { id, .. } = message {
                proto::write_frame(writer, &ServerMessage::Response { id: *id, result: Err(err.to_string()) })
            } else {
                Err(err)
            }
        }
        result => result,
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn output_overflow_wakes_the_socket_threads_and_cleans_the_connection() {
        let (stream, _unread_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let state = Shared::default();
        let shared = state.clone();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || { let _ = done.send(serve(Box::new(stream), shared)); });
        let started = Instant::now();
        let outbox = loop {
            if let Some(outbox) = state.locked().clients.values().next().cloned() {
                break outbox;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        };
        // The peer doesn't read, so the writer must eventually block. The
        // next overflow has to wake both it and serve's idle socket reader.
        for _ in 0..8 {
            if outbox.send(ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT / 2] })).is_err() {
                break;
            }
        }
        finished.recv_timeout(Duration::from_secs(2)).expect("connection stayed blocked").unwrap();
        assert!(state.locked().clients.is_empty());
    }

    #[test]
    fn slow_consumers_disconnect_without_blocking_other_connections() {
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let (outbox, pending) = Outbox::new(Arc::new(move || { flag.store(true, Ordering::Relaxed); }));
        let (other, other_pending) = Outbox::new(Arc::new(|| {}));
        let output = || ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT / 2] });
        outbox.send(output()).unwrap();
        outbox.send(output()).unwrap();
        assert!(outbox.send(output()).is_err());
        assert!(closed.load(Ordering::Relaxed));
        assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), MAX_PENDING_OUTPUT);
        other.send(ServerMessage::Response { id: 1, result: Ok(Response::Ok) }).unwrap();
        assert!(other_pending.try_recv().is_ok());
        drop(pending);
        assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bounds_small_messages_and_releases_consumed_output() {
        let (outbox, pending) = Outbox::new(Arc::new(|| {}));
        for _ in 0..MAX_PENDING_MESSAGES {
            outbox.send(ServerMessage::Response { id: 1, result: Ok(Response::Ok) }).unwrap();
        }
        assert!(outbox.send(ServerMessage::Response { id: 2, result: Ok(Response::Ok) }).is_err());
        assert!(outbox.closed.load(Ordering::Relaxed));
        drop(pending);
        let (outbox, pending) = Outbox::new(Arc::new(|| {}));
        for _ in 0..3 {
            outbox.send(ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT] })).unwrap();
            drop(pending.recv().unwrap());
            assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn oversized_response_returns_an_error_and_keeps_the_stream_usable() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Response {
            id: 7,
            result: Ok(Response::Text("x".repeat(64 * 1024 * 1024))),
        }).unwrap();
        write_message(&mut bytes, &ServerMessage::Response { id: 8, result: Ok(Response::Ok) }).unwrap();
        let mut reader = bytes.as_slice();
        assert!(matches!(proto::read_frame(&mut reader).unwrap(),
            Some(ServerMessage::Response { id: 7, result: Err(error) }) if error.contains("frame too large")));
        assert!(matches!(proto::read_frame(&mut reader).unwrap(),
            Some(ServerMessage::Response { id: 8, result: Ok(Response::Ok) })));
    }
}

/// A UI on Windows builds this machine's paths with its own separator
/// (`~\Downloads`, `/home/me/repo\src`): on Linux and macOS they become `/`.
/// A `\` in a file name there is rare enough to give up.
fn own_separators(request: &mut Request) {
    if cfg!(windows) {
        return;
    }
    let fix = |path: &mut PathBuf| {
        if let Some(text) = path.to_str()
            && text.contains('\\')
        {
            *path = PathBuf::from(text.replace('\\', "/"));
        }
    };
    match request {
        Request::TermCreate { cwd: path, .. }
        | Request::RepoAdd { path }
        | Request::RepoRemove { path }
        | Request::TaskCreate { repo: path, .. }
        | Request::TaskRemove { path }
        | Request::TaskForceRemove { path }
        | Request::GitChanges { path, .. }
        | Request::GitDiff { path, .. }
        | Request::FindFiles { path }
        | Request::Search { path, .. }
        | Request::ReadFile { path }
        | Request::WriteFile { path, .. }
        | Request::ListDir { path }
        | Request::ListDirAll { path }
        | Request::CreateFile { path }
        | Request::CreateDir { path }
        | Request::Trash { path }
        | Request::Watch { path }
        | Request::Unwatch { path }
        | Request::Git { path, .. }
        | Request::Replace { path, .. }
        | Request::Command { cwd: path, .. }
        | Request::ReplaceAgent { exe: path }
        | Request::Resolve { path } => fix(path),
        Request::Rename { from, to } | Request::Copy { from, to } | Request::Untrash { item: from, to } => {
            fix(from);
            fix(to);
        }
        Request::Lsp { root, path, .. } | Request::LspResolve { root, path, .. } | Request::Format { root, path, .. } => {
            fix(root);
            fix(path);
        }
        Request::LspWorkspaceSymbols { root, path, .. } => {
            fix(root);
            if let Some(path) = path {
                fix(path);
            }
        }
        Request::Open { root, file } => {
            fix(root);
            if let Some(file) = file {
                fix(file);
            }
        }
        Request::Hello { .. }
        | Request::Shutdown
        | Request::TermList { .. }
        | Request::TermAttach { .. }
        | Request::TermDetach { .. }
        | Request::TermInput { .. }
        | Request::TermResize { .. }
        | Request::TermKill { .. }
        | Request::TermClear { .. }
        | Request::TermCwd { .. }
        | Request::SavePastedImage { .. }
        | Request::RepoList
        | Request::TaskList
        | Request::Version
        | Request::BlockedList
        | Request::AgentList
        | Request::Ports
        | Request::RelayConnect { .. }
        | Request::RelaySend { .. }
        | Request::RelayClose { .. }
        | Request::Serve
        | Request::CommandDone { .. }
        | Request::TermRead { .. }
        | Request::TermBusy { .. }
        | Request::FreePort => {}
        Request::IdeSelection { selection, .. } => {
            if let Some(selection) = selection {
                fix(&mut selection.file);
            }
        }
    }
}

/// Removes the task at `path` (with `force`, along with its uncommitted
/// changes); its terminals close with it.
fn remove_task(state: &Shared, path: &Path, force: bool) -> Result<Response> {
    tasks::remove(path, force)?;
    // Its language servers too, which would go on indexing a folder that's gone.
    lsp::stop(path);
    let group = path.to_string_lossy().into_owned();
    let mut state = state.locked();
    let terms: Vec<TermId> = state
        .terms
        .iter()
        .filter(|(_, entry)| entry.locked().group == group)
        .map(|(term, _)| *term)
        .collect();
    for term in terms {
        state.broadcast(term, || Event::TermExit { term });
        state.terms.remove(&term);
    }
    state.update_idle();
    Ok(Response::Ok)
}

/// How `handle` answers a request: now, or on a thread of its own.
enum Reply {
    Now(Response),
    Later(Box<dyn FnOnce() -> Result<Response> + Send>),
}

impl Reply {
    /// The response, on this thread even if it takes a while.
    #[cfg(test)]
    fn wait(self) -> Result<Response> {
        match self {
            Reply::Now(response) => Ok(response),
            Reply::Later(work) => work(),
        }
    }
}

fn now(response: Response) -> Result<Reply> {
    Ok(Reply::Now(response))
}

fn later(state: &Shared, work: impl FnOnce(&Shared) -> Result<Response> + Send + 'static) -> Result<Reply> {
    let state = state.clone();
    Ok(Reply::Later(Box::new(move || work(&state))))
}

/// A request that panics fails, and the connection goes on.
fn caught<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the agent failed on this request: see its log")))
}

fn handle(state: &Shared, conn: ConnId, request: Request) -> Result<Reply> {
    match request {
        Request::Hello { .. } => now(Response::Hello {
            protocol: PROTOCOL,
            pid: std::process::id(),
        }),
        Request::ReplaceAgent { exe } => {
            let state = state.locked();
            replace_agent(&state, &tasks::expand_home(&exe)).map(Reply::Now)
        }
        Request::Shutdown => {
            let mut state = state.locked();
            save_for_restart(&state);
            // Dropping the terminals kills their processes.
            state.terms.clear();
            eprintln!("shutdown requested by a UI");
            crate::ide::stop();
            std::process::exit(0);
        }
        Request::TermCreate {
            group,
            cwd,
            command,
            cols,
            rows,
        } => create(state, None, group, cwd, command, cols, rows).map(Reply::Now),
        Request::TermList { group } => {
            let state = state.locked();
            let mut terms: Vec<TermInfo> = state
                .terms
                .iter()
                .map(|(term, entry)| (term, entry.locked()))
                .filter(|(_, entry)| entry.group == group)
                .map(|(term, entry)| TermInfo {
                    term: *term,
                    title: entry.title.clone(),
                })
                .collect();
            terms.sort_by_key(|info| info.term);
            now(Response::TermList(terms))
        }
        Request::TermAttach { term } => {
            let (entry, outbox) = {
                let state = state.locked();
                (state.term(term)?, state.clients.get(&conn).cloned())
            };
            // The snapshot and the subscription happen under the same lock: no pty
            // output falls between the two.
            let mut entry = entry.locked();
            if let Some(outbox) = outbox {
                entry.subscribers.insert(conn, outbox);
            }
            let grid = entry.emulator.grid();
            use alacritty_terminal::grid::Dimensions as _;
            let (cols, rows) = (grid.columns() as u16, grid.screen_lines() as u16);
            let data = snapshot(&entry.emulator, entry.title.as_deref());
            now(Response::TermSnapshot { cols, rows, data })
        }
        Request::TermDetach { term } => {
            if let Ok(entry) = state.locked().term(term) {
                entry.locked().subscribers.remove(&conn);
            }
            now(Response::Ok)
        }
        Request::TermInput { term, data } => {
            let entry = state.locked().term(term)?;
            entry.locked().pty.write(data);
            now(Response::Ok)
        }
        Request::TermResize { term, cols, rows } => {
            let entry = state.locked().term(term)?;
            let mut entry = entry.locked();
            // Both get the same size, or the program and the screen disagree.
            let (cols, rows) = (cols.max(2), rows.max(1));
            entry.emulator.resize(TermSize::new(cols as usize, rows as usize));
            entry.pty.resize(cols, rows);
            now(Response::Ok)
        }
        Request::TermClear { term } => {
            // Under its lock, as the pty's output: it lands between two
            // chunks of it, the same in every emulator.
            let entry = state.locked().term(term)?;
            let mut entry = entry.locked();
            let data = snapshot::clear(&entry.emulator);
            let AgentTerm { parser, emulator, .. } = &mut *entry;
            parser.advance(emulator, &data);
            entry.broadcast(|| Event::TermOutput { term, data: data.clone() });
            now(Response::Ok)
        }
        Request::TermKill { term } => {
            let mut state = state.locked();
            state.broadcast(term, || Event::TermExit { term });
            state.terms.remove(&term);
            state.update_idle();
            now(Response::Ok)
        }
        Request::RepoAdd { path } => {
            tasks::add_repo(&tasks::expand_home(&path))?;
            now(Response::Ok)
        }
        Request::RepoRemove { path } => {
            tasks::remove_repo(&path)?;
            now(Response::Ok)
        }
        Request::RepoList => now(Response::Repos(tasks::repos())),
        Request::BlockedList => now(Response::Files(state.locked().blocked.iter().cloned().collect())),
        Request::AgentList => now(Response::Agents(state.locked().agent_list())),
        Request::Version => now(Response::Text(own_build_id())),
        Request::Open { root, file } => {
            let state = state.locked();
            let mut count = 0;
            for (other, client) in &state.clients {
                if *other != conn {
                    let event = Event::Open { root: root.clone(), file: file.clone() };
                    count += usize::from(client.send(ServerMessage::Event(event)).is_ok());
                }
            }
            now(Response::Count(count))
        }
        // Anything that may take a while (task scripts, git, searches) runs on
        // its own thread, so it doesn't hold up terminal keystrokes.
        Request::TaskList => later(state, move |state| {
            let mut list = tasks::list();
            let state = state.locked();
            for task in &mut list {
                task.working = state.working.contains(task.path.to_string_lossy().as_ref());
            }
            Ok(Response::Tasks(list))
        }),
        Request::Ports => later(state, move |state| {
            let shells: HashMap<u32, String> = state
                .locked()
                .terms
                .values()
                .filter_map(|entry| {
                    let entry = entry.locked();
                    Some((entry.pty.pid()?, entry.group.clone()))
                })
                .collect();
            Ok(Response::Ports(ports::listening(&shells)))
        }),
        Request::TaskCreate { repo, name, open } => later(state, move |state| {
            let task = tasks::add_repo(&repo).and_then(|repo| tasks::create(&repo, &name))?;
            if open {
                let path = task.path.clone();
                state
                    .locked()
                    .broadcast_all(|| Event::OpenTask { path: path.clone() });
            }
            Ok(Response::Task(task))
        }),
        Request::TaskRemove { path } => later(state, move |state| remove_task(state, &path, false)),
        Request::TaskForceRemove { path } => later(state, move |state| remove_task(state, &path, true)),
        Request::GitChanges { path, uncommitted } => later(state, move |_| {
            let (base, files) = git::changes(&path, uncommitted)?;
            Ok(Response::Changes { base, files })
        }),
        Request::GitDiff { path, file, uncommitted } => later(state, move |_| Ok(Response::Text(git::diff(&path, &file, uncommitted)?))),
        Request::Git { path, op } => later(state, move |_| git::run(&path, op)),
        Request::Lsp { root, path, text, line, column, op } => later(state, move |_| lsp::request(&root, &path, &text, line, column, op)),
        Request::LspResolve { root, path, list, item } => later(state, move |_| lsp::resolve(&root, &path, list, item)),
        Request::LspWorkspaceSymbols { root, path, text, query } => later(state, move |_| {
            lsp::workspace_symbols(&root, path.as_deref(), &text, &query)
        }),
        Request::Format { root, path, text } => later(state, move |_| format::format(&root, &path, &text)),
        Request::FindFiles { path } => later(state, move |_| Ok(Response::Files(search::files(&path)))),
        Request::Search {
            path,
            query,
            include,
            exclude,
            max_hits,
        } => later(state, move |_| {
            let (hits, truncated) = search::search(&path, &query, &include, &exclude, max_hits)?;
            Ok(Response::SearchResults { hits, truncated })
        }),
        Request::Replace {
            path,
            files,
            query,
            replacement,
            preserve_case,
        } => later(state, move |_| {
            let (files, replacements) = search::replace(&path, &files, &query, &replacement, preserve_case)?;
            Ok(Response::Replaced { files, replacements })
        }),
        Request::ReadFile { path } => later(state, move |_| Ok(Response::Bytes(fs::read(&path)?))),
        Request::WriteFile { path, data } => later(state, move |_| {
            fs::write(&path, &data)?;
            Ok(Response::Ok)
        }),
        Request::ListDir { path } => later(state, move |_| Ok(Response::Dir(fs::list(&tasks::expand_home(&path), false)?))),
        Request::ListDirAll { path } => later(state, move |_| Ok(Response::Dir(fs::list(&tasks::expand_home(&path), true)?))),
        Request::Trash { path } => later(state, move |_| Ok(Response::Path(fs::trash(&path)?))),
        Request::Untrash { item, to } => later(state, move |_| {
            fs::untrash(&item, &to)?;
            Ok(Response::Ok)
        }),
        Request::Copy { from, to } => later(state, move |_| Ok(Response::Path(Some(fs::copy(&from, &to)?)))),
        Request::Rename { from, to } => {
            fs::rename(&from, &to)?;
            now(Response::Ok)
        }
        Request::CreateFile { path } => {
            fs::create_file(&path)?;
            now(Response::Ok)
        }
        Request::CreateDir { path } => {
            fs::create_dir(&path)?;
            now(Response::Ok)
        }
        // Watching a large folder takes a while on Linux (inotify watches
        // each folder in it).
        Request::Watch { path } => later(state, move |state| {
            let sender = state
                .locked()
                .clients
                .get(&conn)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("connection closed"))?;
            let root = path.clone();
            let watcher = fs::watch(&path, true, move |paths| {
                let _ = sender.send(ServerMessage::Event(Event::FsChanged {
                    root: root.clone(),
                    paths,
                }));
            })?;
            // The connection may have closed meanwhile.
            let mut state = state.locked();
            if state.clients.contains_key(&conn) {
                state.watchers.entry(conn).or_default().insert(path, watcher);
            }
            Ok(Response::Ok)
        }),
        Request::RelayConnect { port } => {
            let sender = state
                .locked()
                .clients
                .get(&conn)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("connection closed"))?;
            let relay = {
                let mut state = state.locked();
                state.next_relay += 1;
                state.next_relay
            };
            let closed = sender.clone();
            let connection = crate::relay::Relay::connect(
                port,
                move |line| {
                    let _ = sender.send(ServerMessage::Event(Event::RelayLine { relay, line }));
                },
                move || {
                    let _ = closed.send(ServerMessage::Event(Event::RelayClosed { relay }));
                },
            )?;
            state.locked().relays.entry(conn).or_default().insert(relay, connection);
            now(Response::Relay(relay))
        }
        Request::RelaySend { relay, line } => {
            // Written without the lock: a program that stops reading would
            // otherwise stall every terminal.
            let writer = state
                .locked()
                .relays
                .get(&conn)
                .and_then(|relays| relays.get(&relay))
                .map(|relay| relay.writer())
                .ok_or_else(|| anyhow::anyhow!("the relay is closed"))?;
            writer.send(&line)?;
            now(Response::Ok)
        }
        Request::RelayClose { relay } => {
            if let Some(relays) = state.locked().relays.get_mut(&conn) {
                relays.remove(&relay);
            }
            now(Response::Ok)
        }
        Request::Serve => {
            let mut state = state.locked();
            state.apps.retain(|app| *app != conn);
            state.apps.push(conn);
            now(Response::Ok)
        }
        Request::Command { .. } => unreachable!("answered when the app is done"),
        Request::IdeSelection { group, selection } => {
            crate::ide::set_selection(group, selection);
            now(Response::Ok)
        }
        Request::CommandDone { command, result } => {
            let mut state = state.locked();
            if let Some((_, asker)) = state.commands.remove(&command) {
                asker.answer(&state, result);
            }
            now(Response::Ok)
        }
        Request::Resolve { path } => {
            let resolved = tasks::expand_home(&path).canonicalize()?;
            now(Response::Path(Some(resolved)))
        }
        Request::TermRead { term, lines } => {
            let entry = state.locked().term(term)?;
            let entry = entry.locked();
            now(Response::Text(last_lines(&entry.emulator, lines as usize).join("\n")))
        }
        Request::Unwatch { path } => {
            if let Some(watchers) = state.locked().watchers.get_mut(&conn) {
                watchers.remove(&path);
            }
            now(Response::Ok)
        }
        Request::SavePastedImage { extension, data } => {
            now(Response::Path(Some(save_pasted_image(&extension, &data)?)))
        }
        Request::TermBusy { term } => {
            let entry = state.locked().term(term)?;
            let entry = entry.locked();
            now(Response::Busy(entry.pty.foreground_pid() != entry.pty.pid()))
        }
        Request::FreePort => {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            now(Response::Port(listener.local_addr()?.port()))
        }
        Request::TermCwd { term } => {
            let entry = state.locked().term(term)?;
            let entry = entry.locked();
            let cwd = entry.pty.foreground_pid().and_then(platform::process_cwd).or_else(|| Some(entry.cwd.path.clone()));
            now(Response::Path(cwd))
        }
    }
}

/// Pasted images are saved here and deleted after a day.
fn save_pasted_image(extension: &str, data: &[u8]) -> Result<PathBuf> {
    const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
    let extension: String = extension.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    let dir = std::env::temp_dir().join(format!("{}-paste", proto::APP));
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified.elapsed().unwrap_or_default() > KEEP);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!("image-{stamp}.{extension}"));
    std::fs::write(&path, data)?;
    Ok(path)
}

/// Fingerprint of this agent's binary, computed once.
fn own_build_id() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        std::env::current_exe()
            .and_then(std::fs::read)
            .map(|bytes| proto::build_id(&bytes))
            .unwrap_or_default()
    })
    .clone()
}

/// The last non-empty lines on screen (without the history).
fn bottom_lines<T: EventListener>(term: &Term<T>) -> Vec<String> {
    let grid = term.grid();
    let mut lines: Vec<String> = (0..grid.screen_lines() as i32)
        .map(|line| {
            let row = &grid[Line(line)];
            let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
            text.trim_end().to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(25);
    lines.drain(..skip);
    lines
}

fn gone(term: TermId) -> anyhow::Error {
    anyhow::anyhow!("terminal {term} no longer exists")
}

/// A terminal as it was when the agent was restarted (to update it), so the
/// new agent opens it again under the same id and the UIs find it in place.
#[derive(Serialize, Deserialize)]
struct Restarted {
    term: TermId,
    group: String,
    cwd: PathBuf,
    cols: u16,
    rows: u16,
    /// Claude Code was running in it: the command that resumes its session
    /// (the same options and config folder, plus `--resume <id>`).
    claude: Option<String>,
}

/// Next to the socket (`agent-6.restart.json`), so an agent on another
/// socket (tests, another protocol) doesn't take it unless it shut that one
/// down. Agents of later protocols read it: fields are only added, with a default.
fn restart_file() -> Result<PathBuf> {
    Ok(restart_file_of(&proto::socket_path()?))
}

fn restart_file_of(socket: &Path) -> PathBuf {
    socket.with_extension("restart.json")
}

/// Replaces this agent with `exe` in the same process, handing it the
/// terminals (see `handover.rs`). Saved first as for a restart, so that a
/// new agent that doesn't start leaves them for the next one.
#[cfg(unix)]
fn replace_agent(state: &State, exe: &Path) -> Result<Response> {
    save_for_restart(state);
    let terms = state
        .terms
        .iter()
        .filter_map(|(term, entry)| {
            let entry = entry.locked();
            Some(crate::handover::Term {
                term: *term,
                group: entry.group.clone(),
                cwd: entry.cwd.path.clone(),
                fd: entry.pty.raw_fd()?,
                pid: entry.pty.pid()?,
                cols: entry.emulator.columns() as u16,
                rows: entry.emulator.screen_lines() as u16,
                title: entry.title.clone(),
                screen: snapshot(&entry.emulator, entry.title.as_deref()),
            })
        })
        .collect();
    let ide = crate::ide::handover().map(|(fd, token)| crate::handover::Ide { fd, token });
    let handover = Handover { listener: state.listener, next_term: state.next_term, terms, ide };
    eprintln!("replacing the agent with {}", exe.display());
    Err(crate::handover::replace(exe, &handover))
}

#[cfg(windows)]
fn replace_agent(_: &State, _: &Path) -> Result<Response> {
    anyhow::bail!("on Windows the agent can't replace itself: it restarts")
}

/// The listener for Claude Code the agent before had.
fn handed_ide(handover: &mut Handover) -> Option<(std::net::TcpListener, String)> {
    let ide = handover.ide.take()?;
    #[cfg(unix)]
    {
        Some((std::net::TcpListener::from(crate::handover::own(ide.fd)), ide.token))
    }
    #[cfg(windows)]
    {
        let _ = ide;
        None
    }
}

/// Goes on with the terminals the agent this process was before handed over,
/// under the same ids, with their screens.
fn take_over(state: &Shared, handover: Handover) {
    {
        let mut state = state.locked();
        state.next_term = state.next_term.max(handover.next_term);
    }
    for handed in handover.terms {
        #[cfg(unix)]
        let pty = Pty::adopt(crate::handover::own(handed.fd), handed.pid);
        #[cfg(windows)]
        let pty: Result<(Pty, crate::pty::PtyIo)> = Err(anyhow::anyhow!("no handover on Windows"));
        match pty {
            Ok(pty) => {
                let screen = Screen { data: handed.screen, title: handed.title };
                start(state, handed.term, handed.group, handed.cwd, pty, handed.cols, handed.rows, Some(screen));
            }
            Err(err) => eprintln!("could not take over terminal {}: {err:#}", handed.term),
        }
    }
    eprintln!("took over the terminals of the agent before");
}

/// Agents of earlier protocols still running (Den was updated to a newer
/// one): their terminals are where the UI left them, so each is shut down,
/// which saves them as for a restart, and the files they leave are returned
/// to be restored here. Every protocol reads `Shutdown` alike.
fn shut_down_older_agents() -> Vec<PathBuf> {
    let Ok(sockets) = proto::older_socket_paths() else {
        return Vec::new();
    };
    sockets
        .into_iter()
        .filter_map(|socket| {
            let mut stream = platform::connect(&socket).ok()?;
            proto::write_frame(&mut stream, &ClientMessage { id: None, request: Request::Shutdown }).ok()?;
            // It saves its terminals and exits, which closes the connection.
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
            eprintln!("shut down the agent at {} to take its terminals", socket.display());
            Some(restart_file_of(&socket))
        })
        .collect()
}

/// Every so many ticks of the foreground check (5 s), the terminals are saved
/// as for a restart, so that an agent that dies without one (the Mac is
/// restarted, it's killed) leaves them for the next to restore.
const SAVE_TICKS: u64 = 10;

/// Saves the terminals for a restart if they changed since `saved`, with the
/// command lines last read: no `ps` every few seconds. Under the lock, so it
/// never overwrites what a shutdown saved.
fn save_if_changed(state: &State, saved: &mut Vec<u8>) {
    let Ok(bytes) = serde_json::to_vec(&restart_terms(state, false)) else {
        return;
    };
    if bytes != *saved {
        write_restart_file(&bytes);
        *saved = bytes;
    }
}

/// Before shutting down to restart: notes each terminal's folder and whether
/// Claude Code is running in it. The processes die; their place doesn't.
fn save_for_restart(state: &State) {
    if let Ok(bytes) = serde_json::to_vec(&restart_terms(state, true)) {
        write_restart_file(&bytes);
    }
}

/// Written beside it and renamed, so dying while writing leaves the last one.
fn write_restart_file(bytes: &[u8]) {
    let saved = restart_file().and_then(|path| {
        let partial = path.with_extension("partial");
        std::fs::write(&partial, bytes)?;
        Ok(std::fs::rename(partial, path)?)
    });
    if let Err(err) = saved {
        eprintln!("could not save the terminals for the restart: {err:#}");
    }
}

/// The terminals as a restart reopens them. `fresh` reads each one's command
/// line now; otherwise the one read when its foreground process last changed
/// (on Windows, where that's the shell, a Claude started later is missed).
fn restart_terms(state: &State, fresh: bool) -> Vec<Restarted> {
    state
        .terms
        .iter()
        .map(|(term, entry)| {
            let entry = entry.locked();
            let foreground = if fresh { entry.pty.foreground_pid() } else { entry.foreground };
            let args = if fresh { foreground.and_then(platform::process_args) } else { entry.agent_args.clone() };
            let args = args.filter(|args| agent_name(args).as_deref() == Some("claude"));
            let session = foreground.filter(|_| args.is_some()).and_then(claude_session);
            // `--resume <id>` finds the session only from the folder it began in.
            let cwd = session.as_ref().and_then(|session| session.cwd.clone());
            Restarted {
                term: *term,
                group: entry.group.clone(),
                cwd: cwd
                    .or_else(|| foreground.and_then(platform::process_cwd))
                    .unwrap_or_else(|| entry.cwd.path.clone()),
                cols: entry.emulator.columns() as u16,
                rows: entry.emulator.screen_lines() as u16,
                claude: args.and_then(|args| resume_command(&args, session.as_ref())),
            }
        })
        .collect()
}

/// On startup after a restart, or after an agent that died: opens the
/// terminals the previous agent had, with the same ids, and resumes Claude
/// Code where it was running. `resumed`: the Claude sessions already
/// resumed, by this file or an earlier one; a terminal that ran one of them
/// again is left out, as the agents of two protocols may both have it (each
/// one's UI opened it).
fn restore_after_restart(state: &Shared, path: &Path, resumed: &mut HashSet<String>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let _ = std::fs::remove_file(path);
    let terms: Vec<Restarted> = match serde_json::from_slice(&bytes) {
        Ok(terms) => terms,
        Err(err) => return eprintln!("invalid {}: {err:#}", path.display()),
    };
    for saved in terms {
        // The folder may be gone (a removed task): the terminal goes with it.
        // An id already taken is another agent's terminal, which the UI shows.
        if !saved.cwd.is_dir() || state.locked().terms.contains_key(&saved.term) {
            continue;
        }
        if let Some(session) = saved.claude.as_deref().and_then(resumed_session)
            && !resumed.insert(session.to_string())
        {
            continue;
        }
        let command = saved.claude.map(|command| format!("{command}\r").into_bytes());
        let created = create(state, Some(saved.term), saved.group, saved.cwd, None, saved.cols, saved.rows);
        match (created, command) {
            (Ok(_), Some(command)) => {
                // Typed into the shell, so it stays when Claude exits.
                if let Ok(entry) = state.locked().term(saved.term) {
                    entry.locked().pty.write(command);
                }
            }
            (Ok(_), None) => {}
            (Err(err), _) => eprintln!("could not reopen terminal {}: {err:#}", saved.term),
        }
    }
}

/// The session `--resume <id>` resumes in a command line; none for a bare
/// `--resume`, which asks.
fn resumed_session(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    words.by_ref().find(|word| *word == "--resume")?;
    words.next().filter(|word| !word.starts_with('-'))
}

/// The coding agent a command line runs, by its program or its npm package
/// (run by node): `claude`, `codex`…
fn agent_name(args: &str) -> Option<String> {
    const AGENTS: [&str; 7] = ["claude", "codex", "gemini", "opencode", "aider", "amp", "cursor-agent"];
    const PACKAGES: [(&str, &str); 4] =
        [("@anthropic-ai/claude-code", "claude"), ("@openai/codex", "codex"), ("@google/gemini-cli", "gemini"), ("opencode-ai", "opencode")];
    let program = |word: &str| {
        let word = word.trim_matches('"').replace('\\', "/");
        let name = word.rsplit('/').next().unwrap_or("").to_string();
        let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".cmd")).unwrap_or(&name).to_string();
        (word, name)
    };
    let mut words = args.split_whitespace();
    let (_, first) = program(words.next()?);
    if AGENTS.contains(&first.as_str()) {
        return Some(first);
    }
    // What an interpreter runs: the agent's package, or its script.
    if !["node", "bun", "deno", "python", "python3"].contains(&first.as_str()) {
        return None;
    }
    let (script, name) = program(words.next()?);
    PACKAGES
        .iter()
        .find(|(package, _)| script.contains(package))
        .map(|(_, name)| name.to_string())
        .or_else(|| AGENTS.contains(&name.as_str()).then_some(name))
}

/// Claude Code's title: what it's doing after `✳` or a spinner (braille,
/// or half circles from 2.1.2xx on).
fn claude_title(title: &str) -> bool {
    claude_working(title).is_some()
}

/// Whether Claude Code works, by its title: a spinner while it does, `✳`
/// while it doesn't; `None` for a title that isn't Claude Code's.
fn claude_working(title: &str) -> Option<bool> {
    match title.chars().next()? {
        '✳' => Some(false),
        '\u{2800}'..='\u{28FF}' | '◐' | '◑' | '◒' | '◓' => Some(true),
        _ => None,
    }
}

/// The Claude Code session a process runs: Claude Code writes it to
/// `sessions/<pid>.json` in its config folder, `CLAUDE_CONFIG_DIR` or `~/.claude`.
struct ClaudeSession {
    id: Option<String>,
    cwd: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` if the process had it: the new shell may not.
    config_dir: Option<String>,
}

fn claude_session(pid: u32) -> Option<ClaudeSession> {
    let config_dir = platform::process_env(pid, "CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty());
    let dir = match &config_dir {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir()?.join(".claude"),
    };
    let file = std::fs::read(dir.join("sessions").join(format!("{pid}.json"))).ok();
    let json: Option<serde_json::Value> = file.and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let field = |name: &str| json.as_ref()?.get(name)?.as_str().map(str::to_string);
    Some(ClaudeSession {
        id: field("sessionId").filter(|id| session_id(id)),
        cwd: field("cwd").map(PathBuf::from).filter(|cwd| cwd.is_dir()),
        config_dir,
    })
}

/// Typed into a shell as is, so only letters, digits and `-` (a UUID).
fn session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// If a command line is Claude Code's (the native `claude` binary or the npm
/// package run by node), the command that resumes its session: the same
/// options and config folder plus `--resume <id>`. With no id known, a bare
/// `--resume` lets the user pick the session, rather than `--continue`
/// guessing one: two terminals in the same folder would get the same.
fn resume_command(args: &str, session: Option<&ClaudeSession>) -> Option<String> {
    resume_command_in(if cfg!(windows) { Shell::PowerShell } else { Shell::Posix }, args, session)
}

/// The shell a resumed command is typed into.
#[derive(Clone, Copy)]
enum Shell {
    Posix,
    PowerShell,
}

fn resume_command_in(shell: Shell, args: &str, session: Option<&ClaudeSession>) -> Option<String> {
    let words = match shell {
        // `ps` joins the arguments with spaces and loses their quoting.
        Shell::Posix => args.split_whitespace().map(str::to_string).collect(),
        // Windows keeps the command line as typed, quotes and all.
        Shell::PowerShell => windows_words(args),
    };
    let start = words.iter().position(|word| {
        let word = word.trim_matches('"');
        matches!(word.rsplit(['/', '\\']).next(), Some("claude" | "claude.exe" | "claude.cmd"))
            || word.replace('\\', "/").contains("@anthropic-ai/claude-code")
    })?;
    let mut command = Vec::new();
    if let Some(dir) = session.and_then(|session| session.config_dir.as_deref()) {
        command.push(format!("CLAUDE_CONFIG_DIR={}", shell_quote(dir)));
    }
    command.push("claude".to_string());
    // The session it resumed, if it was started with one.
    let mut resumed = None;
    let mut rest = words[start + 1..].iter().peekable();
    // A word that isn't an option goes only as the value of the one before
    // it: the others are the prompt it began with, not to be sent again.
    let mut after_option = false;
    while let Some(word) = rest.next() {
        let option = word.starts_with('-');
        match word.as_str() {
            "-c" | "--continue" | "--fork-session" => {}
            "-r" | "--resume" | "--session-id" => {
                if let Some(value) = rest.next_if(|value| !value.starts_with('-')) {
                    resumed = Some(value.to_string());
                }
            }
            _ => match word.strip_prefix("--resume=").or_else(|| word.strip_prefix("--session-id=")) {
                Some(value) => resumed = Some(value.to_string()),
                // Each word goes quoted, so none can be a command.
                None if option || after_option => command.push(shell_word(shell, word)),
                None => {}
            },
        }
        after_option = option;
    }
    let id = session.and_then(|session| session.id.clone()).or(resumed.filter(|id| session_id(id)));
    command.push("--resume".to_string());
    command.extend(id);
    Some(command.join(" "))
}

/// `value` as one word for `shell`, quoted only if it needs it.
fn shell_word(shell: Shell, value: &str) -> String {
    let safe = match shell {
        Shell::Posix => "-_=./:,@+%",
        // `,` makes a list and `@` a splat there.
        Shell::PowerShell => "-_=./:\\",
    };
    let plain = !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric() || safe.contains(ch));
    match shell {
        _ if plain => value.to_string(),
        Shell::Posix => shell_quote(value),
        // PowerShell's single quotes: nothing in them is special but `'`, doubled.
        Shell::PowerShell => format!("'{}'", value.replace('\'', "''")),
    }
}

/// A Windows command line's words: spaces split them except inside double
/// quotes, which go.
fn windows_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// `value` as one word for a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn create(
    state: &Shared,
    id: Option<TermId>,
    group: String,
    cwd: PathBuf,
    command: Option<Vec<String>>,
    cols: u16,
    rows: u16,
) -> Result<Response> {
    let (cols, rows) = (cols.max(2), rows.max(1));
    // The id goes in the terminal's environment, so it's taken before starting it.
    let term = {
        let mut state = state.locked();
        let term = id.unwrap_or(state.next_term);
        state.next_term = state.next_term.max(term + 1);
        term
    };
    let (pty, io) = Pty::spawn(term, &cwd, command.as_deref(), cols, rows)?;
    start(state, term, group, cwd, (pty, io), cols, rows, None);
    Ok(Response::TermCreated { term })
}

/// What a terminal the previous agent left running had on screen.
struct Screen {
    data: Vec<u8>,
    title: Option<String>,
}

/// Puts a started terminal (or one taken over, with its `screen`) in the
/// state, and starts reading its output and waiting for its process.
#[allow(clippy::too_many_arguments)]
fn start(state: &Shared, term: TermId, group: String, cwd: PathBuf, (pty, io): (Pty, crate::pty::PtyIo), cols: u16, rows: u16, screen: Option<Screen>) {
    let events = Listener_::default();
    let mut emulator = Term::new(
        Config::default(),
        &TermSize::new(cols as usize, rows as usize),
        events.clone(),
    );
    let mut parser = Processor::new();
    let mut title = None;
    let settled = screen.is_some();
    if let Some(screen) = screen {
        parser.advance(&mut emulator, &screen.data);
        events.0.locked().clear();
        title = screen.title;
    }
    let entry = Arc::new(Mutex::new(AgentTerm {
        group,
        pty,
        cwd: crate::shell_cwd::ShellCwd::new(cwd),
        emulator,
        parser,
        events,
        title,
        subscribers: HashMap::new(),
        last_output: Instant::now(),
        burst_start: Instant::now(),
        settled,
        blocked: false,
        checked: Instant::now(),
        foreground: None,
        agent: None,
        agent_args: None,
    }));
    // The reader doesn't keep it: closing it drops it, which kills its process.
    let weak = Arc::downgrade(&entry);
    {
        let mut state = state.locked();
        state.terms.insert(term, entry);
        state.update_idle();
    }

    let mut output = io.output;
    std::thread::spawn({
        let state = state.clone();
        move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match output.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let Some(entry) = weak.upgrade() else {
                    // ConPTY may emit final output while closing. Keep draining
                    // until EOF; its close runs on a separate thread.
                    continue;
                };
                // Parsed under the terminal's own lock, not the agent's: a
                // terminal printing a lot doesn't hold up the others.
                let working = {
                    let mut entry = entry.locked();
                    let entry = &mut *entry;
                    entry.cwd.advance(&buf[..n]);
                    entry.parser.advance(&mut entry.emulator, &buf[..n]);
                    let events = std::mem::take(&mut *entry.events.0.locked());
                    let mut title_changed = false;
                    for event in events {
                        match event {
                            // Replies to queries (cursor position, attributes…)
                            // come from the agent, which is always present.
                            AlacEvent::PtyWrite(text) => entry.pty.write(text.into_bytes()),
                            AlacEvent::Title(title) => {
                                entry.title = Some(title);
                                title_changed = true;
                            }
                            AlacEvent::ResetTitle => {
                                entry.title = None;
                                title_changed = true;
                            }
                            _ => {}
                        }
                    }
                    let data = buf[..n].to_vec();
                    let working = entry.note_output().then(|| entry.group.clone());
                    entry.broadcast(|| Event::TermOutput {
                        term,
                        data: data.clone(),
                    });
                    if title_changed {
                        let title = entry.title.clone();
                        entry.broadcast(|| Event::TermTitle {
                            term,
                            title: title.clone(),
                        });
                    }
                    working
                };
                drop(entry);
                if let Some(group) = working {
                    state.locked().mark_working(group);
                }
            }
        }
    });

    let mut child = io.child;
    std::thread::spawn({
        let state = state.clone();
        move || {
            let _ = child.wait();
            let mut state = state.locked();
            state.broadcast(term, || Event::TermExit { term });
            state.terms.remove(&term);
            state.update_idle();
        }
    });
}

#[cfg(test)]
mod blocked_tests {
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::bottom_lines;
    use crate::blocked::is_blocked;

    #[test]
    fn permission_prompt_on_a_real_screen() {
        let mut term = Term::new(Config::default(), &TermSize::new(60, 20), VoidListener);
        let mut parser: Processor = Processor::new();
        let screen = "● Bash(ls)\r\n\x1b[2m──────────────────────────────\x1b[0m\r\n Bash command\r\n   ls -la\r\n Do you want to proceed?\r\n \x1b[36m❯ 1. Yes\x1b[0m\r\n   2. No\r\n";
        parser.advance(&mut term, screen.as_bytes());
        let lines = bottom_lines(&term);
        assert!(lines.iter().any(|line| line.contains("Do you want to proceed?")), "{lines:?}");
        assert!(is_blocked(&lines));

        let mut idle = Term::new(Config::default(), &TermSize::new(60, 20), VoidListener);
        parser.advance(&mut idle, "● Done.\r\n──────────────────────────────\r\n❯ \r\n".as_bytes());
        assert!(!is_blocked(&bottom_lines(&idle)));
    }
}

#[cfg(test)]
mod restart_tests {
    use super::{ClaudeSession, Shell, agent_name, claude_title, claude_working, resume_command_in, resumed_session, windows_words};

    #[test]
    fn tells_the_agents_apart() {
        assert_eq!(agent_name("claude --model opus").as_deref(), Some("claude"));
        assert_eq!(agent_name("/opt/homebrew/bin/codex").as_deref(), Some("codex"));
        assert_eq!(agent_name("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js").as_deref(), Some("claude"));
        assert_eq!(agent_name(r"C:\tools\claude.exe").as_deref(), Some("claude"));
        // A shell, or something that only mentions an agent further on.
        assert_eq!(agent_name("-zsh"), None);
        assert_eq!(agent_name("vim notes/claude"), None);
        assert!(claude_title("✳ Fix the login"));
        assert!(claude_title("⠂ Fix the login"));
        assert!(!claude_title("zsh"));
        assert_eq!(claude_working("✳ Fix the login"), Some(false));
        assert_eq!(claude_working("⠐ Fix the login"), Some(true));
        assert_eq!(claude_working("◐ Fix the login"), Some(true));
        assert_eq!(claude_working("◑ Fix the login"), Some(true));
        assert_eq!(claude_working(""), None);
        assert_eq!(claude_working("zsh"), None);
    }

    #[test]
    fn resumes_claude_code_with_its_options() {
        let resume = |args| resume_command_in(Shell::Posix, args, None);
        assert_eq!(resume("claude").as_deref(), Some("claude --resume"));
        assert_eq!(
            resume("claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --resume")
        );
        assert_eq!(resume("/Users/me/.local/bin/claude -c").as_deref(), Some("claude --resume"));
        assert_eq!(resume("claude --resume abc123").as_deref(), Some("claude --resume abc123"));
        assert_eq!(resume("claude -r abc123 --model opus").as_deref(), Some("claude --model opus --resume abc123"));
        assert_eq!(resume("claude --resume --model opus").as_deref(), Some("claude --model opus --resume"));
        assert_eq!(
            resume("node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js --model opus").as_deref(),
            Some("claude --model opus --resume")
        );
        assert_eq!(resume(r#""C:\Users\me\.local\bin\claude.exe" --model opus"#).as_deref(), Some("claude --model opus --resume"));
        assert_eq!(resume(r"node C:\npm\@anthropic-ai\claude-code\cli.js -c").as_deref(), Some("claude --resume"));
        assert_eq!(resume("-zsh"), None);
        assert_eq!(resume("vim claude.md"), None);
        // An id that isn't one isn't typed into the shell.
        assert_eq!(resume("claude --resume=x;rm").as_deref(), Some("claude --resume"));
        // `ps` lost the quotes of the prompt it began with: none of it is typed.
        assert_eq!(resume("claude fix login; then make clean").as_deref(), Some("claude --resume"));
        assert_eq!(
            resume("claude --allowedTools Bash(git:*) --model opus fix; make clean").as_deref(),
            Some("claude --allowedTools 'Bash(git:*)' --model opus --resume")
        );
        // An option in the prompt goes as an argument, never as a command.
        assert_eq!(resume("claude --verbose do it; rm -rf x").as_deref(), Some("claude --verbose do -rf x --resume"));
    }

    #[test]
    fn resumes_claude_code_in_powershell_with_its_quotes() {
        let resume = |args| resume_command_in(Shell::PowerShell, args, None);
        assert_eq!(
            windows_words(r#""C:\Program Files\claude.exe" --append-system-prompt "be brief" -c"#),
            [r"C:\Program Files\claude.exe", "--append-system-prompt", "be brief", "-c"]
        );
        assert_eq!(
            resume(r#"claude.exe --append-system-prompt "be brief" --add-dir "C:\My Projects" --model opus"#).as_deref(),
            Some(r"claude --append-system-prompt 'be brief' --add-dir 'C:\My Projects' --model opus --resume")
        );
        // A quote inside is doubled: the word can't end early and run the rest.
        assert_eq!(resume(r#"claude --name "x';calc;'""#).as_deref(), Some("claude --name 'x'';calc;''' --resume"));
        // `,` and `@` mean something to PowerShell.
        assert_eq!(resume("claude --tools a,b").as_deref(), Some("claude --tools 'a,b' --resume"));
        // The prompt it began with isn't typed again.
        assert_eq!(resume(r#"claude "fix it; then rm -rf x""#).as_deref(), Some("claude --resume"));
    }

    #[test]
    fn resumes_the_session_the_process_ran_in_its_config_folder() {
        let session = ClaudeSession {
            id: Some("d09e204f-44b6-45ed-8293-f4bf208351c4".to_string()),
            cwd: None,
            config_dir: Some("/Users/me/my claude's".to_string()),
        };
        assert_eq!(
            resume_command_in(Shell::Posix, "claude --dangerously-skip-permissions -c", Some(&session)).as_deref(),
            Some(r"CLAUDE_CONFIG_DIR='/Users/me/my claude'\''s' claude --dangerously-skip-permissions --resume d09e204f-44b6-45ed-8293-f4bf208351c4")
        );
        // The session file wins over the id it was started with (`--fork-session` makes another).
        assert_eq!(
            resume_command_in(Shell::Posix, "claude --resume abc --fork-session", Some(&session)).as_deref(),
            Some(r"CLAUDE_CONFIG_DIR='/Users/me/my claude'\''s' claude --resume d09e204f-44b6-45ed-8293-f4bf208351c4")
        );
        let unknown = ClaudeSession { id: None, cwd: None, config_dir: None };
        assert_eq!(resume_command_in(Shell::Posix, "claude", Some(&unknown)).as_deref(), Some("claude --resume"));
    }

    #[test]
    fn knows_the_session_a_restored_terminal_resumes() {
        assert_eq!(
            resumed_session("CLAUDE_CONFIG_DIR='/x' claude --model opus --resume d09e204f-44b6"),
            Some("d09e204f-44b6")
        );
        assert_eq!(resumed_session("claude --model opus --resume"), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn reads_a_process_environment() {
        // Its own: macOS hides the environment of the system's binaries.
        let pid = std::process::id();
        let home = std::env::var("HOME").unwrap();
        assert_eq!(crate::platform::process_env(pid, "HOME"), Some(home));
        assert_eq!(crate::platform::process_env(pid, "DEN_TEST_MISSING"), None);
    }
}

#[cfg(test)]
mod read_tests {
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::last_lines;

    #[test]
    fn joins_wrapped_lines_and_keeps_the_last_ones() {
        let mut term = Term::new(Config::default(), &TermSize::new(10, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        let text = "one\r\ntwo\r\nthree\r\nabcdefghijklmnop\r\n$ ";
        parser.advance(&mut term, text.as_bytes());
        assert_eq!(last_lines(&term, 100), ["one", "two", "three", "abcdefghijklmnop", "$"]);
        assert_eq!(last_lines(&term, 2), ["abcdefghijklmnop", "$"]);
    }
}

#[cfg(all(test, not(windows)))]
mod path_tests {
    use super::*;

    #[test]
    fn paths_from_a_windows_ui_use_this_machines_separator() {
        let mut request = Request::Rename {
            from: PathBuf::from("/home/me/repo\\src\\a.rs"),
            to: PathBuf::from("~\\Downloads\\a.rs"),
        };
        own_separators(&mut request);
        let Request::Rename { from, to } = request else { unreachable!() };
        assert_eq!(from, PathBuf::from("/home/me/repo/src/a.rs"));
        assert_eq!(to, PathBuf::from("~/Downloads/a.rs"));
    }

    #[test]
    fn resolves_the_home_folder() {
        let state: Shared = Arc::default();
        let home = dirs::home_dir().unwrap().canonicalize().unwrap();
        let response = handle(&state, 0, Request::Resolve { path: PathBuf::from("~") }).and_then(Reply::wait).unwrap();
        assert!(matches!(response, Response::Path(Some(path)) if path == home));
    }
}

#[cfg(test)]
mod panic_tests {
    use super::*;

    #[test]
    fn a_request_that_panics_fails_and_the_agent_goes_on() {
        let state: Shared = Arc::default();
        let poisoned = state.clone();
        let _ = std::thread::spawn(move || {
            let _state = poisoned.locked();
            panic!("a bug while holding the lock");
        })
        .join();
        assert!(state.is_poisoned());
        let response = handle(&state, 0, Request::TermList { group: String::new() }).and_then(Reply::wait);
        assert!(matches!(response, Ok(Response::TermList(terms)) if terms.is_empty()));
        let failed: Result<()> = caught(|| panic!("a bug in a request"));
        assert!(failed.is_err());
    }
}

#[cfg(all(test, unix))]
mod lock_tests {
    use super::*;

    #[test]
    fn a_busy_terminal_holds_up_only_itself() {
        let state: Shared = Arc::default();
        let dir = std::env::temp_dir();
        let cat = || Some(vec!["/bin/cat".to_string()]);
        let (Ok(Response::TermCreated { term: busy }), Ok(Response::TermCreated { term: other })) = (
            create(&state, None, "a".into(), dir.clone(), cat(), 80, 24),
            create(&state, None, "b".into(), dir, cat(), 80, 24),
        ) else {
            panic!("no terminals");
        };
        // As while its reader parses a large output.
        let busy = state.locked().term(busy).unwrap();
        let parsing = busy.locked();
        let (done, finished) = mpsc::channel();
        std::thread::spawn({
            let state = state.clone();
            move || {
                handle(&state, 0, Request::TermInput { term: other, data: b"x".to_vec() }).unwrap();
                handle(&state, 0, Request::TermResize { term: other, cols: 100, rows: 30 }).unwrap();
                handle(&state, 0, Request::TermRead { term: other, lines: 1 }).unwrap();
                let _ = done.send(());
            }
        });
        finished.recv_timeout(Duration::from_secs(2)).expect("the other terminal waited");
        drop(parsing);
        drop(busy);
        state.locked().terms.clear();
    }
}

