//! `den chrome`: debugs the pages of a web app in Chrome with Den's debugger.
//! It speaks Den's debug protocol v1 (docs/debugger.md) to Den and the
//! Chrome DevTools Protocol to Chrome, mapping the bundled JavaScript back to
//! the TypeScript through source maps. See docs/chrome.md.

mod browser;
mod cdp;
mod debugger;
mod fetch;
mod sourcemap;
mod values;

use std::{
    io::{BufRead as _, BufReader, Read as _},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::Value;

use crate::{
    browser::Browser,
    cdp::Cdp,
    debugger::{Core, Input, Out, Settings},
};

const USAGE: &str = "usage: den chrome --port P [--url URL] [--root DIR] [--headless] [--profile DIR] [--hosts LIST]";

/// What `den chrome` was asked for.
#[derive(Clone, Debug)]
pub struct Options {
    /// Where the bridge listens for Den, on 127.0.0.1; 0 takes a free port.
    pub port: u16,
    /// Opened in a tab when the first client sends `run`.
    pub url: Option<String>,
    /// The folder the protocol's paths are relative to.
    pub root: PathBuf,
    pub headless: bool,
    /// Chrome's profile; by default one in Den's data folder.
    pub profile: Option<PathBuf>,
    /// The hosts whose pages are debugged: `name` or `*.name`.
    pub hosts: Vec<String>,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Options> {
        let mut port = None;
        let mut url = None;
        let mut root = None;
        let mut headless = false;
        let mut profile = None;
        let mut hosts = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let (name, inline) = match arg.split_once('=') {
                Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
                _ => (arg.as_str(), None),
            };
            let mut value = || -> Result<String> {
                match &inline {
                    Some(value) => Ok(value.clone()),
                    None => args.next().cloned().with_context(|| format!("{name} needs a value\n{USAGE}")),
                }
            };
            match name {
                "--port" => {
                    let text = value()?;
                    port = Some(text.parse::<u16>().with_context(|| format!("--port {text:?} is not a port"))?);
                }
                "--url" => url = Some(value()?),
                "--root" => root = Some(PathBuf::from(value()?)),
                "--headless" => headless = true,
                "--profile" => profile = Some(PathBuf::from(value()?)),
                "--hosts" => {
                    let list: Vec<String> = value()?
                        .split(',')
                        .map(|host| host.trim().to_ascii_lowercase())
                        .filter(|host| !host.is_empty())
                        .collect();
                    if list.is_empty() {
                        bail!("--hosts names no host");
                    }
                    hosts = Some(list);
                }
                "-h" | "--help" => bail!("{USAGE}"),
                other => bail!("unknown argument {other:?}\n{USAGE}"),
            }
        }
        let port = port.with_context(|| format!("--port is missing\n{USAGE}"))?;
        let root = match root {
            Some(root) => root,
            None => std::env::current_dir().context("the working directory")?,
        };
        let root = root.canonicalize().with_context(|| format!("--root {}", root.display()))?;
        let hosts = hosts.unwrap_or_else(|| vec!["localhost".into(), "127.0.0.1".into(), "*.localhost".into()]);
        Ok(Options { port, url, root, headless, profile, hosts })
    }
}

/// Runs `den chrome` until Chrome goes away. `args` exclude "chrome".
pub fn run(args: &[String]) -> Result<()> {
    let options = Options::parse(args)?;
    let url = options.url.clone();
    let bridge = Bridge::start(options)?;
    println!("debugger listening on 127.0.0.1:{}", bridge.port());
    if let Some(url) = url {
        println!("{url} opens when the debugger connects");
    }
    bridge.wait()
}

/// A running bridge: Chrome, the connection to it, and the port Den
/// connects to. Dropping it ends it; a headless Chrome it launched ends too.
pub struct Bridge {
    port: u16,
    inputs: Sender<Input>,
    worker: Option<JoinHandle<Result<()>>>,
    cdp: Arc<Cdp>,
    browser: Browser,
    closing: Arc<AtomicBool>,
}

impl Bridge {
    pub fn start(options: Options) -> Result<Bridge> {
        let profile = match &options.profile {
            Some(profile) => profile.clone(),
            None => browser::default_profile()?,
        };
        let browser = browser::open(&profile, options.headless)?;
        let (inputs, receiver) = mpsc::channel();
        let events = inputs.clone();
        let closed = inputs.clone();
        let cdp = Cdp::connect(
            browser.port,
            &browser.path,
            move |event| {
                // the receiver is gone only when the bridge has ended
                let _ = events.send(Input::Cdp(event));
            },
            move |reason| {
                let _ = closed.send(Input::CdpClosed(reason));
            },
        )?;

        let listener = TcpListener::bind(("127.0.0.1", options.port))
            .with_context(|| format!("listen on 127.0.0.1:{}", options.port))?;
        let port = listener.local_addr().context("the address listened on")?.port();

        let settings = Settings {
            root: options.root.clone(),
            hosts: options.hosts.clone(),
            url: options.url.clone(),
            launched: browser.launched,
        };
        let core = Core::new(cdp.clone(), Out::default(), settings);
        let worker = thread::Builder::new()
            .name("chrome-bridge".into())
            .spawn(move || core.run(receiver))
            .context("start the bridge")?;

        let closing = Arc::new(AtomicBool::new(false));
        let accepting = inputs.clone();
        let stop = closing.clone();
        thread::Builder::new()
            .name("chrome-accept".into())
            .spawn(move || accept(listener, accepting, stop))
            .context("start accepting clients")?;

        Ok(Bridge { port, inputs, worker: Some(worker), cdp, browser, closing })
    }

    /// The port Den connects to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Serves until Chrome goes away.
    pub fn wait(mut self) -> Result<()> {
        let worker = self.worker.take().context("the bridge already ended")?;
        let result = worker.join().map_err(|_| anyhow!("the bridge's thread panicked"))?;
        self.shutdown();
        result
    }

    fn shutdown(&mut self) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            // wakes the accept loop, which then sees `closing`
            if let Err(err) = TcpStream::connect(("127.0.0.1", self.port)) {
                eprintln!("chrome: wake the listener: {err}");
            }
        }
        // the worker may have ended already, dropping its receiver
        let _ = self.inputs.send(Input::Shutdown);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => eprintln!("chrome: {err:#}"),
                Err(_) => eprintln!("chrome: the bridge's thread panicked"),
            }
        }
        self.cdp.shutdown();
        self.browser.stop();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept(listener: TcpListener, inputs: Sender<Input>, closing: Arc<AtomicBool>) {
    let generations = Arc::new(AtomicU64::new(1));
    for stream in listener.incoming() {
        if closing.load(Ordering::SeqCst) {
            return;
        }
        let stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("chrome: accept a client: {err}");
                continue;
            }
        };
        let generation = generations.fetch_add(1, Ordering::SeqCst);
        let inputs = inputs.clone();
        let spawned = thread::Builder::new().name("chrome-client".into()).spawn(move || {
            if let Err(err) = serve(stream, generation, &inputs) {
                eprintln!("chrome: {err:#}");
            }
        });
        if let Err(err) = spawned {
            eprintln!("chrome: start a client's thread: {err}");
        }
    }
}

/// Reads a client's lines. The first must be a `hello`, or the connection is
/// closed before it replaces the client or runs anything: a web page can send
/// a request to the loopback port, and it starts `POST / HTTP/1.1`.
fn serve(stream: TcpStream, generation: u64, inputs: &Sender<Input>) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).context("set the client's read timeout")?;
    let mut reader = BufReader::new(stream.try_clone().context("clone the client's socket")?);
    let mut first = String::new();
    (&mut reader).take(1 << 20).read_line(&mut first).context("refused a client: no hello")?;
    let hello: Value = serde_json::from_str(&first)
        .map_err(|err| anyhow!("refused a client: the first line is not a hello: {err}"))?;
    if hello.get("cmd").and_then(Value::as_str) != Some("hello") {
        bail!("refused a client: the first line is {:?}, not a hello", hello.get("cmd"));
    }
    stream.set_read_timeout(None).context("clear the client's read timeout")?;
    if inputs.send(Input::Connected { generation, stream, hello }).is_err() {
        bail!("the bridge has ended");
    }
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                eprintln!("chrome: read from the client: {err}");
                break;
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                if inputs.send(Input::Request { generation, request }).is_err() {
                    return Ok(());
                }
            }
            Err(err) => eprintln!("chrome: a request that is not JSON: {err}"),
        }
    }
    // the bridge may have ended; then nobody needs to hear it
    let _ = inputs.send(Input::Disconnected { generation });
    Ok(())
}
