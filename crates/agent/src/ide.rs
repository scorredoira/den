//! Claude Code's IDE integration, as VS Code's extension does it: Claude
//! Code in one of the agent's terminals knows the file and the selection in
//! front in that terminal's workspace, with every message.
//!
//! The agent listens on a WebSocket at 127.0.0.1 and announces it in
//! `~/.claude/ide/<port>.lock`; its terminals have `CLAUDE_CODE_SSE_PORT`,
//! so Claude Code connects by itself. It's an MCP client: it asks for the
//! tools below and calls them, and hears `selection_changed` from here. It
//! says its pid (`ide_connected`), which tells the terminal it runs in, and
//! so the workspace. Not a published protocol: as Claude Code 2.1 speaks it.

use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context as _, Result};
use proto::{IdeSelection, TermId};
use serde_json::{Value, json};
use tungstenite::{
    Message,
    handshake::server::{ErrorResponse, Request as Upgrade, Response as Accepted},
    http::StatusCode,
};

/// What the IDE side needs from the rest of the agent.
pub trait Host: Send + Sync {
    /// The terminal whose shell is `pid` or one of its ancestors, and its
    /// workspace (its group).
    fn terminal_of(&self, pid: u32) -> Option<(TermId, String)>;
    /// Runs `den ide …` in the app showing `term`, and waits for its answer.
    fn ask_app(&self, term: TermId, group: &str, args: Vec<String>) -> Result<String, String>;
}

struct Ide {
    port: u16,
    token: String,
    lock: PathBuf,
    host: Arc<dyn Host>,
    /// The last selection of each workspace, as `selection_changed` says it.
    selections: Mutex<HashMap<String, Value>>,
    clients: Mutex<Vec<Client>>,
    next: AtomicU64,
}

/// A Claude Code connected, once it said where it runs.
struct Client {
    id: u64,
    terminal: Option<(TermId, String)>,
    outbox: mpsc::Sender<String>,
}

static IDE: OnceLock<Arc<Ide>> = OnceLock::new();

/// Listens and announces itself. Without a home folder or a free port the
/// terminals go on without it.
pub fn start(host: Arc<dyn Host>) -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("could not listen for Claude Code")?;
    let port = listener.local_addr()?.port();
    let dir = lock_dir()?;
    std::fs::create_dir_all(&dir)?;
    let ide = Arc::new(Ide {
        port,
        token: token()?,
        lock: dir.join(format!("{port}.lock")),
        host,
        selections: Mutex::default(),
        clients: Mutex::default(),
        next: AtomicU64::new(1),
    });
    let lock = json!({
        "pid": std::process::id(),
        "workspaceFolders": [],
        "ideName": "Den",
        "transport": "ws",
        "authToken": ide.token,
    });
    std::fs::write(&ide.lock, serde_json::to_vec(&lock)?)?;
    let _ = IDE.set(ide.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let ide = ide.clone();
            std::thread::spawn(move || {
                if let Err(err) = ide.serve(stream) {
                    eprintln!("Claude Code connection: {err:#}");
                }
            });
        }
    });
    Ok(())
}

/// Where Claude Code looks for IDEs; an isolated agent's own folder
/// instead, which outlives no test.
fn lock_dir() -> Result<PathBuf> {
    if crate::cli::isolated() {
        return Ok(proto::state_dir()?.join("claude-ide"));
    }
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir().context("no home folder")?.join(".claude"),
    };
    Ok(config.join("ide"))
}

/// The port for the terminals' `CLAUDE_CODE_SSE_PORT`.
pub fn port() -> Option<u16> {
    IDE.get().map(|ide| ide.port)
}

/// The lock file goes with the agent (Claude Code also drops those whose
/// process is gone).
pub fn stop() {
    if let Some(ide) = IDE.get() {
        let _ = std::fs::remove_file(&ide.lock);
    }
}

/// What workspace `group`'s editor shows now: kept for those that connect
/// later, and told to those connected in its terminals.
pub fn set_selection(group: String, selection: Option<IdeSelection>) {
    let Some(ide) = IDE.get() else {
        return;
    };
    let Some(selection) = selection else {
        return;
    };
    let params = selection_params(&selection);
    let message = selection_changed(&params);
    ide.selections.lock().unwrap().insert(group.clone(), params);
    for client in ide.clients.lock().unwrap().iter() {
        if client.terminal.as_ref().is_some_and(|(_, of)| *of == group) {
            let _ = client.outbox.send(message.clone());
        }
    }
}

/// 128 random bits, in hex.
fn token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|err| anyhow::anyhow!("no random numbers: {err}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn selection_params(selection: &IdeSelection) -> Value {
    let point = |(line, character): (u32, u32)| json!({ "line": line, "character": character });
    json!({
        "text": selection.text,
        "filePath": selection.file,
        "fileUrl": file_url(&selection.file),
        "selection": {
            "start": point(selection.start),
            "end": point(selection.end),
            "isEmpty": selection.start == selection.end,
        },
    })
}

/// The selection as told to Claude Code. With nothing selected, no file:
/// otherwise it puts "In main.rs" in its prompt all the time. The file is
/// still there for `getCurrentSelection`.
fn selection_changed(params: &Value) -> String {
    let mut params = params.clone();
    if params["text"].as_str().is_none_or(str::is_empty) {
        let fields = params.as_object_mut().expect("selection_params is an object");
        fields.remove("filePath");
        fields.remove("fileUrl");
    }
    notification("selection_changed", params)
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).map_or_else(|_| format!("file://{}", path.display()), String::from)
}

fn notification(method: &str, params: Value) -> String {
    json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string()
}

impl Ide {
    /// One Claude Code: its token checked, then its requests answered and
    /// what it should hear sent, until it goes.
    fn serve(self: &Arc<Self>, stream: TcpStream) -> Result<()> {
        let token = self.token.clone();
        let mut socket = tungstenite::accept_hdr(stream.try_clone()?, |request: &Upgrade, mut response: Accepted| {
            let headers = request.headers();
            let authorized = headers.get("x-claude-code-ide-authorization").is_some_and(|value| value == token.as_str());
            // A browser always says where the page is from; Claude Code never does.
            if authorized && !headers.contains_key("origin") {
                // MCP's subprotocol: Claude Code drops a connection without it.
                if let Some(protocol) = headers.get("sec-websocket-protocol") {
                    response.headers_mut().insert("sec-websocket-protocol", protocol.clone());
                }
                Ok(response)
            } else {
                let mut refused = ErrorResponse::new(None);
                *refused.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refused)
            }
        })
        .map_err(|err| anyhow::anyhow!("handshake: {err}"))?;
        // Reads wait a little, so what's to be sent doesn't wait for a read.
        stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (outbox, outgoing) = mpsc::channel();
        self.clients.lock().unwrap().push(Client { id, terminal: None, outbox: outbox.clone() });
        let result = (|| -> Result<()> {
            loop {
                match socket.read() {
                    Ok(Message::Text(text)) => self.receive(id, &text, &outbox),
                    Ok(Message::Close(_)) => return Ok(()),
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
                    Err(err) => return Err(err.into()),
                }
                while let Ok(text) = outgoing.try_recv() {
                    socket.send(Message::text(text))?;
                }
                match socket.flush() {
                    Err(tungstenite::Error::Io(err)) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                    other => other?,
                }
            }
        })();
        self.clients.lock().unwrap().retain(|client| client.id != id);
        result
    }

    /// A JSON-RPC message from Claude Code. Tools that ask the app answer
    /// from a thread of their own: the connection goes on meanwhile.
    fn receive(self: &Arc<Self>, client: u64, text: &str, outbox: &mpsc::Sender<String>) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let method = message["method"].as_str().unwrap_or_default();
        let params = &message["params"];
        let Some(id) = message.get("id").cloned() else {
            if method == "ide_connected" {
                self.connected(client, params["pid"].as_u64());
            }
            return;
        };
        let reply = move |result: Result<Value, (i64, String)>| {
            let message = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
            };
            message.to_string()
        };
        match method {
            "initialize" => {
                let version = params["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let _ = outbox.send(reply(Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "den", "version": env!("CARGO_PKG_VERSION") },
                }))));
            }
            "ping" => {
                let _ = outbox.send(reply(Ok(json!({}))));
            }
            "tools/list" => {
                let _ = outbox.send(reply(Ok(json!({ "tools": tools() }))));
            }
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default().to_string();
                let arguments = params["arguments"].clone();
                let terminal = self.terminal(client);
                let (ide, outbox) = (self.clone(), outbox.clone());
                std::thread::spawn(move || {
                    let result = match ide.call(&name, &arguments, terminal) {
                        Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                        Err(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
                    };
                    let _ = outbox.send(reply(Ok(result)));
                });
            }
            _ => {
                let _ = outbox.send(reply(Err((-32601, format!("method not found: {method}")))));
            }
        }
    }

    /// Claude Code said its pid: the terminal it runs in, and the selection
    /// of its workspace right away.
    fn connected(&self, client: u64, pid: Option<u64>) {
        let terminal = pid.and_then(|pid| self.host.terminal_of(pid as u32));
        let selection = terminal.as_ref().and_then(|(_, group)| self.selections.lock().unwrap().get(group).cloned());
        let mut clients = self.clients.lock().unwrap();
        let Some(entry) = clients.iter_mut().find(|entry| entry.id == client) else {
            return;
        };
        entry.terminal = terminal;
        if let Some(selection) = selection {
            let _ = entry.outbox.send(selection_changed(&selection));
        }
    }

    fn terminal(&self, client: u64) -> Option<(TermId, String)> {
        self.clients.lock().unwrap().iter().find(|entry| entry.id == client).and_then(|entry| entry.terminal.clone())
    }

    /// A tool's answer, as the text Claude Code expects.
    fn call(&self, name: &str, arguments: &Value, terminal: Option<(TermId, String)>) -> Result<String, String> {
        let group = terminal.as_ref().map(|(_, group)| group.clone());
        match name {
            "getCurrentSelection" | "getLatestSelection" => {
                let selection = group.and_then(|group| self.selections.lock().unwrap().get(&group).cloned());
                Ok(match selection {
                    Some(mut selection) => {
                        selection["success"] = json!(true);
                        selection.to_string()
                    }
                    None => json!({ "success": false, "message": "No active editor found" }).to_string(),
                })
            }
            "getWorkspaceFolders" => {
                let folders: Vec<Value> = group
                    .iter()
                    .map(|group| {
                        let path = Path::new(group);
                        let name = path.file_name().map_or_else(|| group.clone(), |name| name.to_string_lossy().into_owned());
                        json!({ "name": name, "uri": file_url(path), "path": group })
                    })
                    .collect();
                Ok(json!({ "success": true, "folders": folders, "rootPath": group }).to_string())
            }
            // Den has no diagnostics of its own to report.
            "getDiagnostics" => Ok("[]".into()),
            "closeAllDiffTabs" => Ok("CLOSED_0_DIFF_TABS".into()),
            "close_tab" => Ok("TAB_CLOSED".into()),
            "openFile" | "getOpenEditors" | "checkDocumentDirty" | "saveDocument" => {
                let (term, group) = terminal.ok_or("Claude Code isn't running in one of den's terminals")?;
                self.host.ask_app(term, &group, vec!["ide".into(), name.into(), arguments.to_string()])
            }
            _ => Err(format!("den doesn't have {name}")),
        }
    }
}

/// The tools Claude Code is told of. `getDiagnostics` and `openDiff` are
/// answered but not listed: den has no diagnostics, and shows no diffs to
/// accept, so the model isn't offered them.
fn tools() -> Value {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": { "type": "object", "properties": properties, "required": required },
        })
    };
    let path = json!({ "filePath": { "type": "string", "description": "Path to the file" } });
    json!([
        tool(
            "openFile",
            "Open a file in the editor and optionally select a range of text",
            json!({
                "filePath": { "type": "string", "description": "Path to the file to open" },
                "preview": { "type": "boolean" },
                "startText": { "type": "string" },
                "endText": { "type": "string" },
                "selectToEndOfLine": { "type": "boolean" },
                "makeFrontmost": { "type": "boolean" },
            }),
            &["filePath"],
        ),
        tool("getCurrentSelection", "Get the current text selection in the active editor", json!({}), &[]),
        tool("getLatestSelection", "Get the most recent text selection (even if not in the active editor)", json!({}), &[]),
        tool("getOpenEditors", "Get information about currently open editors", json!({}), &[]),
        tool("getWorkspaceFolders", "Get all workspace folders currently open in the IDE", json!({}), &[]),
        tool("checkDocumentDirty", "Check if a document has unsaved changes", path.clone(), &["filePath"]),
        tool("saveDocument", "Save a document with unsaved changes", path, &["filePath"]),
        tool(
            "close_tab",
            "Close a tab by name",
            json!({ "tab_name": { "type": "string" } }),
            &["tab_name"],
        ),
        tool("closeAllDiffTabs", "Close all diff tabs in the editor", json!({}), &[]),
    ])
}
