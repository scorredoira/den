//! The agent: owner of the terminals. Each connection has one thread reading
//! requests and another writing; each terminal, a thread reading its pty.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use alacritty_terminal::{grid::Dimensions as _, term::test::TermSize};
use anyhow::Result;
use proto::{AgentInfo, Event, PROTOCOL, Request, Response, ServerMessage, TermId, TermInfo};

use crate::{
    platform::{self, Listener},
    format, fs, git, lsp, ports, search,
    snapshot::{self, snapshot},
    tasks,
    handover::Handover,
};

mod activity;
mod claude;
mod connection;
mod restart;
mod terminals;

use self::{
    claude::agent_name,
    connection::{Outbox, serve},
    restart::{
        SAVE_TICKS, handed_ide, replace_agent, restart_file, restore_after_restart, save_for_restart, save_if_changed,
        shut_down_older_agents, take_over,
    },
    terminals::{AgentTerm, SharedTerm, create, gone, last_lines, term_size},
};

/// With no terminals and no connected UIs, the agent exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(10 * 60);

/// After a connection can't be accepted, how long before trying again.
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

type ConnId = u64;

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
        let command = send_command(&self.0, Asker::Ide(sender), args, PathBuf::from(group), Some(term)).map_err(|err| format!("{err:#}"))?;
        let answer = answer.recv_timeout(Duration::from_secs(10));
        // Not waited for any more: the app's answer, if it comes, has no one to go to.
        if answer.is_err() {
            self.0.locked().commands.remove(&command);
        }
        answer.map_err(|_| "den didn't answer".to_string())?
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
        let mut tick = 0u64;
        move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if tick.is_multiple_of(SAVE_TICKS) {
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
            if tick.is_multiple_of(SAVE_TICKS) {
                save_if_changed(&state, &mut saved);
            }
            tick = tick.wrapping_add(1);
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
            Err(err) => {
                // An error that lasts (out of descriptors) would otherwise
                // keep the loop spinning, and fill the log.
                eprintln!("accept failed: {err:#}");
                std::thread::sleep(ACCEPT_RETRY);
            }
        }
    }
}

/// Sends a `den` command, with the workspace of the terminal it ran in, to
/// the app showing that terminal (an app can have several windows, each
/// with its own connection) or else to the one that last said it runs them.
/// The command's number, which `CommandDone` answers.
fn send_command(
    state: &Shared,
    asker: Asker,
    args: Vec<String>,
    cwd: PathBuf,
    term: Option<TermId>,
) -> Result<u64> {
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
    Ok(command)
}

/// Removes the task at `path` (with `force`, to the Trash without the
/// repo's script); its terminals close with it.
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
            let (cols, rows) = term_size(cols, rows);
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
            let report = move |paths| {
                let _ = sender.send(ServerMessage::Event(Event::FsChanged {
                    root: root.clone(),
                    paths,
                }));
            };
            // An open file: watched whatever git ignores, wherever it is.
            let watcher = if path.is_file() { fs::watch_file(&path, report)? } else { fs::watch(&path, true, report)? };
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

#[cfg(all(test, not(windows)))]
mod home_tests {
    use super::*;

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
