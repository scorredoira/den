//! The bridge against a real headless Chrome: a client of the protocol, as
//! Den is, debugs the fixture page (tests/fixture) served from 127.0.0.1.
//! Without Chrome these tests fail: set DEN_CHROME to its binary.

use std::{
    collections::{HashMap, VecDeque},
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use chrome::{Bridge, Options};
use serde_json::{Map, Value, json};

const WAIT: Duration = Duration::from_secs(20);

fn fixture() -> Result<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture");
    path.canonicalize().context("the fixture folder")
}

/// The 1-based line of `src/{file}` that ends with `// @{marker}`.
fn line(file: &str, marker: &str) -> Result<u64> {
    let path = fixture()?.join("src").join(file);
    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let wanted = format!("// @{marker}");
    let index = text
        .lines()
        .position(|line| line.trim_end().ends_with(&wanted))
        .with_context(|| format!("no {wanted} in {file}"))?;
    Ok(index as u64 + 1)
}

/// Serves the fixture's `www` folder on 127.0.0.1.
fn serve_fixture() -> Result<u16> {
    let root = fixture()?.join("www");
    let listener = TcpListener::bind("127.0.0.1:0").context("bind the fixture server")?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let root = root.clone();
            thread::spawn(move || {
                if let Err(err) = answer(stream, &root) {
                    eprintln!("fixture server: {err:#}");
                }
            });
        }
    });
    Ok(port)
}

fn answer(mut stream: TcpStream, root: &Path) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
    }
    let path = request.split(' ').nth(1).unwrap_or("/").split('?').next().unwrap_or("/");
    let path = if path == "/" { "/index.html" } else { path };
    let file = root.join(path.trim_start_matches('/'));
    let (status, body, kind) = match (path.contains(".."), std::fs::read(&file)) {
        (false, Ok(body)) => {
            let kind = match file.extension().and_then(|ext| ext.to_str()) {
                Some("html") => "text/html",
                Some("js") => "text/javascript",
                _ => "application/json",
            };
            ("200 OK", body, kind)
        }
        _ => ("404 Not Found", b"not found".to_vec(), "text/plain"),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&body)?;
    Ok(())
}

/// A client of the protocol, as Den is.
struct Client {
    stream: TcpStream,
    lines: Receiver<Result<Value, String>>,
    next_id: u64,
    events: VecDeque<Value>,
    responses: HashMap<u64, Value>,
}

impl Client {
    fn connect(port: u16) -> Result<Client> {
        let stream = TcpStream::connect(("127.0.0.1", port)).context("connect to the bridge")?;
        let reader = BufReader::new(stream.try_clone()?);
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in reader.lines() {
                let message = match line {
                    Ok(line) => serde_json::from_str(&line).map_err(|err| format!("not JSON: {err}: {line}")),
                    Err(err) => Err(err.to_string()),
                };
                if sender.send(message).is_err() {
                    return;
                }
            }
            let _ = sender.send(Err("the bridge closed the connection".into()));
        });
        let mut client = Client { stream, lines, next_id: 1, events: VecDeque::new(), responses: HashMap::new() };
        client.request("hello", json!({ "version": 1 }))?;
        Ok(client)
    }

    fn send(&mut self, cmd: &str, args: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        let mut message = match args {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        message.insert("id".into(), json!(id));
        message.insert("cmd".into(), json!(cmd));
        let mut line = Value::Object(message).to_string();
        line.push('\n');
        self.stream.write_all(line.as_bytes()).context("send to the bridge")?;
        Ok(id)
    }

    fn next_message(&mut self, deadline: Instant) -> Result<Option<Value>> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.lines.recv_timeout(left) {
            Ok(Ok(message)) => {
                if message.get("event").is_some() {
                    return Ok(Some(message));
                }
                let id = message.get("id").and_then(Value::as_u64).context("a response without id")?;
                self.responses.insert(id, message);
                Ok(None)
            }
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("the connection ended"),
        }
    }

    /// The response to a request sent before, ok or not.
    fn response(&mut self, id: u64) -> Result<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(response) = self.responses.remove(&id) {
                return Ok(response);
            }
            if Instant::now() >= deadline {
                bail!("no response to request {id}; events: {:?}", self.events);
            }
            if let Some(event) = self.next_message(deadline)? {
                self.events.push_back(event);
            }
        }
    }

    fn try_request(&mut self, cmd: &str, args: Value) -> Result<Value> {
        let id = self.send(cmd, args)?;
        self.response(id)
    }

    fn request(&mut self, cmd: &str, args: Value) -> Result<Value> {
        let response = self.try_request(cmd, args)?;
        if response["ok"] != json!(true) {
            bail!("{cmd} failed: {response}");
        }
        Ok(response)
    }

    fn error(&mut self, cmd: &str, args: Value) -> Result<String> {
        let response = self.try_request(cmd, args)?;
        ensure!(response["ok"] == json!(false), "{cmd} should fail: {response}");
        Ok(response["error"].as_str().unwrap_or("").to_string())
    }

    /// The first event, queued or coming, that `matches`.
    fn event(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Result<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(at) = self.events.iter().position(&matches) {
                return Ok(self.events.remove(at).unwrap_or_default());
            }
            if Instant::now() >= deadline {
                bail!("no {what}; events: {:?}", self.events);
            }
            if let Some(event) = self.next_message(deadline)? {
                self.events.push_back(event);
            }
        }
    }

    fn stopped(&mut self) -> Result<Value> {
        self.event("stop", |event| event["event"] == "stopped")
    }

    fn output(&mut self, text: &str) -> Result<Value> {
        self.event(&format!("output {text:?}"), |event| {
            event["event"] == "output" && event["text"].as_str().is_some_and(|line| line.contains(text))
        })
    }

    /// Fails if an event that `matches` comes within `wait`.
    fn no_event(&mut self, what: &str, wait: Duration, matches: impl Fn(&Value) -> bool) -> Result<()> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if let Some(event) = self.next_message(deadline)? {
                self.events.push_back(event);
            }
        }
        if let Some(event) = self.events.iter().find(|event| matches(event)) {
            bail!("unexpected {what}: {event}");
        }
        Ok(())
    }

    fn close(self) {
        if let Err(err) = self.stream.shutdown(Shutdown::Both) {
            eprintln!("close the client: {err}");
        }
    }
}

/// A headless Chrome with a profile of its own, the bridge, the fixture's
/// server and a connected client.
struct Session {
    client: Client,
    base: String,
    bridge: Option<Bridge>,
    profile: PathBuf,
}

impl Session {
    fn start() -> Result<Session> {
        Session::start_with(None)
    }

    fn start_with(url: Option<&str>) -> Result<Session> {
        Session::start_skipping(url, &[])
    }

    fn start_skipping(url: Option<&str>, inspect_skip: &[&str]) -> Result<Session> {
        static COUNT: AtomicU64 = AtomicU64::new(0);
        let profile = std::env::temp_dir().join(format!(
            "den-chrome-test-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::SeqCst)
        ));
        let port = serve_fixture()?;
        let base = format!("http://127.0.0.1:{port}");
        let options = Options {
            port: 0,
            url: url.map(|path| format!("{base}{path}")),
            root: fixture()?,
            headless: true,
            profile: Some(profile.clone()),
            hosts: vec!["localhost".into(), "127.0.0.1".into(), "*.localhost".into()],
            inspect_skip: inspect_skip.iter().map(|glob| glob.to_string()).collect(),
        };
        let bridge = match Bridge::start(options) {
            Ok(bridge) => bridge,
            Err(err) => {
                if profile.exists()
                    && let Err(remove) = std::fs::remove_dir_all(&profile)
                {
                    eprintln!("remove {}: {remove}", profile.display());
                }
                return Err(err.context("start the bridge (is Chrome installed? DEN_CHROME names its binary)"));
            }
        };
        let client = Client::connect(bridge.port())?;
        Ok(Session { client, base, bridge: Some(bridge), profile })
    }

    fn port(&self) -> Result<u16> {
        Ok(self.bridge.as_ref().context("the bridge ended")?.port())
    }

    fn navigate(&mut self, path: &str) -> Result<()> {
        let url = format!("{}{path}", self.base);
        self.client.request("navigate", json!({ "url": url }))?;
        Ok(())
    }

    fn breakpoints(&mut self, file: &str, list: Value) -> Result<Value> {
        let response = self.client.request("setBreakpoints", json!({ "file": file, "breakpoints": list }))?;
        Ok(response["breakpoints"].clone())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.bridge.take());
        if let Err(err) = std::fs::remove_dir_all(&self.profile) {
            eprintln!("remove {}: {err}", self.profile.display());
        }
    }
}

fn var<'a>(vars: &'a Value, name: &str) -> Result<&'a Value> {
    vars.as_array()
        .and_then(|vars| vars.iter().find(|var| var["name"] == name))
        .with_context(|| format!("no variable {name} in {vars}"))
}

fn expect_stop(stop: &Value, reason: &str, file: &str, line: u64) -> Result<()> {
    ensure!(
        stop["reason"] == reason && stop["file"] == file && stop["line"] == line,
        "expected a {reason} stop at {file}:{line}, got {stop}"
    );
    Ok(())
}

#[test]
fn the_first_line_must_be_hello() -> Result<()> {
    let mut session = Session::start()?;
    let port = session.port()?;
    for first in ["POST / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n", "{\"id\":1,\"cmd\":\"threads\"}\n"] {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(first.as_bytes())?;
        let mut buffer = Vec::new();
        let read = stream.read_to_end(&mut buffer).context("the bridge should close the connection")?;
        ensure!(read == 0, "the bridge answered a connection without hello: {:?}", String::from_utf8_lossy(&buffer));
    }
    // the client connected before is still the one
    let hello = session.client.request("hello", json!({ "version": 1 }))?;
    ensure!(hello["version"] == 1, "{hello}");
    ensure!(hello["cwd"] == fixture()?.to_string_lossy().as_ref(), "{hello}");
    ensure!(hello["waiting"] == false && hello["stopped"] == json!([]), "{hello}");
    let pages = session.client.request("pages", json!({}))?;
    ensure!(pages["pages"].as_array().is_some_and(|pages| !pages.is_empty()), "{pages}");
    let error = session.client.error("jump", json!({ "vm": 1, "line": 3 }))?;
    ensure!(error.contains("Chrome can't set the next statement"), "{error}");
    let error = session.client.error("stepBack", json!({}))?;
    ensure!(error.contains("unknown command"), "{error}");
    let error = session.client.error("hello", json!({ "version": 2 }))?;
    ensure!(error.contains("not supported"), "{error}");
    Ok(())
}

#[test]
fn breakpoints_before_load_values_and_eval() -> Result<()> {
    let mut session = Session::start()?;
    let main = line("app.ts", "main")?;
    let call = line("app.ts", "call")?;
    let placed = session.breakpoints("src/app.ts", json!([{ "line": main }, { "line": call }]))?;
    ensure!(placed == json!([{ "line": main }, { "line": call }]), "{placed}");
    session.navigate("/index.html")?;

    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    ensure!(stop["frames"][0]["function"] == "main", "{stop}");
    // a const before its declaration runs
    let order = var(&stop["locals"], "order")?;
    ensure!(order["value"] == "<value unavailable>" && order["ref"] == 0, "{order}");
    let vm = stop["vm"].as_u64().context("vm")?;
    let threads = session.client.request("threads", json!({}))?;
    ensure!(threads["stopped"] == json!([vm]), "{threads}");

    session.client.request("continue", json!({ "vm": vm }))?;
    session.client.event("resumed", |event| event["event"] == "resumed" && event["vm"] == vm)?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", call)?;

    // locals and their children
    let locals = &stop["locals"];
    let order = var(locals, "order")?;
    ensure!(order["type"] == "map" && order["ref"].as_u64() > Some(0), "{order}");
    ensure!(order["value"] == "{id: 3, name: \"Ann\", items: Array(3)}", "{order}");
    let counter = var(locals, "counter")?;
    ensure!(counter["type"] == "Counter", "{counter}");
    // its own fields, then its class's getter, run
    let fields = session.client.request("expand", json!({ "ref": counter["ref"] }))?["vars"].clone();
    let names: Vec<&str> = fields.as_array().context("vars")?.iter().filter_map(|field| field["name"].as_str()).collect();
    ensure!(names == ["name", "count", "doubled"], "{fields}");
    ensure!(var(&fields, "doubled")?["value"] == "2", "{fields}");
    let order_ref = order["ref"].clone();
    let children = session.client.request("expand", json!({ "ref": order_ref }))?["vars"].clone();
    ensure!(var(&children, "id")?["value"] == "3" && var(&children, "id")?["type"] == "int", "{children}");
    ensure!(var(&children, "name")?["value"] == "\"Ann\"", "{children}");
    let items = var(&children, "items")?.clone();
    ensure!(items["type"] == "array" && items["count"] == 3, "{items}");
    let elements = session.client.request("expand", json!({ "ref": items["ref"] }))?["vars"].clone();
    ensure!(elements.as_array().map(Vec::len) == Some(3) && elements[2]["value"] == "3", "{elements}");
    let page =
        session.client.request("expand", json!({ "ref": items["ref"], "start": 1, "count": 1 }))?["vars"].clone();
    ensure!(page.as_array().map(Vec::len) == Some(1) && page[0]["name"] == "1", "{page}");

    // the module's variables
    let globals = stop["globals"].as_u64().context("globals")?;
    ensure!(globals > 0, "{stop}");
    let module = session.client.request("expand", json!({ "ref": globals }))?["vars"].clone();
    ensure!(var(&module, "loads")?["value"] == "1", "{module}");
    let tags = var(&module, "tags")?.clone();
    ensure!(tags["count"] == 2, "{tags}");
    let entries = session.client.request("expand", json!({ "ref": tags["ref"] }))?["vars"].clone();
    ensure!(var(&entries, "\"b\"")?["value"] == "2", "{entries}");

    // eval: values, side effects refused, assignments
    let length = session.client.request("eval", json!({ "vm": vm, "frame": 0, "expr": "order.items.length" }))?;
    ensure!(length["value"] == "3" && length["name"] == "order.items.length", "{length}");
    let error = session.client.error("eval", json!({ "vm": vm, "frame": 0, "expr": "counter.bump()" }))?;
    ensure!(error.contains("side effects"), "{error}");
    let assigned = session.client.request("eval", json!({ "vm": vm, "frame": 0, "expr": "order.name = \"Bob\"" }))?;
    ensure!(assigned["value"] == "\"Bob\"", "{assigned}");
    let error = session.client.error("eval", json!({ "vm": vm, "frame": 0, "expr": "missing + 1" }))?;
    ensure!(error.contains("missing is not defined"), "{error}");
    // the frame of the caller: the bundle's top level
    let frame = session.client.request("frame", json!({ "vm": vm, "frame": 1 }))?;
    ensure!(frame["locals"].is_array(), "{frame}");

    // console output, with the assignment made, at its line
    session.client.request("continue", json!({ "vm": vm }))?;
    let output = session.client.output("total 6 Bob")?;
    ensure!(output["file"] == "src/app.ts" && output["line"] == line("app.ts", "log")?, "{output}");
    Ok(())
}

#[test]
fn steps_stay_on_typescript_lines() -> Result<()> {
    let mut session = Session::start()?;
    let call = line("app.ts", "call")?;
    session.breakpoints("src/app.ts", json!([{ "line": call }]))?;
    session.navigate("/index.html")?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", call)?;
    let vm = stop["vm"].clone();

    session.client.request("next", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "log")?)?;
    session.client.request("continue", json!({ "vm": vm }))?;

    // a reload stops at the breakpoint again
    session.client.request("reload", json!({}))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", call)?;
    let vm = stop["vm"].clone();

    session.client.request("stepIn", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "sum")?)?;
    ensure!(stop["frames"][0]["function"] == "total", "{stop}");

    session.client.request("next", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "sum")? + 1)?;
    session.client.request("next", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "loop")?)?;

    session.client.request("stepIn", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/util.ts", line("util.ts", "add")?)?;
    ensure!(stop["frames"][0]["function"] == "add" && stop["frames"][1]["function"] == "total", "{stop}");
    session.client.request("next", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/util.ts", line("util.ts", "addReturn")?)?;

    // out of add: the next line of total
    session.client.request("stepOut", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    // the loop's header, for the next item
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "loop")? - 1)?;
    ensure!(stop["frames"][0]["function"] == "total", "{stop}");

    // out of total: the line after the call
    session.client.request("stepOut", json!({ "vm": vm }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "log")?)?;
    ensure!(stop["frames"][0]["function"] == "main", "{stop}");
    session.client.request("continue", json!({ "vm": vm }))?;
    Ok(())
}

#[test]
fn conditions_hit_counts_and_logpoints() -> Result<()> {
    let mut session = Session::start()?;
    let spin = line("app.ts", "spin")?;
    session.navigate("/index.html")?;
    // the page has loaded once `main` printed
    session.client.output("total 6 Ann")?;

    session.breakpoints("src/app.ts", json!([{ "line": spin, "condition": "i == 7" }]))?;
    let id = session.client.send("evaluate", json!({ "expr": "app.spin(20)" }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", spin)?;
    let vm = stop["vm"].clone();
    let i = session.client.request("eval", json!({ "vm": vm, "frame": 0, "expr": "i" }))?;
    ensure!(i["value"] == "7", "{i}");
    session.client.request("continue", json!({ "vm": vm }))?;
    let result = session.client.response(id)?;
    ensure!(result["ok"] == true && result["value"] == "190", "{result}");

    // cleared first, the breakpoint counts from zero
    session.breakpoints("src/app.ts", json!([]))?;
    session.breakpoints("src/app.ts", json!([{ "line": spin, "hit": "3" }]))?;
    let id = session.client.send("evaluate", json!({ "expr": "app.spin(20)" }))?;
    let stop = session.client.stopped()?;
    let i = session.client.request("eval", json!({ "vm": stop["vm"], "frame": 0, "expr": "i" }))?;
    ensure!(i["value"] == "2", "the 3rd hit is i == 2: {i}");
    session.client.request("continue", json!({ "vm": stop["vm"] }))?;
    let result = session.client.response(id)?;
    ensure!(result["value"] == "190", "{result}");
    session.client.no_event("stop", Duration::from_millis(300), |event| event["event"] == "stopped")?;

    session.breakpoints("src/app.ts", json!([{ "line": spin, "log": "i is {i}, n {n}" }]))?;
    let id = session.client.send("evaluate", json!({ "expr": "app.spin(3)" }))?;
    let result = session.client.response(id)?;
    ensure!(result["value"] == "3", "{result}");
    for i in 0..3 {
        let output = session.client.output(&format!("i is {i}, n 3"))?;
        ensure!(
            output["text"] == format!("i is {i}, n 3\n") && output["file"] == "src/app.ts" && output["line"] == spin,
            "{output}"
        );
    }
    session.client.no_event("stop", Duration::from_millis(300), |event| event["event"] == "stopped")?;
    Ok(())
}

#[test]
fn exceptions_uncaught_and_all() -> Result<()> {
    let mut session = Session::start()?;
    let throw = line("util.ts", "throw")?;
    session.navigate("/index.html")?;
    session.client.output("total 6 Ann")?;

    session.client.request("setExceptions", json!({ "uncaught": true, "all": true }))?;
    let id = session.client.send("evaluate", json!({ "expr": "app.throwCaught()" }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "exception", "src/util.ts", throw)?;
    ensure!(stop["exception"]["message"] == "Error: caught one", "{stop}");
    let stack = stop["exception"]["stack"].as_str().unwrap_or("");
    ensure!(stack.contains(&format!("src/util.ts:{throw}")), "the stack names the TS lines: {stack}");
    session.client.request("continue", json!({ "vm": stop["vm"] }))?;
    let result = session.client.response(id)?;
    ensure!(result["value"] == "\"Error: caught one\"", "{result}");

    session.client.request("setExceptions", json!({ "uncaught": true, "all": false }))?;
    let result = session.client.request("evaluate", json!({ "expr": "app.throwCaught()" }))?;
    ensure!(result["value"] == "\"Error: caught one\"", "{result}");
    session.client.no_event("stop", Duration::from_millis(300), |event| event["event"] == "stopped")?;
    session.client.request("evaluate", json!({ "expr": "app.throwLater()" }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "exception", "src/util.ts", throw)?;
    ensure!(stop["exception"]["message"] == "Error: uncaught one", "{stop}");
    session.client.request("continue", json!({ "vm": stop["vm"] }))?;
    // seen at the stop: not printed again
    session.client.no_event("output", Duration::from_millis(500), |event| {
        event["event"] == "output" && event["text"].as_str().is_some_and(|text| text.contains("uncaught one"))
    })?;

    session.client.request("setExceptions", json!({ "uncaught": false, "all": false }))?;
    session.client.request("evaluate", json!({ "expr": "app.throwLater()" }))?;
    let output = session.client.output("Uncaught Error: uncaught one")?;
    ensure!(output["file"] == "src/util.ts" && output["line"] == throw, "{output}");
    session.client.no_event("stop", Duration::from_millis(300), |event| event["event"] == "stopped")?;
    Ok(())
}

#[test]
fn run_to_and_pause() -> Result<()> {
    let mut session = Session::start()?;
    let call = line("app.ts", "call")?;
    session.breakpoints("src/app.ts", json!([{ "line": call }]))?;
    session.navigate("/index.html")?;
    let stop = session.client.stopped()?;
    let vm = stop["vm"].clone();
    session.client.request("runTo", json!({ "vm": vm, "file": "src/app.ts", "line": line("app.ts", "after")? }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "step", "src/app.ts", line("app.ts", "after")?)?;
    session.client.request("continue", json!({ "vm": vm }))?;
    session.breakpoints("src/app.ts", json!([]))?;

    for pause in [json!({ "vm": vm }), json!({})] {
        let id = session.client.send("evaluate", json!({ "expr": "app.spin(5e9)" }))?;
        thread::sleep(Duration::from_millis(300));
        session.client.request("pause", pause)?;
        let stop = session.client.stopped()?;
        ensure!(
            stop["reason"] == "pause" && stop["file"] == "src/app.ts" && stop["frames"][0]["function"] == "spin",
            "{stop}"
        );
        // ends the loop: an assignment to a local
        session.client.request("eval", json!({ "vm": stop["vm"], "frame": 0, "expr": "n = 0" }))?;
        session.client.request("continue", json!({ "vm": stop["vm"] }))?;
        let result = session.client.response(id)?;
        ensure!(result["ok"] == true, "{result}");
    }
    Ok(())
}

#[test]
fn a_client_that_goes_resumes_everything() -> Result<()> {
    let mut session = Session::start()?;
    let main = line("app.ts", "main")?;
    session.breakpoints("src/app.ts", json!([{ "line": main }]))?;
    session.navigate("/index.html")?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;

    // a new client replaces this one and finds the stop
    let port = session.port()?;
    let mut second = Client::connect(port)?;
    let hello = second.request("hello", json!({ "version": 1 }))?;
    ensure!(hello["stopped"].as_array().map(Vec::len) == Some(1), "{hello}");

    // when it goes, the page runs on and its breakpoints are gone
    second.close();
    thread::sleep(Duration::from_millis(500));
    let mut third = Client::connect(port)?;
    let hello = third.request("hello", json!({ "version": 1 }))?;
    ensure!(hello["stopped"] == json!([]), "{hello}");
    let loads = third.request("evaluate", json!({ "expr": "app.loads()" }))?;
    ensure!(loads["value"] == "1", "{loads}");
    // a promise is waited for: what it resolves to is the value
    let later = third.request("evaluate", json!({ "expr": "new Promise(done => setTimeout(() => done(41 + 1), 50))" }))?;
    ensure!(later["value"] == "42", "{later}");
    third.request("reload", json!({}))?;
    third.output("total 6 Ann")?;
    third.no_event("stop", Duration::from_millis(300), |event| event["event"] == "stopped")?;
    Ok(())
}

#[test]
fn a_linked_source_map_and_a_new_tab() -> Result<()> {
    let mut session = Session::start()?;
    let main = line("app.ts", "main")?;
    session.breakpoints("src/app.ts", json!([{ "line": main }]))?;
    session.navigate("/linked.html")?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    ensure!(stop["frames"][0]["function"] == "main", "{stop}");
    let first = stop["vm"].clone();
    session.client.request("continue", json!({ "vm": first }))?;

    // a new tab is a VM of its own, held until its breakpoints are in place
    session.client.request("evaluate", json!({ "expr": "window.open('/index.html') !== null" }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    ensure!(stop["vm"] != first, "{stop}");
    let pages = session.client.request("pages", json!({}))?;
    let urls: Vec<&str> =
        pages["pages"].as_array().into_iter().flatten().filter_map(|page| page["url"].as_str()).collect();
    ensure!(
        urls.iter().any(|url| url.ends_with("/index.html")) && urls.iter().any(|url| url.ends_with("/linked.html")),
        "{pages}"
    );
    session.client.request("continue", json!({ "vm": stop["vm"] }))?;
    Ok(())
}

#[test]
fn the_url_opens_on_run() -> Result<()> {
    let mut session = Session::start_with(Some("/index.html"))?;
    let main = line("app.ts", "main")?;
    let hello = session.client.request("hello", json!({ "version": 1 }))?;
    ensure!(hello["waiting"] == true, "{hello}");
    // a line without code moves to the next one with code
    let placed = session.breakpoints("src/app.ts", json!([{ "line": main - 2 }]))?;
    ensure!(placed == json!([{ "line": main - 2 }]), "nothing loaded yet: {placed}");
    session.client.request("run", json!({ "entry": true }))?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    // now loaded, the result says where it went
    let placed = session.breakpoints("src/app.ts", json!([{ "line": main - 2 }, { "line": 1000 }]))?;
    ensure!(placed[0] == json!({ "line": main }) && placed[1]["error"].is_string(), "{placed}");
    Ok(())
}

#[test]
fn a_new_session_loads_the_page_again() -> Result<()> {
    let mut first = Session::start_with(Some("/index.html"))?;
    let main = line("app.ts", "main")?;
    first.client.request("hello", json!({ "version": 1 }))?;
    first.breakpoints("src/app.ts", json!([{ "line": main }]))?;
    first.client.request("run", json!({}))?;
    let stop = first.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    first.client.request("continue", json!({ "vm": stop["vm"] }))?;

    // a second bridge on the same Chrome, whose tab already shows the page: its run loads it
    // again, so the code that runs at load stops
    let options = Options {
        port: 0,
        url: Some(format!("{}/index.html", first.base)),
        root: fixture()?,
        headless: true,
        profile: Some(first.profile.clone()),
        hosts: vec!["localhost".into(), "127.0.0.1".into(), "*.localhost".into()],
        inspect_skip: Vec::new(),
    };
    let second = Bridge::start(options)?;
    let mut client = Client::connect(second.port())?;
    client.request("hello", json!({ "version": 1 }))?;
    client.request("setBreakpoints", json!({ "file": "src/app.ts", "breakpoints": [{ "line": main }] }))?;
    client.request("run", json!({}))?;
    let stop = client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", main)?;
    client.request("continue", json!({ "vm": stop["vm"] }))?;
    client.close();
    drop(second);

    // another address of the same site goes in that same tab, not a new one per session
    let options = Options {
        port: 0,
        url: Some(format!("{}/linked.html", first.base)),
        root: fixture()?,
        headless: true,
        profile: Some(first.profile.clone()),
        hosts: vec!["localhost".into(), "127.0.0.1".into(), "*.localhost".into()],
        inspect_skip: Vec::new(),
    };
    let third = Bridge::start(options)?;
    let mut client = Client::connect(third.port())?;
    client.request("hello", json!({ "version": 1 }))?;
    client.request("run", json!({}))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pages = client.request("pages", json!({}))?;
        let list = pages["pages"].as_array().context("pages")?;
        if list.len() == 1 && list[0]["url"].as_str().is_some_and(|url| url.ends_with("/linked.html")) {
            break;
        }
        ensure!(Instant::now() < deadline, "the site's tab did not go to the address: {pages}");
        thread::sleep(Duration::from_millis(100));
    }
    client.close();
    drop(third);
    Ok(())
}

#[test]
fn an_element_lists_its_accessors() -> Result<()> {
    let mut session = Session::start()?;
    let style = line("app.ts", "style")?;
    session.breakpoints("src/app.ts", json!([{ "line": style }]))?;
    session.navigate("/index.html")?;
    let stop = session.client.stopped()?;
    expect_stop(&stop, "breakpoint", "src/app.ts", style)?;
    let vm = stop["vm"].as_u64().context("vm")?;
    let element = var(&stop["locals"], "box")?.clone();
    ensure!(element["value"] == "div#box" && element["ref"].as_u64() > Some(0), "{element}");
    let all = session.client.request("expand", json!({ "ref": element["ref"] }))?["vars"].clone();
    let all = all.as_array().context("vars")?.clone();
    ensure!(all.len() > 100, "a div has its accessors: {}", all.len());
    let tag = all.iter().find(|var| var["name"] == "tagName").context("tagName")?;
    ensure!(tag["value"] == "\"DIV\"", "{tag}");
    let id = all.iter().find(|var| var["name"] == "id").context("id")?;
    ensure!(id["value"] == "\"box\"", "{id}");
    let names: Vec<&str> = all.iter().filter_map(|var| var["name"].as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    ensure!(names == sorted, "accessors come sorted: {names:?}");
    // a page of them, as Den asks for them
    let page = session.client.request("expand", json!({ "ref": element["ref"], "start": 10, "count": 5 }))?["vars"].clone();
    ensure!(page.as_array().map(Vec::len) == Some(5) && page[0]["name"] == all[10]["name"], "{page}");
    session.client.request("continue", json!({ "vm": vm }))?;
    Ok(())
}

#[test]
fn inspecting_an_element_reveals_the_line_that_made_it() -> Result<()> {
    let create = line("util.ts", "create")?;
    let caller = line("app.ts", "box")?;

    // the helper's own line, with nothing skipped
    let mut session = Session::start()?;
    session.navigate("/index.html")?;
    session.client.output("total 6 Ann")?;
    let picked = session.client.request("inspectNode", json!({ "expr": "document.getElementById(\"box\")" }))?;
    ensure!(picked["file"] == "src/util.ts" && picked["line"] == create, "{picked}");
    let stack = picked["stack"].as_array().context("stack")?;
    ensure!(stack.iter().any(|frame| frame.as_str().is_some_and(|frame| frame.starts_with(&format!("src/app.ts:{caller} ")))), "{picked}");
    let reveal = session.client.event("reveal", |event| event["event"] == "reveal")?;
    ensure!(reveal["file"] == "src/util.ts" && reveal["line"] == create, "{reveal}");
    // text the HTML parser made: its nearest ancestor a script made
    let parsed = session.client.request("inspectNode", json!({ "expr": "document.getElementById(\"box\").firstChild" }))?;
    ensure!(parsed["file"] == "src/util.ts", "{parsed}");
    let none = session.client.request("inspectNode", json!({ "expr": "document.body" }))?;
    ensure!(none.get("file").is_none(), "{none}");
    let error = session.client.error("inspectNode", json!({ "expr": "1 + 1" }))?;
    ensure!(error.contains("is not an element"), "{error}");
    drop(session);

    // the helper skipped: the line that asked for it
    let mut session = Session::start_skipping(None, &["src/util.ts"])?;
    session.navigate("/index.html")?;
    session.client.output("total 6 Ann")?;
    let picked = session.client.request("inspectNode", json!({ "expr": "document.getElementById(\"box\")" }))?;
    ensure!(picked["file"] == "src/app.ts" && picked["line"] == caller, "{picked}");

    // the real pick: inspect on, a click on the element
    session.client.request("inspect", json!({ "on": true }))?;
    let center = session.client.request(
        "evaluate",
        json!({ "expr": "(() => { const r = document.getElementById(\"box\").getBoundingClientRect(); return JSON.stringify([r.x + r.width / 2, r.y + r.height / 2]) })()" }),
    )?;
    let center: Vec<f64> = serde_json::from_str(center["value"].as_str().context("center")?.trim_matches('"'))?;
    session.client.request("click", json!({ "x": center[0], "y": center[1] }))?;
    let reveal = session.client.event("reveal", |event| event["event"] == "reveal")?;
    ensure!(reveal["file"] == "src/app.ts" && reveal["line"] == caller, "{reveal}");
    Ok(())
}

#[test]
fn an_alt_click_in_the_page_reveals_the_line_that_made_the_element() -> Result<()> {
    let caller = line("app.ts", "box")?;
    let mut session = Session::start_skipping(None, &["src/util.ts"])?;
    session.navigate("/index.html")?;
    session.client.output("total 6 Ann")?;
    let center = session.client.request(
        "evaluate",
        json!({ "expr": "(() => { const r = document.getElementById(\"box\").getBoundingClientRect(); return JSON.stringify([r.x + r.width / 2, r.y + r.height / 2]) })()" }),
    )?;
    let center: Vec<f64> = serde_json::from_str(center["value"].as_str().context("center")?.trim_matches('"'))?;

    // a plain click is the page's, and reveals nothing
    session.client.request("click", json!({ "x": center[0], "y": center[1] }))?;
    session.client.no_event("reveal", Duration::from_millis(500), |event| event["event"] == "reveal")?;
    let clicks = session.client.request("evaluate", json!({ "expr": "app.boxClicks()" }))?;
    ensure!(clicks["value"] == "1", "{clicks}");

    // an Alt+click reveals the line that made the element, and the page never sees it
    session.client.request("click", json!({ "x": center[0], "y": center[1], "modifiers": 1 }))?;
    let reveal = session.client.event("reveal", |event| event["event"] == "reveal")?;
    ensure!(reveal["file"] == "src/app.ts" && reveal["line"] == caller, "{reveal}");
    let clicks = session.client.request("evaluate", json!({ "expr": "app.boxClicks()" }))?;
    ensure!(clicks["value"] == "1", "the page saw the Alt+click: {clicks}");
    Ok(())
}
