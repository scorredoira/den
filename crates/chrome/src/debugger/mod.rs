//! The bridge: Den's debug protocol v1 on one side, CDP on the other. One
//! thread runs it, taking the client's requests and Chrome's events in the
//! order they come; only the commands that run page code (`navigate`,
//! `reload`, `evaluate`) answer from a thread of their own, since the code
//! they run can stop at a breakpoint.

mod breakpoints;
mod inspect;
mod pages;
mod variables;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender},
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use crate::{
    cdp::{self, Cdp},
    fetch,
    sourcemap::{Original, Resolved, SourceMap},
    values,
};
use pages::Maps;

const VERSION: u64 = 1;

/// How many steps a step takes at most to leave code without a source map,
/// or a line made of several generated positions, before it stops anyway.
const MAX_STEPS: u32 = 300;

/// How often `--url`'s server is tried until it listens.
const LISTEN_POLL: Duration = Duration::from_millis(100);

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
    /// The server of `--url` listens: the page opens.
    Open(String),
    Shutdown,
}

/// The binding the page calls on an Alt+click, and the script that listens
/// for it: in the capture phase, before the page's own listeners, the press,
/// the release and the click with Alt alone are kept from the page, and the
/// click keeps its element where the bridge reads it.
const PICK_BINDING: &str = "__denInspect";
const ALT_CLICK_SCRIPT: &str = r#"(() => {
  if (window.__denAltClick) return;
  Object.defineProperty(window, "__denAltClick", { value: true });
  const alt = (e) => e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
  const keep = (e) => { if (alt(e)) { e.preventDefault(); e.stopImmediatePropagation(); } };
  for (const type of ["pointerdown", "mousedown", "pointerup", "mouseup", "auxclick", "dblclick"]) {
    addEventListener(type, keep, true);
  }
  addEventListener("click", (e) => {
    if (!alt(e)) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    Object.defineProperty(window, "__denPicked", { value: e.target, configurable: true, writable: true });
    __denInspect("");
  }, true);
})()"#;

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
    /// Globs of the files an inspected element's creation stack passes over.
    pub inspect_skip: Vec<String>,
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
    inputs: Sender<Input>,
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
    /// Source maps already parsed.
    maps: Maps,
    resolved: Resolved,
    /// The tabs that have shown the app (a debugged host), open or not.
    app_tabs: HashSet<String>,
    /// The app showed, and every tab of it has closed since: the bridge ends.
    app_closed: bool,
}

impl Core {
    pub fn new(cdp: Arc<Cdp>, inputs: Sender<Input>, out: Out, settings: Settings) -> Core {
        let root_text = settings.root.to_string_lossy().to_string();
        Core {
            cdp,
            inputs,
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
            maps: Maps::default(),
            resolved: Resolved::default(),
            app_tabs: HashSet::new(),
            app_closed: false,
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
                Input::Open(url) => {
                    if let Err(err) = self.open_url(&url) {
                        self.report(&format!("open {url}: {err:#}"));
                    }
                }
                Input::Shutdown => {
                    self.close_app();
                    return Ok(());
                }
            }
            if self.app_closed {
                self.close_app();
                return Ok(());
            }
        }
        Ok(())
    }

    /// The bridge ends: so does the Chrome it launched, every window of it;
    /// in a Chrome it found running, the tabs of the app close.
    fn close_app(&mut self) {
        // Chrome may go before it answers: nothing to tell then
        if self.settings.launched {
            let _ = self.cdp.call(None, "Browser.close", json!({}));
            return;
        }
        for target in self.app_tabs.drain() {
            let _ = self.cdp.call(None, "Target.closeTarget", json!({ "targetId": target }));
        }
    }

    /// A tab showed `url`: one of the app's, if its host is debugged.
    fn note_tab(&mut self, target: &str, url: &str) {
        if self.debugged_url(url) {
            self.app_tabs.insert(target.to_string());
        }
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
        if matches!(cmd.as_str(), "navigate" | "reload" | "evaluate" | "click") {
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
                // opening --url, once the breakpoints are in place and its
                // server listens: one started with this bridge listens later
                if let Some(url) = self.settings.url.take() {
                    self.open_when_listening(url)?;
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
            "inspect" => {
                let on = request.get("on").and_then(Value::as_bool).unwrap_or(false);
                self.set_inspecting(on)?;
            }
            "inspectNode" => {
                let vm = self.page_arg(request)?;
                let expr = request.get("expr").and_then(Value::as_str).context("expr is missing")?;
                let node = self.node_of(vm, expr)?;
                let picked = self.pick(vm, node)?;
                if let Value::Object(picked) = picked {
                    body = picked;
                }
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

    fn open_when_listening(&self, url: String) -> Result<()> {
        let address = fetch::address(&url)?;
        let inputs = self.inputs.clone();
        thread::Builder::new()
            .name("chrome-open".into())
            .spawn(move || {
                while TcpStream::connect_timeout(&address, LISTEN_POLL).is_err() {
                    thread::sleep(LISTEN_POLL);
                }
                // the receiver is gone only when the bridge has ended
                let _ = inputs.send(Input::Open(url));
            })
            .context("start waiting for the page's server")?;
        Ok(())
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

    /// `navigate`, `reload`, `evaluate` and `click`: they run page code, which
    /// can stop at a breakpoint, so they wait for Chrome on a thread of their own.
    fn page_command(&mut self, generation: u64, cmd: &str, id: Value, request: &Value) -> Result<()> {
        let vm = self.page_arg(request)?;
        let session = self.pages.get(&vm).map(|page| page.session.clone()).with_context(|| format!("no vm {vm}"))?;
        let mut before: Option<(&'static str, Value)> = None;
        let (method, params, name) = match cmd {
            "navigate" => {
                let url = request.get("url").and_then(Value::as_str).context("url is missing")?;
                ("Page.navigate", json!({ "url": url }), String::new())
            }
            "reload" => ("Page.reload", json!({}), String::new()),
            "click" => {
                let x = request.get("x").and_then(Value::as_f64).context("x is missing")?;
                let y = request.get("y").and_then(Value::as_f64).context("y is missing")?;
                // the press, then the release, which this answers
                // Alt 1, Ctrl 2, Meta 4, Shift 8, as Input.dispatchMouseEvent takes them
                let modifiers = request.get("modifiers").and_then(Value::as_i64).unwrap_or(0);
                let press = json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1, "modifiers": modifiers });
                before = Some(("Input.dispatchMouseEvent", press));
                let release = json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1, "modifiers": modifiers });
                ("Input.dispatchMouseEvent", release, String::new())
            }
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
                let result = match before {
                    Some((method, params)) => cdp.call_timeout(Some(&session), method, params, None),
                    None => Ok(Value::Null),
                };
                let result = result.and_then(|_| cdp.call_timeout(Some(&session), method, params, None));
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

}

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
                bail!("{}", values::exception_text(details));
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
