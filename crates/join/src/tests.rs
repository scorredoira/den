//! join over fake programs: this test binary run again as each of them
//! (`fake_program`), speaking the protocol as a program would.

use std::{
    io::{BufRead, BufReader},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    thread::JoinHandle,
    time::Duration,
};

use serde_json::{Value, json};

use super::*;

/// A fake program: listens on `JOIN_FAKE_PORT`, reports `JOIN_FAKE_CWD`,
/// knows only the file `JOIN_FAKE_FILE`, and answers so a test can tell who
/// answered and what numbers it got. With `JOIN_FAKE_EXIT` it ends at once
/// with that code, after a line on stderr. Run only as a child of a test.
#[test]
#[ignore]
fn fake_program() {
    let Ok(port) = std::env::var("JOIN_FAKE_PORT") else {
        return;
    };
    let name = std::env::var("JOIN_FAKE_NAME").unwrap();
    if let Ok(code) = std::env::var("JOIN_FAKE_EXIT") {
        eprintln!("{name}: does not compile");
        std::process::exit(code.parse().unwrap());
    }
    // started only once the program before listens
    if let Ok(before) = std::env::var("JOIN_FAKE_AFTER")
        && TcpStream::connect(("127.0.0.1", before.parse::<u16>().unwrap())).is_err()
    {
        eprintln!("{name}: started before the program before listened");
        std::process::exit(4);
    }
    if let Ok(delay) = std::env::var("JOIN_FAKE_DELAY") {
        thread::sleep(Duration::from_millis(delay.parse().unwrap()));
    }
    let cwd = std::env::var("JOIN_FAKE_CWD").unwrap();
    let file = std::env::var("JOIN_FAKE_FILE").unwrap();
    let inspects = std::env::var("JOIN_FAKE_INSPECT").is_ok();
    // a test's connection, which the system gives a port of its own, may
    // hold this one a moment
    let port: u16 = port.parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => break listener,
            Err(error) if error.kind() == ErrorKind::AddrInUse && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => panic!("{name}: port {port}: {error}"),
        }
    };
    for conn in listener.incoming() {
        let conn = conn.unwrap();
        let mut writer = conn.try_clone().unwrap();
        let mut send = move |value: Value| {
            writer.write_all(format!("{value}\n").as_bytes()).unwrap();
        };
        for line in BufReader::new(conn).lines() {
            let Ok(line) = line else { break };
            let request: Value = serde_json::from_str(&line).unwrap();
            let id = request["id"].clone();
            let mut answer = match request["cmd"].as_str().unwrap() {
                "hello" => json!({
                    "version": 1, "cwd": cwd, "waiting": true, "running": 1,
                    "stopped": [{"vm": 1, "reason": "breakpoint", "file": file, "line": 3,
                        "locals": [{"name": "a", "value": "{}", "ref": 3}], "globals": 4}],
                    "page": format!("http://localhost:1/{name}"),
                }),
                "threads" => json!({"running": 1, "stopped": [1]}),
                "setBreakpoints" => {
                    let lines: Vec<Value> = request["breakpoints"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|bp| {
                            if request["file"] == file.as_str() {
                                json!({"line": bp["line"]})
                            } else {
                                json!({"line": bp["line"], "error": format!("{name} doesn't load it")})
                            }
                        })
                        .collect();
                    json!({"breakpoints": lines})
                }
                "expand" => json!({"vars": [{"name": "x", "value": format!("{name} ref {}", request["ref"]), "ref": 5}]}),
                "eval" if request["expr"] == "die" => std::process::exit(0),
                "eval" => json!({"name": request["expr"], "value": format!("{name} vm {}", request["vm"]), "ref": 0}),
                "pause" => {
                    send(json!({"event": "stopped", "vm": 2, "reason": "pause", "locals": [], "globals": 6}));
                    json!({})
                }
                "inspect" if inspects => json!({"inspecting": name}),
                "run" | "setExceptions" => json!({}),
                other => json!({"ok": false, "error": format!("unknown command {other}")}),
            };
            if answer.get("ok").is_none() {
                answer["ok"] = json!(true);
            }
            answer["id"] = id;
            send(answer);
        }
    }
}

/// A free port no other test has: tests run at once, and a port one frees
/// could be given to another before its program listens on it.
fn port() -> u16 {
    static GIVEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    loop {
        let port = free_port().unwrap().local_addr().unwrap().port();
        let mut given = GIVEN.lock().unwrap();
        if !given.contains(&port) {
            given.push(port);
            return port;
        }
    }
}

/// A fake program named `name`.
fn fake(name: &str, cwd: &str, file: &str, extra: &[(&str, &str)]) -> Program {
    let port = port();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "tests::fake_program", "--ignored", "--nocapture", "--test-threads=1"])
        .env("JOIN_FAKE_PORT", port.to_string())
        .env("JOIN_FAKE_NAME", name)
        .env("JOIN_FAKE_CWD", cwd)
        .env("JOIN_FAKE_FILE", file);
    for (key, value) in extra {
        command.env(key, value);
    }
    Program { name: name.to_string(), port, command }
}

/// join serving `programs`, stopped by its flag.
struct Running {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
    ports: Vec<u16>,
}

impl Running {
    fn start(programs: Vec<Program>, page: bool) -> Self {
        let port = port();
        let stop = Arc::new(AtomicBool::new(false));
        let ports = programs.iter().map(|program| program.port).collect();
        let flag = stop.clone();
        let thread = thread::spawn(move || serve(programs, port, page, &flag));
        Self { port, stop, thread: Some(thread), ports }
    }

    /// A client that said hello, and join's answer. Until join listens,
    /// a connection may reach a listener another test opens a moment on
    /// that port (to find a free one), which closes it: it is tried again.
    fn client(&mut self) -> (Client, Value) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(mut client) = self.connect() {
                let id = client.next + 1;
                let sent = write_line(&client.stream, &json!({"id": id, "cmd": "hello", "version": 1}));
                client.next = id;
                if sent.is_ok()
                    && let Some(answer) = client.read()
                {
                    assert_eq!(answer["id"], id, "{answer}");
                    return (client, answer);
                }
            }
            if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                panic!("join ended: {:?}", self.thread.take().map(|thread| thread.join()));
            }
            assert!(Instant::now() < deadline, "join never listened");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// A connection to join, not to itself (see `Kids::wait_listening`).
    fn connect(&self) -> Option<Client> {
        let stream = TcpStream::connect(loopback(self.port)).ok()?;
        if stream.local_addr().ok()? == stream.peer_addr().ok()? {
            return None;
        }
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
        let reader = stream.try_clone().ok()?;
        Some(Client { reader: BufReader::new(reader), stream, next: 0 })
    }

    fn stop(self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        self.wait()
    }

    /// What join ended with.
    fn wait(mut self) -> Result<()> {
        self.thread.take().unwrap().join().unwrap()
    }
}

/// A test that fails leaves no programs running.
impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // the test failed already: what join ended with adds nothing
            thread.join().ok();
        }
    }
}

struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    next: u64,
}

impl Client {
    fn send(&mut self, cmd: &str, args: Value) -> u64 {
        self.next += 1;
        let mut request = args;
        request["id"] = json!(self.next);
        request["cmd"] = json!(cmd);
        if let Err(error) = write_line(&self.stream, &request) {
            panic!("sending {request}: {error}");
        }
        self.next
    }

    /// The next message; None once the connection is closed or broken.
    fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => None,
            Err(_) => None,
            Ok(_) => Some(serde_json::from_str(&line).unwrap()),
        }
    }

    /// The answer to request `id`, and the events before it.
    fn answer(&mut self, id: u64) -> (Value, Vec<Value>) {
        let mut events = Vec::new();
        loop {
            let Some(message) = self.read() else {
                panic!("no answer to {id}; the events before: {events:?}");
            };
            if message.get("event").is_some() {
                events.push(message);
            } else {
                assert_eq!(message["id"], id, "{message}");
                return (message, events);
            }
        }
    }

    fn ask(&mut self, cmd: &str, args: Value) -> Value {
        let id = self.send(cmd, args);
        self.answer(id).0
    }

    fn event(&mut self, name: &str) -> Value {
        loop {
            let message = self.read().expect("an event");
            if message["event"] == name {
                return message;
            }
        }
    }
}

fn two(page: bool) -> Running {
    Running::start(
        vec![
            fake("server", "/w", "server.ts", &[]),
            fake("phone", "/w", "app.ts", &[("JOIN_FAKE_INSPECT", "1")]),
        ],
        page,
    )
}

#[test]
fn the_programs_look_like_one() {
    let mut join = two(true);
    let (mut client, hello) = join.client();
    assert_eq!(hello["ok"], true, "{hello}");
    assert_eq!(hello["cwd"], "/w");
    assert_eq!(hello["waiting"], true);
    assert_eq!(hello["running"], 2);
    assert_eq!(hello["page"], "http://localhost:1/server", "the first program's page");
    // each program's VM 1 and refs 3 and 4, told apart
    let stopped = hello["stopped"].as_array().unwrap();
    assert_eq!(stopped.iter().map(|stop| stop["vm"].clone()).collect::<Vec<_>>(), [json!(16), json!(17)]);
    assert_eq!(stopped[1]["locals"][0]["ref"], 49);
    assert_eq!(stopped[1]["globals"], 65);

    // a VM's request goes to its program, with its own number
    let eval = client.ask("eval", json!({"vm": 17, "frame": 0, "expr": "a"}));
    assert_eq!(eval["value"], "phone vm 1");
    let eval = client.ask("eval", json!({"vm": 16, "frame": 0, "expr": "a"}));
    assert_eq!(eval["value"], "server vm 1");
    // and a ref's; the refs in the answer are the program's, told apart
    let expand = client.ask("expand", json!({"ref": 49}));
    assert_eq!(expand["vars"][0]["value"], "phone ref 3");
    assert_eq!(expand["vars"][0]["ref"], 81);
    let unknown = client.ask("expand", json!({"ref": 50}));
    assert_eq!(unknown["ok"], false);
    assert!(unknown["error"].as_str().unwrap().contains("of no program"), "{unknown}");

    // every program's answer, merged
    let threads = client.ask("threads", json!({}));
    assert_eq!(threads["running"], 2);
    assert_eq!(threads["stopped"], json!([16, 17]));
    let set = client.ask("setBreakpoints", json!({"file": "app.ts", "breakpoints": [{"line": 4}]}));
    assert_eq!(set["breakpoints"], json!([{"line": 4}]), "the program that loads it");
    let set = client.ask("setBreakpoints", json!({"file": "lib.ts", "breakpoints": [{"line": 2}]}));
    assert_eq!(set["breakpoints"][0]["error"], "server doesn't load it");
    assert_eq!(client.ask("run", json!({"entry": true}))["ok"], true);
    assert_eq!(client.ask("setExceptions", json!({"uncaught": true, "all": false}))["ok"], true);
    let inspect = client.ask("inspect", json!({"on": true}));
    assert_eq!(inspect["inspecting"], "phone", "the one that has it");
    let nobody = client.ask("frobnicate", json!({}));
    assert_eq!(nobody["error"], "server: unknown command frobnicate");

    // events come with the numbers told apart
    let id = client.send("pause", json!({}));
    let (answer, mut events) = client.answer(id);
    assert_eq!(answer["ok"], true);
    while events.len() < 2 {
        events.push(client.event("stopped"));
    }
    let mut vms: Vec<u64> = events.iter().map(|event| event["vm"].as_u64().unwrap()).collect();
    vms.sort();
    assert_eq!(vms, [32, 33]);
    assert!(events.iter().any(|event| event["globals"] == 97));

    join.stop().unwrap();
}

#[test]
fn no_page_hides_it() {
    let mut join = two(false);
    let (_client, hello) = join.client();
    assert_eq!(hello["ok"], true);
    assert!(hello.get("page").is_none(), "{hello}");
    join.stop().unwrap();
}

#[test]
fn a_client_says_hello_first_and_a_new_one_replaces_it() {
    let mut join = two(true);
    let (mut first, hello) = join.client();
    assert_eq!(hello["ok"], true);
    let mut stranger = join.connect().expect("join listens");
    stranger.send("threads", json!({}));
    assert!(stranger.read().is_none(), "dropped without a hello");
    assert_eq!(first.ask("threads", json!({}))["running"], 2, "the client there is stays");

    let (mut second, hello) = join.client();
    assert_eq!(hello["ok"], true);
    assert!(first.read().is_none(), "the one before is closed");
    assert_eq!(second.ask("threads", json!({}))["running"], 2);
    join.stop().unwrap();
}

#[test]
fn the_programs_must_share_a_folder() {
    let join = Running::start(vec![fake("a", "/w", "a.ts", &[]), fake("b", "/x", "b.ts", &[])], true);
    let ports = join.ports.clone();
    let error = format!("{:#}", join.wait().unwrap_err());
    assert!(error.contains("different folders") && error.contains("/x"), "{error}");
    for port in ports {
        assert!(TcpStream::connect(loopback(port)).is_err(), "the programs are ended");
    }
}

#[test]
fn a_program_that_ends_before_listening_fails_the_join() {
    let join = Running::start(vec![fake("a", "/w", "a.ts", &[]), fake("b", "/w", "b.ts", &[("JOIN_FAKE_EXIT", "3")])], true);
    let error = format!("{:#}", join.wait().unwrap_err());
    assert!(error.contains("b ended") && error.contains('3') && error.contains("b: does not compile"), "{error}");
}

#[test]
fn a_program_that_ends_ends_the_others() {
    let mut join = two(true);
    let ports = join.ports.clone();
    let (mut client, hello) = join.client();
    assert_eq!(hello["ok"], true);

    // the phone's VM 1 stopped; the phone ends
    let id = client.send("eval", json!({"vm": 17, "frame": 0, "expr": "die"}));
    let (answer, events) = client.answer(id);
    assert_eq!(answer["ok"], false);
    let told = events.iter().any(|event| event["event"] == "output" && event["text"].as_str().unwrap().contains("phone"));
    assert!(told, "{events:?}");
    join.wait().unwrap();
    for port in ports {
        assert!(TcpStream::connect(loopback(port)).is_err(), "the server is ended too");
    }
}

#[test]
fn arguments() {
    let args = |line: &str| -> Vec<String> { line.split(' ').map(str::to_string).collect() };
    let (port, page, commands) = parse_args(&args("--port 9 --no-page -- sim -dp {port} x -- den chrome --port {port}")).unwrap();
    assert_eq!((port, page), (9, false));
    assert_eq!(commands, [args("sim -dp {port} x"), args("den chrome --port {port}")]);
    assert!(parse_args(&args("-- sim {port}")).unwrap_err().to_string().contains("--port"));
    assert!(parse_args(&args("--port 9 -- sim")).unwrap_err().to_string().contains("no {port}"));
    assert!(parse_args(&args("--port 9 -- a {port} -- -- b {port}")).is_err());
    assert!(parse_args(&args("--port x -- a {port}")).is_err());
    assert_eq!(names(&[args("/bin/sim a"), args("den chrome"), args("sim b")]), ["sim1", "den", "sim3"]);
}

#[test]
fn the_programs_start_in_order() {
    let first = fake("server", "/w", "server.ts", &[("JOIN_FAKE_DELAY", "500")]);
    let after = first.port.to_string();
    let second = fake("app", "/w", "app.ts", &[("JOIN_FAKE_AFTER", after.as_str())]);
    let mut join = Running::start(vec![first, second], true);
    let (_client, hello) = join.client();
    assert_eq!(hello["running"], 2);
    join.stop().unwrap();
}
