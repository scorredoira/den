//! The debugger of a workspace: breakpoints, and a session with a program
//! that speaks the debug protocol (`protocol`), reached through the agent so
//! a program on a server is debugged like a local one.
//!
//! Nothing here knows the language or VM of the program. A launch
//! configuration (`.sik/debug.json`) says which command starts it and on
//! which port it listens.

mod breakpoints;
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

pub use breakpoints::{Breakpoint, Breakpoints};
use protocol::{Event, Message, Stop, Var};

use crate::config::{Config, DebugSaved};

/// Where the launch configurations are, relative to the workspace.
pub const LAUNCH_FILE: &str = ".sik/debug.json";

const LAUNCH_TEMPLATE: &str = r#"{
    "configurations": [
        {
            "name": "Launch",
            "command": "sim -d main.ts",
            "port": 4444
        },
        {
            "name": "Attach",
            "port": 4444
        }
    ]
}
"#;

/// How long to keep trying to reach a program that is starting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_RETRY: Duration = Duration::from_millis(100);

/// A VM that resumes keeps showing its stop this long, dimmed: a step that
/// stops again right away replaces it without the views blinking empty.
const RESUME_GRACE: Duration = Duration::from_millis(250);

/// Children of a value asked for at a time.
const PAGE: u64 = 200;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Launch {
    pub name: String,
    /// A shell command line that starts the program, run in a terminal.
    /// Without it, the debugger attaches to a program already running.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    4444
}

#[derive(Deserialize)]
struct LaunchFile {
    configurations: Vec<Launch>,
}

pub fn parse_launches(text: &str) -> Result<Vec<Launch>, String> {
    let file: LaunchFile = serde_json::from_str(text).map_err(|err| format!("{LAUNCH_FILE}: {err}"))?;
    Ok(file.configurations)
}

pub enum DebugEvent {
    /// Show `line` of `path`: where a VM stopped (`focus`: it just stopped)
    /// or something clicked in the panel.
    Show { path: PathBuf, line: u32, focus: bool },
    /// Breakpoints or the line stopped at changed: the editors redraw their
    /// marks.
    Marks,
    /// Run `line` in the debugger's terminal (`term` if it's still open);
    /// the workspace answers with `set_terminal`.
    Run { term: Option<TermId>, line: String },
    /// Stop what runs in the debugger's terminal.
    Interrupt { term: TermId },
    /// The breakpoint editor closed: the keys go back to the code.
    Refocus,
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
}

#[derive(Clone)]
struct Children {
    vars: Vec<Var>,
    total: u64,
}

struct Watch {
    expr: String,
    result: Option<Result<Var, String>>,
}

enum ConsoleLine {
    Info(String),
    Output { text: String, path: Option<PathBuf>, line: u32 },
    Input(String),
    Result(Var),
    Error(String),
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
    launches: Vec<Launch>,
    launch: Option<String>,
    launch_error: Option<String>,

    status: Status,
    conn: Option<Conn>,
    /// Bumped by every start and stop: work of an older session is dropped.
    generation: u64,
    /// The terminal the launch command runs in, reused by the next launch.
    term: Option<TermId>,
    launched: bool,
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
    console: Vec<ConsoleLine>,
    console_scroll: ScrollHandle,
    /// Lines of the console already scrolled to.
    console_seen: usize,
    console_input: Entity<InputState>,
    watch_input: Entity<InputState>,
    history: Vec<String>,
    history_at: Option<usize>,
    value_edit: Option<ValueEdit>,
    pub edit: Option<BreakpointEdit>,
    /// The panel is shown.
    pub visible: bool,
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
            launch: saved.launch,
            term: saved.terminal,
            root,
            session_key,
            client,
            launches: Vec::new(),
            launch_error: None,
            status: Status::Idle,
            conn: None,
            generation: 0,
            launched: false,
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
            console_seen: 0,
            console_input,
            watch_input,
            history: Vec::new(),
            history_at: None,
            value_edit: None,
            edit: None,
            visible: false,
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
            launch: self.launch.clone(),
            terminal: self.term,
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

    pub fn is_stopped(&self) -> bool {
        self.current().is_some()
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
            return;
        };
        self.visible = true;
        self.status = Status::Connecting("Reading the launch configuration…".into());
        self.generation += 1;
        let generation = self.generation;
        let path = self.root.join(LAUNCH_FILE);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                let launches = match read {
                    Ok(Response::Bytes(bytes)) => parse_launches(&String::from_utf8_lossy(&bytes)),
                    Ok(other) => Err(format!("unexpected response {other:?}")),
                    Err(_) => Err(format!("There is no {LAUNCH_FILE}: create it to say how to start the program.")),
                };
                match launches {
                    Ok(launches) if !launches.is_empty() => {
                        this.launches = launches;
                        this.launch_error = None;
                        let launch = this
                            .launch
                            .as_ref()
                            .and_then(|name| this.launches.iter().find(|launch| &launch.name == name))
                            .unwrap_or(&this.launches[0])
                            .clone();
                        this.begin(launch, window, cx);
                    }
                    Ok(_) => this.fail(format!("{LAUNCH_FILE} has no configurations"), cx),
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

    /// Reads the launch configurations again, for the panel's menu.
    pub fn refresh_launches(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.join(LAUNCH_FILE);
        cx.spawn(async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update(cx, |this, cx| {
                match read.ok().and_then(|response| match response {
                    Response::Bytes(bytes) => Some(parse_launches(&String::from_utf8_lossy(&bytes))),
                    _ => None,
                }) {
                    Some(Ok(launches)) => {
                        this.launches = launches;
                        this.launch_error = None;
                    }
                    Some(Err(error)) => this.launch_error = Some(error),
                    None => {
                        this.launches.clear();
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
            let dir = self.root.join(".sik");
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

    pub fn select_launch(&mut self, name: String, cx: &mut Context<Self>) {
        self.launch = Some(name);
        self.save(cx);
        cx.notify();
    }

    fn begin(&mut self, launch: Launch, window: &mut Window, cx: &mut Context<Self>) {
        self.console.clear();
        self.launched = false;
        self.status = Status::Connecting(format!("Connecting to port {}…", launch.port));
        self.connect(launch.port, launch.command, window, cx);
        cx.notify();
    }

    pub fn set_terminal(&mut self, term: Option<TermId>, cx: &mut Context<Self>) {
        if term.is_some() && term != self.term {
            self.term = term;
            self.save(cx);
        }
    }

    /// Tries to reach the program until it listens, or the time is up. A
    /// program that already listens is attached to; otherwise `command`
    /// starts it.
    fn connect(&mut self, port: u16, mut command: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.generation;
        let started = Instant::now();
        cx.spawn_in(window, async move |this, cx| {
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
                        let gone = this.update(cx, |this, _| this.generation != generation).unwrap_or(true);
                        if gone {
                            return;
                        }
                        if let Some(command) = command.take() {
                            this.update(cx, |this, cx| {
                                this.launched = true;
                                this.info(format!("$ {command}"), cx);
                                this.status = Status::Connecting("Starting the program…".into());
                                cx.emit(DebugEvent::Run { term: this.term, line: command });
                                cx.notify();
                            })
                            .ok();
                            continue;
                        }
                        if started.elapsed() > CONNECT_TIMEOUT {
                            this.update(cx, |this, cx| {
                                this.fail(format!("Nothing answered on port {port}: {error:#}"), cx)
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
        self.running = body.get("running").and_then(Value::as_u64).unwrap_or(0);

        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        for path in files {
            self.send_breakpoints(&path, cx);
        }
        self.send_exceptions();
        if body.get("waiting").and_then(Value::as_bool).unwrap_or(false) {
            self.send("run", json!({}), |_, _, _| {});
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
            return;
        }
        if self.launched
            && let Some(term) = self.term
        {
            cx.emit(DebugEvent::Interrupt { term });
        }
        self.end("Stopped", cx);
    }

    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was = self.status != Status::Idle;
        self.stop(cx);
        if !was {
            self.start(window, cx);
            return;
        }
        // let the program interrupted free its port first
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            this.update_in(cx, |this, window, cx| this.start(window, cx)).ok();
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
        self.stops.clear();
        self.focus = None;
        self.locals.clear();
        self.children.clear();
        self.loading.clear();
        self.running = 0;
        for watch in &mut self.watches {
            watch.result = None;
        }
        if was {
            self.info(why.into(), cx);
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.end("", cx);
        self.console.push(ConsoleLine::Error(error));
        cx.notify();
    }

    fn info(&mut self, text: String, cx: &mut Context<Self>) {
        if !text.is_empty() {
            self.console.push(ConsoleLine::Info(text));
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
            Ok(Message::Event(Event::Output { text, file, line })) => {
                let path = (!file.is_empty()).then(|| self.local_path(&file));
                self.console.push(ConsoleLine::Output { text: text.trim_end().to_string(), path, line });
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
            self.console.push(ConsoleLine::Error(format!("Exception: {}", exc.message)));
        }
        self.changed = changed_locals(self.stops.get(&vm).map(|previous| &previous.stop), &stop);
        self.locals = stop.locals.clone();
        self.globals = stop.globals;
        let show = stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
        self.stops.insert(vm, VmStop { stop, serial: self.serial, resumed: false });
        self.focus = Some(vm);
        self.frame = 0;
        self.children.clear();
        self.loading.clear();
        self.value_edit = None;
        self.visible = true;
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

    /// After the focused VM went on, shows another one still stopped.
    fn focus_next(&mut self, cx: &mut Context<Self>) {
        self.changed.clear();
        self.focus = self.stops.iter().find(|(_, stop)| !stop.resumed).map(|(vm, _)| *vm);
        self.frame = 0;
        self.children.clear();
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
        let Some(vm) = self.focus.filter(|vm| self.stops.get(vm).is_some_and(|stop| !stop.resumed)) else {
            return;
        };
        self.send(cmd, json!({ "vm": vm }), |this, result, cx| {
            if let Err(error) = result {
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
        let Some(vm) = self.focus.filter(|_| self.is_stopped()) else {
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
        let Some(vm) = self.focus.filter(|_| self.is_stopped()) else {
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

    pub fn toggle_expanded(&mut self, key: String, reference: u64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
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
        let generation = self.generation;
        self.send("expand", json!({ "ref": reference, "start": start, "count": PAGE }), move |this, result, cx| {
            this.loading.remove(&reference);
            if this.generation != generation {
                return;
            }
            if let Ok(body) = result {
                let vars: Vec<Var> = protocol::field(&body, "vars").unwrap_or_default();
                let children = this.children.entry(reference).or_insert(Children { vars: Vec::new(), total: 0 });
                children.vars.truncate(start as usize);
                children.vars.extend(vars);
                children.total = children.total.max(children.vars.len() as u64);
                this.refetch();
                cx.notify();
            }
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
        let Some(vm) = self.focus.filter(|_| self.is_stopped()) else {
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

    fn submit_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expr = self.console_input.read(cx).value().trim().to_string();
        if expr.is_empty() {
            return;
        }
        self.console_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.history.retain(|old| *old != expr);
        self.history.push(expr.clone());
        self.history_at = None;
        self.console.push(ConsoleLine::Input(expr.clone()));
        if !self.is_stopped() {
            self.console.push(ConsoleLine::Error("Nothing is stopped to evaluate in".into()));
            cx.notify();
            return;
        }
        let assigns = is_assignment(&expr);
        self.evaluate(expr, move |this, result, cx| {
            match result {
                Ok(var) => this.console.push(ConsoleLine::Result(var)),
                Err(error) => this.console.push(ConsoleLine::Error(error)),
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
                    this.console.push(ConsoleLine::Error(error));
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

    pub fn remove_all_breakpoints(&mut self, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        self.breakpoints.clear();
        for path in files {
            self.breakpoints_changed(&path, cx);
        }
    }

    /// Follows an edit of a file: see `Breakpoints::shift`. The program gets
    /// the new lines when the file is saved.
    pub fn shift_breakpoints(&mut self, path: &Path, at: u32, delta: i64, cx: &mut Context<Self>) {
        if self.breakpoints.shift(path, at, delta) {
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
        self.send("setBreakpoints", json!({ "file": file, "breakpoints": list }), move |this, result, cx| {
            match result {
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

/// The identifier or member chain at `offset` of `line` (`a.b.c` up to the
/// word under the mouse), for hovering.
pub fn expression_at(line: &str, offset: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let at = line[..offset.min(line.len())].chars().count();
    if at >= chars.len() || !is_word(chars[at]) {
        return None;
    }
    let mut end = at;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    let mut start = at;
    loop {
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        if start > 1 && chars[start - 1] == '.' && is_word(chars[start - 2]) {
            start -= 1;
            continue;
        }
        break;
    }
    let expr: String = chars[start..end].iter().collect();
    let first = expr.chars().next()?;
    (first.is_alphabetic() || first == '_' || first == '$').then_some(expr)
}

#[cfg(test)]
mod tests {
    use super::{changed_locals, child_path, expression_at, is_assignment, names_in, parse_launches, starts_function};
    use super::protocol::{Frame, Stop, Var};

    #[test]
    fn launches_parse_with_a_default_port() {
        let launches = parse_launches(r#"{"configurations":[{"name":"server","command":"sim -d server"},{"name":"attach","port":5000}]}"#).unwrap();
        assert_eq!(launches[0].port, 4444);
        assert_eq!(launches[0].command.as_deref(), Some("sim -d server"));
        assert_eq!(launches[1].command, None);
        assert_eq!(launches[1].port, 5000);
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
        assert_eq!(expression_at(line, at).as_deref(), Some("order.customer"));
        let at = line.find("name").unwrap();
        assert_eq!(expression_at(line, at).as_deref(), Some("order.customer.name"));
        assert_eq!(expression_at(line, line.find('+').unwrap()), None);
    }
}
