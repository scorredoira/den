//! The debugger of a workspace: breakpoints, and a session with a program
//! that speaks the debug protocol (`protocol`), reached through the agent so
//! a program on a server is debugged like a local one.
//!
//! Nothing here knows the language or VM of the program. The launch file
//! (`.den/debug.json`) says which command starts it, given the open file,
//! and on which port it listens: the program decides what debugging that
//! file means.

mod breakpoints;
mod commands;
pub mod hover;
pub mod panel;
pub mod protocol;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use client::{Client, RelayUpdate};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;
use proto::{Request, Response, TermId};
use serde::Deserialize;
use serde_json::{Map, Value, json};

pub use breakpoints::{Breakpoint, Breakpoints, LineEdit};
pub use commands::WaitFor;
pub use panel::DebugView;
use protocol::{Event, Message, Stop, Var};

use ui_term::TerminalView;

use crate::config::{Config, DebugPart, DebugSaved};
use crate::drag_drop::DropPlacement;

/// Where the launch file is, relative to the workspace.
pub const LAUNCH_FILE: &str = ".den/debug.json";

const LAUNCH_TEMPLATE: &str = r#"{
    "command": "sim -d --debugger-port ${port} ${file}"
}
"#;

/// How often to look whether the program listens on the port of its page.
const OPEN_POLL: Duration = Duration::from_millis(100);

/// How long a value's card waits, once the pointer leaves its name, before
/// it goes or shows another name's: time to reach it across other names.
const HOVER_GRACE: Duration = Duration::from_millis(300);

/// How long to keep trying to reach a program that is starting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How often the command's last line is read while it starts the program.
const PROGRESS_POLL: Duration = Duration::from_millis(500);

/// How long Restart waits for the program it stopped to free its terminal
/// and its port.
const RESTART_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the VMs running are counted: they come and go (a server's
/// requests) without events.
const THREADS_POLL: Duration = Duration::from_secs(1);

/// A VM that resumes keeps showing its stop this long, dimmed: a step that
/// stops again right away replaces it without the views blinking empty.
const RESUME_GRACE: Duration = Duration::from_millis(250);

/// Children of a value asked for at a time.
const PAGE: u64 = 200;

/// How the program is started and reached.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Launch {
    /// A shell command line that starts the program, run in a terminal.
    /// Without it, the debugger attaches to a program already running.
    /// `${port}` in it is a free port, which `port` then doesn't say.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    4444
}

#[derive(Deserialize)]
pub struct LaunchFile {
    #[serde(flatten)]
    pub launch: Launch,
    /// How the project runs a test, for the Run and Debug on each one's line.
    #[serde(default)]
    pub tests: Option<Tests>,
    /// Where the launch command runs the program (`${target}`): names of
    /// the project's, picked in the panel's toolbar.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Files from when there were several to choose from: the first one.
    #[serde(default)]
    configurations: Vec<Launch>,
}

/// `match` finds a test's declaration on a line, its first group being the
/// test's name; `run` and `debug` start it, with `${file}` and `${test}`.
/// `port` is where `debug` listens, unless it has `${port}`: apart from a
/// program that may be running, so a test is never attached to it.
#[derive(Clone, Debug, Deserialize)]
pub struct Tests {
    #[serde(rename = "match", deserialize_with = "regex_field")]
    pattern: regex::Regex,
    run: String,
    debug: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Tests {
    /// The test declared on `line`, if one is.
    pub fn name_in(&self, line: &str) -> Option<String> {
        let captures = self.pattern.captures(line)?;
        captures.get(1).map(|name| name.as_str().to_string())
    }

    pub fn command(&self, debug: bool, test: &str) -> String {
        let command = if debug { &self.debug } else { &self.run };
        command.replace("${test}", test)
    }
}

fn regex_field<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<regex::Regex, D::Error> {
    let pattern = String::deserialize(deserializer)?;
    regex::Regex::new(&pattern).map_err(serde::de::Error::custom)
}

pub fn parse_launch_file(text: &str) -> Result<LaunchFile, String> {
    let mut file: LaunchFile = serde_json::from_str(text).map_err(|err| format!("{LAUNCH_FILE}: {err}"))?;
    if file.launch.command.is_none() && !file.configurations.is_empty() {
        file.launch = file.configurations.remove(0);
    }
    Ok(file)
}

/// The program's page in its `hello`, a URL of the loopback: a server's,
/// opened once it listens there. Any other address is not opened.
fn page_of(hello: &Map<String, Value>) -> Option<String> {
    hello
        .get("page")
        .and_then(Value::as_str)
        .filter(|url| Client::loopback_port(url).is_some())
        .map(str::to_string)
}

/// The target a launch runs on: the one picked, if the launch file still
/// lists it, else its first. None without targets.
fn current_target<'a>(targets: &'a [String], chosen: Option<&str>) -> Option<&'a str> {
    targets
        .iter()
        .find(|target| Some(target.as_str()) == chosen)
        .or_else(|| targets.first())
        .map(String::as_str)
}

/// `${target}` in a command is the target picked, empty without targets.
pub fn with_target(command: &str, target: Option<&str>) -> String {
    command.replace("${target}", target.unwrap_or_default())
}

/// A configuration's `command` as it is run: `${file}` is the open file,
/// relative to the workspace, quoted for the shell when it needs it. With no
/// file open it is empty, and the program decides what to debug without one.
pub fn command_line(command: &str, file: Option<&str>) -> String {
    let Some(file) = file else {
        return command.replace("${file}", "");
    };
    let quoted = if file.chars().all(|c| c.is_alphanumeric() || "/._-+".contains(c)) {
        file.to_string()
    } else {
        format!("'{}'", file.replace('\'', r"'\''"))
    };
    command.replace("${file}", &quoted)
}

/// A value shown by hovering its name, opened like a variable.
pub struct HoverValue {
    pub var: Var,
    /// Where the name is, in window coordinates.
    pub anchor: Bounds<Pixels>,
}

pub enum DebugEvent {
    /// Show `line` of `path`: where a VM stopped (`focus`: it just stopped)
    /// or something clicked in the panel.
    Show { path: PathBuf, line: u32, focus: bool },
    /// Breakpoints or the line stopped at changed: the editors redraw their
    /// marks.
    Marks,
    /// Run `line` in the debugger's terminal (`term` if it's still open)
    /// for the session `session`; the workspace answers with `set_ran` and
    /// `set_terminal`.
    Run { session: u64, term: Option<TermId>, line: String },
    /// Stop what runs in the debugger's terminal.
    Interrupt { term: TermId },
    /// The breakpoint editor closed: the keys go back to the code.
    Refocus,
    /// Show the panel (it started or stopped somewhere).
    Reveal,
    /// The program asked to show a place (an inspect pick on a phone or in
    /// Chrome): Den's window comes to the front.
    Raise,
    /// Its close button.
    Hide,
}

type Reply = Box<dyn FnOnce(&mut Debugger, Result<Map<String, Value>, String>, &mut Context<Debugger>)>;

/// A connection to a program.
struct Conn {
    client: Arc<Client>,
    relay: u64,
    next_id: u64,
    pending: HashMap<u64, Reply>,
    /// The program's working directory, which its paths are relative to.
    cwd: PathBuf,
}

/// A stopped VM. `resumed` while it runs again within the grace period.
struct VmStop {
    stop: Stop,
    serial: u64,
    resumed: bool,
    /// Asked to go on (a step, continue), and not yet said it did: still
    /// shown stopped, but no longer what `den debug wait` waits for.
    going: bool,
}

#[derive(Clone, Default)]
struct Children {
    vars: Vec<Var>,
    /// The last page came back full: there may be more, for a value whose
    /// `count` isn't known (the module's).
    more: bool,
    /// Why the last page couldn't be had: shown instead of loading forever.
    error: Option<String>,
}

struct Watch {
    expr: String,
    result: Option<Result<Var, String>>,
}

/// Lines the console keeps.
const MAX_CONSOLE: usize = 5000;

enum ConsoleLine {
    Info(String),
    Output { text: String, path: Option<PathBuf>, line: u32, error: bool },
    Input(String),
    /// A value and the VM it was evaluated in: its `ref` is cleared once
    /// that VM goes on, as it no longer names anything.
    Result(Var, u64),
    Error(String),
}

/// How a session started, which Restart starts again.
#[derive(Clone)]
struct Started {
    launch: Launch,
    /// The test it debugs, launched from the code.
    test: Option<(PathBuf, String)>,
    /// What `${file}` was when its command ran (`None` until it ran): the
    /// file open then, not the one a stop opened since.
    file: Option<Option<String>>,
}

/// What the session is doing.
#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    /// Starting the program or waiting for it to listen.
    Connecting(String),
    Connected,
}

/// Editing a breakpoint's condition, hit count or log message.
pub struct BreakpointEdit {
    pub path: PathBuf,
    pub line: u32,
    pub kind: EditKind,
    pub input: Entity<InputState>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum EditKind {
    Condition,
    Hit,
    Log,
}

impl EditKind {
    pub fn label(self) -> &'static str {
        match self {
            EditKind::Condition => "Expression",
            EditKind::Hit => "Hit Count",
            EditKind::Log => "Log Message",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            EditKind::Condition => "Stop when this is true, e.g. i == 3",
            EditKind::Hit => "Stop on the nth hit: 5, >= 5 or % 5",
            EditKind::Log => "Print instead of stopping, e.g. total is {total}",
        }
    }
}

/// Editing a value in the variables view.
struct ValueEdit {
    key: String,
    target: String,
    input: Entity<InputState>,
}

pub struct Debugger {
    root: PathBuf,
    session_key: String,
    client: Option<Arc<Client>>,
    pub breakpoints: Breakpoints,
    watches: Vec<Watch>,
    uncaught: bool,
    all: bool,
    launch_error: Option<String>,
    /// How the project runs a test (`tests` in the launch file).
    pub tests: Option<Tests>,
    /// The launch file's `targets`, and the one picked (kept per workspace).
    pub targets: Vec<String>,
    chosen_target: Option<String>,

    status: Status,
    conn: Option<Conn>,
    /// Bumped by every start and stop: work of an older session is dropped.
    generation: u64,
    /// The terminal the launch command runs in, reused by the next launch.
    term: Option<TermId>,
    /// That terminal, drawn in the console (the terminals' area keeps it
    /// out of its tabs).
    term_view: Option<Entity<TerminalView>>,
    /// The part a part dragged over its tab would go beside, and where.
    part_drop: Option<(DebugPart, DropPlacement)>,
    /// The session whose command went to a terminal not known yet (a new
    /// one), and whether Stop was asked meanwhile: that terminal is
    /// interrupted once known, not the one before.
    term_unknown: Option<(u64, bool)>,
    launched: bool,
    /// The program's page (`page` in its `hello`) opens once it listens:
    /// a launch started it, and not a restart.
    open_page: bool,
    running: u64,
    stops: BTreeMap<u64, VmStop>,
    serial: u64,
    focus: Option<u64>,
    frame: usize,
    locals: Vec<Var>,
    /// Locals whose value changed since the VM's last stop in the same
    /// function: the step that changed them.
    changed: HashSet<String>,
    globals: u64,
    children: HashMap<u64, Children>,
    loading: HashSet<u64>,
    /// Keys of the expanded values, kept across stops.
    expanded: HashSet<String>,
    /// The console's last lines (`MAX_CONSOLE` at most: a chatty program
    /// doesn't make it heavier with every line).
    console: Vec<ConsoleLine>,
    /// Lines written to the console since it began, and of them those
    /// dropped from the start.
    console_written: usize,
    console_dropped: usize,
    console_scroll: ScrollHandle,
    /// Lines of the console already scrolled to (of `console_written`).
    console_seen: usize,
    console_input: Entity<InputState>,
    watch_input: Entity<InputState>,
    history: Vec<String>,
    history_at: Option<usize>,
    value_edit: Option<ValueEdit>,
    pub edit: Option<BreakpointEdit>,
    /// The value shown by hovering its name in the code.
    pub hover: Option<HoverValue>,
    /// The pointer is over the card or its name: the code under the card
    /// asks for nothing.
    hover_inside: bool,
    /// What replaces the card once the grace is over: another name's value,
    /// or nothing.
    hover_change: Option<(Option<(String, Bounds<Pixels>)>, Task<()>)>,
    /// The test (its file and name) launched from the code, until it connects.
    launching_test: Option<(PathBuf, String)>,
    /// How the session started, for Restart.
    started: Option<Started>,
    /// The line the launch ran in its terminal, `${file}` replaced: what
    /// `den debug state` says is being debugged.
    ran: Option<String>,
    /// The program's page, from its `hello`.
    page: Option<String>,
    /// The last place the program asked to show (`reveal`), 1-based line.
    revealed: Option<(String, u32)>,
    /// The session ended without Stop (the program ended, or failed to
    /// start): the debugger stays in sight, with what it said, until Stop.
    ended: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DebugEvent> for Debugger {}

impl Debugger {
    pub fn new(
        root: PathBuf,
        client: Option<Arc<Client>>,
        session_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let saved = Config::get(cx).debug.get(&session_key).cloned().unwrap_or_default();
        let console_input = cx.new(|cx| InputState::new(window, cx).placeholder("Evaluate an expression or assign: x = 5"));
        let watch_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add an expression to watch"));
        let subscriptions = vec![
            cx.subscribe_in(&console_input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.submit_console(window, cx);
                }
            }),
            cx.subscribe_in(&watch_input, window, |this, input, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let expr = input.read(cx).value().trim().to_string();
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                    this.add_watch(expr, cx);
                }
            }),
        ];
        Self {
            breakpoints: Breakpoints::load(&root, &saved.breakpoints),
            watches: saved.watches.into_iter().map(|expr| Watch { expr, result: None }).collect(),
            uncaught: saved.uncaught,
            all: saved.all,
            term: saved.terminal,
            chosen_target: saved.target,
            targets: Vec::new(),
            term_view: None,
            part_drop: None,
            term_unknown: None,
            root,
            session_key,
            client,
            launch_error: None,
            status: Status::Idle,
            conn: None,
            generation: 0,
            launched: false,
            open_page: false,
            running: 0,
            stops: BTreeMap::new(),
            serial: 0,
            focus: None,
            frame: 0,
            locals: Vec::new(),
            changed: HashSet::new(),
            globals: 0,
            children: HashMap::new(),
            loading: HashSet::new(),
            expanded: HashSet::new(),
            console: Vec::new(),
            console_scroll: ScrollHandle::new(),
            console_written: 0,
            console_dropped: 0,
            console_seen: 0,
            console_input,
            watch_input,
            history: Vec::new(),
            history_at: None,
            value_edit: None,
            edit: None,
            hover: None,
            hover_inside: false,
            hover_change: None,
            launching_test: None,
            started: None,
            ran: None,
            page: None,
            revealed: None,
            ended: false,
            tests: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn set_client(&mut self, client: Arc<Client>, cx: &mut Context<Self>) {
        self.client = Some(client);
        // the relay died with the old connection
        self.end("Lost the connection to the agent", cx);
    }

    fn save(&self, cx: &mut Context<Self>) {
        let saved = DebugSaved {
            breakpoints: self.breakpoints.save(&self.root),
            watches: self.watches.iter().map(|watch| watch.expr.clone()).collect(),
            uncaught: self.uncaught,
            all: self.all,
            terminal: self.term,
            target: self.chosen_target.clone(),
        };
        let key = self.session_key.clone();
        Config::update(cx, move |config| {
            if saved == DebugSaved::default() {
                config.debug.remove(&key);
            } else {
                config.debug.insert(key, saved);
            }
        });
    }

    // ---- state the workspace reads ----

    /// The target the launch command runs the program on (`${target}`).
    pub fn target(&self) -> Option<&str> {
        current_target(&self.targets, self.chosen_target.as_deref())
    }

    /// Picks the target the next launch runs on.
    pub fn set_target(&mut self, target: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.targets.iter().any(|known| known == target) {
            return Err(if self.targets.is_empty() {
                format!("{target}: {LAUNCH_FILE} has no targets")
            } else {
                format!("{target}: not a target; one of {}", self.targets.join(", "))
            });
        }
        self.chosen_target = Some(target.to_string());
        self.save(cx);
        cx.notify();
        Ok(())
    }

    /// The terminal the launch command runs in.
    pub fn terminal(&self) -> Option<TermId> {
        self.term
    }

    /// That terminal's view, for the console, or `None` once it's gone.
    pub fn set_terminal_view(&mut self, view: Option<Entity<TerminalView>>, cx: &mut Context<Self>) {
        self.term_view = view;
        cx.notify();
    }

    pub fn is_stopped(&self) -> bool {
        self.current().is_some()
    }

    /// The focused VM is stopped, and neither resuming nor asked to go on:
    /// it can be evaluated in, run to a line or jumped.
    pub fn is_halted(&self) -> bool {
        self.halted().is_some()
    }

    fn halted(&self) -> Option<u64> {
        self.focus.filter(|vm| self.stops.get(vm).is_some_and(|stop| !stop.resumed && !stop.going))
    }

    /// The test `test` of `path` was launched and its program isn't connected yet.
    pub fn is_launching(&self, path: &Path, test: &str) -> bool {
        self.launching_test.as_ref().is_some_and(|(at, name)| at == path && name == test)
    }

    /// A session started or connected.
    pub fn is_active(&self) -> bool {
        self.status != Status::Idle
    }

    /// The debugger is in sight: a session is active, or one ended and
    /// Stop hasn't closed it.
    pub fn is_shown(&self) -> bool {
        self.is_active() || self.ended
    }

    fn current(&self) -> Option<&VmStop> {
        self.stops.get(&self.focus?)
    }

    /// Where the selected frame of the focused VM is: its file, its line
    /// (0-based) and whether it's the innermost frame (where execution is).
    pub fn execution(&self) -> Option<(PathBuf, u32, bool)> {
        let stop = self.current()?;
        let frame = stop.stop.frames.get(self.frame)?;
        Some((self.local_path(&frame.file), frame.line.saturating_sub(1), self.frame == 0))
    }

    /// The exception the focused VM stopped at, if it did.
    pub fn exception(&self) -> Option<&str> {
        let stop = self.current()?;
        (self.frame == 0).then_some(())?;
        stop.stop.exception.as_ref().map(|exc| exc.message.as_str())
    }

    /// The variables of the selected frame, for the values shown in the code.
    pub fn frame_locals(&self) -> &[Var] {
        if self.is_stopped() { &self.locals } else { &[] }
    }

    /// The path of a file named by the program, absolute.
    fn local_path(&self, file: &str) -> PathBuf {
        let cwd = self.conn.as_ref().map_or(self.root.as_path(), |conn| conn.cwd.as_path());
        cwd.join(file)
    }

    /// How the program names a file: relative to its working directory.
    fn program_path(&self, path: &Path) -> String {
        let cwd = self.conn.as_ref().map_or(self.root.as_path(), |conn| conn.cwd.as_path());
        let path = path.strip_prefix(cwd).unwrap_or(path);
        path.to_string_lossy().replace('\\', "/")
    }

    // ---- launching ----

    /// F5: continues the focused VM, or starts a session.
    pub fn start_or_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_stopped() {
            self.resume("continue", cx);
        } else if self.status == Status::Idle {
            self.start(window, cx);
        }
    }

    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status != Status::Idle {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.info("No agent: can't debug".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        };
        self.started = None;
        self.status = Status::Connecting("Reading the launch file…".into());
        self.generation += 1;
        let generation = self.generation;
        let path = self.root.join(LAUNCH_FILE);
        cx.notify();
        // after the notify: the workspace is debugging by then, and the tab
        // shows in the debugging layout, not in the editing one it goes back to
        cx.emit(DebugEvent::Reveal);
        cx.spawn_in(window, async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                let file = match read {
                    Ok(Response::Bytes(bytes)) => parse_launch_file(&String::from_utf8_lossy(&bytes)),
                    Ok(other) => Err(format!("unexpected response {other:?}")),
                    Err(_) => Err(format!("There is no {LAUNCH_FILE}: create it to say how to start the program.")),
                };
                match file {
                    Ok(mut file) => {
                        this.tests = file.tests;
                        this.targets = file.targets;
                        this.launch_error = None;
                        let target = this.target().map(str::to_string);
                        if let Some(command) = &mut file.launch.command {
                            *command = with_target(command, target.as_deref());
                        }
                        this.begin(file.launch, true, window, cx);
                    }
                    Err(error) => {
                        this.launch_error = Some(error.clone());
                        this.fail(error, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Debugs `command`, which listens on `port`: the test `test` of `path`, from the code.
    pub fn launch_command(
        &mut self,
        command: String,
        port: u16,
        path: PathBuf,
        test: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.status != Status::Idle {
            self.info("A program is being debugged: stop it first (Shift-F5)".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        }
        if self.client.is_none() {
            self.info("No agent: can't debug".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        }
        self.generation += 1;
        self.launching_test = Some((path, test));
        self.begin(Launch { command: Some(command), port }, false, window, cx);
        // after begin's notify, as in start
        cx.emit(DebugEvent::Reveal);
    }

    /// Reads the launch file again: its problems show in the panel, its
    /// tests in the code.
    pub fn refresh_launches(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.join(LAUNCH_FILE);
        cx.spawn(async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update(cx, |this, cx| {
                match read.ok().and_then(|response| match response {
                    Response::Bytes(bytes) => Some(parse_launch_file(&String::from_utf8_lossy(&bytes))),
                    _ => None,
                }) {
                    Some(Ok(file)) => {
                        this.tests = file.tests;
                        this.targets = file.targets;
                        this.launch_error = None;
                    }
                    Some(Err(error)) => {
                        this.tests = None;
                        this.targets = Vec::new();
                        this.launch_error = Some(error);
                    }
                    None => {
                        this.tests = None;
                        this.targets = Vec::new();
                        this.launch_error = Some(format!("There is no {LAUNCH_FILE}."));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn create_launch_file(&mut self, cx: &mut Context<Self>) -> PathBuf {
        let path = self.root.join(LAUNCH_FILE);
        if let Some(client) = self.client.clone() {
            let dir = self.root.join(".den");
            let file = path.clone();
            cx.spawn(async move |this, cx| {
                let _ = client.request(Request::CreateDir { path: dir }).await;
                let _ = client
                    .request(Request::WriteFile { path: file, data: LAUNCH_TEMPLATE.as_bytes().to_vec() })
                    .await;
                this.update(cx, |this, cx| this.refresh_launches(cx)).ok();
            })
            .detach();
        }
        path
    }

    /// Adds a line to the console, dropping the oldest ones past `MAX_CONSOLE`
    /// (a few hundred at a time).
    fn push_console(&mut self, line: ConsoleLine) {
        self.console.push(line);
        self.console_written += 1;
        if self.console.len() > MAX_CONSOLE + 500 {
            let drop = self.console.len() - MAX_CONSOLE;
            self.console.drain(..drop);
            self.console_dropped += drop;
        }
    }

    /// With `open_page`, the program's page is opened once it listens.
    fn begin(&mut self, launch: Launch, open_page: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.console.clear();
        self.console_dropped = self.console_written;
        self.ran = None;
        self.page = None;
        self.revealed = None;
        self.launched = false;
        self.open_page = open_page;
        self.started = Some(Started { launch: launch.clone(), test: self.launching_test.clone(), file: None });
        self.status = Status::Connecting(format!("Connecting to port {}…", launch.port));
        self.connect(launch.port, launch.command, window, cx);
        cx.notify();
    }

    /// A session connected, for tests.
    #[cfg(test)]
    pub fn pretend_connected(&mut self, cx: &mut Context<Self>) {
        // attached to a program on a port nothing listens on: a restart tries it again
        let launch = parse_launch_file(r#"{"port":1}"#).map(|file| file.launch).ok();
        self.started = launch.map(|launch| Started { launch, test: None, file: None });
        self.status = Status::Connected;
        cx.notify();
    }

    /// The program never listened, for tests: the session fails.
    #[cfg(test)]
    pub fn pretend_failed(&mut self, cx: &mut Context<Self>) {
        self.fail("the program ended before it listened".into(), cx);
    }

    /// A line from the program, for tests.
    #[cfg(test)]
    pub fn receive(&mut self, line: &str, cx: &mut Context<Self>) {
        self.on_line(line, cx);
    }

    /// The launch's line as its terminal runs it (`ran`), and the file its
    /// `${file}` was, which a restart of the session debugs again.
    pub fn set_ran(&mut self, session: u64, line: String, file: Option<String>) {
        if session != self.generation {
            return;
        }
        self.ran = Some(line);
        if let Some(started) = &mut self.started
            && started.file.is_none()
        {
            started.file = Some(file);
        }
    }

    /// The terminal the command of `session` went to, once it is known.
    pub fn set_terminal(&mut self, session: u64, term: Option<TermId>, cx: &mut Context<Self>) {
        if let Some((_, interrupt)) = self.term_unknown.take_if(|(unknown, _)| *unknown == session)
            && interrupt
            && let Some(term) = term
        {
            cx.emit(DebugEvent::Interrupt { term });
        }
        if term.is_some() && term != self.term {
            self.term = term;
            self.save(cx);
        }
    }

    /// Tries to reach the program until it listens, the time is up or the
    /// command started ends. A program that already listens is attached to;
    /// otherwise `command` starts it. A command with `${port}` listens on a
    /// free port of its own: it is always started, never attached to
    /// another program, a debugger of another window included.
    fn connect(
        &mut self,
        port: u16,
        mut command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.generation;
        let started = Instant::now();
        cx.spawn_in(window, async move |this, cx| {
            let mut port = port;
            if let Some(line) = command.as_mut().filter(|line| line.contains("${port}")) {
                let free = match client.request(Request::FreePort).await {
                    Ok(Response::Port(free)) => free,
                    Ok(other) => {
                        this.update(cx, |this, cx| this.fail_session(generation, format!("unexpected response {other:?}"), cx)).ok();
                        return;
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| this.fail_session(generation, format!("No free port: {error:#}"), cx)).ok();
                        return;
                    }
                };
                port = free;
                *line = line.replace("${port}", &free.to_string());
            }
            // Seen running in its terminal: once the terminal is idle again,
            // the program ended without listening. Until a new terminal is
            // known, `term` is the one before.
            let mut seen_running = false;
            let mut waited: Option<Instant> = None;
            let mut watched = None;
            let mut last_read = Instant::now();
            loop {
                let (tx, rx) = smol::channel::unbounded::<RelayUpdate>();
                match client.connect_relay(port, move |update| {
                    let _ = tx.try_send(update);
                }).await {
                    Ok(relay) => {
                        this.update(cx, |this, cx| this.connected(generation, client.clone(), relay, rx, cx)).ok();
                        return;
                    }
                    Err(error) => {
                        let Ok((gone, term)) = this.update(cx, |this, _| (this.generation != generation, this.term)) else {
                            return;
                        };
                        if gone {
                            return;
                        }
                        let busy = match term {
                            Some(term) => matches!(client.request(Request::TermBusy { term }).await, Ok(Response::Busy(true))),
                            None => false,
                        };
                        // What still runs in its terminal (the program before,
                        // ending) gets a moment; then the command takes a new
                        // terminal, and that one is closed.
                        if command.is_some() && busy && waited.get_or_insert_with(Instant::now).elapsed() < RESTART_TIMEOUT {
                            this.update(cx, |this, cx| {
                                if this.generation == generation {
                                    this.status = Status::Connecting("Waiting for the program before to end…".into());
                                    cx.notify();
                                }
                            })
                            .ok();
                            cx.background_executor().timer(CONNECT_RETRY).await;
                            continue;
                        }
                        // every await above may have outlived the session
                        if let Some(command) = command.take() {
                            let ran = this.update(cx, |this, cx| {
                                if this.generation != generation {
                                    return false;
                                }
                                this.launched = true;
                                this.info(format!("$ {command}"), cx);
                                this.status = Status::Connecting("Starting the program…".into());
                                // what still runs in its terminal would read the line as its input
                                let term = if busy { None } else { this.term };
                                this.term_unknown = Some((generation, false));
                                cx.emit(DebugEvent::Run { session: generation, term, line: command });
                                cx.notify();
                                true
                            });
                            if !matches!(ran, Ok(true)) {
                                return;
                            }
                            continue;
                        }
                        if term != watched {
                            watched = term;
                            seen_running = false;
                        }
                        if busy {
                            seen_running = true;
                            // what the command says it's doing (building an app takes a while)
                            if let Some(term) = term
                                && last_read.elapsed() >= PROGRESS_POLL
                            {
                                last_read = Instant::now();
                                if let Ok(Response::Text(text)) = client.request(Request::TermRead { term, lines: 8 }).await
                                    && let Some(line) = text.lines().rev().map(str::trim).find(|line| !line.is_empty())
                                {
                                    let line = line.to_string();
                                    this.update(cx, |this, cx| {
                                        if this.generation == generation {
                                            this.status = Status::Connecting(format!("Starting: {line}"));
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                            }
                        } else if seen_running {
                            this.update(cx, |this, cx| {
                                let error = format!("The program ended without listening on port {port}: see its terminal");
                                this.fail_session(generation, error, cx)
                            })
                            .ok();
                            return;
                        }
                        // a command still at work (a build) is waited for; Stop ends it
                        if started.elapsed() > CONNECT_TIMEOUT && !busy {
                            this.update(cx, |this, cx| {
                                this.fail_session(generation, format!("Nothing answered on port {port}: {error:#}"), cx)
                            })
                            .ok();
                            return;
                        }
                        cx.background_executor().timer(CONNECT_RETRY).await;
                    }
                }
            }
        })
        .detach();
    }

    /// Opens `url` in the browser once a terminal of the workspace listens on
    /// its port, forwarded over SSH from a server; never if the session ends
    /// first (a script the command ran instead of the server).
    fn open_when_listening(&mut self, url: String, cx: &mut Context<Self>) {
        let (Some(client), Some(port)) = (self.client.clone(), Client::loopback_port(&url)) else {
            return;
        };
        let generation = self.generation;
        let group = self.root.to_string_lossy().into_owned();
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(gone) = this.update(cx, |this, _| this.generation != generation) else {
                    return;
                };
                if gone {
                    return;
                }
                if let Ok(Response::Ports(ports)) = client.request(Request::Ports).await
                    && ports.iter().any(|info| info.port == port && info.group == group)
                {
                    break;
                }
                cx.background_executor().timer(OPEN_POLL).await;
            }
            // a tab already showing the program comes to the front as it is
            let result = cx
                .background_spawn(async move {
                    let url = client.local_url(&url)?;
                    let focused = crate::browser::focus_tab(&url);
                    anyhow::Ok((url, focused))
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok((_, Ok(true))) => {}
                Ok((url, Ok(false))) => cx.open_url(&url),
                Ok((url, Err(err))) => {
                    this.info(format!("Can't look for its tab in Chrome: {err:#}"), cx);
                    cx.open_url(&url);
                }
                Err(err) => this.info(format!("Can't open the page: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn connected(
        &mut self,
        generation: u64,
        client: Arc<Client>,
        relay: u64,
        rx: smol::channel::Receiver<RelayUpdate>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            client.close_relay(relay);
            return;
        }
        self.conn = Some(Conn { client, relay, next_id: 0, pending: HashMap::new(), cwd: self.root.clone() });
        cx.spawn(async move |this, cx| {
            while let Ok(update) = rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        match update {
                            RelayUpdate::Line(line) => this.on_line(&line, cx),
                            RelayUpdate::Closed => this.end("The program ended", cx),
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        // `running` in `hello` is only how many ran then
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(THREADS_POLL).await;
                let alive = this
                    .update(cx, |this, _| {
                        if this.generation != generation {
                            return false;
                        }
                        if this.status == Status::Connected {
                            this.send("threads", json!({}), |this, result, cx| {
                                let running = result.ok().and_then(|body| body.get("running").and_then(Value::as_u64));
                                if let Some(running) = running.filter(|running| *running != this.running) {
                                    this.running = running;
                                    cx.notify();
                                }
                            });
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        self.send("hello", json!({ "version": protocol::VERSION }), |this, result, cx| match result {
            Ok(body) => this.on_hello(body, cx),
            Err(error) => this.fail(format!("The program refused the debugger: {error}"), cx),
        });
    }

    fn on_hello(&mut self, body: Map<String, Value>, cx: &mut Context<Self>) {
        if let Some(conn) = &mut self.conn
            && let Some(cwd) = body.get("cwd").and_then(Value::as_str)
        {
            conn.cwd = PathBuf::from(cwd);
        }
        self.status = Status::Connected;
        self.launching_test = None;
        self.running = body.get("running").and_then(Value::as_u64).unwrap_or(0);

        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        for path in files {
            self.send_breakpoints(&path, cx);
        }
        self.send_exceptions();
        // As Visual Studio does, a program that starts stops at its entry,
        // wherever the program says that is; a server, which has a page,
        // just runs.
        let page = page_of(&body);
        self.page = page.clone();
        if body.get("waiting").and_then(Value::as_bool).unwrap_or(false) {
            self.send("run", json!({ "entry": page.is_none() }), |_, _, _| {});
        }
        // the page of a program this launch started, which an attached one
        // or a restart doesn't open again
        if let Some(url) = page
            && self.launched
            && std::mem::take(&mut self.open_page)
        {
            self.open_when_listening(url, cx);
        }
        let stops: Vec<Stop> = protocol::field(&body, "stopped").unwrap_or_default();
        for stop in stops {
            self.on_stop(stop, cx);
        }
        let connected = if self.launched { "Connected" } else { "Attached to the program already running" };
        self.info(connected.into(), cx);
        cx.notify();
    }

    /// Stops the session: the program launched is interrupted, one attached
    /// to goes on running.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Idle {
            if std::mem::take(&mut self.ended) {
                cx.notify();
            }
            return;
        }
        if self.launched {
            match &mut self.term_unknown {
                // the command went to a terminal not known yet: that one, once it is
                Some((session, interrupt)) if *session == self.generation => *interrupt = true,
                _ => {
                    if let Some(term) = self.term {
                        cx.emit(DebugEvent::Interrupt { term });
                    }
                }
            }
        }
        self.end("Stopped", cx);
        self.ended = false;
    }

    /// Stops the session and starts it again as it started: the same
    /// command, test and file, not the one open now (a stop opens others).
    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let started = self.started.clone().filter(|_| self.status != Status::Idle);
        let launched = self.launched;
        self.stop(cx);
        // nothing to repeat, or not past reading the launch file: F5's start
        let Some(started) = started else {
            self.start(window, cx);
            return;
        };
        self.launched = false;
        self.status = Status::Connecting("Restarting…".into());
        let generation = self.generation;
        // A program it started frees its terminal first, and its port when
        // the command has no `${port}`: what still answers there is the
        // program before, which the new session would attach to.
        let port = started
            .launch
            .command
            .as_ref()
            .filter(|command| launched && !command.contains("${port}"))
            .map(|_| started.launch.port);
        let client = self.client.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let deadline = Instant::now() + RESTART_TIMEOUT;
            loop {
                let Ok(Some(term)) = this.update(cx, |this, _| (this.generation == generation).then_some(this.term)) else {
                    return;
                };
                let busy = match (launched, term, &client) {
                    (true, Some(term), Some(client)) => {
                        matches!(client.request(Request::TermBusy { term }).await, Ok(Response::Busy(true)))
                    }
                    _ => false,
                };
                let held = match (port, &client) {
                    (Some(port), Some(client)) => match client.connect_relay(port, |_: RelayUpdate| {}).await {
                        Ok(relay) => {
                            client.close_relay(relay);
                            true
                        }
                        Err(_) => false,
                    },
                    _ => false,
                };
                if !busy && !held {
                    break;
                }
                if Instant::now() >= deadline {
                    if let Some(port) = port.filter(|_| held) {
                        let error = format!("The program before still listens on port {port}: see its terminal");
                        this.update(cx, |this, cx| this.fail_session(generation, error, cx)).ok();
                        return;
                    }
                    // a terminal still busy: the command goes to a new one
                    break;
                }
                cx.background_executor().timer(CONNECT_RETRY).await;
            }
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                let mut launch = started.launch;
                if let (Some(command), Some(file)) = (&mut launch.command, &started.file) {
                    *command = command_line(command, file.as_deref());
                }
                this.launching_test = started.test;
                // the page is open already
                this.begin(launch, false, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn end(&mut self, why: &str, cx: &mut Context<Self>) {
        self.changed.clear();
        let was = self.status != Status::Idle;
        self.generation += 1;
        if let Some(conn) = self.conn.take() {
            conn.client.close_relay(conn.relay);
        }
        self.status = Status::Idle;
        self.launching_test = None;
        self.stops.clear();
        self.focus = None;
        self.locals.clear();
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        self.forget_console_refs(None);
        self.running = 0;
        for watch in &mut self.watches {
            watch.result = None;
        }
        if was {
            self.ended = true;
            self.info(why.into(), cx);
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    pub fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.end("", cx);
        self.push_console(ConsoleLine::Error(error));
        cx.notify();
    }

    /// `fail`, if `generation` is still the session: an older one's work
    /// that comes back late ends nothing.
    fn fail_session(&mut self, generation: u64, error: String, cx: &mut Context<Self>) {
        if self.generation == generation {
            self.fail(error, cx);
        }
    }

    /// Asks the program to let the person pick a widget on its screen (its
    /// `inspect` command): the line that made it comes back as a reveal.
    /// False when nothing is being debugged.
    pub fn inspect(&mut self) -> bool {
        if self.conn.is_none() {
            return false;
        }
        self.send("inspect", json!({ "on": true }), |this, result, cx| {
            if let Err(error) = result {
                this.info(format!("The program can't inspect: {error}"), cx);
            }
        });
        true
    }

    fn info(&mut self, text: String, cx: &mut Context<Self>) {
        if !text.is_empty() {
            self.push_console(ConsoleLine::Info(text));
            cx.notify();
        }
    }

    // ---- the protocol ----

    fn send(
        &mut self,
        cmd: &str,
        args: Value,
        reply: impl FnOnce(&mut Debugger, Result<Map<String, Value>, String>, &mut Context<Debugger>) + 'static,
    ) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.next_id += 1;
        let id = conn.next_id;
        conn.pending.insert(id, Box::new(reply));
        conn.client.relay_send(conn.relay, protocol::request(id, cmd, args));
    }

    fn on_line(&mut self, line: &str, cx: &mut Context<Self>) {
        match protocol::parse(line) {
            Ok(Message::Response { id, result }) => {
                let reply = self.conn.as_mut().and_then(|conn| conn.pending.remove(&id));
                if let Some(reply) = reply {
                    reply(self, result, cx);
                }
            }
            Ok(Message::Event(Event::Stopped(stop))) => self.on_stop(*stop, cx),
            Ok(Message::Event(Event::Resumed { vm })) => self.on_resumed(vm, cx),
            Ok(Message::Event(Event::Reveal { file, line })) => {
                let path = self.local_path(&file);
                self.revealed = Some((file, line));
                cx.emit(DebugEvent::Show { path, line: line.saturating_sub(1), focus: true });
                cx.emit(DebugEvent::Raise);
            }
            Ok(Message::Event(Event::Output { text, file, line, error })) => {
                let path = (!file.is_empty()).then(|| self.local_path(&file));
                self.push_console(ConsoleLine::Output { text: text.trim_end().to_string(), path, line, error });
                cx.notify();
            }
            Ok(Message::Unknown) => {}
            Err(error) => self.info(format!("debugger: {error}"), cx),
        }
    }

    fn on_stop(&mut self, stop: Stop, cx: &mut Context<Self>) {
        self.serial += 1;
        let vm = stop.vm;
        if let Some(exc) = &stop.exception {
            self.push_console(ConsoleLine::Error(format!("Exception: {}", exc.message)));
        }
        self.forget_console_refs(Some(vm));
        self.changed = changed_locals(self.stops.get(&vm).map(|previous| &previous.stop), &stop);
        self.locals = stop.locals.clone();
        self.globals = stop.globals;
        let show = stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
        self.stops.insert(vm, VmStop { stop, serial: self.serial, resumed: false, going: false });
        self.focus = Some(vm);
        self.frame = 0;
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        self.value_edit = None;
        cx.emit(DebugEvent::Reveal);
        self.refetch();
        self.evaluate_watches(cx);
        if let Some((path, line)) = show {
            cx.emit(DebugEvent::Show { path, line, focus: true });
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    fn on_resumed(&mut self, vm: u64, cx: &mut Context<Self>) {
        let Some(stop) = self.stops.get_mut(&vm) else {
            return;
        };
        stop.resumed = true;
        let serial = stop.serial;
        if self.focus == Some(vm) {
            self.hover = None;
        }
        self.forget_console_refs(Some(vm));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESUME_GRACE).await;
            this.update(cx, |this, cx| {
                if this.stops.get(&vm).is_some_and(|stop| stop.serial == serial && stop.resumed) {
                    this.stops.remove(&vm);
                    if this.focus == Some(vm) {
                        this.focus_next(cx);
                    }
                    cx.emit(DebugEvent::Marks);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The console's values of `vm` (of every VM, without one) can't be
    /// opened any more: a ref is valid only while its VM stays stopped.
    fn forget_console_refs(&mut self, vm: Option<u64>) {
        for line in &mut self.console {
            if let ConsoleLine::Result(var, from) = line
                && vm.is_none_or(|vm| vm == *from)
            {
                var.reference = 0;
            }
        }
    }

    /// After the focused VM went on, shows another one still stopped.
    fn focus_next(&mut self, cx: &mut Context<Self>) {
        self.changed.clear();
        self.focus = self.stops.iter().find(|(_, stop)| !stop.resumed).map(|(vm, _)| *vm);
        self.frame = 0;
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        match self.focus.and_then(|vm| self.stops.get(&vm)) {
            Some(stop) => {
                self.locals = stop.stop.locals.clone();
                self.globals = stop.stop.globals;
                let show = stop.stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
                self.refetch();
                self.evaluate_watches(cx);
                if let Some((path, line)) = show {
                    cx.emit(DebugEvent::Show { path, line, focus: false });
                }
            }
            None => {
                self.locals.clear();
                for watch in &mut self.watches {
                    watch.result = None;
                }
            }
        }
    }

    // ---- controlling the focused VM ----

    fn resume(&mut self, cmd: &str, cx: &mut Context<Self>) {
        // asked twice, the second one's error would undo `going` of the first
        let Some(vm) = self.halted() else {
            return;
        };
        // From the moment it's asked: `den debug wait` right after `next`
        // waits for the next stop, not the one it leaves.
        let Some(stop) = self.stops.get_mut(&vm) else {
            return;
        };
        stop.going = true;
        let serial = stop.serial;
        self.send(cmd, json!({ "vm": vm }), move |this, result, cx| {
            if let Err(error) = result {
                // It didn't go on: still stopped where it was, however late
                // the answer.
                if let Some(stop) = this.stops.get_mut(&vm).filter(|stop| stop.serial == serial) {
                    stop.going = false;
                }
                this.info(error, cx);
            }
        });
        cx.notify();
    }

    pub fn continue_(&mut self, cx: &mut Context<Self>) {
        self.resume("continue", cx);
    }

    pub fn step_over(&mut self, cx: &mut Context<Self>) {
        self.resume("next", cx);
    }

    pub fn step_in(&mut self, cx: &mut Context<Self>) {
        self.resume("stepIn", cx);
    }

    pub fn step_out(&mut self, cx: &mut Context<Self>) {
        self.resume("stepOut", cx);
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Connected {
            self.send("pause", json!({}), |_, _, _| {});
            self.info("Pausing: the next VM that runs code stops".into(), cx);
        }
    }

    /// Resumes the focused VM until it reaches `line` of `path`.
    pub fn run_to(&mut self, path: &Path, line: u32) {
        let file = self.program_path(path);
        let Some(vm) = self.halted() else {
            return;
        };
        self.send("runTo", json!({ "vm": vm, "file": file, "line": line + 1 }), |this, result, cx| {
            if let Err(error) = result {
                this.info(error, cx);
            }
        });
    }

    /// Makes `line` the next statement of the focused VM.
    pub fn jump(&mut self, line: u32) {
        let Some(vm) = self.halted() else {
            return;
        };
        self.send("jump", json!({ "vm": vm, "line": line + 1 }), move |this, result, cx| match result {
            Ok(body) => match protocol::decode::<Stop>(Value::Object(body)) {
                Ok(stop) => this.on_stop(stop, cx),
                Err(error) => this.info(error, cx),
            },
            Err(error) => this.info(error, cx),
        });
    }

    pub fn select_vm(&mut self, vm: u64, cx: &mut Context<Self>) {
        if self.focus == Some(vm) {
            return;
        }
        let Some(stop) = self.stops.get(&vm) else {
            return;
        };
        self.focus = Some(vm);
        self.changed.clear();
        self.locals = stop.stop.locals.clone();
        self.globals = stop.stop.globals;
        let show = stop.stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
        self.frame = 0;
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        self.refetch();
        self.evaluate_watches(cx);
        if let Some((path, line)) = show {
            cx.emit(DebugEvent::Show { path, line, focus: false });
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    pub fn select_frame(&mut self, frame: usize, cx: &mut Context<Self>) {
        self.changed.clear();
        let Some(vm) = self.focus else {
            return;
        };
        let Some(target) = self.current().and_then(|stop| stop.stop.frames.get(frame)).cloned() else {
            return;
        };
        self.frame = frame;
        cx.emit(DebugEvent::Show { path: self.local_path(&target.file), line: target.line.saturating_sub(1), focus: false });
        cx.emit(DebugEvent::Marks);
        cx.notify();
        self.send("frame", json!({ "vm": vm, "frame": frame }), move |this, result, cx| {
            if this.focus != Some(vm) || this.frame != frame {
                return;
            }
            match result {
                Ok(body) => {
                    this.locals = protocol::field(&body, "locals").unwrap_or_default();
                    this.globals = protocol::field(&body, "globals").unwrap_or(0);
                    this.children.clear();
                    this.loading.clear();
                    this.refetch();
                    this.evaluate_watches(cx);
                    cx.emit(DebugEvent::Marks);
                    cx.notify();
                }
                Err(error) => this.info(error, cx),
            }
        });
    }

    // ---- values ----

    /// Shows the value of `expr` by `anchor`; an expression without a value
    /// (a function's name, a type) shows nothing. Over another name's card,
    /// it waits a little: the pointer may be on its way to that card.
    pub fn show_hover(&mut self, expr: String, anchor: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.hover_inside && self.hover.is_some() {
            return;
        }
        if self.hover.as_ref().is_some_and(|hover| hover.var.name == expr && hover.anchor == anchor) {
            self.hover_change = None;
            return;
        }
        if self.hover.is_none() {
            self.hover_change = None;
            self.evaluate_hover(expr, anchor);
            return;
        }
        self.change_hover(Some((expr, anchor)), cx);
    }

    fn evaluate_hover(&mut self, expr: String, anchor: Bounds<Pixels>) {
        let serial = self.serial;
        self.evaluate(expr.clone(), move |this, result, cx| {
            if this.serial != serial {
                return;
            }
            this.expanded.retain(|key| key != "h" && !key.starts_with("h/"));
            this.hover = result.ok().map(|var| HoverValue { var: Var { name: expr, ..var }, anchor });
            // open, as what's in it is what one hovers for
            if let Some(reference) = this.hover.as_ref().map(|hover| hover.var.reference).filter(|&reference| reference != 0) {
                this.expanded.insert("h".into());
                if !this.children.contains_key(&reference) {
                    this.fetch(reference, 0);
                }
            }
            cx.notify();
        });
    }

    /// The pointer is on no name: the card goes, after the grace.
    pub fn leave_hover(&mut self, cx: &mut Context<Self>) {
        if self.hover.is_none() {
            self.hover_change = None;
        } else if !self.hover_inside {
            self.change_hover(None, cx);
        }
    }

    /// The pointer moved, over the card or its name (`inside`) or not.
    pub fn track_hover(&mut self, inside: bool, cx: &mut Context<Self>) {
        self.hover_inside = inside;
        if inside {
            self.hover_change = None;
        } else if self.hover_change.is_none() {
            self.change_hover(None, cx);
        }
    }

    fn change_hover(&mut self, to: Option<(String, Bounds<Pixels>)>, cx: &mut Context<Self>) {
        if self.hover_change.as_ref().is_some_and(|(pending, _)| *pending == to) {
            return;
        }
        let next = to.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_GRACE).await;
            this.update(cx, |this, cx| {
                this.hover_change = None;
                if this.hover_inside || this.hover.is_none() {
                    return;
                }
                // the card stays until the other value comes, or doesn't
                match next {
                    Some((expr, anchor)) => this.evaluate_hover(expr, anchor),
                    None => {
                        this.hover = None;
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.hover_change = Some((to, task));
    }

    pub fn clear_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_change = None;
        self.hover_inside = false;
        if self.hover.take().is_some() {
            cx.notify();
        }
    }

    pub fn toggle_expanded(&mut self, key: String, reference: u64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
            // opening it again asks again for children it couldn't have
            if self.children.get(&reference).is_some_and(|children| children.vars.is_empty() && children.error.is_some()) {
                self.children.remove(&reference);
            }
            if !self.children.contains_key(&reference) {
                self.fetch(reference, 0);
            }
        }
        cx.notify();
    }

    fn fetch(&mut self, reference: u64, start: u64) {
        if reference == 0 || !self.loading.insert(reference) {
            return;
        }
        let (generation, serial) = (self.generation, self.serial);
        self.send("expand", json!({ "ref": reference, "start": start, "count": PAGE }), move |this, result, cx| {
            // a stop since cleared what was loading, and the ref may name another value now
            if this.generation != generation || this.serial != serial {
                return;
            }
            this.loading.remove(&reference);
            let children = this.children.entry(reference).or_default();
            match result {
                Ok(body) => {
                    let vars: Vec<Var> = protocol::field(&body, "vars").unwrap_or_default();
                    children.more = vars.len() as u64 >= PAGE;
                    children.error = None;
                    children.vars.truncate(start as usize);
                    children.vars.extend(vars);
                    this.refetch();
                }
                Err(error) => children.error = Some(error),
            }
            cx.notify();
        });
    }

    /// Loads the next page of an array.
    pub fn fetch_more(&mut self, reference: u64) {
        let start = self.children.get(&reference).map_or(0, |children| children.vars.len() as u64);
        self.fetch(reference, start);
    }

    /// Asks for the children of every expanded value that hasn't got them,
    /// so values expanded before a stop stay expanded after it.
    fn refetch(&mut self) {
        let mut wanted = Vec::new();
        let mut walk = Vec::new();
        for var in &self.locals {
            walk.push((format!("l/{}", var.name), var.clone()));
        }
        if self.globals != 0 && self.expanded.contains("g") {
            if let Some(children) = self.children.get(&self.globals) {
                for var in &children.vars {
                    walk.push((format!("g/{}", var.name), var.clone()));
                }
            } else {
                wanted.push(self.globals);
            }
        }
        for (ix, watch) in self.watches.iter().enumerate() {
            if let Some(Ok(var)) = &watch.result {
                walk.push((format!("w{ix}"), var.clone()));
            }
        }
        for (ix, line) in self.console.iter().enumerate() {
            if let ConsoleLine::Result(var, _) = line {
                walk.push((format!("c{}", self.console_dropped + ix), var.clone()));
            }
        }
        if let Some(hover) = &self.hover {
            walk.push(("h".into(), hover.var.clone()));
        }
        while let Some((key, var)) = walk.pop() {
            if var.reference == 0 || !self.expanded.contains(&key) {
                continue;
            }
            match self.children.get(&var.reference) {
                Some(children) => {
                    for child in &children.vars {
                        walk.push((format!("{key}/{}", child.name), child.clone()));
                    }
                }
                None => wanted.push(var.reference),
            }
        }
        for reference in wanted {
            self.fetch(reference, 0);
        }
    }

    /// Evaluates in the selected frame of the focused VM.
    pub fn evaluate(
        &mut self,
        expr: String,
        reply: impl FnOnce(&mut Debugger, Result<Var, String>, &mut Context<Debugger>) + 'static,
    ) {
        let Some(vm) = self.halted() else {
            return;
        };
        self.send("eval", json!({ "vm": vm, "frame": self.frame, "expr": expr }), move |this, result, cx| {
            let result = result.and_then(|body| protocol::decode::<Var>(Value::Object(body)));
            reply(this, result, cx);
        });
    }

    fn evaluate_watches(&mut self, cx: &mut Context<Self>) {
        for ix in 0..self.watches.len() {
            self.evaluate_watch(ix, cx);
        }
    }

    fn evaluate_watch(&mut self, ix: usize, _cx: &mut Context<Self>) {
        let expr = self.watches[ix].expr.clone();
        let serial = self.serial;
        self.evaluate(expr.clone(), move |this, result, cx| {
            if this.serial != serial {
                return;
            }
            if let Some(watch) = this.watches.get_mut(ix).filter(|watch| watch.expr == expr) {
                watch.result = Some(result);
                this.refetch();
                cx.notify();
            }
        });
    }

    pub fn add_watch(&mut self, expr: String, cx: &mut Context<Self>) {
        if expr.is_empty() || self.watches.iter().any(|watch| watch.expr == expr) {
            return;
        }
        self.watches.push(Watch { expr, result: None });
        self.evaluate_watch(self.watches.len() - 1, cx);
        self.save(cx);
        cx.notify();
    }

    pub fn remove_watch(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.watches.len() {
            self.watches.remove(ix);
            self.save(cx);
            cx.notify();
        }
    }

    pub fn remove_all_watches(&mut self, cx: &mut Context<Self>) {
        self.watches.clear();
        self.save(cx);
        cx.notify();
    }

    fn submit_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expr = self.console_input.read(cx).value().trim().to_string();
        if expr.is_empty() {
            return;
        }
        self.console_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.history.retain(|old| *old != expr);
        self.history.push(expr.clone());
        self.history_at = None;
        self.evaluate_in_console(expr, cx);
    }

    /// Writes `expr` and its value in the console.
    pub fn evaluate_in_console(&mut self, expr: String, cx: &mut Context<Self>) {
        self.push_console(ConsoleLine::Input(expr.clone()));
        let Some((vm, serial)) = self.halted().and_then(|vm| Some((vm, self.stops.get(&vm)?.serial))) else {
            self.push_console(ConsoleLine::Error("Nothing is stopped to evaluate in".into()));
            cx.notify();
            return;
        };
        let assigns = is_assignment(&expr);
        self.evaluate(expr, move |this, result, cx| {
            match result {
                Ok(mut var) => {
                    // the VM went on before the answer: its ref names nothing
                    if !this.stops.get(&vm).is_some_and(|stop| stop.serial == serial && !stop.resumed) {
                        var.reference = 0;
                    }
                    this.push_console(ConsoleLine::Result(var, vm));
                }
                Err(error) => this.push_console(ConsoleLine::Error(error)),
            }
            if assigns {
                this.reload_frame();
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Up and down in the console go through what was evaluated.
    pub fn console_history(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        let at = match (self.history_at, up) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => None,
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) if at + 1 < self.history.len() => Some(at + 1),
            (Some(_), false) => None,
        };
        self.history_at = at;
        let text = at.map(|at| self.history[at].clone()).unwrap_or_default();
        self.console_input.update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// After an assignment: the variables, children and watches again.
    fn reload_frame(&mut self) {
        let frame = self.frame;
        let Some(vm) = self.focus else {
            return;
        };
        self.send("frame", json!({ "vm": vm, "frame": frame }), move |this, result, cx| {
            if let Ok(body) = result
                && this.focus == Some(vm)
                && this.frame == frame
            {
                this.locals = protocol::field(&body, "locals").unwrap_or_default();
                this.globals = protocol::field(&body, "globals").unwrap_or(0);
                this.children.clear();
                this.loading.clear();
                this.refetch();
                this.evaluate_watches(cx);
                cx.emit(DebugEvent::Marks);
                cx.notify();
            }
        });
    }

    pub fn start_value_edit(&mut self, key: String, target: String, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(value, window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => {
                let value = input.read(cx).value().trim().to_string();
                this.finish_value_edit(Some(value), cx);
            }
            InputEvent::Blur => this.finish_value_edit(None, cx),
            _ => {}
        });
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.value_edit = Some(ValueEdit { key, target, input });
        cx.notify();
    }

    fn finish_value_edit(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        let Some(edit) = self.value_edit.take() else {
            return;
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            self.evaluate(format!("{} = {value}", edit.target), |this, result, _| {
                if let Err(error) = result {
                    this.push_console(ConsoleLine::Error(error));
                }
                this.reload_frame();
            });
        }
        cx.notify();
    }

    // ---- breakpoints ----

    pub fn toggle_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        self.breakpoints.toggle(path, line);
        self.breakpoints_changed(path, cx);
    }

    pub fn set_breakpoint_enabled(&mut self, path: &Path, line: u32, enabled: bool, cx: &mut Context<Self>) {
        if let Some(mut bp) = self.breakpoints.at(path, line).cloned() {
            bp.enabled = enabled;
            self.breakpoints.put(path, bp);
            self.breakpoints_changed(path, cx);
        }
    }

    pub fn remove_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        if self.breakpoints.remove(path, line).is_some() {
            self.breakpoints_changed(path, cx);
        }
    }

    /// Enables or disables every breakpoint.
    pub fn enable_all_breakpoints(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        for path in files {
            for mut bp in self.breakpoints.of(&path).to_vec() {
                bp.enabled = enabled;
                self.breakpoints.put(&path, bp);
            }
            self.breakpoints_changed(&path, cx);
        }
    }

    pub fn remove_all_breakpoints(&mut self, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        self.breakpoints.clear();
        for path in files {
            self.breakpoints_changed(&path, cx);
        }
    }

    /// Follows an edit of a file: see `Breakpoints::shift`. The program gets
    /// the new lines when the file is saved.
    pub fn shift_breakpoints(&mut self, path: &Path, edit: LineEdit, cx: &mut Context<Self>) {
        if self.breakpoints.shift(path, edit) {
            self.save(cx);
            cx.emit(DebugEvent::Marks);
            cx.notify();
        }
    }

    /// A file was saved: a running program reloads it with lines that moved.
    pub fn file_saved(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.breakpoints.of(path).is_empty() {
            self.send_breakpoints(path, cx);
        }
    }

    fn breakpoints_changed(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.send_breakpoints(path, cx);
        self.save(cx);
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    fn send_breakpoints(&mut self, path: &Path, _cx: &mut Context<Self>) {
        if self.status != Status::Connected && !matches!(self.status, Status::Connecting(_)) {
            return;
        }
        let list: Vec<Value> = self
            .breakpoints
            .of(path)
            .iter()
            .filter(|bp| bp.enabled)
            .map(|bp| json!({ "line": bp.line + 1, "condition": bp.condition, "hit": bp.hit, "log": bp.log }))
            .collect();
        let file = self.program_path(path);
        let path = path.to_path_buf();
        let sent = self.breakpoints.of(&path).to_vec();
        self.send("setBreakpoints", json!({ "file": file, "breakpoints": list }), move |this, result, cx| {
            match result {
                // Changed since: the reply places a list that's no longer
                // there, and the one sent after it will place this one.
                Ok(_) if this.breakpoints.of(&path) != sent.as_slice() => {}
                Ok(body) => {
                    let placed: Vec<Value> = protocol::field(&body, "breakpoints").unwrap_or_default();
                    let placed: Vec<(u32, Option<String>)> = placed
                        .iter()
                        .map(|bp| {
                            let line = bp["line"].as_u64().unwrap_or(1).saturating_sub(1) as u32;
                            (line, bp["error"].as_str().map(str::to_string))
                        })
                        .collect();
                    let before = this.breakpoints.of(&path).to_vec();
                    this.breakpoints.placed(&path, &placed);
                    if this.breakpoints.of(&path) != before.as_slice() {
                        this.save(cx);
                        cx.emit(DebugEvent::Marks);
                        cx.notify();
                    }
                }
                Err(error) => this.info(error, cx),
            }
        });
    }

    pub fn set_exceptions(&mut self, uncaught: bool, all: bool, cx: &mut Context<Self>) {
        self.uncaught = uncaught;
        self.all = all;
        self.send_exceptions();
        self.save(cx);
        cx.notify();
    }

    fn send_exceptions(&mut self) {
        let (uncaught, all) = (self.uncaught, self.all);
        self.send("setExceptions", json!({ "uncaught": uncaught, "all": all }), |_, _, _| {});
    }

    /// Opens the editor of a breakpoint's condition, hit count or message.
    pub fn edit_breakpoint(&mut self, path: PathBuf, line: u32, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        let bp = self.breakpoints.at(&path, line).cloned();
        let value = match (kind, &bp) {
            (EditKind::Condition, Some(bp)) => bp.condition.clone(),
            (EditKind::Hit, Some(bp)) => bp.hit.clone(),
            (EditKind::Log, Some(bp)) => bp.log.clone(),
            (_, None) => String::new(),
        };
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder(kind.placeholder());
            input.set_value(value, window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.finish_breakpoint_edit(true, cx);
            }
        });
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.edit = Some(BreakpointEdit { path, line, kind, input });
        cx.notify();
    }

    pub fn switch_edit_kind(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = &self.edit {
            let (path, line) = (edit.path.clone(), edit.line);
            self.edit_breakpoint(path, line, kind, window, cx);
        }
    }

    /// Saves (`save`) or drops what the breakpoint editor has.
    pub fn finish_breakpoint_edit(&mut self, save: bool, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else {
            return;
        };
        cx.emit(DebugEvent::Refocus);
        if save {
            let text = edit.input.read(cx).value().trim().to_string();
            let mut bp = self.breakpoints.at(&edit.path, edit.line).cloned().unwrap_or_else(|| Breakpoint::new(edit.line));
            match edit.kind {
                EditKind::Condition => bp.condition = text,
                EditKind::Hit => bp.hit = text,
                EditKind::Log => bp.log = text,
            }
            bp.enabled = true;
            bp.error = None;
            self.breakpoints.put(&edit.path, bp);
            self.breakpoints_changed(&edit.path, cx);
        }
        cx.notify();
    }
}

/// The locals of `stop` that changed since `previous`, a stop of the same VM:
/// only when both are in the same call of the same function.
fn changed_locals(previous: Option<&Stop>, stop: &Stop) -> HashSet<String> {
    let Some(previous) = previous else {
        return HashSet::new();
    };
    let same_call = previous.frames.len() == stop.frames.len()
        && previous.frames.first().map(|frame| &frame.function) == stop.frames.first().map(|frame| &frame.function);
    if !same_call {
        return HashSet::new();
    }
    stop.locals
        .iter()
        .filter(|var| previous.locals.iter().find(|old| old.name == var.name).is_none_or(|old| old.value != var.value))
        .map(|var| var.name.clone())
        .collect()
}

/// `a = 1` assigns; `a == 1` and `a <= 1` don't.
fn is_assignment(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut quote = None;
    for (ix, &byte) in bytes.iter().enumerate() {
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' || byte == b'`' => quote = Some(byte),
            None if byte == b'=' => {
                let before = ix.checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(ix + 1).copied();
                let compares = matches!(before, Some(b'=' | b'!' | b'<' | b'>')) || matches!(after, Some(b'=' | b'>'));
                if !compares {
                    return true;
                }
            }
            None => {}
        }
    }
    false
}

/// The expression that names a child of a value named by `parent`.
fn child_path(parent: &str, name: &str) -> String {
    let identifier = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if identifier {
        format!("{parent}.{name}")
    } else if name.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
        format!("{parent}[{name}]")
    } else {
        format!("{parent}[{}]", serde_json::to_string(name).unwrap_or_default())
    }
}

/// The identifiers and member chains of a line of code (`a`, `b.c.d`),
/// outside strings and comments, in order and without repeating.
pub fn names_in(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut ix = 0;
    let mut quote: Option<char> = None;
    while ix < chars.len() {
        let c = chars[ix];
        if let Some(q) = quote {
            if c == '\\' {
                ix += 1;
            } else if c == q {
                quote = None;
            }
            ix += 1;
            continue;
        }
        if c == '"' || c == '\'' || c == '`' {
            quote = Some(c);
            ix += 1;
            continue;
        }
        if c == '/' && chars.get(ix + 1) == Some(&'/') {
            break;
        }
        let starts = (c.is_alphabetic() || c == '_' || c == '$') && (ix == 0 || !is_word(chars[ix - 1]) && chars[ix - 1] != '.');
        if !starts {
            ix += 1;
            continue;
        }
        let begin = ix;
        while ix < chars.len() && is_word(chars[ix]) {
            ix += 1;
        }
        let name: String = chars[begin..ix].iter().collect();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Whether a line declares a function or method: where the values shown
/// in the code for a frame begin.
pub fn starts_function(line: &str) -> bool {
    let mut text = line.trim_start();
    for prefix in ["export ", "default ", "async ", "static ", "private ", "public "] {
        text = text.strip_prefix(prefix).unwrap_or(text);
    }
    if text.starts_with("function ") || text.starts_with("function(") {
        return true;
    }
    // a method: `name(args) {`
    let word: String = text.chars().take_while(|c| is_word(*c)).collect();
    !word.is_empty()
        && !matches!(word.as_str(), "if" | "for" | "while" | "switch" | "catch" | "return" | "else")
        && text[word.len()..].starts_with('(')
        && text.trim_end().ends_with('{')
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Where the identifier or member chain at `offset` of `line` is (`a.b.c`
/// up to the word under the mouse), in bytes, for hovering.
pub fn expression_span(line: &str, offset: usize) -> Option<std::ops::Range<usize>> {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let at = line[..offset.min(line.len())].chars().count();
    if at >= chars.len() || !is_word(chars[at].1) {
        return None;
    }
    let mut end = at;
    while end < chars.len() && is_word(chars[end].1) {
        end += 1;
    }
    let mut start = at;
    loop {
        while start > 0 && is_word(chars[start - 1].1) {
            start -= 1;
        }
        if start > 1 && chars[start - 1].1 == '.' && is_word(chars[start - 2].1) {
            start -= 1;
            continue;
        }
        break;
    }
    let first = chars[start].1;
    if !(first.is_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    let end = chars.get(end).map_or(line.len(), |(byte, _)| *byte);
    Some(chars[start].0..end)
}

#[cfg(test)]
mod tests {
    use super::{
        changed_locals, child_path, command_line, current_target, expression_span, is_assignment, names_in, page_of,
        parse_launch_file, starts_function, with_target,
    };
    use serde_json::{Map, Value};
    use super::protocol::{Frame, Stop, Var};

    #[test]
    fn a_command_gets_the_open_file() {
        assert_eq!(command_line("sim -d ${file}", Some("cmd/tool.ts")), "sim -d cmd/tool.ts");
        assert_eq!(command_line("sim -d ${file}", Some("my dir/it's.ts")), r"sim -d 'my dir/it'\''s.ts'");
        assert_eq!(command_line("sim -d server", None), "sim -d server");
        // no file open: the program decides what to debug without one
        assert_eq!(command_line("sim -d ${file}", None), "sim -d ");
    }

    #[test]
    fn tests_are_found_by_the_project_pattern() {
        let file = parse_launch_file(
            r#"{"command":"sim -d ${file}","tests":{"match":"^export function (test\\w+)\\(","run":"sim test ${file} ${test} -x","debug":"sim -d test ${file} ${test} -x","port":4445}}"#,
        )
        .unwrap();
        let tests = file.tests.unwrap();
        assert_eq!(tests.port, 4445);
        assert_eq!(tests.name_in("export function testRefund() {").as_deref(), Some("testRefund"));
        assert_eq!(tests.name_in("function helper() {"), None);
        assert_eq!(tests.name_in("    // export function testOld() {"), None);
        assert_eq!(tests.command(false, "testRefund"), "sim test ${file} testRefund -x");
        assert_eq!(tests.command(true, "testRefund"), "sim -d test ${file} testRefund -x");
        assert!(parse_launch_file(r#"{"tests":{"match":"(","run":"","debug":""}}"#).is_err());
    }

    #[test]
    fn targets_go_in_the_commands() {
        let file = parse_launch_file(r#"{"command":"app run ${target} ${file}","targets":["ios","android"]}"#).unwrap();
        assert_eq!(file.targets, ["ios", "android"]);
        // the first until one is picked; a picked one the file no longer lists falls back to it
        assert_eq!(current_target(&file.targets, None), Some("ios"));
        assert_eq!(current_target(&file.targets, Some("android")), Some("android"));
        assert_eq!(current_target(&file.targets, Some("chrome")), Some("ios"));
        assert_eq!(current_target(&[], Some("ios")), None);
        assert_eq!(with_target("app run ${target} ${file}", Some("android")), "app run android ${file}");
        assert_eq!(with_target("app run ${target}", None), "app run ");
        assert!(parse_launch_file(r#"{"command":"x"}"#).unwrap().targets.is_empty());
        assert!(parse_launch_file(r#"{"targets":"ios"}"#).is_err());
    }

    #[test]
    fn a_launch_parses_with_a_default_port() {
        let launch = parse_launch_file(r#"{"command":"sim -d ${file}"}"#).unwrap().launch;
        assert_eq!(launch.command.as_deref(), Some("sim -d ${file}"));
        assert_eq!(launch.port, 4444);
        // Only a port: it attaches.
        let launch = parse_launch_file(r#"{"port":5000}"#).unwrap().launch;
        assert_eq!((launch.command, launch.port), (None, 5000));
        // A file with several configurations starts the first.
        let launch = parse_launch_file(r#"{"configurations":[{"name":"Debug","command":"sim -d server"},{"name":"Attach"}]}"#).unwrap().launch;
        assert_eq!(launch.command.as_deref(), Some("sim -d server"));
    }

    #[test]
    fn a_program_opens_only_a_page_of_localhost() {
        let hello = |text: &str| serde_json::from_str::<Map<String, Value>>(text).unwrap();
        assert_eq!(
            page_of(&hello(r#"{"cwd":"/","page":"http://localhost:9092/platform/tenants"}"#)).as_deref(),
            Some("http://localhost:9092/platform/tenants")
        );
        assert_eq!(page_of(&hello(r#"{"cwd":"/"}"#)), None);
        assert_eq!(page_of(&hello(r#"{"page":"https://example.com/"}"#)), None);
        assert_eq!(page_of(&hello(r#"{"page":"file:///etc/passwd"}"#)), None);
    }

    #[test]
    fn only_a_lone_equals_sign_assigns() {
        assert!(is_assignment("a = 1"));
        assert!(is_assignment("a.b[0]=x"));
        assert!(!is_assignment("a == 1"));
        assert!(!is_assignment("a <= 1 && b >= 2 && c != 3"));
        assert!(!is_assignment(r#"s == "x = 1""#));
        assert!(!is_assignment("list.map(x => x)"));
    }

    #[test]
    fn child_paths_are_expressions() {
        assert_eq!(child_path("a", "name"), "a.name");
        assert_eq!(child_path("a", "3"), "a[3]");
        assert_eq!(child_path("a", "first name"), r#"a["first name"]"#);
    }

    #[test]
    fn names_skip_strings_comments_and_members() {
        assert_eq!(
            names_in(r#"let total = item.price * qty + "count" // ignored"#),
            vec!["let", "total", "item", "qty"]
        );
    }

    #[test]
    fn a_step_marks_the_locals_it_changed() {
        let var = |name: &str, value: &str| Var { name: name.into(), value: value.into(), ..Var::default() };
        let frame = |function: &str| Frame { function: function.into(), ..Frame::default() };
        let before = Stop { frames: vec![frame("work"), frame("main")], locals: vec![var("a", "1"), var("b", "2")], ..Stop::default() };
        let after = Stop { frames: vec![frame("work"), frame("main")], locals: vec![var("a", "1"), var("b", "3"), var("c", "4")], ..Stop::default() };
        let changed = changed_locals(Some(&before), &after);
        assert_eq!(changed, ["b".to_string(), "c".to_string()].into_iter().collect());

        // another call: nothing is a change
        let deeper = Stop { frames: vec![frame("price"), frame("work"), frame("main")], ..after.clone() };
        assert!(changed_locals(Some(&before), &deeper).is_empty());
    }

    #[test]
    fn functions_and_methods_start_a_frame() {
        assert!(starts_function("export function save(record) {"));
        assert!(starts_function("    async load(id: number) {"));
        assert!(!starts_function("    if (x) {"));
        assert!(!starts_function("    total += price(item)"));
    }

    #[test]
    fn hover_takes_the_member_chain_up_to_the_word() {
        let line = "    return order.customer.name + x";
        let at = line.find("customer").unwrap() + 2;
        let expression_at = |line: &'static str, at: usize| expression_span(line, at).map(|span| &line[span]);
        assert_eq!(expression_at(line, at), Some("order.customer"));
        let at = line.find("name").unwrap();
        assert_eq!(expression_at(line, at), Some("order.customer.name"));
        assert_eq!(expression_at(line, line.find('+').unwrap()), None);
        let line = "  ñ = café.total";
        let at = line.find("total").unwrap();
        assert_eq!(&line[expression_span(line, at).unwrap()], "café.total");
    }
}
