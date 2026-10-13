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
mod connect;
mod console;
mod launch;
mod variables;
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
use serde_json::{Map, Value, json};

pub use breakpoints::{Breakpoint, Breakpoints, LineEdit};
pub use commands::WaitFor;
pub use launch::{Launch, Tests, command_line, parse_launch_file, with_target};
use launch::current_target;
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

/// How often to look whether the program listens on the port of its page,
/// and for how long: a program that never does isn't waited for forever.
const OPEN_POLL: Duration = Duration::from_millis(100);
const OPEN_TIMEOUT: Duration = Duration::from_secs(120);

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

/// The program's page in its `hello`, a URL of the loopback: a server's,
/// opened once it listens there. Any other address is not opened.
fn page_of(hello: &Map<String, Value>) -> Option<String> {
    hello
        .get("page")
        .and_then(Value::as_str)
        .filter(|url| Client::loopback_port(url).is_some())
        .map(str::to_string)
}

/// A name hovered and where it is, in window coordinates.
pub type HoverAt = (String, Bounds<Pixels>);

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
    /// The running VMs were asked for and haven't been answered: they
    /// aren't asked again until they are, so a program that doesn't answer
    /// doesn't pile up a request a second.
    counting: bool,
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
    hover_change: Option<(Option<HoverAt>, Task<()>)>,
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
