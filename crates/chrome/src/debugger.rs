//! The bridge: Den's debug protocol v1 on one side, CDP on the other. One
//! thread runs it, taking the client's requests and Chrome's events in the
//! order they come; only the commands that run page code (`navigate`,
//! `reload`, `evaluate`) answer from a thread of their own, since the code
//! they run can stop at a breakpoint.

use std::{
    collections::{BTreeMap, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc::Receiver},
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::{
    cdp::{self, Cdp},
    fetch,
    sourcemap::{Original, SourceMap},
    values,
};

const VERSION: u64 = 1;

/// How many steps a step takes at most to leave code without a source map,
/// or a line made of several generated positions, before it stops anyway.
const MAX_STEPS: u32 = 300;

pub enum Input {
    Cdp(cdp::Event),
    CdpClosed(String),
    /// A client sent its `hello`: it replaces the one before.
    Connected {
        generation: u64,
        stream: TcpStream,
        hello: Value,
    },
    Request {
        generation: u64,
        request: Value,
    },
    Disconnected {
        generation: u64,
    },
    Shutdown,
}

/// The connected client, written to by the bridge's threads.
#[derive(Clone, Default)]
pub struct Out(Arc<Mutex<Option<(u64, TcpStream)>>>);

impl Out {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u64, TcpStream)>> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sends a line to the client, if one is connected.
    pub fn send(&self, value: &Value) {
        self.send_if(None, value);
    }

    /// Sends a line to the client of that generation, if it is still the one.
    fn send_if(&self, generation: Option<u64>, value: &Value) {
        let mut client = self.lock();
        let Some((current, stream)) = client.as_mut() else { return };
        if generation.is_some_and(|generation| generation != *current) {
            return;
        }
        let mut line = value.to_string();
        line.push('\n');
        if let Err(err) = stream.write_all(line.as_bytes()) {
            // closing it ends its reader, which disconnects it
            eprintln!("chrome: the client is gone: {err}");
            if let Err(err) = stream.shutdown(Shutdown::Both) {
                eprintln!("chrome: close the client: {err}");
            }
            *client = None;
        }
    }

    fn replace(&self, generation: u64, stream: TcpStream) -> Result<()> {
        stream.set_write_timeout(Some(Duration::from_secs(10))).context("set the client's write timeout")?;
        let old = self.lock().replace((generation, stream));
        if let Some((_, old)) = old
            && let Err(err) = old.shutdown(Shutdown::Both)
            && err.kind() != std::io::ErrorKind::NotConnected
        {
            eprintln!("chrome: close the client replaced: {err}");
        }
        Ok(())
    }

    fn clear(&self, generation: u64) {
        let mut client = self.lock();
        if client.as_ref().is_some_and(|(current, _)| *current == generation) {
            *client = None;
        }
    }
}

pub struct Settings {
    pub root: PathBuf,
    pub hosts: Vec<String>,
    /// Opened when the first client sends `run`.
    pub url: Option<String>,
    /// Chrome was launched now: its blank tab can show the URL.
    pub launched: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Exceptions {
    None,
    Uncaught,
    All,
}

impl Exceptions {
    fn state(self) -> &'static str {
        match self {
            Exceptions::None => "none",
            Exceptions::Uncaught => "uncaught",
            Exceptions::All => "all",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HitOp {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    Every,
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    op: HitOp,
    n: u64,
}

impl Hit {
    fn parse(text: &str) -> Result<Option<Hit>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let mut op = HitOp::Eq;
        let mut rest = text;
        for (prefix, which) in [
            (">=", HitOp::Ge),
            ("<=", HitOp::Le),
            ("==", HitOp::Eq),
            (">", HitOp::Gt),
            ("<", HitOp::Lt),
            ("%", HitOp::Every),
            ("=", HitOp::Eq),
        ] {
            if let Some(after) = text.strip_prefix(prefix) {
                op = which;
                rest = after.trim();
                break;
            }
        }
        let n = rest.parse::<u64>().map_err(|_| anyhow!("invalid hit count {text:?}"))?;
        Ok(Some(Hit { op, n }))
    }

    fn matches(self, hits: u64) -> bool {
        match self.op {
            HitOp::Eq => hits == self.n,
            HitOp::Ge => hits >= self.n,
            HitOp::Gt => hits > self.n,
            HitOp::Le => hits <= self.n,
            HitOp::Lt => hits < self.n,
            HitOp::Every => self.n > 0 && hits.is_multiple_of(self.n),
        }
    }
}

struct Breakpoint {
    /// 1-based, as the client asked.
    line: u32,
    condition: Option<String>,
    hit: Option<Hit>,
    log: Option<String>,
    hits: u64,
}

struct Script {
    url: String,
    context: i64,
    map: Option<Arc<SourceMap>>,
    start_line: u32,
    start_column: u32,
}

/// A breakpoint placed in Chrome: the breakpoint of the client it stands for.
struct Placed {
    file: String,
    line: u32,
    script: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepKind {
    Over,
    Into,
    Out,
}

struct Step {
    kind: StepKind,
    depth: usize,
    at: Option<Original>,
    /// The original line of each frame when the step started, frame 0 first.
    lines: Vec<Option<Original>>,
    count: u32,
}

struct Page {
    target: String,
    session: String,
    url: String,
    title: String,
    /// Execution contexts, and whether their origin is a debugged host.
    contexts: HashMap<i64, bool>,
    scripts: HashMap<String, Script>,
    placed: HashMap<String, Placed>,
    /// The params of `Debugger.paused` while Chrome holds the page.
    paused: Option<Value>,
    /// The stop the client sees, while it sees one.
    stop: Option<Value>,
    step: Option<Step>,
    run_to: bool,
    /// A `pause` asked for: `true` when it was for any VM.
    pause_request: Option<bool>,
    /// The exception of the last exception stop, so its `exceptionThrown`
    /// isn't printed again.
    last_exception: Option<String>,
}

struct Ref {
    vm: u64,
    object: String,
    subtype: String,
}

pub struct Core {
    cdp: Arc<Cdp>,
    out: Out,
    settings: Settings,
    root_text: String,
    client: Option<u64>,
    pages: BTreeMap<u64, Page>,
    sessions: HashMap<String, u64>,
    targets: HashMap<String, u64>,
    next_vm: u64,
    breakpoints: BTreeMap<String, Vec<Breakpoint>>,
    exceptions: Exceptions,
    refs: HashMap<u64, Ref>,
    next_ref: u64,
    pause_any: bool,
    /// Inline source maps already parsed, by a hash of their URL.
    maps: HashMap<u64, Arc<SourceMap>>,
}

impl Core {
    pub fn new(cdp: Arc<Cdp>, out: Out, settings: Settings) -> Core {
        let root_text = settings.root.to_string_lossy().to_string();
        Core {
            cdp,
            out,
            settings,
            root_text,
            client: None,
            pages: BTreeMap::new(),
            sessions: HashMap::new(),
            targets: HashMap::new(),
            next_vm: 1,
            breakpoints: BTreeMap::new(),
            exceptions: Exceptions::Uncaught,
            refs: HashMap::new(),
            next_ref: 1,
            pause_any: false,
            maps: HashMap::new(),
        }
    }

    /// Attaches to the pages, then serves until Chrome goes away or the
    /// bridge is shut down.
    pub fn run(mut self, inputs: Receiver<Input>) -> Result<()> {
        self.cdp
            .call(None, "Target.setDiscoverTargets", json!({ "discover": true }))
            .context("discover Chrome's targets")?;
        self.cdp
            .call(
                None,
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            )
            .context("attach to Chrome's pages")?;
        for input in inputs {
            match input {
                Input::Cdp(event) => {
                    let method = event.method.clone();
                    if let Err(err) = self.on_cdp(event) {
                        self.report(&format!("{method}: {err:#}"));
                    }
                }
                Input::CdpClosed(reason) => {
                    self.out.send(&json!({ "event": "output", "text": format!("{reason}\n"), "file": "", "line": 0 }));
                    return Err(anyhow!(reason));
                }
                Input::Connected { generation, stream, hello } => {
                    if let Err(err) = self.connect(generation, stream, hello) {
                        eprintln!("chrome: connect a client: {err:#}");
                    }
                }
                Input::Request { generation, request } => {
                    if self.client == Some(generation) {
                        self.request(generation, request);
                    }
                }
                Input::Disconnected { generation } => {
                    if self.client == Some(generation) {
                        self.disconnect(generation);
                    }
                }
                Input::Shutdown => return Ok(()),
            }
        }
        Ok(())
    }

    /// Says something went wrong: on stderr and in the client's console.
    fn report(&self, text: &str) {
        eprintln!("chrome: {text}");
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": "", "line": 0 }));
    }

    // ---- the client ----

    fn connect(&mut self, generation: u64, stream: TcpStream, hello: Value) -> Result<()> {
        self.out.replace(generation, stream)?;
        let first = self.client.is_none();
        self.client = Some(generation);
        if first {
            // the client's own settings come next; until then, sim's defaults
            self.exceptions = Exceptions::Uncaught;
            self.apply_exceptions();
        }
        self.request(generation, hello);
        Ok(())
    }

    /// The client went away: nothing it asked for stays, and no page is left
    /// stopped.
    fn disconnect(&mut self, generation: u64) {
        self.client = None;
        self.out.clear(generation);
        let files: Vec<String> = self.breakpoints.keys().cloned().collect();
        for file in files {
            self.remove_placed(&file);
        }
        self.breakpoints.clear();
        self.exceptions = Exceptions::Uncaught;
        self.apply_exceptions();
        self.pause_any = false;
        self.refs.clear();
        let vms: Vec<u64> = self.pages.keys().copied().collect();
        for vm in vms {
            let Some(page) = self.pages.get_mut(&vm) else { continue };
            page.step = None;
            page.run_to = false;
            page.pause_request = None;
            page.stop = None;
            if page.paused.take().is_some()
                && let Err(err) = self.page_call(vm, "Debugger.resume", json!({}))
            {
                eprintln!("chrome: resume vm {vm}: {err:#}");
            }
        }
    }

    fn request(&mut self, generation: u64, request: Value) {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let cmd = request.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
        // the commands that run page code answer from their own thread
        if matches!(cmd.as_str(), "navigate" | "reload" | "evaluate") {
            if let Err(err) = self.page_command(generation, &cmd, id.clone(), &request) {
                self.out.send(&json!({ "id": id, "ok": false, "error": format!("{err:#}") }));
            }
            return;
        }
        let response = match self.handle(&cmd, &request) {
            Ok(mut body) => {
                body.insert("id".into(), id);
                body.insert("ok".into(), json!(true));
                Value::Object(body)
            }
            Err(err) => json!({ "id": id, "ok": false, "error": format!("{err:#}") }),
        };
        self.out.send_if(Some(generation), &response);
    }

    fn handle(&mut self, cmd: &str, request: &Value) -> Result<Map<String, Value>> {
        let mut body = Map::new();
        match cmd {
            "hello" => {
                if let Some(version) = request.get("version").and_then(Value::as_u64)
                    && version != VERSION
                {
                    bail!("protocol version {version} is not supported; this is {VERSION}");
                }
                body.insert("version".into(), json!(VERSION));
                body.insert("cwd".into(), json!(self.root_text));
                body.insert("waiting".into(), json!(self.settings.url.is_some()));
                body.insert("running".into(), json!(self.running()));
                let stopped: Vec<Value> = self.pages.values().filter_map(|page| page.stop.clone()).collect();
                body.insert("stopped".into(), json!(stopped));
            }
            "run" => {
                // a program held before running is released; here that is
                // opening --url, once the breakpoints are in place
                if let Some(url) = self.settings.url.take() {
                    self.open_url(&url)?;
                }
            }
            "setBreakpoints" => {
                let file = self.file_arg(request)?;
                let list = request.get("breakpoints").and_then(Value::as_array).cloned().unwrap_or_default();
                let placed = self.set_breakpoints(&file, &list);
                body.insert("breakpoints".into(), json!(placed));
            }
            "setExceptions" => {
                let all = request.get("all").and_then(Value::as_bool).unwrap_or(false);
                let uncaught = request.get("uncaught").and_then(Value::as_bool).unwrap_or(false);
                self.exceptions = if all {
                    Exceptions::All
                } else if uncaught {
                    Exceptions::Uncaught
                } else {
                    Exceptions::None
                };
                self.apply_exceptions();
            }
            "continue" => {
                let vm = self.stopped_vm(request)?;
                self.resume_stopped(vm, "Debugger.resume", json!({}))?;
            }
            "next" => self.start_step(request, StepKind::Over)?,
            "stepIn" => self.start_step(request, StepKind::Into)?,
            "stepOut" => self.start_step(request, StepKind::Out)?,
            "pause" => self.pause(request)?,
            "runTo" => self.run_to(request)?,
            "jump" => bail!("Chrome can't set the next statement"),
            "frame" => {
                let vm = self.stopped_vm(request)?;
                let frame = request.get("frame").and_then(Value::as_u64).unwrap_or(0) as usize;
                let (locals, globals) = self.scope_vars(vm, frame)?;
                body.insert("locals".into(), json!(locals));
                body.insert("globals".into(), json!(globals));
            }
            "expand" => {
                let reference = request.get("ref").and_then(Value::as_u64).context("ref is missing")?;
                let start = request.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
                let count = request.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
                let vars = self.expand(reference, start, count)?;
                body.insert("vars".into(), json!(vars));
            }
            "eval" => {
                let vm = self.stopped_vm(request)?;
                let frame = request.get("frame").and_then(Value::as_u64).unwrap_or(0) as usize;
                let expr = request.get("expr").and_then(Value::as_str).context("expr is missing")?.to_string();
                let var = self.eval(vm, frame, &expr)?;
                if let Value::Object(var) = var {
                    body = var;
                }
            }
            "threads" => {
                let stopped: Vec<u64> =
                    self.pages.iter().filter(|(_, page)| page.stop.is_some()).map(|(vm, _)| *vm).collect();
                body.insert("running".into(), json!(self.running()));
                body.insert("stopped".into(), json!(stopped));
            }
            "pages" => {
                let pages: Vec<Value> = self
                    .pages
                    .iter()
                    .filter(|(_, page)| self.listed(page))
                    .map(|(vm, page)| json!({ "vm": vm, "url": page.url, "title": page.title }))
                    .collect();
                body.insert("pages".into(), json!(pages));
            }
            "" => bail!("a request without cmd"),
            other => bail!("unknown command {other:?}"),
        }
        Ok(body)
    }

    /// How many debugged pages are running.
    fn running(&self) -> usize {
        self.pages.values().filter(|page| self.listed(page) && page.stop.is_none()).count()
    }

    /// The pages that are VMs of the protocol: those of a debugged host, and
    /// blank ones, which may become one.
    fn listed(&self, page: &Page) -> bool {
        is_blank(&page.url) || self.debugged_url(&page.url)
    }

    fn debugged_url(&self, url: &str) -> bool {
        fetch::host(url).is_some_and(|host| fetch::host_matches(&host, &self.settings.hosts))
    }

    /// A file argument as a path relative to the root, with `/`.
    fn file_arg(&self, request: &Value) -> Result<String> {
        let file = request.get("file").and_then(Value::as_str).context("file is missing")?;
        let path = Path::new(file);
        let relative = path.strip_prefix(&self.settings.root).unwrap_or(path);
        let text = relative.to_string_lossy().replace('\\', "/");
        Ok(text.trim_start_matches("./").to_string())
    }

    fn vm_arg(&self, request: &Value) -> Result<u64> {
        let vm = request.get("vm").and_then(Value::as_u64).context("vm is missing")?;
        if !self.pages.contains_key(&vm) {
            bail!("no vm {vm}");
        }
        Ok(vm)
    }

    fn stopped_vm(&self, request: &Value) -> Result<u64> {
        let vm = self.vm_arg(request)?;
        if self.pages.get(&vm).is_none_or(|page| page.stop.is_none()) {
            bail!("vm {vm} is not stopped");
        }
        Ok(vm)
    }

    /// The `vm` of a bridge command, or the first debugged page.
    fn page_arg(&self, request: &Value) -> Result<u64> {
        if request.get("vm").is_some_and(|vm| !vm.is_null()) {
            return self.vm_arg(request);
        }
        self.pages.iter().find(|(_, page)| self.listed(page)).map(|(vm, _)| *vm).context("no page is debugged")
    }

    fn page_call(&self, vm: u64, method: &str, params: Value) -> Result<Value> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        self.cdp.call(Some(&page.session), method, params)
    }

    fn open_url(&mut self, url: &str) -> Result<()> {
        let same = |page: &Page| page.url == url || page.url.trim_end_matches('/') == url.trim_end_matches('/');
        // a tab of an earlier session already showing it is loaded again: a session starts
        // the page, so a breakpoint in the code that runs at load stops
        if let Some(page) = self.pages.values().find(|page| same(page)) {
            self.cdp
                .call(None, "Target.activateTarget", json!({ "targetId": page.target }))
                .context("bring the tab to front")?;
            let session = page.session.clone();
            self.call_in_background(Some(session), "Page.reload", json!({}));
            return Ok(());
        }
        // a tab of the same site (an earlier session's, or one the site sent elsewhere, as a
        // sign-in does) goes to the address, rather than a new tab for every session
        let origin = origin_of(url);
        if let Some((vm, target)) = self
            .pages
            .iter()
            .find(|(_, page)| origin.is_some() && origin_of(&page.url) == origin)
            .map(|(vm, page)| (*vm, page.target.clone()))
        {
            self.cdp
                .call(None, "Target.activateTarget", json!({ "targetId": target }))
                .context("bring the tab to front")?;
            let session =
                self.pages.get(&vm).map(|page| page.session.clone()).with_context(|| format!("no vm {vm}"))?;
            self.call_in_background(Some(session), "Page.navigate", json!({ "url": url }));
            return Ok(());
        }
        if self.settings.launched
            && let Some((vm, target)) =
                self.pages.iter().find(|(_, page)| is_blank(&page.url)).map(|(vm, page)| (*vm, page.target.clone()))
        {
            self.cdp
                .call(None, "Target.activateTarget", json!({ "targetId": target }))
                .context("bring the tab to front")?;
            let session =
                self.pages.get(&vm).map(|page| page.session.clone()).with_context(|| format!("no vm {vm}"))?;
            self.call_in_background(Some(session), "Page.navigate", json!({ "url": url }));
            return Ok(());
        }
        // the new tab waits for this thread to set it up before it loads
        self.call_in_background(None, "Target.createTarget", json!({ "url": url }));
        Ok(())
    }

    /// Calls Chrome without waiting for it: the answer to a call that lets
    /// a page run can come after the page stops at a breakpoint, whose event
    /// this thread has to handle first.
    fn call_in_background(&self, session: Option<String>, method: &'static str, params: Value) {
        // sent now, so it keeps its order with the calls after it
        let receiver = match self.cdp.send(session.as_deref(), method, params) {
            Ok(receiver) => receiver,
            Err(err) => return self.report(&format!("{err:#}")),
        };
        let out = self.out.clone();
        let spawned = thread::Builder::new().name("chrome-call".into()).spawn(move || {
            let error = match receiver.recv() {
                Ok(Ok(result)) => result.get("errorText").and_then(Value::as_str).map(str::to_string),
                Ok(Err(error)) => Some(error),
                Err(_) => Some("the connection to Chrome ended".to_string()),
            };
            if let Some(error) = error {
                eprintln!("chrome: {method}: {error}");
                out.send(&json!({ "event": "output", "text": format!("{method}: {error}\n"), "file": "", "line": 0 }));
            }
        });
        if let Err(err) = spawned {
            self.report(&format!("{method}: start its thread: {err}"));
        }
    }

    /// `navigate`, `reload` and `evaluate`: they run page code, which can stop
    /// at a breakpoint, so they wait for Chrome on a thread of their own.
    fn page_command(&mut self, generation: u64, cmd: &str, id: Value, request: &Value) -> Result<()> {
        let vm = self.page_arg(request)?;
        let session = self.pages.get(&vm).map(|page| page.session.clone()).with_context(|| format!("no vm {vm}"))?;
        let (method, params, name) = match cmd {
            "navigate" => {
                let url = request.get("url").and_then(Value::as_str).context("url is missing")?;
                ("Page.navigate", json!({ "url": url }), String::new())
            }
            "reload" => ("Page.reload", json!({}), String::new()),
            _ => {
                let expr = request.get("expr").and_then(Value::as_str).context("expr is missing")?;
                let params = json!({
                    "expression": expr,
                    "awaitPromise": true,
                    "generatePreview": true,
                    "userGesture": true,
                });
                ("Runtime.evaluate", params, expr.to_string())
            }
        };
        let cdp = self.cdp.clone();
        let out = self.out.clone();
        thread::Builder::new()
            .name(format!("chrome-{cmd}"))
            .spawn(move || {
                let result = cdp.call_timeout(Some(&session), method, params, None);
                let response = match result {
                    Ok(result) => page_command_body(method, &name, &result),
                    Err(err) => Err(err),
                };
                let response = match response {
                    Ok(mut body) => {
                        body.insert("id".into(), id);
                        body.insert("ok".into(), json!(true));
                        Value::Object(body)
                    }
                    Err(err) => json!({ "id": id, "ok": false, "error": format!("{err:#}") }),
                };
                out.send_if(Some(generation), &response);
            })
            .context("start the command's thread")?;
        Ok(())
    }

    // ---- breakpoints ----

    fn set_breakpoints(&mut self, file: &str, list: &[Value]) -> Vec<Value> {
        self.remove_placed(file);
        let old = self.breakpoints.remove(file).unwrap_or_default();
        let mut results = Vec::new();
        let mut kept = Vec::new();
        for item in list {
            let line = item.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let text = |key: &str| {
                item.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
            };
            if line == 0 {
                results.push(json!({ "line": line, "error": "a breakpoint needs a line" }));
                continue;
            }
            let hit = match Hit::parse(&text("hit").unwrap_or_default()) {
                Ok(hit) => hit,
                Err(err) => {
                    results.push(json!({ "line": line, "error": err.to_string() }));
                    continue;
                }
            };
            // a breakpoint that stays keeps its hit count
            let hits = old.iter().find(|bp| bp.line == line).map(|bp| bp.hits).unwrap_or(0);
            kept.push(Breakpoint { line, condition: text("condition"), hit, log: text("log"), hits });
            results.push(json!({ "line": line }));
        }
        self.breakpoints.insert(file.to_string(), kept);

        // place them in every script that has the file; the first answer is
        // where each went
        let mut outcome: HashMap<u32, Result<u32, String>> = HashMap::new();
        let targets: Vec<(u64, String)> = self
            .pages
            .iter()
            .flat_map(|(vm, page)| {
                page.scripts
                    .iter()
                    .filter(|(_, script)| script.map.as_ref().is_some_and(|map| map.has_file(file)))
                    .map(move |(id, _)| (*vm, id.clone()))
            })
            .collect();
        for (vm, script) in targets {
            for (line, result) in self.place_file(vm, &script, file) {
                let entry = outcome.entry(line).or_insert_with(|| result.clone());
                if entry.is_err() && result.is_ok() {
                    *entry = result;
                }
            }
        }
        for result in &mut results {
            if result.get("error").is_some() {
                continue;
            }
            let line = result["line"].as_u64().unwrap_or(0) as u32;
            match outcome.get(&line) {
                Some(Ok(placed)) => result["line"] = json!(placed),
                Some(Err(error)) => result["error"] = json!(error),
                // no script has the file yet: it waits for one
                None => {}
            }
        }
        results
    }

    /// Places the breakpoints of a file in a script. Returns where each one
    /// went (1-based), by the line asked.
    fn place_file(&mut self, vm: u64, script_id: &str, file: &str) -> Vec<(u32, Result<u32, String>)> {
        let wanted: Vec<(u32, Option<String>)> = match self.breakpoints.get(file) {
            Some(list) => list.iter().map(|bp| (bp.line, bp.condition.clone())).collect(),
            None => return Vec::new(),
        };
        wanted
            .into_iter()
            .map(|(line, condition)| {
                let result = self.place(vm, script_id, file, line, condition).map_err(|err| format!("{err:#}"));
                (line, result)
            })
            .collect()
    }

    fn place(&mut self, vm: u64, script_id: &str, file: &str, line: u32, condition: Option<String>) -> Result<u32> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let script = page.scripts.get(script_id).context("the script is gone")?;
        let map = script.map.clone().context("the script has no source map")?;
        let (original_line, (gen_line, gen_column)) =
            map.generated(file, line - 1).with_context(|| format!("no code on line {line} or after it"))?;
        let (line_number, column_number) = script.to_resource(gen_line, gen_column);
        let mut params =
            json!({ "location": { "scriptId": script_id, "lineNumber": line_number, "columnNumber": column_number } });
        if let Some(condition) = condition {
            params["condition"] = json!(condition);
        }
        let result = self.page_call(vm, "Debugger.setBreakpoint", params)?;
        let id = result.get("breakpointId").and_then(Value::as_str).context("setBreakpoint gave no id")?.to_string();
        // where Chrome put it, when that is still on the file
        let actual = result.get("actualLocation").and_then(|location| self.original_of(vm, location));
        let placed_line = match actual {
            Some(original) if original.file == file && original.line >= original_line => original.line,
            _ => original_line,
        };
        if let Some(page) = self.pages.get_mut(&vm) {
            page.placed.insert(id, Placed { file: file.to_string(), line, script: script_id.to_string() });
        }
        Ok(placed_line + 1)
    }

    /// Removes a file's breakpoints from Chrome, in every page.
    fn remove_placed(&mut self, file: &str) {
        let mut removals = Vec::new();
        for (vm, page) in &mut self.pages {
            let ids: Vec<String> =
                page.placed.iter().filter(|(_, placed)| placed.file == file).map(|(id, _)| id.clone()).collect();
            for id in ids {
                page.placed.remove(&id);
                removals.push((*vm, id));
            }
        }
        for (vm, id) in removals {
            if let Err(err) = self.page_call(vm, "Debugger.removeBreakpoint", json!({ "breakpointId": id })) {
                eprintln!("chrome: remove breakpoint {id}: {err:#}");
            }
        }
    }

    fn apply_exceptions(&mut self) {
        let state = if self.client.is_some() { self.exceptions.state() } else { "none" };
        let vms: Vec<u64> = self.pages.keys().copied().collect();
        for vm in vms {
            if let Err(err) = self.page_call(vm, "Debugger.setPauseOnExceptions", json!({ "state": state })) {
                self.report(&format!("vm {vm}: pause on exceptions: {err:#}"));
            }
        }
    }

    // ---- running and stepping ----

    /// Resumes a VM the client sees stopped: `resumed` first, then Chrome.
    fn resume_stopped(&mut self, vm: u64, method: &str, params: Value) -> Result<()> {
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        page.stop = None;
        page.paused = None;
        self.refs.retain(|_, entry| entry.vm != vm);
        self.out.send(&json!({ "event": "resumed", "vm": vm }));
        self.page_call(vm, method, params)?;
        Ok(())
    }

    fn start_step(&mut self, request: &Value, kind: StepKind) -> Result<()> {
        let vm = self.stopped_vm(request)?;
        let lines = self.frame_lines(vm)?;
        let (depth, at) = (lines.len(), lines.first().cloned().flatten());
        if let Some(page) = self.pages.get_mut(&vm) {
            page.step = Some(Step { kind, depth, at, lines, count: 0 });
        }
        self.resume_stopped(vm, step_method(kind), json!({}))
    }

    /// The original line of each frame of a paused page, frame 0 first.
    fn frame_lines(&self, vm: u64) -> Result<Vec<Option<Original>>> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let frames = paused.get("callFrames").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(frames.iter().map(|frame| self.original_of(vm, &frame["location"])).collect())
    }

    fn pause(&mut self, request: &Value) -> Result<()> {
        if request.get("vm").is_some_and(|vm| !vm.is_null()) {
            let vm = self.vm_arg(request)?;
            let Some(page) = self.pages.get_mut(&vm) else { return Ok(()) };
            if page.stop.is_some() {
                return Ok(());
            }
            page.pause_request = Some(false);
            self.page_call(vm, "Debugger.pause", json!({}))?;
            return Ok(());
        }
        self.pause_any = true;
        let vms: Vec<u64> = self.pages.iter().filter(|(_, page)| page.stop.is_none() && self.listed(page)).map(|(vm, _)| *vm).collect();
        for vm in vms {
            if let Some(page) = self.pages.get_mut(&vm)
                && page.pause_request.is_none()
            {
                page.pause_request = Some(true);
            }
            self.page_call(vm, "Debugger.pause", json!({}))?;
        }
        Ok(())
    }

    fn run_to(&mut self, request: &Value) -> Result<()> {
        let vm = self.stopped_vm(request)?;
        let file = self.file_arg(request)?;
        let line = request.get("line").and_then(Value::as_u64).context("line is missing")? as u32;
        if line == 0 {
            bail!("line is missing");
        }
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let location = page
            .scripts
            .iter()
            .find_map(|(id, script)| {
                let map = script.map.as_ref()?;
                let (_, (gen_line, gen_column)) = map.generated(&file, line - 1)?;
                let (line_number, column_number) = script.to_resource(gen_line, gen_column);
                Some(json!({ "scriptId": id, "lineNumber": line_number, "columnNumber": column_number }))
            })
            .with_context(|| format!("no code at {file}:{line} in this page"))?;
        if let Some(page) = self.pages.get_mut(&vm) {
            page.run_to = true;
        }
        self.resume_stopped(
            vm,
            "Debugger.continueToLocation",
            json!({ "location": location, "targetCallFrames": "any" }),
        )
    }

    // ---- Chrome's events ----

    fn on_cdp(&mut self, event: cdp::Event) -> Result<()> {
        let params = &event.params;
        match event.method.as_str() {
            "Target.attachedToTarget" => return self.on_attached(params),
            "Target.detachedFromTarget" => {
                if let Some(vm) = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .and_then(|session| self.sessions.get(session).copied())
                {
                    self.remove_page(vm);
                }
                return Ok(());
            }
            "Target.targetDestroyed" => {
                if let Some(vm) =
                    params.get("targetId").and_then(Value::as_str).and_then(|target| self.targets.get(target).copied())
                {
                    self.remove_page(vm);
                }
                return Ok(());
            }
            "Target.targetInfoChanged" => {
                let info = &params["targetInfo"];
                if let Some(vm) = self.targets.get(values::str_of(info, "targetId")).copied()
                    && let Some(page) = self.pages.get_mut(&vm)
                {
                    page.url = values::str_of(info, "url").to_string();
                    page.title = values::str_of(info, "title").to_string();
                }
                return Ok(());
            }
            _ => {}
        }
        let Some(vm) = event.session.as_deref().and_then(|session| self.sessions.get(session).copied()) else {
            return Ok(());
        };
        match event.method.as_str() {
            "Runtime.executionContextCreated" => {
                let context = &params["context"];
                let id = context["id"].as_i64().context("a context without id")?;
                let debugged = self.debugged_url(values::str_of(context, "origin"));
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.insert(id, debugged);
                }
            }
            "Runtime.executionContextDestroyed" => {
                let id = params["executionContextId"].as_i64().context("a context without id")?;
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.remove(&id);
                    page.scripts.retain(|_, script| script.context != id);
                    let scripts = &page.scripts;
                    page.placed.retain(|_, placed| scripts.contains_key(&placed.script));
                }
            }
            "Runtime.executionContextsCleared" => {
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.clear();
                    page.scripts.clear();
                    page.placed.clear();
                }
            }
            "Debugger.scriptParsed" => self.on_script(vm, params)?,
            "Debugger.paused" => self.on_paused(vm, params.clone())?,
            "Debugger.resumed" => {
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.paused = None;
                    if page.stop.take().is_some() {
                        // Chrome resumed on its own: a navigation, a closed tab
                        self.refs.retain(|_, entry| entry.vm != vm);
                        self.out.send(&json!({ "event": "resumed", "vm": vm }));
                    }
                }
            }
            "Runtime.consoleAPICalled" => self.on_console(vm, params),
            "Runtime.exceptionThrown" => self.on_exception_thrown(vm, params),
            _ => {}
        }
        Ok(())
    }

    fn on_attached(&mut self, params: &Value) -> Result<()> {
        let session = values::str_of(params, "sessionId").to_string();
        let info = &params["targetInfo"];
        let waiting = params.get("waitingForDebugger").and_then(Value::as_bool).unwrap_or(false);
        if values::str_of(info, "type") != "page" {
            // workers, frames of other sites: not debugged
            if waiting {
                self.call_in_background(Some(session.clone()), "Runtime.runIfWaitingForDebugger", json!({}));
            }
            self.cdp.call(None, "Target.detachFromTarget", json!({ "sessionId": session }))?;
            return Ok(());
        }
        let target = values::str_of(info, "targetId").to_string();
        let vm = match self.targets.get(&target) {
            Some(vm) => *vm,
            None => {
                let vm = self.next_vm;
                self.next_vm += 1;
                vm
            }
        };
        if let Some(old) = self.pages.get(&vm) {
            self.sessions.remove(&old.session);
        }
        self.targets.insert(target.clone(), vm);
        self.sessions.insert(session.clone(), vm);
        self.pages.insert(
            vm,
            Page {
                target,
                session: session.clone(),
                url: values::str_of(info, "url").to_string(),
                title: values::str_of(info, "title").to_string(),
                contexts: HashMap::new(),
                scripts: HashMap::new(),
                placed: HashMap::new(),
                paused: None,
                stop: None,
                step: None,
                run_to: false,
                pause_request: None,
                last_exception: None,
            },
        );
        // Every page is enabled, whatever it shows: a page decides by the
        // origin of each script's context, which is known before the script
        // runs, so a navigation to a debugged host misses nothing.
        let state = if self.client.is_some() { self.exceptions.state() } else { "none" };
        let setup = [
            ("Runtime.enable", json!({})),
            ("Debugger.enable", json!({})),
            (
                "Debugger.setInstrumentationBreakpoint",
                json!({ "instrumentation": "beforeScriptWithSourceMapExecution" }),
            ),
            ("Debugger.setPauseOnExceptions", json!({ "state": state })),
        ];
        for (method, params) in setup {
            self.cdp.call(Some(&session), method, params).with_context(|| format!("set up vm {vm}"))?;
        }
        if waiting {
            self.call_in_background(Some(session), "Runtime.runIfWaitingForDebugger", json!({}));
        }
        Ok(())
    }

    fn remove_page(&mut self, vm: u64) {
        let Some(page) = self.pages.remove(&vm) else { return };
        self.sessions.remove(&page.session);
        self.targets.remove(&page.target);
        self.refs.retain(|_, entry| entry.vm != vm);
        if page.stop.is_some() {
            self.out.send(&json!({ "event": "resumed", "vm": vm }));
        }
    }

    fn on_script(&mut self, vm: u64, params: &Value) -> Result<()> {
        let id = values::str_of(params, "scriptId").to_string();
        let url = values::str_of(params, "url").to_string();
        let context = params["executionContextId"].as_i64().unwrap_or(0);
        let source_map = values::str_of(params, "sourceMapURL").to_string();
        let debugged = self.pages.get(&vm).is_some_and(|page| page.contexts.get(&context) == Some(&true));
        let mut script = Script {
            url: url.clone(),
            context,
            map: None,
            start_line: params["startLine"].as_u64().unwrap_or(0) as u32,
            start_column: params["startColumn"].as_u64().unwrap_or(0) as u32,
        };
        if debugged && !source_map.is_empty() {
            match self.load_map(&url, &source_map) {
                Ok(map) => script.map = Some(map),
                Err(err) => self.report(&format!("the source map of {}: {err:#}", fetch::short(&url))),
            }
        }
        let files: Vec<String> = match &script.map {
            Some(map) => map.files().filter(|file| self.breakpoints.contains_key(*file)).cloned().collect(),
            None => Vec::new(),
        };
        if let Some(page) = self.pages.get_mut(&vm) {
            page.scripts.insert(id.clone(), script);
        }
        for file in files {
            for (line, result) in self.place_file(vm, &id, &file) {
                if let Err(error) = result {
                    eprintln!("chrome: breakpoint {file}:{line}: {error}");
                }
            }
        }
        Ok(())
    }

    fn load_map(&mut self, script_url: &str, map_url: &str) -> Result<Arc<SourceMap>> {
        if map_url.starts_with("data:") {
            let mut hasher = DefaultHasher::new();
            map_url.hash(&mut hasher);
            let key = hasher.finish();
            if let Some(map) = self.maps.get(&key) {
                return Ok(map.clone());
            }
            let text = fetch::load(map_url)?;
            let map = Arc::new(SourceMap::parse(&text, &self.settings.root)?);
            self.maps.insert(key, map.clone());
            return Ok(map);
        }
        let url = fetch::resolve_url(script_url, map_url);
        let text = fetch::load(&url).with_context(|| format!("load {}", fetch::short(&url)))?;
        Ok(Arc::new(SourceMap::parse(&text, &self.settings.root)?))
    }

    fn on_paused(&mut self, vm: u64, params: Value) -> Result<()> {
        let reason = values::str_of(&params, "reason").to_string();
        let script_debugged = {
            let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
            page.paused = Some(params.clone());
            let script_id = if reason == "instrumentation" {
                values::str_of(&params["data"], "scriptId").to_string()
            } else {
                params["callFrames"][0]["location"]["scriptId"].as_str().unwrap_or("").to_string()
            };
            page.scripts.get(&script_id).is_some_and(|script| page.contexts.get(&script.context) == Some(&true))
        };
        // Before a script with a source map runs: its breakpoints were placed
        // when it was parsed, an event that comes before this one.
        if reason == "instrumentation" || self.client.is_none() || !script_debugged {
            return self.resume_quietly(vm);
        }

        let (pause_request, step_active) = {
            let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
            page.last_exception = None;
            (page.pause_request.take(), page.step.is_some())
        };

        let hit_ids: Vec<String> = params["hitBreakpoints"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let placed = self.pages.get(&vm).and_then(|page| {
            hit_ids.iter().find_map(|id| page.placed.get(id)).map(|placed| (placed.file.clone(), placed.line))
        });
        if let Some((file, line)) = placed {
            match self.breakpoint_hit(vm, &file, line)? {
                true => return self.stop(vm, "breakpoint", None),
                false if step_active => return self.step_landed(vm),
                false => return self.resume_quietly(vm),
            }
        }

        if reason == "exception" || reason == "promiseRejection" {
            let data = &params["data"];
            let description = match data.get("description").and_then(Value::as_str) {
                Some(description) => description.to_string(),
                None => values::plain(data),
            };
            let stack = self.map_stack(vm, &description);
            if let Some(page) = self.pages.get_mut(&vm) {
                page.last_exception = Some(description.clone());
            }
            let exception = json!({ "message": values::first_line(&description), "stack": stack });
            return self.stop(vm, "exception", Some(exception));
        }

        if let Some(any) = pause_request {
            if any && !self.pause_any {
                // another VM took the pause asked for any
                return self.resume_quietly(vm);
            }
            if any {
                self.pause_any = false;
            }
            return self.stop(vm, "pause", None);
        }
        if step_active {
            return self.step_landed(vm);
        }
        if self.pages.get(&vm).is_some_and(|page| page.run_to) {
            return self.stop(vm, "step", None);
        }
        // a `debugger` statement
        self.stop(vm, "breakpoint", None)
    }

    /// A breakpoint was reached: counts it, prints its log message, and
    /// says whether the VM stops.
    fn breakpoint_hit(&mut self, vm: u64, file: &str, line: u32) -> Result<bool> {
        let Some(bp) = self.breakpoints.get_mut(file).and_then(|list| list.iter_mut().find(|bp| bp.line == line))
        else {
            return Ok(false);
        };
        bp.hits += 1;
        if let Some(hit) = bp.hit
            && !hit.matches(bp.hits)
        {
            return Ok(false);
        }
        let Some(message) = bp.log.clone() else { return Ok(true) };
        let text = self.interpolate(vm, &message);
        let (file, line) = self.top_location(vm);
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
        Ok(false)
    }

    /// A log message with each `{expr}` replaced by its value in frame 0.
    fn interpolate(&mut self, vm: u64, message: &str) -> String {
        let mut out = String::new();
        let mut rest = message;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else { break };
            out.push_str(&rest[..open]);
            let expr = &rest[open + 1..open + close];
            match self.evaluate_on_frame(vm, 0, expr, true) {
                Ok(object) => out.push_str(&values::plain(&object)),
                Err(err) => out.push_str(&format!("<{err:#}>")),
            }
            rest = &rest[open + close + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Where frame 0 is, as the client names it.
    fn top_location(&self, vm: u64) -> (String, u32) {
        let Some(top) =
            self.pages.get(&vm).and_then(|page| page.paused.as_ref()).and_then(|paused| paused["callFrames"].get(0))
        else {
            return (String::new(), 0);
        };
        self.location_of(vm, &top["location"], values::str_of(top, "url"))
    }

    /// A step landed somewhere: it stops there, or steps on.
    fn step_landed(&mut self, vm: u64) -> Result<()> {
        let lines = self.frame_lines(vm)?;
        let (depth, at) = (lines.len(), lines.first().cloned().flatten());
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        let Some(step) = page.step.as_mut() else { return self.stop(vm, "step", None) };
        step.count += 1;
        if step.count > MAX_STEPS {
            return self.stop(vm, "step", None);
        }
        let next = match &at {
            // code without a map: on until code with one
            None if step.kind == StepKind::Into => Some(StepKind::Into),
            None if step.kind == StepKind::Over && depth == step.depth => Some(StepKind::Over),
            None => Some(StepKind::Out),
            // in a function the line called
            Some(_) if depth > step.depth => match step.kind {
                StepKind::Into => None,
                _ => Some(StepKind::Out),
            },
            Some(original) if depth == step.depth => match step.kind {
                StepKind::Out => Some(StepKind::Out),
                // more code of the same line: a TS line can be several
                // statements of JS (a one-line loop is stepped through whole)
                kind if step.at.as_ref() == Some(original) => Some(kind),
                _ => None,
            },
            // back in a caller: still on the line that made the call, it
            // goes on to the next line, as sim does
            Some(original) => {
                let returned = step.depth - depth;
                if step.lines.get(returned).is_some_and(|line| line.as_ref() == Some(original)) {
                    step.lines.drain(..returned.min(step.lines.len()));
                    step.depth = depth;
                    step.at = at.clone();
                    if step.kind == StepKind::Out {
                        step.kind = StepKind::Over;
                    }
                    Some(step.kind)
                } else {
                    None
                }
            }
        };
        match next {
            Some(kind) => {
                page.paused = None;
                if let Err(err) = self.page_call(vm, step_method(kind), json!({})) {
                    self.report(&format!("vm {vm}: step: {err:#}"));
                    return self.stop(vm, "step", None);
                }
                Ok(())
            }
            None => self.stop(vm, "step", None),
        }
    }

    fn resume_quietly(&mut self, vm: u64) -> Result<()> {
        if let Some(page) = self.pages.get_mut(&vm) {
            page.paused = None;
        }
        self.page_call(vm, "Debugger.resume", json!({}))?;
        Ok(())
    }

    /// The VM stops where Chrome holds it: the client is told everything
    /// about it.
    fn stop(&mut self, vm: u64, reason: &str, exception: Option<Value>) -> Result<()> {
        let frames = self.frames(vm);
        let (locals, globals) = match self.scope_vars(vm, 0) {
            Ok(vars) => vars,
            Err(err) => {
                self.report(&format!("vm {vm}: the variables: {err:#}"));
                (Vec::new(), 0)
            }
        };
        let (file, line) =
            frames.first().map(|frame| (frame["file"].clone(), frame["line"].clone())).unwrap_or((json!(""), json!(0)));
        let mut stop = json!({
            "vm": vm,
            "reason": reason,
            "file": file,
            "line": line,
            "frames": frames,
            "locals": locals,
            "globals": globals,
        });
        if let Some(exception) = exception {
            stop["exception"] = exception;
        }
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        page.step = None;
        page.run_to = false;
        page.stop = Some(stop.clone());
        stop["event"] = json!("stopped");
        self.out.send(&stop);
        Ok(())
    }

    fn frames(&self, vm: u64) -> Vec<Value> {
        let Some(paused) = self.pages.get(&vm).and_then(|page| page.paused.as_ref()) else { return Vec::new() };
        let frames = paused["callFrames"].as_array().cloned().unwrap_or_default();
        frames
            .iter()
            .map(|frame| {
                let (file, line) = self.location_of(vm, &frame["location"], values::str_of(frame, "url"));
                let name = values::str_of(frame, "functionName");
                let name = if name.is_empty() { "(anonymous)" } else { name };
                json!({ "function": name, "file": file, "line": line })
            })
            .collect()
    }

    /// A CDP location as the client names it: the original file and line,
    /// or the generated URL and line when it has no mapping.
    fn location_of(&self, vm: u64, location: &Value, url: &str) -> (String, u32) {
        if let Some(original) = self.original_of(vm, location) {
            return (original.file, original.line + 1);
        }
        let url = if url.is_empty() {
            self.pages
                .get(&vm)
                .and_then(|page| page.scripts.get(values::str_of(location, "scriptId")))
                .map(|script| script.url.clone())
                .unwrap_or_default()
        } else {
            url.to_string()
        };
        (url, location["lineNumber"].as_u64().unwrap_or(0) as u32 + 1)
    }

    fn original_of(&self, vm: u64, location: &Value) -> Option<Original> {
        let page = self.pages.get(&vm)?;
        let script = page.scripts.get(values::str_of(location, "scriptId"))?;
        let map = script.map.as_ref()?;
        let line = location["lineNumber"].as_u64()? as u32;
        let column = location["columnNumber"].as_u64().unwrap_or(0) as u32;
        let (line, column) = script.to_map(line, column)?;
        map.original(line, column)
    }

    /// An exception's stack with each `url:line:column` of a script with a
    /// map replaced by its original `file:line`.
    fn map_stack(&self, vm: u64, stack: &str) -> String {
        let Some(page) = self.pages.get(&vm) else { return stack.to_string() };
        let mut lines = Vec::new();
        for line in stack.lines() {
            let mut mapped = line.to_string();
            for (id, script) in &page.scripts {
                if script.url.is_empty() || script.map.is_none() {
                    continue;
                }
                let pattern = format!("{}:", script.url);
                let Some(at) = line.find(&pattern) else { continue };
                let after = &line[at + pattern.len()..];
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                let rest = &after[digits.len()..];
                let Some(rest) = rest.strip_prefix(':') else { continue };
                let column_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                let (Ok(line_number), Ok(column)) = (digits.parse::<u32>(), column_digits.parse::<u32>()) else {
                    continue;
                };
                let location = json!({ "scriptId": id, "lineNumber": line_number.saturating_sub(1), "columnNumber": column.saturating_sub(1) });
                let Some(original) = self.original_of(vm, &location) else { continue };
                let end = at + pattern.len() + digits.len() + 1 + column_digits.len();
                mapped = format!("{}{}:{}{}", &line[..at], original.file, original.line + 1, &line[end..]);
                break;
            }
            lines.push(mapped);
        }
        lines.join("\n")
    }

    fn on_console(&mut self, vm: u64, params: &Value) {
        if self.client.is_none() {
            return;
        }
        let context = params["executionContextId"].as_i64().unwrap_or(0);
        if self.pages.get(&vm).is_none_or(|page| page.contexts.get(&context) != Some(&true)) {
            return;
        }
        let args = params["args"].as_array().cloned().unwrap_or_default();
        let text = values::console_text(&args);
        let (file, line) = match params["stackTrace"]["callFrames"].get(0) {
            Some(frame) => self.location_of(vm, frame, values::str_of(frame, "url")),
            None => (String::new(), 0),
        };
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
    }

    fn on_exception_thrown(&mut self, vm: u64, params: &Value) {
        if self.client.is_none() {
            return;
        }
        let details = &params["exceptionDetails"];
        let context = details["executionContextId"].as_i64().unwrap_or(0);
        let Some(page) = self.pages.get_mut(&vm) else { return };
        if page.contexts.get(&context) != Some(&true) {
            return;
        }
        let description = details["exception"].get("description").and_then(Value::as_str).map(str::to_string);
        // the exception the VM stopped for, which the client has seen
        if description.is_some() && page.last_exception == description {
            page.last_exception = None;
            return;
        }
        let text = match &description {
            Some(description) => format!("Uncaught {}", self.map_stack(vm, description)),
            None => values::str_of(details, "text").to_string(),
        };
        let (file, line) = match details["stackTrace"]["callFrames"].get(0) {
            Some(frame) => self.location_of(vm, frame, values::str_of(frame, "url")),
            None => self.location_of(vm, details, values::str_of(details, "url")),
        };
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
    }

    // ---- values ----

    /// The variables of a frame: its own, its blocks' and its closures', and
    /// a ref to its module's.
    fn scope_vars(&mut self, vm: u64, frame: usize) -> Result<(Vec<Value>, u64)> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let call_frame = paused["callFrames"].get(frame).with_context(|| format!("no frame {frame}"))?.clone();
        let script_has_map = page
            .scripts
            .get(values::str_of(&call_frame["location"], "scriptId"))
            .is_some_and(|script| script.map.is_some());

        let mut groups: Vec<Vec<Value>> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut module: Option<Value> = None;
        let mut module_rank = 0;
        let this = &call_frame["this"];
        let this_shown = values::str_of(this, "type") == "object"
            && !matches!(values::str_of(this, "subtype"), "null")
            && values::str_of(this, "className") != "Window";
        for scope in call_frame["scopeChain"].as_array().cloned().unwrap_or_default() {
            let kind = values::str_of(&scope, "type");
            // The bundle's wrapper (code of no source) holds the modules'
            // variables: that is the frame's module, not its locals.
            let wrapper = kind == "closure"
                && script_has_map
                && scope.get("startLocation").is_some_and(|start| self.original_of(vm, start).is_none());
            let rank = match kind {
                "closure" if wrapper => 3,
                "module" => 2,
                "script" => 1,
                _ => 0,
            };
            if rank > 0 {
                if rank > module_rank {
                    module = Some(scope["object"].clone());
                    module_rank = rank;
                }
                continue;
            }
            if !matches!(kind, "local" | "block" | "closure" | "catch" | "with" | "eval") {
                continue;
            }
            let Some(object) = scope["object"].get("objectId").and_then(Value::as_str) else { continue };
            let properties = self.properties(vm, object)?;
            let mut group = Vec::new();
            for (name, value) in properties {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                group.push(self.var(vm, &name, &value));
            }
            groups.push(group);
        }
        // outer scopes were declared first
        let mut locals: Vec<Value> = groups.into_iter().rev().flatten().collect();
        if this_shown {
            let this = this.clone();
            locals.insert(0, self.var(vm, "this", &this));
        }
        let globals = match module {
            Some(object) => self.add_ref(vm, &object),
            None => 0,
        };
        Ok((locals, globals))
    }

    /// The own properties of an object, with their values, in order.
    fn properties(&self, vm: u64, object: &str) -> Result<Vec<(String, Value)>> {
        let result = self.page_call(
            vm,
            "Runtime.getProperties",
            json!({ "objectId": object, "ownProperties": true, "generatePreview": true }),
        )?;
        let mut list = Vec::new();
        for property in result["result"].as_array().cloned().unwrap_or_default() {
            let name = values::str_of(&property, "name").to_string();
            if name == "__proto__" {
                continue;
            }
            match property.get("value") {
                Some(value) => list.push((name, value.clone())),
                // a getter: running it could change something
                None if property.get("get").is_some_and(|get| get.get("objectId").is_some()) => {
                    list.push((name, json!({ "type": "accessor", "description": "(…)" })));
                }
                None => {}
            }
        }
        for property in result["privateProperties"].as_array().cloned().unwrap_or_default() {
            if let Some(value) = property.get("value") {
                list.push((values::str_of(&property, "name").to_string(), value.clone()));
            }
        }
        for property in result["internalProperties"].as_array().cloned().unwrap_or_default() {
            let name = values::str_of(&property, "name");
            if matches!(name, "[[Entries]]" | "[[PrimitiveValue]]" | "[[Target]]")
                && let Some(value) = property.get("value")
            {
                list.push((name.to_string(), value.clone()));
            }
        }
        Ok(list)
    }

    fn add_ref(&mut self, vm: u64, object: &Value) -> u64 {
        let Some(id) = object.get("objectId").and_then(Value::as_str) else { return 0 };
        let reference = self.next_ref;
        self.next_ref += 1;
        self.refs.insert(
            reference,
            Ref { vm, object: id.to_string(), subtype: values::str_of(object, "subtype").to_string() },
        );
        reference
    }

    fn var(&mut self, vm: u64, name: &str, object: &Value) -> Value {
        let reference = if values::expandable(object) { self.add_ref(vm, object) } else { 0 };
        let mut var = json!({
            "name": name,
            "value": values::preview(object),
            "type": values::type_name(object),
            "ref": reference,
        });
        let count = values::count(object);
        if reference > 0 && count > 0 {
            var["count"] = json!(count);
        }
        var
    }

    fn expand(&mut self, reference: u64, start: usize, count: usize) -> Result<Vec<Value>> {
        let entry = self.refs.get(&reference).with_context(|| format!("ref {reference} is no longer valid"))?;
        let (vm, object, subtype) = (entry.vm, entry.object.clone(), entry.subtype.clone());
        let mut children: Vec<(String, Value)> = Vec::new();
        match subtype.as_str() {
            "map" | "set" => {
                // a map's children are its entries, not its properties
                let properties = self.properties(vm, &object)?;
                let entries = properties.iter().find(|(name, _)| name == "[[Entries]]").map(|(_, value)| value.clone());
                if let Some(entries_id) =
                    entries.as_ref().and_then(|entries| entries.get("objectId")).and_then(Value::as_str)
                {
                    for (index, entry) in self.properties(vm, entries_id)? {
                        if index.parse::<usize>().is_err() {
                            continue;
                        }
                        let Some(entry_id) = entry.get("objectId").and_then(Value::as_str) else { continue };
                        let fields = self.properties(vm, entry_id)?;
                        let field =
                            |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
                        let value = field("value").unwrap_or(json!({ "type": "undefined" }));
                        let name = match field("key") {
                            Some(key) => values::preview(&key),
                            None => index,
                        };
                        children.push((name, value));
                    }
                }
            }
            "array" | "typedarray" => {
                children = self
                    .properties(vm, &object)?
                    .into_iter()
                    .filter(|(name, _)| name.parse::<usize>().is_ok())
                    .collect();
            }
            _ => children = self.properties(vm, &object)?,
        }
        let end = if count == 0 { children.len() } else { (start + count).min(children.len()) };
        let page: Vec<(String, Value)> = children.into_iter().skip(start).take(end.saturating_sub(start)).collect();
        Ok(page.into_iter().map(|(name, value)| self.var(vm, &name, &value)).collect())
    }

    fn evaluate_on_frame(&self, vm: u64, frame: usize, expr: &str, no_side_effects: bool) -> Result<Value> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let call_frame = values::str_of(&paused["callFrames"][frame], "callFrameId").to_string();
        if call_frame.is_empty() {
            bail!("no frame {frame}");
        }
        let result = self.page_call(
            vm,
            "Debugger.evaluateOnCallFrame",
            json!({
                "callFrameId": call_frame,
                "expression": expr,
                "throwOnSideEffect": no_side_effects,
                "generatePreview": true,
                "silent": true,
            }),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let description = details["exception"].get("description").and_then(Value::as_str).map(values::first_line);
            let message = description.unwrap_or_else(|| values::str_of(details, "text").to_string());
            if message.contains("Possible side-effect") {
                bail!(SideEffect);
            }
            bail!("{message}");
        }
        Ok(result["result"].clone())
    }

    fn eval(&mut self, vm: u64, frame: usize, expr: &str) -> Result<Value> {
        let object = match self.evaluate_on_frame(vm, frame, expr, true) {
            Ok(object) => object,
            Err(err) if err.is::<SideEffect>() && is_assignment(expr) => {
                self.evaluate_on_frame(vm, frame, expr, false)?
            }
            Err(err) => return Err(err),
        };
        Ok(self.var(vm, expr, &object))
    }
}

/// An expression refused because it could change something.
#[derive(Debug)]
struct SideEffect;

impl std::fmt::Display for SideEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the expression may have side effects, so it didn't run; only an assignment does")
    }
}

impl std::error::Error for SideEffect {}

impl Script {
    /// A position of the map in the resource: an inline script starts where
    /// its tag is.
    fn to_resource(&self, line: u32, column: u32) -> (u32, u32) {
        if line == 0 { (self.start_line, column + self.start_column) } else { (line + self.start_line, column) }
    }

    fn to_map(&self, line: u32, column: u32) -> Option<(u32, u32)> {
        let line = line.checked_sub(self.start_line)?;
        let column = if line == 0 { column.saturating_sub(self.start_column) } else { column };
        Some((line, column))
    }
}

fn step_method(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Over => "Debugger.stepOver",
        StepKind::Into => "Debugger.stepInto",
        StepKind::Out => "Debugger.stepOut",
    }
}

/// The scheme, host and port of an address: `http://localhost:8080` of
/// `http://localhost:8080/main/login`. None for one without `://`.
fn origin_of(url: &str) -> Option<&str> {
    let at = url.find("://")? + 3;
    let end = url[at..].find(['/', '?', '#']).map_or(url.len(), |end| at + end);
    Some(&url[..end])
}

fn is_blank(url: &str) -> bool {
    url.is_empty()
        || url.starts_with("about:")
        || url.starts_with("chrome://newtab")
        || url.starts_with("chrome://new-tab-page")
}

/// `a = x`, `a.b[0] = x`: a name, then fields or indexes, then `=`.
fn is_assignment(expr: &str) -> bool {
    let chars: Vec<char> = expr.trim_start().chars().collect();
    let is_start = |c: char| c.is_alphabetic() || c == '_' || c == '$';
    let is_part = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut index = 0;
    if !chars.first().copied().is_some_and(is_start) {
        return false;
    }
    while index < chars.len() && is_part(chars[index]) {
        index += 1;
    }
    loop {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        match chars.get(index) {
            Some('.') => {
                index += 1;
                while index < chars.len() && chars[index].is_whitespace() {
                    index += 1;
                }
                if !chars.get(index).copied().is_some_and(is_start) {
                    return false;
                }
                while index < chars.len() && is_part(chars[index]) {
                    index += 1;
                }
            }
            Some('[') => {
                let mut depth = 0;
                loop {
                    match chars.get(index) {
                        Some('[') => depth += 1,
                        Some(']') => {
                            depth -= 1;
                            if depth == 0 {
                                index += 1;
                                break;
                            }
                        }
                        Some(_) => {}
                        None => return false,
                    }
                    index += 1;
                }
            }
            Some('=') => return chars.get(index + 1) != Some(&'='),
            _ => return false,
        }
    }
}

fn page_command_body(method: &str, name: &str, result: &Value) -> Result<Map<String, Value>> {
    let mut body = Map::new();
    match method {
        "Page.navigate" => {
            if let Some(error) = result.get("errorText").and_then(Value::as_str) {
                bail!("{error}");
            }
        }
        "Runtime.evaluate" => {
            if let Some(details) = result.get("exceptionDetails") {
                let description =
                    details["exception"].get("description").and_then(Value::as_str).map(values::first_line);
                bail!("{}", description.unwrap_or_else(|| values::str_of(details, "text").to_string()));
            }
            let object = &result["result"];
            body.insert("name".into(), json!(name));
            body.insert("value".into(), json!(values::preview(object)));
            body.insert("type".into(), json!(values::type_name(object)));
            // the page runs on: there is no stop for a ref to live in
            body.insert("ref".into(), json!(0));
        }
        _ => {}
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_origin_of_an_address() {
        assert_eq!(super::origin_of("http://localhost:8080/main/login?ret=1"), Some("http://localhost:8080"));
        assert_eq!(super::origin_of("http://club.localhost:9092"), Some("http://club.localhost:9092"));
        assert_eq!(super::origin_of("http://club.localhost:9092#app=/"), Some("http://club.localhost:9092"));
        assert_eq!(super::origin_of("about:blank"), None);
    }

    use super::*;

    #[test]
    fn assignments() {
        assert!(is_assignment("a = 1"));
        assert!(is_assignment("order.name = \"Bob\""));
        assert!(is_assignment("a.b[0] = x"));
        assert!(is_assignment("a[b[1]].c=2"));
        assert!(!is_assignment("a == 1"));
        assert!(!is_assignment("a.b()"));
        assert!(!is_assignment("f(a = 1)"));
        assert!(!is_assignment("1 = 2"));
    }

    #[test]
    fn hit_counts() {
        let hit = Hit::parse("5").unwrap().unwrap();
        assert!(hit.matches(5) && !hit.matches(4) && !hit.matches(6));
        let hit = Hit::parse(">= 3").unwrap().unwrap();
        assert!(hit.matches(3) && hit.matches(9) && !hit.matches(2));
        let hit = Hit::parse("% 5").unwrap().unwrap();
        assert!(hit.matches(10) && !hit.matches(7));
        assert!(Hit::parse("").unwrap().is_none());
        assert!(Hit::parse("lots").is_err());
    }
}
