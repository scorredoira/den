//! `den debug join`: one debug session over several programs. It starts
//! each command in order, each once the one before listens for the
//! debugger, and serves the
//! debug protocol (v1, `docs/debugger.md`) to one client on its own port,
//! as if the programs were one: their VM and ref numbers made distinct
//! (`merge`), a request about a VM or a ref sent to the program it belongs
//! to, any other sent to all of them and their answers merged.
//!
//! A client's session is a connection to each program, opened when the
//! client says hello and closed when it goes: a program then does what it
//! does when its client goes (clears its breakpoints, resumes its stopped
//! VMs), and no program holds a connection while nobody debugs.

mod merge;
#[cfg(test)]
mod tests;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};

use merge::{Answer, MAX_PROGRAMS, decode, merge, namespace};

const USAGE: &str = "usage: den debug join --port <port> [--no-page] -- <command> [<args>...] -- <command> [<args>...] [-- ...]
  {port} in a command is a free port of its own: where that program listens for the debugger.";

/// The protocol's version, which join speaks to the programs.
const VERSION: u64 = 1;
/// How often to try to reach a program that is starting.
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How long a program, or a client, has to say hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to reach a program that already listens, for a new client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the loop looks at the programs and at an interrupt.
const TICK: Duration = Duration::from_millis(100);
/// How long a program has to end after SIGINT, before SIGKILL.
const END_GRACE: Duration = Duration::from_secs(3);
/// How long to wait for the last lines of a program that ended.
const TAIL_WAIT: Duration = Duration::from_secs(1);
/// How long the port may be held by a connection of this machine.
const BIND_WAIT: Duration = Duration::from_secs(3);
/// Lines of a program's stderr kept, to say why it ended.
const TAIL_LINES: usize = 20;

/// `den debug join --port P [--no-page] -- <cmd> … -- <cmd> …`, until
/// interrupted.
pub fn run(args: &[String]) -> Result<()> {
    let (port, page, commands) = parse_args(args)?;
    let names = names(&commands);
    // held until every program has its own: none is given twice
    let held = commands.iter().map(|_| free_port()).collect::<Result<Vec<_>>>()?;
    let mut programs = Vec::new();
    for ((argv, name), listener) in commands.iter().zip(names).zip(&held) {
        let free = listener.local_addr()?.port();
        let argv: Vec<String> = argv.iter().map(|arg| arg.replace("{port}", &free.to_string())).collect();
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]);
        programs.push(Program { name, port: free, command });
    }
    drop(held);
    let stop = interrupts()?;
    serve(programs, port, page, stop)
}

/// A program to start: `command` listens for the debugger on `port`.
pub(crate) struct Program {
    pub name: String,
    pub port: u16,
    pub command: Command,
}

fn parse_args(args: &[String]) -> Result<(u16, bool, Vec<Vec<String>>)> {
    let mut port = None;
    let mut page = true;
    let mut rest = args;
    loop {
        match rest.first().map(String::as_str) {
            Some("--port") => {
                let value = rest.get(1).with_context(|| format!("--port: which port?\n{USAGE}"))?;
                let parsed: u16 = value.parse().with_context(|| format!("--port {value}: not a port"))?;
                port = Some(parsed);
                rest = &rest[2..];
            }
            Some("--no-page") => {
                page = false;
                rest = &rest[1..];
            }
            Some("--") => break,
            Some(other) => bail!("{other}: not an option of join\n{USAGE}"),
            None => bail!("no commands to join\n{USAGE}"),
        }
    }
    let port = port.with_context(|| format!("--port is missing: where the debugger reaches join\n{USAGE}"))?;
    let commands: Vec<Vec<String>> = rest[1..].split(|arg| arg == "--").map(<[String]>::to_vec).collect();
    if commands.iter().any(Vec::is_empty) {
        bail!("an empty command between two --\n{USAGE}");
    }
    if commands.len() > MAX_PROGRAMS {
        bail!("{} commands: join takes {MAX_PROGRAMS} at most", commands.len());
    }
    if let Some(command) = commands.iter().find(|argv| !argv.iter().any(|arg| arg.contains("{port}"))) {
        bail!("`{}` has no {{port}}: join can't know where it listens for the debugger", command.join(" "));
    }
    Ok((port, page, commands))
}

/// What each program's output lines start with: its command's name, and
/// its place when two have the same.
fn names(commands: &[Vec<String>]) -> Vec<String> {
    let base = |argv: &Vec<String>| {
        std::path::Path::new(&argv[0])
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| argv[0].clone())
    };
    let bases: Vec<String> = commands.iter().map(base).collect();
    bases
        .iter()
        .enumerate()
        .map(|(ix, name)| {
            if bases.iter().filter(|other| *other == name).count() > 1 { format!("{name}{}", ix + 1) } else { name.clone() }
        })
        .collect()
}

/// A free port of the loopback, taken while the listener is held.
fn free_port() -> Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("no free port")
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

#[cfg(unix)]
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Set once join is interrupted (Ctrl-C, or the terminal closing).
#[cfg(unix)]
fn interrupts() -> Result<&'static AtomicBool> {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        let previous = unsafe { libc::signal(signal, on_interrupt as *const () as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err(std::io::Error::last_os_error()).context("could not handle interrupts");
        }
    }
    Ok(&INTERRUPTED)
}

/// On Windows Ctrl-C ends every process of the console, join's programs
/// with it: nothing is left to end.
#[cfg(not(unix))]
fn interrupts() -> Result<&'static AtomicBool> {
    static NEVER: AtomicBool = AtomicBool::new(false);
    Ok(&NEVER)
}

/// Starts the programs, waits until each listens and serves the debugger
/// on `port` until `stop` is set or every program has ended. The programs
/// are ended when it returns, whatever the reason.
pub(crate) fn serve(programs: Vec<Program>, port: u16, page: bool, stop: &AtomicBool) -> Result<()> {
    // In order, each once the one before listens: a program may start what
    // the one before would (an app starts its own server when none runs).
    // Waiting has no time limit (an app's build takes minutes); its output
    // goes on showing meanwhile.
    let mut kids = Kids(Vec::new());
    let mut cwds = Vec::new();
    for program in programs {
        kids.0.push(Kid::spawn(program)?);
        let ix = kids.0.len() - 1;
        match kids.wait_listening(ix, stop)? {
            Some(cwd) => cwds.push(cwd),
            None => return Ok(()),
        }
    }
    if let Some((ix, cwd)) = cwds.iter().enumerate().find(|(_, cwd)| *cwd != &cwds[0]) {
        bail!(
            "the programs run in different folders: {} in {}, {} in {cwd}",
            kids.0[0].name,
            cwds[0],
            kids.0[ix].name
        );
    }

    let listener = listen(port).with_context(|| format!("could not listen on port {port}"))?;
    listener.set_nonblocking(true)?;
    let names: Vec<&str> = kids.0.iter().map(|kid| kid.name.as_str()).collect();
    eprintln!("den debug join: debugging {} on 127.0.0.1:{port}", names.join(", "));

    let (tx, rx) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    accept(listener, tx.clone(), done.clone());
    let _done = SetOnDrop(done);
    let mut hub = Hub { kids, session: None, page, tx };
    hub.serve(&rx, stop)
}

/// Listens on `port`. A connection of this machine (join's own, to a
/// program not listening yet) may have been given it as its own port, and
/// hold it a moment: tried again for a while.
fn listen(port: u16) -> std::io::Result<TcpListener> {
    let deadline = Instant::now() + BIND_WAIT;
    loop {
        match TcpListener::bind(loopback(port)) {
            Err(error) if error.kind() == ErrorKind::AddrInUse && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// Sets the flag when dropped: the thread that accepts clients ends.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A program started.
struct Kid {
    name: String,
    port: u16,
    process: Child,
    exit: Option<ExitStatus>,
    tail: Arc<Mutex<VecDeque<String>>>,
    /// Its stderr was read to the end.
    tail_done: Arc<AtomicBool>,
}

impl Kid {
    fn spawn(mut program: Program) -> Result<Self> {
        let mut process = program
            .command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("could not start {}", program.name))?;
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail_done = Arc::new(AtomicBool::new(false));
        let prefix = format!("[{}] ", program.name);
        let stdout = process.stdout.take().context("no stdout")?;
        let stderr = process.stderr.take().context("no stderr")?;
        pass(stdout, prefix.clone(), false, None);
        pass(stderr, prefix, true, Some((tail.clone(), tail_done.clone())));
        Ok(Self { name: program.name, port: program.port, process, exit: None, tail, tail_done })
    }

    /// Why it ended before listening: its exit and its last lines.
    fn ended_early(&self, status: ExitStatus) -> anyhow::Error {
        let deadline = Instant::now() + TAIL_WAIT;
        while !self.tail_done.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let tail = self.tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
        let last = if lines.is_empty() { String::new() } else { format!("; its last lines:\n{}", lines.join("\n")) };
        anyhow!("{} ended ({status}) before listening on port {}{last}", self.name, self.port)
    }

    fn interrupt(&mut self) {
        #[cfg(unix)]
        {
            let Ok(pid) = libc::pid_t::try_from(self.process.id()) else {
                eprintln!("den debug join: {}: pid {} out of range", self.name, self.process.id());
                return;
            };
            // SAFETY: a signal to a child of ours, not yet waited for.
            if unsafe { libc::kill(pid, libc::SIGINT) } == -1 {
                eprintln!("den debug join: interrupting {}: {}", self.name, std::io::Error::last_os_error());
            }
        }
        #[cfg(not(unix))]
        if let Err(error) = self.process.kill() {
            eprintln!("den debug join: ending {}: {error}", self.name);
        }
    }
}

/// Copies a program's output to join's, each line after its name. Lines
/// of stderr are kept in `tail` too.
fn pass(from: impl Read + Send + 'static, prefix: String, stderr: bool, tail: Option<(Arc<Mutex<VecDeque<String>>>, Arc<AtomicBool>)>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(from);
        let mut line = Vec::new();
        let mut broken = false;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line);
                    let text = text.trim_end_matches(['\n', '\r']);
                    if !broken {
                        let written = if stderr {
                            writeln!(std::io::stderr().lock(), "{prefix}{text}")
                        } else {
                            writeln!(std::io::stdout().lock(), "{prefix}{text}")
                        };
                        // the program goes on writing: its pipe is still read, so it isn't blocked
                        if let Err(error) = written {
                            eprintln!("den debug join: can't pass on {prefix}output: {error}");
                            broken = true;
                        }
                    }
                    if let Some((tail, _)) = &tail {
                        let mut tail = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        tail.push_back(text.to_string());
                        if tail.len() > TAIL_LINES {
                            tail.pop_front();
                        }
                    }
                }
                Err(error) => {
                    eprintln!("den debug join: reading {prefix}output: {error}");
                    break;
                }
            }
        }
        if let Some((_, done)) = tail {
            done.store(true, Ordering::SeqCst);
        }
    });
}

/// The programs started, ended when dropped: SIGINT, and SIGKILL to those
/// still running after a few seconds.
struct Kids(Vec<Kid>);

impl Kids {
    /// Waits until program `ix` listens and says hello, and gives its
    /// `cwd`; None if `stop` comes first. Any program ending meanwhile
    /// fails the join.
    fn wait_listening(&mut self, ix: usize, stop: &AtomicBool) -> Result<Option<String>> {
        let (name, port) = (self.0[ix].name.clone(), self.0[ix].port);
        // said once: a port forwarded to a phone (adb forward) takes connections and closes
        // them until the app listens, for as long as the app takes to start
        let mut said = false;
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(None);
            }
            for kid in &mut self.0 {
                if let Some(status) = kid.process.try_wait()? {
                    kid.exit = Some(status);
                    return Err(kid.ended_early(status));
                }
            }
            // Refused until the program listens. A connection to a port
            // nobody listens on can be given that same port as its own and
            // reach itself (TCP's simultaneous open), which would keep the
            // program from listening there: closed at once.
            if let Ok(stream) = TcpStream::connect_timeout(&loopback(port), CONNECT_RETRY)
                && stream.local_addr()? != stream.peer_addr()?
            {
                match greet(&stream) {
                    Ok(Ok(cwd)) => return Ok(Some(cwd)),
                    Ok(Err(refused)) => bail!("{name} on port {port} refused the debugger: {refused}"),
                    // what answered went away before the program's hello
                    // (something else a moment on the port): tried again
                    Err(error) => {
                        if !said {
                            eprintln!("den debug join: {name} on port {port}: {error}; trying again until it answers");
                            said = true;
                        }
                    }
                }
            }
            thread::sleep(CONNECT_RETRY);
        }
    }
}

/// Says hello to a program and gives its `cwd`, or why it refused; an
/// error is the connection's.
fn greet(stream: &TcpStream) -> std::io::Result<Result<String, String>> {
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    write_line(stream, &json!({ "id": 1, "cmd": "hello", "version": VERSION }))?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(ErrorKind::UnexpectedEof.into());
        }
        let answer: Value = match serde_json::from_str(&line) {
            Ok(answer) => answer,
            Err(error) => return Ok(Err(format!("not a message ({error}): {}", line.trim_end()))),
        };
        // an event before the answer
        if answer.get("id").and_then(Value::as_u64) != Some(1) {
            continue;
        }
        if answer.get("ok").and_then(Value::as_bool) != Some(true) {
            return Ok(Err(answer.get("error").and_then(Value::as_str).unwrap_or("no reason given").to_string()));
        }
        return Ok(match answer.get("cwd").and_then(Value::as_str) {
            Some(cwd) => Ok(cwd.to_string()),
            None => Err("its hello has no cwd".to_string()),
        });
    }
}

impl Drop for Kids {
    fn drop(&mut self) {
        for kid in self.0.iter_mut().filter(|kid| kid.exit.is_none()) {
            kid.interrupt();
        }
        let deadline = Instant::now() + END_GRACE;
        while self.0.iter().any(|kid| kid.exit.is_none()) && Instant::now() < deadline {
            for kid in self.0.iter_mut().filter(|kid| kid.exit.is_none()) {
                match kid.process.try_wait() {
                    Ok(status) => kid.exit = status,
                    Err(error) => eprintln!("den debug join: waiting for {}: {error}", kid.name),
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        for kid in self.0.iter_mut().filter(|kid| kid.exit.is_none()) {
            eprintln!("den debug join: {} didn't end: killing it", kid.name);
            if let Err(error) = kid.process.kill() {
                eprintln!("den debug join: killing {}: {error}", kid.name);
            }
            match kid.process.wait() {
                Ok(status) => kid.exit = Some(status),
                Err(error) => eprintln!("den debug join: waiting for {}: {error}", kid.name),
            }
        }
    }
}

/// What the threads tell the loop.
enum Msg {
    /// A client said hello: it replaces the one before.
    Client { session: u64, stream: TcpStream, hello: String },
    ClientLine { session: u64, line: String },
    ClientGone { session: u64 },
    ChildLine { session: u64, child: usize, line: String },
    ChildGone { session: u64, child: usize, error: Option<String> },
}

/// Accepts clients until `done`; each one's first line must be a hello, or
/// it is dropped before it replaces the client there is.
fn accept(listener: TcpListener, tx: Sender<Msg>, done: Arc<AtomicBool>) {
    thread::spawn(move || {
        let mut session = 0;
        while !done.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    session += 1;
                    let (tx, id) = (tx.clone(), session);
                    thread::spawn(move || client(id, stream, tx));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    eprintln!("den debug join: accepting a debugger: {error}");
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });
}

fn client(session: u64, stream: TcpStream, tx: Sender<Msg>) {
    let reader = stream.set_nonblocking(false).and_then(|()| stream.set_read_timeout(Some(HELLO_TIMEOUT))).and_then(|()| stream.try_clone());
    let mut reader = match reader {
        Ok(reader) => BufReader::new(reader),
        Err(error) => {
            eprintln!("den debug join: a debugger's connection: {error}");
            return;
        }
    };
    let mut hello = String::new();
    match reader.read_line(&mut hello) {
        Ok(0) => return eprintln!("den debug join: refused a debugger: it said nothing"),
        Err(error) => return eprintln!("den debug join: refused a debugger: {error}"),
        Ok(_) => {}
    }
    let said_hello = serde_json::from_str::<Value>(&hello).is_ok_and(|value| value.get("cmd").and_then(Value::as_str) == Some("hello"));
    if !said_hello {
        return eprintln!("den debug join: refused a debugger: its first line is not a hello");
    }
    if let Err(error) = stream.set_read_timeout(None) {
        return eprintln!("den debug join: a debugger's connection: {error}");
    }
    if tx.send(Msg::Client { session, stream, hello }).is_err() {
        // join is ending
        return;
    }
    let error = read_lines(reader, |line| tx.send(Msg::ClientLine { session, line }).is_ok());
    if let Some(error) = error {
        eprintln!("den debug join: the debugger's connection: {error}");
    }
    // nobody to tell once join is ending
    tx.send(Msg::ClientGone { session }).ok();
}

/// Reads lines until the end, an error (which it gives) or `each` says to stop.
fn read_lines(mut reader: impl BufRead, mut each: impl FnMut(String) -> bool) -> Option<String> {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let line = line.trim_end_matches(['\n', '\r']);
                if !line.is_empty() && !each(line.to_string()) {
                    return None;
                }
            }
            Err(error) => return Some(error.to_string()),
        }
    }
}

fn write_line(mut stream: &TcpStream, value: &Value) -> std::io::Result<()> {
    let mut text = value.to_string();
    text.push('\n');
    stream.write_all(text.as_bytes())
}

/// Closes a connection; one the other side closed already is closed.
fn close(stream: &TcpStream) {
    if let Err(error) = stream.shutdown(Shutdown::Both)
        && error.kind() != ErrorKind::NotConnected
    {
        eprintln!("den debug join: closing a connection: {error}");
    }
}

/// A request sent on to the programs, by the number join gave it.
enum Pending {
    /// To one program.
    One { id: u64, child: usize },
    /// To every program connected: those still to answer, and the answers.
    All { id: u64, cmd: String, waiting: BTreeSet<usize>, answers: BTreeMap<usize, Answer> },
}

/// A client, and its connection to each program.
struct Session {
    id: u64,
    client: TcpStream,
    conns: Vec<Option<TcpStream>>,
    pending: HashMap<u64, Pending>,
    next: u64,
    /// Each program's stopped VMs, by their combined number: resumed for
    /// the client when the program goes.
    stops: Vec<HashSet<u64>>,
}

struct Hub {
    kids: Kids,
    session: Option<Session>,
    page: bool,
    tx: Sender<Msg>,
}

impl Hub {
    fn serve(&mut self, rx: &Receiver<Msg>, stop: &AtomicBool) -> Result<()> {
        loop {
            if stop.load(Ordering::SeqCst) {
                eprintln!("den debug join: interrupted: ending the programs");
                self.end_session();
                return Ok(());
            }
            self.reap()?;
            // one session: a program that ends (the browser closed, the
            // server failed) ends the others, which serve nothing alone
            if let Some(kid) = self.kids.0.iter().find(|kid| kid.exit.is_some()) {
                let (name, status) = (kid.name.clone(), kid.exit.unwrap_or_default());
                self.end_session();
                if status.success() {
                    eprintln!("den debug join: {name} ended: ending the others");
                    return Ok(());
                }
                bail!("{name} ended ({status})");
            }
            match rx.recv_timeout(TICK) {
                Ok(msg) => self.handle(msg),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => bail!("join's threads ended"),
            }
        }
    }

    /// Notes the programs that ended.
    fn reap(&mut self) -> Result<()> {
        for ix in 0..self.kids.0.len() {
            let kid = &mut self.kids.0[ix];
            if kid.exit.is_some() {
                continue;
            }
            let name = kid.name.clone();
            if let Some(status) = kid.process.try_wait().with_context(|| format!("waiting for {name}"))? {
                self.died(ix, status);
            }
        }
        Ok(())
    }

    fn died(&mut self, child: usize, status: ExitStatus) {
        let kid = &mut self.kids.0[child];
        kid.exit = Some(status);
        let why = format!("{} ended ({status})", kid.name);
        eprintln!("den debug join: {why}");
        self.lose(child, why);
    }

    fn handle(&mut self, msg: Msg) {
        let current = self.session.as_ref().map(|session| session.id);
        match msg {
            Msg::Client { session, stream, hello } => self.open_session(session, stream, &hello),
            Msg::ClientLine { session, line } if Some(session) == current => self.from_client(&line),
            Msg::ClientGone { session } if Some(session) == current => self.end_session(),
            Msg::ChildLine { session, child, line } if Some(session) == current => self.from_child(child, &line),
            Msg::ChildGone { session, child, error } if Some(session) == current => {
                let kid = &mut self.kids.0[child];
                match kid.process.try_wait() {
                    Ok(Some(status)) => self.died(child, status),
                    Ok(None) => {
                        let error = error.map(|error| format!(": {error}")).unwrap_or_default();
                        let why = format!("lost the connection to {}{error}", kid.name);
                        self.lose(child, why);
                    }
                    Err(error) => {
                        let why = format!("lost the connection to {}, and can't wait for it: {error}", kid.name);
                        self.lose(child, why);
                    }
                }
            }
            // of a session that ended
            _ => {}
        }
    }

    /// A client said hello: a connection to each program that runs, and
    /// its hello sent on to them.
    fn open_session(&mut self, id: u64, client: TcpStream, hello: &str) {
        self.end_session();
        let mut notes = Vec::new();
        let mut conns = Vec::new();
        for (child, kid) in self.kids.0.iter().enumerate() {
            if let Some(status) = kid.exit {
                notes.push(format!("{} ended ({status})", kid.name));
                conns.push(None);
                continue;
            }
            let conn = TcpStream::connect_timeout(&loopback(kid.port), CONNECT_TIMEOUT).and_then(|conn| {
                let reader = conn.try_clone()?;
                Ok((conn, reader))
            });
            match conn {
                Ok((conn, reader)) => {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let error = read_lines(BufReader::new(reader), |line| tx.send(Msg::ChildLine { session: id, child, line }).is_ok());
                        // nobody to tell once join is ending
                        tx.send(Msg::ChildGone { session: id, child, error }).ok();
                    });
                    conns.push(Some(conn));
                }
                Err(error) => {
                    notes.push(format!("can't reach {} on port {}: {error}", kid.name, kid.port));
                    conns.push(None);
                }
            }
        }
        let count = conns.len();
        self.session = Some(Session { id, client, conns, pending: HashMap::new(), next: 0, stops: vec![HashSet::new(); count] });
        self.from_client(hello);
        for note in notes {
            self.output(note);
        }
    }

    /// The client went, or another replaced it: the programs see their
    /// clients go too.
    fn end_session(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        close(&session.client);
        for conn in session.conns.iter().flatten() {
            close(conn);
        }
    }

    fn from_client(&mut self, line: &str) {
        let mut request = match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(request)) => request,
            Ok(_) => return self.reply(0, Err("a request must be an object".into())),
            Err(error) => return self.reply(0, Err(format!("invalid request: {error}"))),
        };
        let Some(id) = request.get("id").and_then(Value::as_u64) else {
            return self.reply(0, Err("a request without id".into()));
        };
        let cmd = request.get("cmd").and_then(Value::as_str).unwrap_or_default().to_string();
        let count = self.kids.0.len();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.next += 1;
        let number = session.next;
        request.insert("id".into(), json!(number));

        // about a VM or a value: its program's
        let target = ["vm", "ref"]
            .into_iter()
            .find_map(|key| request.get(key).and_then(Value::as_u64).filter(|n| *n != 0).map(|n| (key, n)));
        if let Some((key, n)) = target {
            let Some((child, own)) = decode(n, count) else {
                return self.reply(id, Err(format!("{key} {n} is of no program")));
            };
            request.insert(key.into(), json!(own));
            if !self.send(child, &Value::Object(request)) {
                let why = format!("{} isn't connected", self.kids.0[child].name);
                return self.reply(id, Err(why));
            }
            if let Some(session) = self.session.as_mut() {
                session.pending.insert(number, Pending::One { id, child });
            }
            return;
        }

        let request = Value::Object(request);
        let mut waiting = BTreeSet::new();
        for child in 0..count {
            if self.send(child, &request) {
                waiting.insert(child);
            }
        }
        if waiting.is_empty() {
            return self.reply(id, Err("no program is connected".into()));
        }
        if let Some(session) = self.session.as_mut() {
            session.pending.insert(number, Pending::All { id, cmd, waiting, answers: BTreeMap::new() });
        }
    }

    /// Sends a request to a program, if it is connected.
    fn send(&mut self, child: usize, request: &Value) -> bool {
        let Some(conn) = self.session.as_ref().and_then(|session| session.conns[child].as_ref()) else {
            return false;
        };
        match write_line(conn, request) {
            Ok(()) => true,
            Err(error) => {
                let why = format!("lost the connection to {}: {error}", self.kids.0[child].name);
                self.lose(child, why);
                false
            }
        }
    }

    fn from_child(&mut self, child: usize, line: &str) {
        let name = self.kids.0[child].name.clone();
        let mut value = serde_json::from_str::<Value>(line).unwrap_or(Value::Null);
        let numbered = namespace(&mut value, child);
        let Value::Object(mut message) = value else {
            eprintln!("den debug join: {name} sent what isn't a message: {line}");
            return;
        };

        if let Some(event) = message.get("event").and_then(Value::as_str) {
            if let Err(error) = numbered {
                return self.output(format!("{name}: an event dropped: {error}"));
            }
            let vm = message.get("vm").and_then(Value::as_u64);
            if let (Some(session), Some(vm)) = (self.session.as_mut(), vm) {
                match event {
                    "stopped" => {
                        session.stops[child].insert(vm);
                    }
                    "resumed" => {
                        session.stops[child].remove(&vm);
                    }
                    _ => {}
                }
            }
            return self.to_client(&Value::Object(message));
        }

        let Some(number) = message.get("id").and_then(Value::as_u64) else {
            eprintln!("den debug join: {name} sent a response without id: {line}");
            return;
        };
        let answer = match numbered {
            Err(error) => Err(error),
            Ok(()) if message.get("ok").and_then(Value::as_bool) == Some(true) => {
                message.remove("id");
                message.remove("ok");
                Ok(message)
            }
            Ok(()) => Err(message.get("error").and_then(Value::as_str).unwrap_or("the request failed").to_string()),
        };
        self.answer(child, number, answer);
    }

    /// A program's answer to the request join numbered `number`.
    fn answer(&mut self, child: usize, number: u64, answer: Answer) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.pending.get_mut(&number) {
            None => eprintln!("den debug join: {} answered no request ({number})", self.kids.0[child].name),
            Some(Pending::One { id, .. }) => {
                let id = *id;
                session.pending.remove(&number);
                self.reply(id, answer);
            }
            Some(Pending::All { cmd, waiting, answers, .. }) => {
                // the VMs stopped when a client comes
                if cmd == "hello"
                    && let Ok(hello) = &answer
                {
                    let vms = hello.get("stopped").and_then(Value::as_array).into_iter().flatten();
                    session.stops[child].extend(vms.filter_map(|stop| stop.get("vm").and_then(Value::as_u64)));
                }
                waiting.remove(&child);
                answers.insert(child, answer);
                if waiting.is_empty() {
                    self.finish(number);
                }
            }
        }
    }

    /// Merges the answers to a request every program got, and answers it.
    fn finish(&mut self, number: u64) {
        let Some(Pending::All { id, cmd, answers, .. }) = self.session.as_mut().and_then(|session| session.pending.remove(&number)) else {
            return;
        };
        let named: Vec<(String, Answer)> =
            answers.into_iter().map(|(child, answer)| (self.kids.0[child].name.clone(), answer)).collect();
        let merged = if named.is_empty() { Err("every program ended before answering".to_string()) } else { merge(&cmd, &named, self.page) };
        self.reply(id, merged);
    }

    /// A program's connection is gone (it ended, or closed it): the client
    /// is told, its stopped VMs resume for the client as they do in the
    /// program, and what was asked of it gets an answer.
    fn lose(&mut self, child: usize, why: String) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(conn) = session.conns[child].take() else {
            return;
        };
        close(&conn);
        let stops: Vec<u64> = session.stops[child].drain().collect();
        let mut failed = Vec::new();
        let mut finished = Vec::new();
        for (number, pending) in session.pending.iter_mut() {
            match pending {
                Pending::One { id, child: of } if *of == child => failed.push((*number, *id)),
                Pending::All { waiting, .. } => {
                    if waiting.remove(&child) && waiting.is_empty() {
                        finished.push(*number);
                    }
                }
                Pending::One { .. } => {}
            }
        }
        for (number, _) in &failed {
            session.pending.remove(number);
        }
        self.output(why.clone());
        for vm in stops {
            self.to_client(&json!({ "event": "resumed", "vm": vm }));
        }
        for (_, id) in failed {
            self.reply(id, Err(why.clone()));
        }
        for number in finished {
            self.finish(number);
        }
    }

    fn reply(&mut self, id: u64, answer: Answer) {
        let mut response = Map::new();
        response.insert("id".into(), json!(id));
        match answer {
            Ok(fields) => {
                response.insert("ok".into(), json!(true));
                response.extend(fields);
            }
            Err(error) => {
                response.insert("ok".into(), json!(false));
                response.insert("error".into(), json!(error));
            }
        }
        self.to_client(&Value::Object(response));
    }

    /// A line in the client's console.
    fn output(&mut self, text: String) {
        self.to_client(&json!({ "event": "output", "text": format!("den debug join: {text}\n"), "file": "", "line": 0 }));
    }

    /// A client that can't be written to is closed: its reader then ends
    /// the session.
    fn to_client(&mut self, message: &Value) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Err(error) = write_line(&session.client, message) {
            eprintln!("den debug join: writing to the debugger: {error}");
            close(&session.client);
        }
    }
}
