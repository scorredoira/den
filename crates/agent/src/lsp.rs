//! Minimal language server (LSP) client, for F12 and Shift-F12.
//!
//! There's one server per project and language, started the first time it's
//! asked and kept alive as long as the agent lives. Only `initialize`,
//! `didOpen`, `didChange`, `didClose`, `didChangeWatchedFiles`, `definition`
//! and `references` are used. For the file we send the editor's text (saved
//! or not) and close it when asking about another one, so the server reads
//! everything else from disk; changes on disk (from Claude, for example) reach
//! it through the same watcher the tree uses.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use proto::{LspLocation, LspOp, Response};
use serde_json::{Value, json};

/// How long a server may take to start (rust-analyzer on a large repo).
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// LSP "content modified" error: retried.
const CONTENT_MODIFIED: i64 = -32801;

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

static SERVERS: LazyLock<Mutex<HashMap<(PathBuf, &'static str), Arc<Server>>>> = LazyLock::new(Default::default);
/// Binaries already looked up (including missing ones).
static BINARIES: LazyLock<Mutex<HashMap<String, Option<PathBuf>>>> = LazyLock::new(Default::default);

pub fn request(task: &Path, path: &Path, text: &str, line: u32, column: u32, op: LspOp) -> Result<Response> {
    let Some((language, language_id)) = language(path) else {
        return Ok(Response::Lsp { server: None, locations: Vec::new() });
    };
    let root = project_root(language, task, path);
    let Some(server) = server(language, &root)? else {
        return Ok(Response::Lsp { server: None, locations: Vec::new() });
    };
    server.sync(path, text, language_id)?;
    let line_text = text.lines().nth(line as usize).unwrap_or("");
    let params = json!({
        "textDocument": { "uri": uri(path) },
        "position": { "line": line, "character": server.encode_column(line_text, column) },
        "context": { "includeDeclaration": true },
    });
    let method = match op {
        LspOp::Definition => "textDocument/definition",
        LspOp::References => "textDocument/references",
    };
    // While the project loads, the server may answer "content modified":
    // keep retrying for a while.
    let mut tries = 0;
    let result = loop {
        match server.request(method, params.clone(), REQUEST_TIMEOUT) {
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
    let mut locations = server.locations(&result, path, text);
    locations.sort_by(|a, b| (&a.path, a.line, a.column).cmp(&(&b.path, b.line, b.column)));
    locations.dedup();
    Ok(Response::Lsp { server: Some(language.name.to_string()), locations })
}

/// The `language` server for `root`, starting it if needed.
/// `None` if it isn't installed.
fn server(language: &'static Language, root: &Path) -> Result<Option<Arc<Server>>> {
    let key = (root.to_path_buf(), language.name);
    let mut servers = SERVERS.lock().unwrap();
    if let Some(server) = servers.get(&key) {
        if server.alive.load(Ordering::Relaxed) {
            return Ok(Some(server.clone()));
        }
        servers.remove(&key);
    }
    let command = if language.name == TYPESCRIPT.name {
        typescript_command(root)
    } else {
        language.commands.iter().find_map(|command| {
            let args = command[1..].iter().map(|arg| arg.to_string()).collect();
            binary(command[0]).map(|binary| (binary, args, Value::Null))
        })
    };
    let Some((binary, args, options)) = command else {
        return Ok(None);
    };
    // Starting may take a while; meanwhile, other LSP requests wait.
    let server = Server::start(&binary, &args, root, options).with_context(|| format!("{} did not start", language.name))?;
    servers.insert(key, server.clone());
    Ok(Some(server))
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
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    dirs.extend(
        [".cargo/bin", "go/bin", ".local/bin", ".bun/bin", ".npm-global/bin"]
            .into_iter()
            .map(|dir| home.join(dir)),
    );
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].into_iter().map(PathBuf::from));
    let found = dirs
        .iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .or_else(|| from_shell(name));
    BINARIES.lock().unwrap().insert(name.to_string(), found.clone());
    found
}

/// `command -v` in an interactive user shell (nvm, pyenv… are configured
/// there), with a timeout in case the shell hangs.
fn from_shell(name: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut child = Command::new(shell)
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

struct Server {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>,
    /// The currently open document and its version (only one: the rest come from disk).
    open: Mutex<Option<(PathBuf, i64, String)>>,
    /// Positions in UTF-8 bytes instead of UTF-16.
    utf8: bool,
    alive: Arc<AtomicBool>,
    _child: Mutex<Child>,
    /// Tells the server what changes on disk while it lives.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

impl Server {
    fn start(binary: &Path, args: &[String], root: &Path, options: Value) -> Result<Arc<Server>> {
        let mut child = Command::new(binary)
            .args(args)
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
        let mut server = Server {
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            pending,
            open: Mutex::new(None),
            utf8: false,
            alive,
            _child: Mutex::new(child),
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
        server.notify("initialized", json!({}))?;
        let server = Arc::new(server);
        // Requests from the server (configuration, capability
        // registration, progress) are answered on their own thread.
        {
            let weak = Arc::downgrade(&server);
            std::thread::spawn(move || {
                while let Ok(request) = replies_rx.recv() {
                    let Some(server) = weak.upgrade() else { break };
                    let result = match request["method"].as_str() {
                        Some("workspace/configuration") => {
                            let items = request["params"]["items"].as_array().map_or(0, Vec::len);
                            Value::Array(vec![Value::Null; items])
                        }
                        _ => Value::Null,
                    };
                    let _ = server.send(&json!({ "jsonrpc": "2.0", "id": request["id"], "result": result }));
                }
            });
        }
        // Whatever changes on disk (minus ignored files) goes to the server.
        *server.watcher.lock().unwrap() = {
            let weak = Arc::downgrade(&server);
            crate::fs::watch(root, move |paths| {
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
        let body = serde_json::to_vec(message)?;
        let mut stdin = self.stdin.lock().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
        stdin.write_all(&body)?;
        stdin.flush()?;
        Ok(())
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let result = rx.recv_timeout(timeout);
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => bail!("{method}: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => bail!("{method}: the server did not respond (still indexing?)"),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("{method}: the server exited"),
        }
    }

    /// Leaves `path` open on the server with `text`; closes the previous one.
    fn sync(&self, path: &Path, text: &str, language_id: &str) -> Result<()> {
        let mut open = self.open.lock().unwrap();
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

/// Reads the server's messages: responses go to whoever awaits them, the
/// server's requests to `replies`; notifications are ignored.
fn read_loop(
    stdout: impl Read,
    pending: Arc<Mutex<HashMap<i64, mpsc::Sender<Result<Value, String>>>>>,
    alive: Arc<AtomicBool>,
    replies: mpsc::Sender<Value>,
) {
    let mut reader = BufReader::new(stdout);
    while let Ok(message) = read_message(&mut reader) {
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

fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            bail!("end");
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let mut body = vec![0; length.ok_or_else(|| anyhow!("no Content-Length"))?];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

/// `file://` with the path encoded.
fn uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%' && ix + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[ix + 1..ix + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            ix += 3;
        } else {
            out.push(bytes[ix]);
            ix += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_round_trip() {
        let path = Path::new("/tmp/with space/ñ%.rs");
        assert_eq!(uri(path), "file:///tmp/with%20space/%C3%B1%25.rs");
        assert_eq!(path_from_uri(&uri(path)).as_deref(), Some(path));
    }

    /// With real servers: `cargo test -p agent lsp -- --ignored`.
    #[test]
    #[ignore]
    fn definition_and_references_with_real_servers() {
        let dir = std::env::temp_dir().join(format!("sik-lsp-real-{}", std::process::id()));
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
