//! Minimal language server (LSP) client, for F12, Shift-F12 and completions.
//!
//! There's one server per project and language, started the first time it's
//! asked and kept alive as long as the agent lives. Only `initialize`,
//! `didOpen`, `didChange`, `didClose`, `didChangeWatchedFiles`, `definition`,
//! `references` and `completion` are used. For the file we send the editor's
//! text (saved or not) and close it when asking about another one, so the
//! server reads everything else from disk; changes on disk (from Claude, for
//! example) reach it through the same watcher the tree uses.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use proto::{LspCompletion, LspLocation, LspOp, LspSignature, Response};

use crate::format::Indent;
use serde_json::{Value, json};

/// How long a server may take to start (rust-analyzer on a large repo).
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// After a server fails to start, how long before trying again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Messages beyond this size are taken as garbage.
const MAX_MESSAGE: usize = 64 << 20;
/// LSP "content modified" error: retried.
const CONTENT_MODIFIED: i64 = -32801;
/// Beyond this many, a completion list counts as incomplete.
const MAX_COMPLETIONS: usize = 2000;
/// Names each completion list, for `resolve`.
static NEXT_LIST: AtomicU64 = AtomicU64::new(1);

/// A language: its server and how to find its project root.
struct Language {
    /// Server name (the key, together with the root).
    name: &'static str,
    /// Candidate commands, in order of preference.
    commands: &'static [&'static [&'static str]],
    /// Files that mark a project root.
    markers: &'static [&'static str],
    /// Root: the topmost folder with a marker (a Cargo workspace) or the one
    /// closest to the file (a Go module).
    topmost: bool,
}

const RUST: Language = Language {
    name: "rust-analyzer",
    commands: &[&["rust-analyzer"]],
    markers: &["Cargo.toml"],
    topmost: true,
};
/// Its command is picked by `typescript_command`, depending on the TypeScript available.
const TYPESCRIPT: Language = Language {
    name: "typescript",
    commands: &[],
    markers: &["tsconfig.json", "jsconfig.json", "package.json"],
    topmost: true,
};
const GO: Language = Language {
    name: "gopls",
    commands: &[&["gopls"]],
    markers: &["go.work", "go.mod"],
    topmost: false,
};
const PYTHON: Language = Language {
    name: "python",
    commands: &[&["pyright-langserver", "--stdio"], &["basedpyright-langserver", "--stdio"], &["pylsp"]],
    markers: &["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt"],
    topmost: true,
};
const C: Language = Language {
    name: "clangd",
    commands: &[&["clangd"]],
    markers: &["compile_commands.json", ".clangd", "CMakeLists.txt", "Makefile"],
    topmost: true,
};

/// A file's language and its LSP `languageId`.
fn language(path: &Path) -> Option<(&'static Language, &'static str)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "rs" => (&RUST, "rust"),
        "ts" | "mts" | "cts" => (&TYPESCRIPT, "typescript"),
        "tsx" => (&TYPESCRIPT, "typescriptreact"),
        "js" | "mjs" | "cjs" => (&TYPESCRIPT, "javascript"),
        "jsx" => (&TYPESCRIPT, "javascriptreact"),
        "go" => (&GO, "go"),
        "py" | "pyi" => (&PYTHON, "python"),
        "c" | "h" => (&C, "c"),
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => (&C, "cpp"),
        _ => return None,
    })
}

/// Project root of the file, within the task (`task`).
fn project_root(language: &Language, task: &Path, file: &Path) -> PathBuf {
    let mut found = None;
    let mut dir = file.parent();
    while let Some(d) = dir {
        if !d.starts_with(task) {
            break;
        }
        if language.markers.iter().any(|marker| d.join(marker).exists()) {
            found = Some(d.to_path_buf());
            if !language.topmost {
                break;
            }
        }
        dir = d.parent();
    }
    found.unwrap_or_else(|| task.to_path_buf())
}

static SERVERS: LazyLock<Mutex<HashMap<(PathBuf, &'static str), Arc<Mutex<Slot>>>>> = LazyLock::new(Default::default);

/// A project and language's server. Locked on its own while the server
/// starts, so other servers aren't kept waiting.
#[derive(Default)]
struct Slot {
    server: Option<Arc<Server>>,
    /// When and why it last failed to start.
    failed: Option<(Instant, String)>,
}
/// Binaries already looked up (including missing ones).
static BINARIES: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> = LazyLock::new(Default::default);

pub fn request(task: &Path, path: &Path, text: &str, line: u32, column: u32, op: LspOp) -> Result<Response> {
    let none = || match op {
        LspOp::Completion => Response::Completions { server: None, list: 0, items: Vec::new(), incomplete: false },
        LspOp::SignatureHelp => Response::Signature(None),
        _ => Response::Lsp { server: None, locations: Vec::new() },
    };
    let Some((language, language_id)) = language(path) else {
        return Ok(none());
    };
    let root = project_root(language, task, path);
    let Some(server) = server(language, &root)? else {
        return Ok(none());
    };
    let line_text = text.lines().nth(line as usize).unwrap_or("");
    let mut params = json!({
        "textDocument": { "uri": uri(path) },
        "position": { "line": line, "character": server.encode_column(line_text, column) },
    });
    let method = match op {
        LspOp::Definition => "textDocument/definition",
        LspOp::References => {
            params["context"] = json!({ "includeDeclaration": true });
            "textDocument/references"
        }
        LspOp::Completion => {
            params["context"] = json!({ "triggerKind": 1 });
            "textDocument/completion"
        }
        LspOp::SignatureHelp => "textDocument/signatureHelp",
    };
    // While the project loads, the server may answer "content modified":
    // keep retrying for a while.
    let mut tries = 0;
    let result = loop {
        let sent = server.sync_and_send(path, text, language_id, method, params.clone())?;
        match server.wait(sent, REQUEST_TIMEOUT) {
            Err(err) if err.to_string().contains(&format!("({CONTENT_MODIFIED})")) && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(err) if err.to_string().contains(&format!("({CONTENT_MODIFIED})")) => {
                bail!("{}: still loading the project, try again in a moment", language.name)
            }
            result => break result?,
        }
    };
    if op == LspOp::SignatureHelp {
        return Ok(Response::Signature(server.signature(&result)));
    }
    if op == LspOp::Completion {
        let (items, raw, incomplete) = server.completions(&result, line, line_text, column);
        let list = NEXT_LIST.fetch_add(1, Ordering::Relaxed);
        *server.last_completions.lock().unwrap() = (list, raw);
        return Ok(Response::Completions { server: Some(language.name.to_string()), list, items, incomplete });
    }
    let mut locations = server.locations(&result, path, text);
    locations.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    locations.dedup();
    Ok(Response::Lsp { server: Some(language.name.to_string()), locations })
}

/// Asks the server that gave completion list `list` for the rest of its
/// `item`; nothing if it has already given another list or doesn't resolve.
pub fn resolve(task: &Path, path: &Path, list: u64, item: u32) -> Result<Response> {
    let nothing = Response::Resolved { detail: None, documentation: None };
    let Some((language, _)) = language(path) else {
        return Ok(nothing);
    };
    let key = (project_root(language, task, path), language.name);
    let Some(slot) = SERVERS.lock().unwrap().get(&key).cloned() else {
        return Ok(nothing);
    };
    // If it's starting there's no list to resolve yet.
    let Some(server) = slot.try_lock().ok().and_then(|slot| slot.server.clone()) else {
        return Ok(nothing);
    };
    let raw = {
        let last = server.last_completions.lock().unwrap();
        match last.1.get(item as usize) {
            Some(raw) if last.0 == list && server.resolves => raw.clone(),
            _ => return Ok(nothing),
        }
    };
    let resolved = server.request("completionItem/resolve", raw, REQUEST_TIMEOUT)?;
    Ok(Response::Resolved { detail: detail(&resolved), documentation: documentation(&resolved) })
}

/// `text` formatted by its language server, and the server's name; `None`
/// if there's no server or it doesn't format. Without `indent` (nothing is
/// indented yet), tabs.
pub fn format(task: &Path, path: &Path, text: &str, indent: Option<Indent>) -> Result<Option<(String, String)>> {
    let Some((language, language_id)) = language(path) else {
        return Ok(None);
    };
    let Some(server) = server(language, &project_root(language, task, path))? else {
        return Ok(None);
    };
    if !server.formats {
        return Ok(None);
    }
    let (tab_size, spaces) = match indent {
        Some(Indent::Spaces(n)) => (n, true),
        _ => (4, false),
    };
    let params = json!({
        "textDocument": { "uri": uri(path) },
        "options": { "tabSize": tab_size, "insertSpaces": spaces, "trimTrailingWhitespace": true, "insertFinalNewline": true },
    });
    let sent = server.sync_and_send(path, text, language_id, "textDocument/formatting", params)?;
    let edits = server.wait(sent, REQUEST_TIMEOUT)?;
    Ok(Some((server.apply(text, &edits), language.name.to_string())))
}

/// The `language` server for `root`, starting it if needed.
/// `None` if it isn't installed.
fn server(language: &'static Language, root: &Path) -> Result<Option<Arc<Server>>> {
    let key = (root.to_path_buf(), language.name);
    let slot = SERVERS.lock().unwrap().entry(key).or_default().clone();
    let mut slot = slot.lock().unwrap();
    if let Some(server) = &slot.server {
        if server.alive.load(Ordering::Relaxed) {
            return Ok(Some(server.clone()));
        }
        slot.server = None;
    }
    if let Some((when, why)) = &slot.failed
        && when.elapsed() < RETRY_AFTER
    {
        bail!("{why}");
    }
    let command = if language.name == TYPESCRIPT.name {
        typescript_command(root)
    } else {
        language.commands.iter().find_map(|command| {
            let args = command[1..].iter().map(|arg| arg.to_string()).collect();
            binary(command[0]).map(|binary| (binary, args, Value::Null))
        })
    };
    let Some((program, args, options)) = command else {
        return Ok(None);
    };
    // GUI apps and SSH sessions may have a minimal PATH. Finding the server
    // by absolute path isn't enough: npm scripts use `/usr/bin/env node`,
    // and servers may spawn other tools from their installation directory.
    let mut path = binary_dirs();
    if let Some(dir) = program.parent() {
        path.push(dir.to_path_buf());
    }
    if language.name == TYPESCRIPT.name
        && let Some(node) = binary("node")
        && let Some(dir) = node.parent()
    {
        path.push(dir.to_path_buf());
    }
    let mut command = crate::platform::script_command(&program);
    command.args(args).env("PATH", std::env::join_paths(path)?);
    // Starting may take a while; meanwhile, requests for this server wait.
    match Server::start(command, root, options) {
        Ok(server) => {
            slot.server = Some(server.clone());
            slot.failed = None;
            Ok(Some(server))
        }
        Err(err) => {
            let why = format!("{} did not start: {err:#}", language.name);
            slot.failed = Some((Instant::now(), why.clone()));
            bail!("{why}")
        }
    }
}

/// TypeScript server, as in VS Code: the project's `typescript` or, if it has
/// none, the global one (`tsc`'s). TypeScript 7 ships its own server
/// (`tsc --lsp --stdio`); up to 6, `typescript-language-server` on top of its
/// `tsserver.js`.
fn typescript_command(root: &Path) -> Option<(PathBuf, Vec<String>, Value)> {
    let native = |tsc: PathBuf| Some((tsc, vec!["--lsp".to_string(), "--stdio".to_string()], Value::Null));
    let classic = |options: Value| binary("typescript-language-server").map(|server| (server, vec!["--stdio".to_string()], options));
    if let Some(modules) = root.ancestors().map(|dir| dir.join("node_modules")).find(|dir| dir.join("typescript").is_dir()) {
        if modules.join("typescript/lib/tsserver.js").is_file() {
            return classic(Value::Null);
        }
        if modules.join(".bin/tsc").is_file() {
            return native(modules.join(".bin/tsc"));
        }
    }
    let tsc = binary("tsc")?;
    // `…/typescript/bin/tsc` → `…/typescript`.
    let package = tsc.canonicalize().ok()?.parent()?.parent()?.to_path_buf();
    let tsserver = package.join("lib/tsserver.js");
    if tsserver.is_file() {
        classic(json!({ "tsserver": { "path": tsserver } }))
    } else {
        native(tsc)
    }
}

/// Path of an executable: in the agent's PATH, in the usual install folders
/// or, failing that, in the user's shell PATH (the agent may have started
/// over SSH, without the interactive shell's PATH).
fn binary(name: &str) -> Option<PathBuf> {
    if let Some(found) = BINARIES.lock().unwrap().get(name) {
        return found.clone();
    }
    let found = binary_dirs()
        .iter()
        .find_map(|dir| crate::platform::executable(dir, name))
        .or_else(|| from_shell(name));
    BINARIES.lock().unwrap().insert(name.to_string(), found.clone());
    found
}

/// Share lookup directories with the environment inherited by servers.
fn binary_dirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(
        [".cargo/bin", "go/bin", ".local/bin", ".bun/bin", ".npm-global/bin"]
            .into_iter()
            .map(|dir| home.join(dir)),
    );
    #[cfg(unix)]
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].into_iter().map(PathBuf::from));
    dirs
}

/// `command -v` in an interactive user shell (nvm, pyenv… are configured
/// there), with a timeout in case the shell hangs.
#[cfg(unix)]
fn from_shell(name: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut child = std::process::Command::new(shell)
        .args(["-ilc", &format!("command -v {name}")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_secs(5)).ok();
    let _ = child.kill();
    let _ = child.wait();
    out?.lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('/'))
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

#[cfg(windows)]
fn from_shell(_name: &str) -> Option<PathBuf> { None }

struct Server {
    stdin: Arc<Mutex<ChildStdin>>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>,
    /// The currently open document and its version (only one: the rest come from disk).
    open: Mutex<Option<(PathBuf, i64, String)>>,
    /// Positions in UTF-8 bytes instead of UTF-16.
    utf8: bool,
    /// It answers `completionItem/resolve`.
    resolves: bool,
    /// It answers `textDocument/formatting`.
    formats: bool,
    /// The last completion list and its items, for `resolve`.
    last_completions: Mutex<(u64, Vec<Value>)>,
    alive: Arc<AtomicBool>,
    child: Mutex<Option<Child>>,
    /// Tells the server what changes on disk while it lives.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

/// A server that failed to start or was replaced: its process goes too,
/// even if it hung.
impl Drop for Server {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(mut child) = child {
            std::thread::spawn(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        }
    }
}

impl Server {
    fn start(mut command: std::process::Command, root: &Path, options: Value) -> Result<Arc<Server>> {
        let mut child = command
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().context("no stdin")?;
        // The last thing it says on stderr, to explain why it didn't start.
        let stderr_tail = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let tail = stderr_tail.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut tail = tail.lock().unwrap();
                    if tail.len() > 2000 {
                        tail.clear();
                    }
                    tail.push_str(&line);
                    tail.push('\n');
                }
            });
        }
        let stdout = child.stdout.take().context("no stdout")?;
        let pending: Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>> = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (replies_tx, replies_rx) = mpsc::channel::<Value>();
        {
            let (pending, alive) = (pending.clone(), alive.clone());
            std::thread::spawn(move || read_loop(stdout, pending, alive, replies_tx));
        }
        let stdin = Arc::new(Mutex::new(stdin));
        // Requests from the server (configuration, capability registration,
        // progress) are answered on their own thread, from the start: some
        // servers ask before answering `initialize`.
        {
            let stdin = stdin.clone();
            std::thread::spawn(move || {
                while let Ok(request) = replies_rx.recv() {
                    let result = match request["method"].as_str() {
                        Some("workspace/configuration") => {
                            let items = request["params"]["items"].as_array().map_or(0, Vec::len);
                            Value::Array(vec![Value::Null; items])
                        }
                        _ => Value::Null,
                    };
                    if send(&stdin, &json!({ "jsonrpc": "2.0", "id": request["id"], "result": result })).is_err() {
                        break;
                    }
                }
            });
        }
        let mut server = Server {
            stdin,
            next_id: AtomicI64::new(1),
            pending,
            open: Mutex::new(None),
            utf8: false,
            resolves: false,
            formats: false,
            last_completions: Default::default(),
            alive,
            child: Mutex::new(Some(child)),
            watcher: Mutex::new(None),
        };
        let init = server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": uri(root),
                "rootPath": root,
                "workspaceFolders": [{
                    "uri": uri(root),
                    "name": root.file_name().map(|name| name.to_string_lossy()).unwrap_or_default(),
                }],
                "clientInfo": { "name": "sik" },
                "initializationOptions": options,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-8", "utf-16"] },
                    "textDocument": {
                        "synchronization": { "dynamicRegistration": false },
                        "definition": { "linkSupport": true },
                        "references": {},
                        "formatting": {},
                        "signatureHelp": {
                            "signatureInformation": {
                                "documentationFormat": ["plaintext", "markdown"],
                                "parameterInformation": { "labelOffsetSupport": true },
                                "activeParameterSupport": true,
                            },
                        },
                        "completion": {
                            "completionItem": {
                                "snippetSupport": false,
                                "labelDetailsSupport": true,
                                "documentationFormat": ["markdown", "plaintext"],
                                "resolveSupport": { "properties": ["detail", "documentation"] },
                            },
                            "contextSupport": true,
                        },
                    },
                    "workspace": {
                        "workspaceFolders": true,
                        "configuration": true,
                        "didChangeWatchedFiles": { "dynamicRegistration": false },
                    },
                },
            }),
            INIT_TIMEOUT,
        )
        .map_err(|err| {
            // Give time for whatever it wrote while dying to arrive.
            std::thread::sleep(Duration::from_millis(200));
            match stderr_tail.lock().unwrap().trim() {
                "" => err,
                tail => anyhow!("{err}: {}", tail.lines().last().unwrap_or(tail)),
            }
        })?;
        server.utf8 = init["capabilities"]["positionEncoding"] == "utf-8";
        server.resolves = init["capabilities"]["completionProvider"]["resolveProvider"] == true;
        let formatting = &init["capabilities"]["documentFormattingProvider"];
        server.formats = formatting == true || formatting.is_object();
        server.notify("initialized", json!({}))?;
        let server = Arc::new(server);
        // Whatever changes on disk (minus ignored files) goes to the server.
        *server.watcher.lock().unwrap() = {
            let weak = Arc::downgrade(&server);
            crate::fs::watch(root, false, move |paths| {
                let Some(server) = weak.upgrade() else { return };
                let changes: Vec<Value> = paths
                    .iter()
                    .map(|path| json!({ "uri": uri(path), "type": if path.exists() { 2 } else { 3 } }))
                    .collect();
                let _ = server.notify("workspace/didChangeWatchedFiles", json!({ "changes": changes }));
            })
            .ok()
        };
        Ok(server)
    }

    fn send(&self, message: &Value) -> Result<()> {
        send(&self.stdin, message)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let sent = self.send_request(method, params)?;
        self.wait(sent, timeout)
    }

    /// Sends a request; its answer comes with `wait`.
    fn send_request(&self, method: &str, params: Value) -> Result<Sent> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        // Once the reader has stopped nobody would answer.
        if !self.alive.load(Ordering::Relaxed) {
            self.pending.lock().unwrap().remove(&id);
            bail!("{method}: the server exited");
        }
        if let Err(err) = self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            self.pending.lock().unwrap().remove(&id);
            return Err(err);
        }
        Ok(Sent { id, method: method.to_string(), rx })
    }

    fn wait(&self, sent: Sent, timeout: Duration) -> Result<Value> {
        let Sent { id, method, rx } = sent;
        let result = rx.recv_timeout(timeout);
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => bail!("{method}: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // So it doesn't keep working on it.
                let _ = self.notify("$/cancelRequest", json!({ "id": id }));
                bail!("{method}: the server did not respond (still indexing?)")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("{method}: the server exited"),
        }
    }

    /// Sends a request about `path` as it is in `text`. Both go out together:
    /// a request sent after another thread synced other text would ask
    /// about positions in a document they don't belong to.
    fn sync_and_send(&self, path: &Path, text: &str, language_id: &str, method: &str, params: Value) -> Result<Sent> {
        let mut open = self.open.lock().unwrap();
        self.sync(&mut open, path, text, language_id)?;
        self.send_request(method, params)
    }

    /// Leaves `path` open on the server with `text`; closes the previous one.
    fn sync(&self, open: &mut Option<(PathBuf, i64, String)>, path: &Path, text: &str, language_id: &str) -> Result<()> {
        match open.as_mut() {
            Some((open_path, _, open_text)) if open_path == path && open_text == text => return Ok(()),
            Some((open_path, version, open_text)) if open_path == path => {
                *version += 1;
                *open_text = text.to_string();
                return self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri(path), "version": version },
                        "contentChanges": [{ "text": text }],
                    }),
                );
            }
            Some((open_path, _, _)) => {
                self.notify("textDocument/didClose", json!({ "textDocument": { "uri": uri(open_path) } }))?;
            }
            None => {}
        }
        *open = Some((path.to_path_buf(), 1, text.to_string()));
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": { "uri": uri(path), "languageId": language_id, "version": 1, "text": text },
            }),
        )
    }

    /// Column in characters → the server's unit.
    fn encode_column(&self, line: &str, column: u32) -> u32 {
        let prefix = line.chars().take(column as usize);
        if self.utf8 {
            prefix.map(char::len_utf8).sum::<usize>() as u32
        } else {
            prefix.map(char::len_utf16).sum::<usize>() as u32
        }
    }

    /// Server's unit → characters.
    fn decode_column(&self, line: &str, units: u32) -> u32 {
        let mut count = 0u32;
        let mut chars = 0u32;
        for ch in line.chars() {
            if count >= units {
                break;
            }
            count += if self.utf8 { ch.len_utf8() } else { ch.len_utf16() } as u32;
            chars += 1;
        }
        chars
    }

    /// The items of a `completion` response (a list or `CompletionList`),
    /// with what each one writes from where on the cursor's line.
    /// Also the items themselves, one per completion, for `resolve`.
    fn completions(&self, result: &Value, line: u32, line_text: &str, column: u32) -> (Vec<LspCompletion>, Vec<Value>, bool) {
        let (items, incomplete) = match result {
            Value::Array(items) => (items.as_slice(), false),
            Value::Object(list) => (
                list.get("items").and_then(Value::as_array).map_or(&[][..], Vec::as_slice),
                list.get("isIncomplete").and_then(Value::as_bool).unwrap_or(false),
            ),
            _ => (&[][..], false),
        };
        let word_start = word_start(line_text, column);
        let (items, raw): (Vec<LspCompletion>, Vec<Value>) = items
            .iter()
            .take(MAX_COMPLETIONS)
            .filter_map(|item| {
                let label = item["label"].as_str()?.to_string();
                let edit = &item["textEdit"];
                let range = edit.get("insert").or_else(|| edit.get("range"));
                let (mut text, start) = match (edit["newText"].as_str(), range) {
                    (Some(text), Some(range)) if range["start"]["line"] == line => {
                        let start = self.decode_column(line_text, range["start"]["character"].as_u64()? as u32);
                        (text.to_string(), start.min(column))
                    }
                    _ => (item["insertText"].as_str().unwrap_or(&label).to_string(), word_start),
                };
                if item["insertTextFormat"] == 2 {
                    text = strip_snippet(&text);
                }
                let completion = LspCompletion {
                    filter: item["filterText"].as_str().unwrap_or(&label).to_string(),
                    sort: item["sortText"].as_str().unwrap_or(&label).to_string(),
                    kind: item["kind"].as_u64().map(|kind| kind as u32),
                    detail: detail(item),
                    documentation: documentation(item),
                    label,
                    text,
                    start,
                };
                Some((completion, item.clone()))
            })
            .unzip();
        let incomplete = incomplete || result["items"].as_array().is_some_and(|items| items.len() > MAX_COMPLETIONS);
        (items, raw, incomplete)
    }

    /// `text` with a list of `TextEdit`s applied.
    fn apply(&self, text: &str, edits: &Value) -> String {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(ix, _)| ix + 1));
        let offset = |position: &Value| -> Option<usize> {
            let line = position["line"].as_u64()? as usize;
            let Some(&start) = starts.get(line) else {
                return Some(text.len());
            };
            let line_text = text[start..].split('\n').next().unwrap_or("");
            let chars = self.decode_column(line_text, position["character"].as_u64()? as u32) as usize;
            Some(start + line_text.char_indices().nth(chars).map_or(line_text.len(), |(ix, _)| ix))
        };
        let mut edits: Vec<(usize, usize, &str)> = edits
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|edit| Some((offset(&edit["range"]["start"])?, offset(&edit["range"]["end"])?, edit["newText"].as_str()?)))
            .collect();
        edits.sort_by_key(|(start, end, _)| (*start, *end));
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (start, end, new) in edits {
            if start < at || end < start {
                continue;
            }
            out.push_str(&text[at..start]);
            out.push_str(new);
            at = end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// The active signature of a `signatureHelp` response, with its active
    /// parameter.
    fn signature(&self, help: &Value) -> Option<LspSignature> {
        let signatures = help["signatures"].as_array()?;
        let signature = signatures.get(help["activeSignature"].as_u64().unwrap_or(0) as usize).or(signatures.first())?;
        let label = signature["label"].as_str()?.to_string();
        let active = signature["activeParameter"].as_u64().or_else(|| help["activeParameter"].as_u64());
        let parameter = active.and_then(|ix| signature["parameters"].get(ix as usize));
        let range = parameter.and_then(|parameter| match &parameter["label"] {
            Value::String(name) => {
                let start = label.find(name.as_str())?;
                let start = label[..start].chars().count() as u32;
                Some((start, start + name.chars().count() as u32))
            }
            Value::Array(offsets) => Some((
                self.decode_column(&label, offsets.first()?.as_u64()? as u32),
                self.decode_column(&label, offsets.get(1)?.as_u64()? as u32),
            )),
            _ => None,
        });
        let documentation = parameter
            .and_then(documentation)
            .or_else(|| documentation(signature))
            .map(|text| text.split("\n\n").next().unwrap_or(&text).trim().to_string());
        Some(LspSignature { label, active: range, documentation })
    }

    /// The locations in a `definition` or `references` response: nothing, a
    /// `Location`, or a list of `Location` or `LocationLink`.
    fn locations(&self, result: &Value, current: &Path, current_text: &str) -> Vec<LspLocation> {
        let items: Vec<&Value> = match result {
            Value::Array(items) => items.iter().collect(),
            Value::Null => Vec::new(),
            item => vec![item],
        };
        let mut files: HashMap<PathBuf, Option<String>> = HashMap::new();
        items
            .into_iter()
            .filter_map(|item| {
                let (uri, range) = match item.get("targetUri") {
                    Some(uri) => (uri, item.get("targetSelectionRange").or_else(|| item.get("targetRange"))?),
                    None => (item.get("uri")?, item.get("range")?),
                };
                let path = path_from_uri(uri.as_str()?)?;
                let text = if path == current {
                    Some(current_text.to_string())
                } else {
                    files
                        .entry(path.clone())
                        .or_insert_with(|| std::fs::read_to_string(&path).ok())
                        .clone()
                };
                let start = &range["start"];
                let end = &range["end"];
                let line = start["line"].as_u64()? as u32;
                let line_text = text.as_deref().and_then(|text| text.lines().nth(line as usize)).unwrap_or("");
                let column = self.decode_column(line_text, start["character"].as_u64()? as u32);
                let length = if end["line"] == start["line"] {
                    self.decode_column(line_text, end["character"].as_u64().unwrap_or(0) as u32)
                        .saturating_sub(column)
                } else {
                    0
                };
                Some(LspLocation {
                    path,
                    line,
                    column,
                    length,
                    text: line_text.chars().take(500).collect(),
                })
            })
            .collect()
    }
}

/// A completion's type or signature.
fn detail(item: &Value) -> Option<String> {
    item["detail"]
        .as_str()
        .or_else(|| item["labelDetails"]["detail"].as_str())
        .or_else(|| item["labelDetails"]["description"].as_str())
        .map(|detail| detail.trim().to_string())
        .filter(|detail| !detail.is_empty())
}

/// A completion's documentation, a string or `MarkupContent`, as Markdown.
fn documentation(item: &Value) -> Option<String> {
    let documentation = &item["documentation"];
    documentation
        .as_str()
        .or_else(|| documentation["value"].as_str())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// Start, in characters, of the identifier that ends at `column`.
pub fn word_start(line: &str, column: u32) -> u32 {
    let before: Vec<char> = line.chars().take(column as usize).collect();
    let word = before.iter().rev().take_while(|ch| ch.is_alphanumeric() || **ch == '_' || **ch == '$').count();
    (before.len() - word) as u32
}

/// The text of a snippet: its placeholders' text, without tab stops.
fn strip_snippet(snippet: &str) -> String {
    let mut out = String::new();
    let mut open = 0;
    let mut chars = snippet.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.extend(chars.next()),
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                open += 1;
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
                if chars.peek() == Some(&':') {
                    chars.next();
                }
            }
            '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    chars.next();
                }
            }
            '}' if open > 0 => open -= 1,
            ch => out.push(ch),
        }
    }
    out
}

/// A request sent and not yet answered.
struct Sent {
    id: i64,
    method: String,
    rx: mpsc::Receiver<Result<Value, String>>,
}

fn send(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    let mut stdin = stdin.lock().unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

/// Reads the server's messages: responses go to whoever awaits them, the
/// server's requests to `replies`; notifications are ignored.
fn read_loop(
    stdout: impl Read,
    pending: Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>,
    alive: Arc<AtomicBool>,
    replies: mpsc::Sender<Value>,
) {
    let mut reader = BufReader::new(stdout);
    // Only a closed or broken pipe ends it; a bad message is skipped.
    while let Ok(message) = read_message(&mut reader) {
        let Some(message) = message else { continue };
        let is_request = message.get("method").is_some() && message.get("id").is_some();
        if is_request {
            let _ = replies.send(message);
            continue;
        }
        let Some(id) = message.get("id").and_then(Value::as_i64) else {
            continue;
        };
        let Some(tx) = pending.lock().unwrap().remove(&id) else {
            continue;
        };
        let result = match message.get("error") {
            Some(error) => Err(format!(
                "{} ({})",
                error["message"].as_str().unwrap_or("error"),
                error["code"].as_i64().unwrap_or(0)
            )),
            None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    }
    alive.store(false, Ordering::Relaxed);
    // Anyone waiting for a response learns it won't arrive.
    pending.lock().unwrap().clear();
}

/// The next message; `None` if it isn't one (bad header or JSON), which is
/// skipped. Errors only when the pipe closes or breaks.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut header = Vec::new();
        if reader.read_until(b'\n', &mut header)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let header = String::from_utf8_lossy(&header);
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Ok(None);
    };
    if length > MAX_MESSAGE {
        std::io::copy(&mut reader.take(length as u64), &mut std::io::sink())?;
        return Ok(None);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

/// `file://` with the path encoded.
fn uri(path: &Path) -> String {
    // Paths are absolute; just in case, a bad URI and not a panic.
    url::Url::from_file_path(path).map_or_else(|()| format!("file://{}", path.display()), Into::into)
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_and_word_starts() {
        assert_eq!(strip_snippet("push(${1:value})$0"), "push(value)");
        assert_eq!(strip_snippet("fn ${1}() {\\}"), "fn () {}");
        assert_eq!(word_start("    let ñame", 12), 8);
        assert_eq!(word_start("foo.", 4), 4);
    }

    #[test]
    fn bad_messages_are_skipped() {
        let stream = b"Content-Length: x\r\n\r\nContent-Length: 3\r\n\r\n{x}Content-Length: 2\r\n\r\n{}";
        let mut reader = BufReader::new(&stream[..]);
        assert!(read_message(&mut reader).unwrap().is_none());
        assert!(read_message(&mut reader).unwrap().is_none());
        assert_eq!(read_message(&mut reader).unwrap(), Some(json!({})));
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn uris_round_trip() {
        let path = if cfg!(windows) { Path::new(r"C:\tmp\with space\ñ%.rs") } else { Path::new("/tmp/with space/ñ%.rs") };
        let expected = if cfg!(windows) { "file:///C:/tmp/with%20space/%C3%B1%25.rs" } else { "file:///tmp/with%20space/%C3%B1%25.rs" };
        assert_eq!(uri(path), expected);
        assert_eq!(path_from_uri(&uri(path)).as_deref(), Some(path));
    }

    /// With real servers: `cargo test -p agent lsp -- --ignored`.
    #[test]
    #[ignore]
    fn rust_with_real_server() {
        let dir = std::env::temp_dir().join(format!("sik-lsp-rust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let rust = dir.join("rust");
        std::fs::create_dir_all(rust.join("src")).unwrap();
        std::fs::write(rust.join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        std::fs::write(rust.join("src/util.rs"), "pub fn twice(x: i32) -> i32 {\n    x * 2\n}\n").unwrap();
        let main = "mod util;\n\nfn main() {\n    let ñ = util::twice(1);\n    let _ = util::twice(ñ);\n}\n";
        std::fs::write(rust.join("src/main.rs"), main).unwrap();
        let file = rust.join("src/main.rs");
        // `twice` on line 3, after `    let ñ = util::` (column in characters).
        let column = "    let ñ = util::".chars().count() as u32;
        let mut found = Vec::new();
        // rust-analyzer answers empty until it finishes loading the project.
        for _ in 0..60 {
            let Response::Lsp { locations, .. } = request(&rust, &file, main, 3, column, LspOp::Definition).unwrap() else {
                panic!()
            };
            if !locations.is_empty() {
                found = locations;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].path.canonicalize().unwrap(), rust.join("src/util.rs").canonicalize().unwrap());
        assert_eq!((found[0].line, found[0].column, found[0].length), (0, 7, 5));
        let Response::Lsp { locations, .. } = request(&rust, &file, main, 3, column, LspOp::References).unwrap() else {
            panic!()
        };
        assert_eq!(locations.len(), 3, "{locations:?}");
        // `tw` typed after `util::`: `twice` replaces it.
        let typed = main.replace("util::twice(1)", "util::tw");
        let column = "    let ñ = util::tw".chars().count() as u32;
        let Response::Completions { items, .. } = request(&rust, &file, &typed, 3, column, LspOp::Completion).unwrap() else {
            panic!()
        };
        let twice = items.iter().find(|item| item.label.starts_with("twice")).expect("twice");
        assert_eq!((twice.start, twice.text.as_str()), (column - 2, "twice"), "{twice:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Also run the compiled test with PATH=/usr/bin:/bin to reproduce a
    /// macOS GUI launch: both the server and Node must be discovered.
    #[test]
    #[ignore]
    fn typescript_with_real_server() {
        let dir = std::env::temp_dir().join(format!("sik-lsp-typescript-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ts = dir.join("ts");
        std::fs::create_dir_all(&ts).unwrap();
        std::fs::write(ts.join("tsconfig.json"), "{}").unwrap();
        std::fs::write(ts.join("util.ts"), "export function twice(x: number) {\n  return x * 2\n}\n").unwrap();
        let text = "import { twice } from \"./util\"\nconst ñ = twice(1)\n";
        std::fs::write(ts.join("main.ts"), text).unwrap();
        let column = "const ñ = ".chars().count() as u32;
        let Response::Lsp { locations, server } = request(&ts, &ts.join("main.ts"), text, 1, column, LspOp::Definition).unwrap() else {
            panic!()
        };
        assert_eq!(server.as_deref(), Some("typescript"));
        assert!(locations.iter().any(|l| l.path.ends_with("util.ts") && l.line == 0 && l.column == 16), "{locations:?}");
        let Response::Lsp { locations, .. } = request(&ts, &ts.join("main.ts"), text, 1, column, LspOp::References).unwrap() else {
            panic!()
        };
        assert_eq!(locations.len(), 3, "{locations:?}");
        // TypeScript gives the signature only on resolving the item.
        let typed = format!("{text}tw");
        let Response::Completions { list, items, .. } = request(&ts, &ts.join("main.ts"), &typed, 2, 2, LspOp::Completion).unwrap() else {
            panic!()
        };
        let ix = items.iter().position(|item| item.label == "twice").expect("twice");
        assert!(items[ix].kind.is_some(), "{:?}", items[ix]);
        let Response::Resolved { detail, .. } = resolve(&ts, &ts.join("main.ts"), list, ix as u32).unwrap() else {
            panic!()
        };
        assert!(detail.as_deref().is_some_and(|detail| detail.contains("twice(x: number)")), "{detail:?}");
        let typed = format!("{text}twice(");
        let Response::Signature(Some(signature)) = request(&ts, &ts.join("main.ts"), &typed, 2, 6, LspOp::SignatureHelp).unwrap() else {
            panic!()
        };
        let (start, end) = signature.active.expect("active parameter");
        let active: String = signature.label.chars().skip(start as usize).take((end - start) as usize).collect();
        assert_eq!(active, "x: number", "{signature:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore]
    fn go_with_real_server() {
        let dir = std::env::temp_dir().join(format!("sik-lsp-go-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let go = dir.join("go");
        std::fs::create_dir_all(go.join("util")).unwrap();
        std::fs::write(go.join("go.mod"), "module demo\n\ngo 1.21\n").unwrap();
        std::fs::write(go.join("util/util.go"), "package util\n\nfunc Twice(x int) int {\n\treturn x * 2\n}\n").unwrap();
        let text = "package main\n\nimport \"demo/util\"\n\nfunc main() {\n\tñ := util.Twice(1)\n\t_ = util.Twice(ñ)\n}\n";
        std::fs::write(go.join("main.go"), text).unwrap();
        let column = "\tñ := util.".chars().count() as u32;
        let Response::Lsp { locations, server } = request(&go, &go.join("main.go"), text, 5, column, LspOp::Definition).unwrap() else {
            panic!()
        };
        assert_eq!(server.as_deref(), Some("gopls"));
        assert_eq!(locations.len(), 1, "{locations:?}");
        assert!(locations[0].path.ends_with("util/util.go"));
        assert_eq!((locations[0].line, locations[0].column, locations[0].length), (2, 5, 5));
        let Response::Lsp { locations, .. } = request(&go, &go.join("main.go"), text, 5, column, LspOp::References).unwrap() else {
            panic!()
        };
        assert_eq!(locations.len(), 3, "{locations:?}");
        let typed = text.replace("\t_ = util.Twice(ñ)", "\t_ = util.T");
        let column = "\t_ = util.T".chars().count() as u32;
        let Response::Completions { items, .. } = request(&go, &go.join("main.go"), &typed, 6, column, LspOp::Completion).unwrap() else {
            panic!()
        };
        assert!(items.iter().any(|item| item.label == "Twice" && item.start == column - 1), "{items:?}");
        let typed = text.replace("\t_ = util.Twice(ñ)", "\t_ = util.Twice(");
        let column = "\t_ = util.Twice(".chars().count() as u32;
        let Response::Signature(Some(signature)) = request(&go, &go.join("main.go"), &typed, 6, column, LspOp::SignatureHelp).unwrap() else {
            panic!()
        };
        assert!(signature.label.contains("Twice(x int) int") && signature.active.is_some(), "{signature:?}");
        let (formatted, server) = format(&go, &go.join("main.go"), "package main\nfunc main(){\nx:=1\n_=x}\n", None).unwrap().unwrap();
        assert_eq!((formatted.as_str(), server.as_str()), ("package main\n\nfunc main() {\n\tx := 1\n\t_ = x\n}\n", "gopls"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_roots() {
        let dir = std::env::temp_dir().join(format!("sik-lsp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.join("crates/a/Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(dir.join("svc/sub")).unwrap();
        std::fs::write(dir.join("svc/go.mod"), "").unwrap();
        let file = dir.join("crates/a/src/lib.rs");
        assert_eq!(project_root(&RUST, &dir, &file), dir);
        assert_eq!(project_root(&GO, &dir, &dir.join("svc/sub/x.go")), dir.join("svc"));
        assert_eq!(project_root(&TYPESCRIPT, &dir, &dir.join("web/x.ts")), dir);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
