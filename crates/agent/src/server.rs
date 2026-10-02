//! The agent: owner of the terminals. Each connection has one thread reading
//! requests and another writing; each terminal, a thread reading its pty.

use std::{
    collections::{HashMap, HashSet},
    io::Read as _,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
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
    ClientEnvelope, ClientMessage, Decoded, Event, PROTOCOL, Request, Response, ServerMessage, TermId, TermInfo,
};

use crate::{
    platform::{self, Listener, Stream},
    pty::Pty,
    blocked, format, fs, git, lsp, ports, search,
    snapshot::snapshot,
    tasks,
};

/// With no terminals and no connected UIs, the agent exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);

/// A task is working while its terminals produced output less than this long ago.
const WORKING_WINDOW: Duration = Duration::from_secs(2);

/// After this long without output, check whether the screen is asking for an answer.
const QUIET: Duration = Duration::from_millis(800);

type ConnId = u64;

/// Collects the events of the agent's emulator.
#[derive(Clone, Default)]
struct Listener_(Arc<Mutex<Vec<AlacEvent>>>);

impl EventListener for Listener_ {
    fn send_event(&self, event: AlacEvent) {
        self.0.lock().unwrap().push(event);
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
    subscribers: HashSet<ConnId>,
    last_output: Instant,
    /// Its first quiet moment has passed: what a shell or a resumed session
    /// prints on starting up is not work, so until then output doesn't mark
    /// the task as working (on an agent restart every task would end up
    /// "finished").
    settled: bool,
    /// Whether the screen (no output since `last_output`) asks for an answer,
    /// and up to which output it was checked.
    blocked: bool,
    checked: Instant,
}

#[derive(Default)]
struct State {
    terms: HashMap<TermId, AgentTerm>,
    clients: HashMap<ConnId, mpsc::Sender<ServerMessage>>,
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
    /// Connections of apps, which run `sik` commands: the last one runs them.
    apps: Vec<ConnId>,
    /// `sik` commands an app is running: the app, and who asked (its
    /// connection and request).
    commands: HashMap<u64, (ConnId, ConnId, Option<u64>)>,
    next_command: u64,
}

type Shared = Arc<Mutex<State>>;

impl State {
    fn send(&self, conn: ConnId, message: ServerMessage) {
        if let Some(client) = self.clients.get(&conn) {
            let _ = client.send(message);
        }
    }

    fn broadcast(&self, term: TermId, make: impl Fn() -> Event) {
        if let Some(entry) = self.terms.get(&term) {
            for conn in &entry.subscribers {
                self.send(*conn, ServerMessage::Event(make()));
            }
        }
    }

    fn broadcast_all(&self, event: impl Fn() -> Event) {
        for client in self.clients.values() {
            let _ = client.send(ServerMessage::Event(event()));
        }
    }

    /// Marks `term`'s task as working, notifying if it just started.
    fn note_output(&mut self, term: TermId) {
        let Some(entry) = self.terms.get_mut(&term) else {
            return;
        };
        entry.last_output = Instant::now();
        entry.blocked = false;
        if !entry.settled {
            return;
        }
        let group = entry.group.clone();
        if self.working.insert(group.clone()) {
            self.broadcast_all(|| Event::Activity {
                group: group.clone(),
                working: true,
            });
        }
    }

    /// Considers tasks without recent output as stopped.
    fn expire_activity(&mut self) {
        for entry in self.terms.values_mut() {
            if !entry.settled && entry.last_output.elapsed() >= WORKING_WINDOW {
                entry.settled = true;
            }
        }
        let stopped: Vec<String> = self
            .working
            .iter()
            .filter(|group| {
                !self
                    .terms
                    .values()
                    .any(|entry| &&entry.group == group && entry.last_output.elapsed() < WORKING_WINDOW)
            })
            .cloned()
            .collect();
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
        for entry in self.terms.values_mut() {
            if entry.checked != entry.last_output && entry.last_output.elapsed() >= QUIET {
                entry.checked = entry.last_output;
                entry.blocked = blocked::is_blocked(&bottom_lines(&entry.emulator));
            }
        }
        let now: HashSet<String> = self
            .terms
            .values()
            .filter(|entry| entry.blocked)
            .map(|entry| entry.group.clone())
            .collect();
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

    fn update_idle(&mut self) {
        self.idle_since = (self.terms.is_empty() && self.clients.is_empty()).then(Instant::now);
    }
}

pub fn run(listener: Listener) -> Result<()> {
    let state: Shared = Arc::default();
    restore_after_restart(&state);
    state.lock().unwrap().update_idle();

    std::thread::spawn({
        let state = state.clone();
        move || loop {
            std::thread::sleep(Duration::from_millis(500));
            state.lock().unwrap().expire_activity();
        }
    });

    std::thread::spawn({
        let state = state.clone();
        move || loop {
            std::thread::sleep(Duration::from_secs(30));
            let idle = state.lock().unwrap().idle_since;
            if idle.is_some_and(|since| since.elapsed() >= IDLE_EXIT) {
                eprintln!("no terminals or connections: exiting");
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
    let (tx, rx) = mpsc::channel::<ServerMessage>();
    let conn = {
        let mut state = state.lock().unwrap();
        let conn = state.next_conn;
        state.next_conn += 1;
        state.clients.insert(conn, tx);
        state.update_idle();
        conn
    };

    let mut writer = stream.try_clone_stream()?;
    std::thread::spawn(move || {
        while let Ok(message) = rx.recv() {
            if write_message(&mut writer, &message).is_err() {
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
                        state.lock().unwrap().send(conn, ServerMessage::Response { id, result });
                    }
                    continue;
                }
            };
            let id = message.id;
            own_separators(&mut message.request);
            // Answered when the app is done, not now.
            if let Request::Command { args, cwd, term } = message.request {
                if let Err(err) = send_command(&state, conn, id, args, cwd, term) {
                    reply_to(&state, conn, id, Err(err));
                }
                continue;
            }
            // Anything that may take a while (task scripts, git, searches) runs on
            // its own thread, so it doesn't hold up terminal keystrokes.
            if is_slow(&message.request) {
                let state = state.clone();
                std::thread::spawn(move || {
                    let reply = handle_slow(&state, message.request);
                    reply_to(&state, conn, id, reply);
                });
                continue;
            }
            // Watching a large folder takes a while on Linux (inotify watches
            // each folder in it).
            if matches!(message.request, Request::Watch { .. }) {
                let state = state.clone();
                std::thread::spawn(move || {
                    let reply = handle(&state, conn, message.request);
                    reply_to(&state, conn, id, reply);
                });
                continue;
            }
            let reply = handle(&state, conn, message.request);
            reply_to(&state, conn, id, reply);
        }
        Ok(())
    })();

    let mut state = state.lock().unwrap();
    state.clients.remove(&conn);
    state.watchers.remove(&conn);
    state.relays.remove(&conn);
    state.apps.retain(|app| *app != conn);
    // What that app was running won't be answered.
    let lost: Vec<_> = state.commands.extract_if(|_, (app, ..)| *app == conn).collect();
    for (_, (_, asker, id)) in lost {
        if let Some(id) = id {
            let result = Err("the app closed before answering".into());
            state.send(asker, ServerMessage::Response { id, result });
        }
    }
    for entry in state.terms.values_mut() {
        entry.subscribers.remove(&conn);
    }
    state.update_idle();
    result
}

/// Sends a `sik` command to the app that last said it runs them, with the
/// workspace of the terminal it ran in.
fn send_command(
    state: &Shared,
    conn: ConnId,
    id: Option<u64>,
    args: Vec<String>,
    cwd: PathBuf,
    term: Option<TermId>,
) -> Result<()> {
    let mut state = state.lock().unwrap();
    let app = *state
        .apps
        .last()
        .ok_or_else(|| anyhow::anyhow!("no sik app is connected to this machine"))?;
    let group = term.and_then(|term| state.terms.get(&term)).map(|entry| entry.group.clone());
    let term = term.filter(|_| group.is_some());
    state.next_command += 1;
    let command = state.next_command;
    state.commands.insert(command, (app, conn, id));
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
        state.lock().unwrap().send(conn, ServerMessage::Response { id, result });
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
        | Request::GitChanges { path, .. }
        | Request::GitDiff { path, .. }
        | Request::FindFiles { path }
        | Request::Search { path, .. }
        | Request::ReadFile { path }
        | Request::WriteFile { path, .. }
        | Request::ListDir { path }
        | Request::CreateFile { path }
        | Request::CreateDir { path }
        | Request::Trash { path }
        | Request::Watch { path }
        | Request::Unwatch { path }
        | Request::Git { path, .. }
        | Request::Replace { path, .. }
        | Request::Command { cwd: path, .. }
        | Request::Resolve { path } => fix(path),
        Request::Rename { from, to } => {
            fix(from);
            fix(to);
        }
        Request::Lsp { root, path, .. } | Request::LspResolve { root, path, .. } | Request::Format { root, path, .. } => {
            fix(root);
            fix(path);
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
        | Request::TermCwd { .. }
        | Request::SavePastedImage { .. }
        | Request::RepoList
        | Request::TaskList
        | Request::Version
        | Request::BlockedList
        | Request::Ports
        | Request::RelayConnect { .. }
        | Request::RelaySend { .. }
        | Request::RelayClose { .. }
        | Request::Serve
        | Request::CommandDone { .. }
        | Request::TermRead { .. } => {}
    }
}

fn is_slow(request: &Request) -> bool {
    matches!(
        request,
        Request::TaskCreate { .. }
            | Request::TaskRemove { .. }
            | Request::GitChanges { .. }
            | Request::GitDiff { .. }
            | Request::FindFiles { .. }
            | Request::Search { .. }
            | Request::ReadFile { .. }
            | Request::WriteFile { .. }
            | Request::ListDir { .. }
            | Request::Trash { .. }
            | Request::Git { .. }
            | Request::Lsp { .. }
            | Request::LspResolve { .. }
            | Request::Format { .. }
            | Request::Replace { .. }
            | Request::Ports
    )
}

fn handle_slow(state: &Shared, request: Request) -> Result<Response> {
    match request {
        Request::Ports => {
            let shells: HashMap<u32, String> = state
                .lock()
                .unwrap()
                .terms
                .values()
                .filter_map(|entry| Some((entry.pty.pid()?, entry.group.clone())))
                .collect();
            Ok(Response::Ports(ports::listening(&shells)))
        }
        Request::TaskCreate { repo, name, open } => {
            let task = tasks::add_repo(&repo).and_then(|repo| tasks::create(&repo, &name))?;
            if open {
                let path = task.path.clone();
                state
                    .lock()
                    .unwrap()
                    .broadcast_all(|| Event::OpenTask { path: path.clone() });
            }
            Ok(Response::Task(task))
        }
        Request::TaskRemove { path } => {
            tasks::remove(&path)?;
            // Its terminals close with it.
            let group = path.to_string_lossy().into_owned();
            let mut state = state.lock().unwrap();
            let terms: Vec<TermId> = state
                .terms
                .iter()
                .filter(|(_, entry)| entry.group == group)
                .map(|(term, _)| *term)
                .collect();
            for term in terms {
                state.broadcast(term, || Event::TermExit { term });
                state.terms.remove(&term);
            }
            state.update_idle();
            Ok(Response::Ok)
        }
        Request::GitChanges { path, uncommitted } => {
            let (base, files) = git::changes(&path, uncommitted)?;
            Ok(Response::Changes { base, files })
        }
        Request::GitDiff { path, file, uncommitted } => Ok(Response::Text(git::diff(&path, &file, uncommitted)?)),
        Request::Git { path, op } => git::run(&path, op),
        Request::Lsp { root, path, text, line, column, op } => lsp::request(&root, &path, &text, line, column, op),
        Request::LspResolve { root, path, list, item } => lsp::resolve(&root, &path, list, item),
        Request::Format { root, path, text } => format::format(&root, &path, &text),
        Request::FindFiles { path } => Ok(Response::Files(search::files(&path))),
        Request::Search {
            path,
            query,
            regex,
            case_sensitive,
            max_hits,
        } => {
            let (hits, truncated) = search::search(&path, &query, regex, case_sensitive, max_hits)?;
            Ok(Response::SearchResults { hits, truncated })
        }
        Request::Replace {
            path,
            files,
            query,
            regex,
            case_sensitive,
            replacement,
            preserve_case,
        } => {
            let (files, replacements) =
                search::replace(&path, &files, &query, regex, case_sensitive, &replacement, preserve_case)?;
            Ok(Response::Replaced { files, replacements })
        }
        Request::ReadFile { path } => Ok(Response::Bytes(fs::read(&path)?)),
        Request::WriteFile { path, data } => {
            fs::write(&path, &data)?;
            Ok(Response::Ok)
        }
        Request::ListDir { path } => Ok(Response::Dir(fs::list(&tasks::expand_home(&path))?)),
        Request::Trash { path } => {
            fs::trash(&path)?;
            Ok(Response::Ok)
        }
        _ => unreachable!("not a slow request"),
    }
}

fn handle(state: &Shared, conn: ConnId, request: Request) -> Result<Response> {
    match request {
        Request::Hello { .. } => Ok(Response::Hello {
            protocol: PROTOCOL,
            pid: std::process::id(),
        }),
        Request::Shutdown => {
            let mut state = state.lock().unwrap();
            save_for_restart(&state);
            // Dropping the terminals kills their processes.
            state.terms.clear();
            eprintln!("shutdown requested by a UI");
            std::process::exit(0);
        }
        Request::TermCreate {
            group,
            cwd,
            command,
            cols,
            rows,
        } => create(state, None, group, cwd, command, cols, rows),
        Request::TermList { group } => {
            let state = state.lock().unwrap();
            let mut terms: Vec<TermInfo> = state
                .terms
                .iter()
                .filter(|(_, entry)| entry.group == group)
                .map(|(term, entry)| TermInfo {
                    term: *term,
                    title: entry.title.clone(),
                })
                .collect();
            terms.sort_by_key(|info| info.term);
            Ok(Response::TermList(terms))
        }
        Request::TermAttach { term } => {
            // The snapshot and the subscription happen under the same lock: no pty
            // output falls between the two.
            let mut state = state.lock().unwrap();
            let entry = state.terms.get_mut(&term).ok_or_else(|| gone(term))?;
            entry.subscribers.insert(conn);
            let grid = entry.emulator.grid();
            use alacritty_terminal::grid::Dimensions as _;
            let (cols, rows) = (grid.columns() as u16, grid.screen_lines() as u16);
            let data = snapshot(&entry.emulator, entry.title.as_deref());
            Ok(Response::TermSnapshot { cols, rows, data })
        }
        Request::TermDetach { term } => {
            if let Some(entry) = state.lock().unwrap().terms.get_mut(&term) {
                entry.subscribers.remove(&conn);
            }
            Ok(Response::Ok)
        }
        Request::TermInput { term, data } => {
            let state = state.lock().unwrap();
            state.terms.get(&term).ok_or_else(|| gone(term))?.pty.write(data);
            Ok(Response::Ok)
        }
        Request::TermResize { term, cols, rows } => {
            let mut state = state.lock().unwrap();
            let entry = state.terms.get_mut(&term).ok_or_else(|| gone(term))?;
            entry
                .emulator
                .resize(TermSize::new(cols.max(2) as usize, rows.max(1) as usize));
            entry.pty.resize(cols, rows);
            Ok(Response::Ok)
        }
        Request::TermKill { term } => {
            let mut state = state.lock().unwrap();
            state.broadcast(term, || Event::TermExit { term });
            state.terms.remove(&term);
            state.update_idle();
            Ok(Response::Ok)
        }
        Request::RepoAdd { path } => {
            tasks::add_repo(&tasks::expand_home(&path))?;
            Ok(Response::Ok)
        }
        Request::TaskList => {
            let mut list = tasks::list();
            let state = state.lock().unwrap();
            for task in &mut list {
                task.working = state.working.contains(task.path.to_string_lossy().as_ref());
            }
            Ok(Response::Tasks(list))
        }
        Request::RepoRemove { path } => {
            tasks::remove_repo(&path)?;
            Ok(Response::Ok)
        }
        Request::RepoList => Ok(Response::Repos(tasks::repos())),
        Request::BlockedList => Ok(Response::Files(state.lock().unwrap().blocked.iter().cloned().collect())),
        Request::Version => Ok(Response::Text(own_build_id())),
        Request::Open { root, file } => {
            let state = state.lock().unwrap();
            let mut count = 0;
            for (other, client) in &state.clients {
                if *other != conn {
                    let event = Event::Open { root: root.clone(), file: file.clone() };
                    count += usize::from(client.send(ServerMessage::Event(event)).is_ok());
                }
            }
            Ok(Response::Count(count))
        }
        Request::TaskCreate { .. }
        | Request::TaskRemove { .. }
        | Request::GitChanges { .. }
        | Request::GitDiff { .. }
        | Request::FindFiles { .. }
        | Request::Search { .. }
        | Request::ReadFile { .. }
        | Request::WriteFile { .. }
        | Request::ListDir { .. }
        | Request::Trash { .. }
        | Request::Git { .. }
        | Request::Lsp { .. }
        | Request::LspResolve { .. }
        | Request::Format { .. }
        | Request::Replace { .. }
        | Request::Ports => unreachable!("handled on its own thread"),
        Request::Rename { from, to } => {
            fs::rename(&from, &to)?;
            Ok(Response::Ok)
        }
        Request::CreateFile { path } => {
            fs::create_file(&path)?;
            Ok(Response::Ok)
        }
        Request::CreateDir { path } => {
            fs::create_dir(&path)?;
            Ok(Response::Ok)
        }
        Request::Watch { path } => {
            let sender = state
                .lock()
                .unwrap()
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
            let mut state = state.lock().unwrap();
            if state.clients.contains_key(&conn) {
                state.watchers.entry(conn).or_default().insert(path, watcher);
            }
            Ok(Response::Ok)
        }
        Request::RelayConnect { port } => {
            let sender = state
                .lock()
                .unwrap()
                .clients
                .get(&conn)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("connection closed"))?;
            let relay = {
                let mut state = state.lock().unwrap();
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
            state.lock().unwrap().relays.entry(conn).or_default().insert(relay, connection);
            Ok(Response::Relay(relay))
        }
        Request::RelaySend { relay, line } => {
            let mut state = state.lock().unwrap();
            let connection = state
                .relays
                .get_mut(&conn)
                .and_then(|relays| relays.get_mut(&relay))
                .ok_or_else(|| anyhow::anyhow!("the relay is closed"))?;
            connection.send(&line)?;
            Ok(Response::Ok)
        }
        Request::RelayClose { relay } => {
            if let Some(relays) = state.lock().unwrap().relays.get_mut(&conn) {
                relays.remove(&relay);
            }
            Ok(Response::Ok)
        }
        Request::Serve => {
            let mut state = state.lock().unwrap();
            state.apps.retain(|app| *app != conn);
            state.apps.push(conn);
            Ok(Response::Ok)
        }
        Request::Command { .. } => unreachable!("answered when the app is done"),
        Request::CommandDone { command, result } => {
            let mut state = state.lock().unwrap();
            if let Some((_, asker, Some(id))) = state.commands.remove(&command) {
                state.send(asker, ServerMessage::Response { id, result: result.map(Response::Text) });
            }
            Ok(Response::Ok)
        }
        Request::Resolve { path } => {
            let resolved = tasks::expand_home(&path).canonicalize()?;
            Ok(Response::Path(Some(resolved)))
        }
        Request::TermRead { term, lines } => {
            let state = state.lock().unwrap();
            let entry = state.terms.get(&term).ok_or_else(|| gone(term))?;
            Ok(Response::Text(last_lines(&entry.emulator, lines as usize).join("\n")))
        }
        Request::Unwatch { path } => {
            if let Some(watchers) = state.lock().unwrap().watchers.get_mut(&conn) {
                watchers.remove(&path);
            }
            Ok(Response::Ok)
        }
        Request::SavePastedImage { extension, data } => {
            Ok(Response::Path(Some(save_pasted_image(&extension, &data)?)))
        }
        Request::TermCwd { term } => {
            let state = state.lock().unwrap();
            let entry = state.terms.get(&term).ok_or_else(|| gone(term))?;
            let cwd = entry.pty.foreground_pid().and_then(platform::process_cwd).or_else(|| Some(entry.cwd.path.clone()));
            Ok(Response::Path(cwd))
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
    /// Claude Code was running in it: the command that resumes it (the
    /// same options, plus `--continue`).
    claude: Option<String>,
}

/// Next to the socket (`agent-6.restart.json`), so an agent on another
/// socket (tests, another protocol) doesn't take it.
fn restart_file() -> Result<PathBuf> {
    Ok(proto::socket_path()?.with_extension("restart.json"))
}

/// Before shutting down to restart: notes each terminal's folder and whether
/// Claude Code was running in it. The processes die; their place doesn't.
fn save_for_restart(state: &State) {
    let terms: Vec<Restarted> = state
        .terms
        .iter()
        .map(|(term, entry)| {
            let foreground = entry.pty.foreground_pid();
            Restarted {
                term: *term,
                group: entry.group.clone(),
                cwd: foreground
                    .and_then(platform::process_cwd)
                    .unwrap_or_else(|| entry.cwd.path.clone()),
                cols: entry.emulator.columns() as u16,
                rows: entry.emulator.screen_lines() as u16,
                claude: foreground.and_then(platform::process_args).and_then(|args| resume_command(&args)),
            }
        })
        .collect();
    let saved = restart_file().and_then(|path| Ok(std::fs::write(path, serde_json::to_vec(&terms)?)?));
    if let Err(err) = saved {
        eprintln!("could not save the terminals for the restart: {err:#}");
    }
}

/// On startup after a restart: opens the terminals the previous agent had,
/// with the same ids, and resumes Claude Code where it was running.
fn restore_after_restart(state: &Shared) {
    let Ok(path) = restart_file() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let terms: Vec<Restarted> = match serde_json::from_slice(&bytes) {
        Ok(terms) => terms,
        Err(err) => return eprintln!("invalid {}: {err:#}", path.display()),
    };
    for saved in terms {
        // The folder may be gone (a removed task): the terminal goes with it.
        if !saved.cwd.is_dir() {
            continue;
        }
        let command = saved.claude.map(|command| format!("{command}\r").into_bytes());
        let created = create(state, Some(saved.term), saved.group, saved.cwd, None, saved.cols, saved.rows);
        match (created, command) {
            (Ok(_), Some(command)) => {
                // Typed into the shell, so it stays when Claude exits.
                if let Some(entry) = state.lock().unwrap().terms.get(&saved.term) {
                    entry.pty.write(command);
                }
            }
            (Ok(_), None) => {}
            (Err(err), _) => eprintln!("could not reopen terminal {}: {err:#}", saved.term),
        }
    }
}

/// If a command line is Claude Code's (the native `claude` binary or the npm
/// package run by node), the command that resumes it: the same options plus
/// `--continue`, unless it already resumes a given session.
fn resume_command(args: &str) -> Option<String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let start = words.iter().position(|word| {
        let word = word.trim_matches('"');
        matches!(word.rsplit(['/', '\\']).next(), Some("claude" | "claude.exe" | "claude.cmd"))
            || word.replace('\\', "/").contains("@anthropic-ai/claude-code")
    })?;
    let mut command = vec!["claude"];
    command.extend(words[start + 1..].iter().filter(|word| !matches!(**word, "-c" | "--continue")));
    if !command.iter().any(|word| matches!(*word, "-r" | "--resume") || word.starts_with("--resume=")) {
        command.push("--continue");
    }
    Some(command.join(" "))
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
        let mut state = state.lock().unwrap();
        let term = id.unwrap_or(state.next_term);
        state.next_term = state.next_term.max(term + 1);
        term
    };
    let (pty, io) = Pty::spawn(term, &cwd, command.as_deref(), cols, rows)?;
    let events = Listener_::default();
    let emulator = Term::new(
        Config::default(),
        &TermSize::new(cols as usize, rows as usize),
        events.clone(),
    );
    {
        let mut state = state.lock().unwrap();
        state.terms.insert(
            term,
            AgentTerm {
                group,
                pty,
                cwd: crate::shell_cwd::ShellCwd::new(cwd),
                emulator,
                parser: Processor::new(),
                events,
                title: None,
                subscribers: HashSet::new(),
                last_output: Instant::now(),
                settled: false,
                blocked: false,
                checked: Instant::now(),
            },
        );
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
                let mut state = state.lock().unwrap();
                let Some(entry) = state.terms.get_mut(&term) else {
                    // ConPTY may emit final output while closing. Keep draining
                    // until EOF; its close runs on a separate thread.
                    continue;
                };
                entry.cwd.advance(&buf[..n]);
                entry.parser.advance(&mut entry.emulator, &buf[..n]);
                let events = std::mem::take(&mut *entry.events.0.lock().unwrap());
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
                state.note_output(term);
                state.broadcast(term, || Event::TermOutput {
                    term,
                    data: data.clone(),
                });
                if title_changed {
                    let title = state.terms[&term].title.clone();
                    state.broadcast(term, || Event::TermTitle {
                        term,
                        title: title.clone(),
                    });
                }
            }
        }
    });

    let mut child = io.child;
    std::thread::spawn({
        let state = state.clone();
        move || {
            let _ = child.wait();
            let mut state = state.lock().unwrap();
            state.broadcast(term, || Event::TermExit { term });
            state.terms.remove(&term);
            state.update_idle();
        }
    });

    Ok(Response::TermCreated { term })
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
    use super::resume_command;

    #[test]
    fn resumes_claude_code_with_its_options() {
        let resume = resume_command;
        assert_eq!(resume("claude").as_deref(), Some("claude --continue"));
        assert_eq!(
            resume("claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --continue")
        );
        assert_eq!(resume("/Users/me/.local/bin/claude -c").as_deref(), Some("claude --continue"));
        assert_eq!(resume("claude --resume abc123").as_deref(), Some("claude --resume abc123"));
        assert_eq!(
            resume("node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js --model opus").as_deref(),
            Some("claude --model opus --continue")
        );
        assert_eq!(resume(r#""C:\Users\me\.local\bin\claude.exe" --model opus"#).as_deref(), Some("claude --model opus --continue"));
        assert_eq!(resume(r"node C:\npm\@anthropic-ai\claude-code\cli.js -c").as_deref(), Some("claude --continue"));
        assert_eq!(resume("-zsh"), None);
        assert_eq!(resume("vim claude.md"), None);
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
        let response = handle(&state, 0, Request::Resolve { path: PathBuf::from("~") }).unwrap();
        assert!(matches!(response, Response::Path(Some(path)) if path == home));
    }
}
