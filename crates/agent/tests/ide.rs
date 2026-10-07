//! End-to-end test: Claude Code in one of the agent's terminals connects to
//! it as to an IDE (`ide.rs`), as Claude Code 2.1 does: the lock file, the
//! token, MCP's handshake and tools, and the selection of the terminal's
//! workspace, sent by the app. The test plays both Claude Code and the app.

#![cfg(unix)]

use std::{
    net::TcpStream,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

use client::Client;
use proto::{Event, IdeSelection, Request, Response};
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, client::IntoClientRequest as _};

type Socket = WebSocket<TcpStream>;

fn connect(port: u16, token: Option<&str>, origin: bool) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://127.0.0.1:{port}").into_client_request().unwrap();
    if let Some(token) = token {
        request.headers_mut().insert("x-claude-code-ide-authorization", token.parse().unwrap());
    }
    if origin {
        request.headers_mut().insert("origin", "https://example.com".parse().unwrap());
    }
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tungstenite::client(request, stream).map(|(socket, _)| socket).map_err(|err| match err {
        tungstenite::HandshakeError::Failure(err) => err,
        tungstenite::HandshakeError::Interrupted(_) => panic!("interrupted handshake"),
    })
}

fn send(socket: &mut Socket, message: Value) {
    socket.send(Message::text(message.to_string())).unwrap();
}

/// The next message that isn't a notification other than `method`, if any.
fn next(socket: &mut Socket) -> Value {
    loop {
        match socket.read().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            _ => continue,
        }
    }
}

fn call(socket: &mut Socket, id: u64, name: &str, arguments: Value) -> String {
    send(socket, json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": arguments } }));
    let reply = next(socket);
    assert_eq!(reply["id"], id, "{reply}");
    reply["result"]["content"][0]["text"].as_str().unwrap().to_string()
}

#[test]
fn claude_code_connects_as_to_an_ide() {
    let dir = std::env::temp_dir().join(format!("den-agent-ide-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("DEN_AGENT_SOCKET", dir.join("agent.sock"));
        std::env::set_var("DEN_STATE_DIR", dir.join("state"));
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));
    let app = Client::connect_local(agent).unwrap();
    let request = |request| smol::block_on(app.request(request));

    // an isolated agent announces itself in its own folder, never in ~/.claude
    let locks: Vec<_> = std::fs::read_dir(dir.join("state/claude-ide")).unwrap().flatten().collect();
    assert_eq!(locks.len(), 1);
    let port: u16 = locks[0].path().file_stem().unwrap().to_string_lossy().parse().unwrap();
    let lock: Value = serde_json::from_slice(&std::fs::read(locks[0].path()).unwrap()).unwrap();
    assert_eq!((lock["ideName"].as_str(), lock["transport"].as_str()), (Some("Den"), Some("ws")));
    let token = lock["authToken"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 32);

    // its terminals say where it listens; this one's shell is the "Claude Code"
    let group = dir.to_string_lossy().into_owned();
    let out = dir.join("out");
    let Ok(Response::TermCreated { .. }) = request(Request::TermCreate {
        group: group.clone(),
        cwd: dir.clone(),
        command: Some(vec!["/bin/sh".into(), "-c".into(), format!("echo $CLAUDE_CODE_SSE_PORT $$ > {}; sleep 30", out.display())]),
        cols: 80,
        rows: 24,
    }) else {
        panic!("no terminal");
    };
    let start = Instant::now();
    let written = loop {
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        if text.ends_with('\n') {
            break text;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "the terminal never wrote");
        std::thread::sleep(Duration::from_millis(20));
    };
    let (env_port, pid) = written.trim().split_once(' ').unwrap();
    assert_eq!(env_port, port.to_string());

    // no token, a wrong one, or from a browser: refused
    assert!(connect(port, None, false).is_err());
    assert!(connect(port, Some("0123456789abcdef0123456789abcdef"), false).is_err());
    assert!(connect(port, Some(&token), true).is_err());

    // MCP's handshake and its tools
    let mut socket = connect(port, Some(&token), false).unwrap();
    send(&mut socket, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "claude-code", "version": "2.1" } } }));
    let reply = next(&mut socket);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert!(reply["result"]["capabilities"]["tools"].is_object());
    send(&mut socket, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    send(&mut socket, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let reply = next(&mut socket);
    let tools: Vec<&str> = reply["result"]["tools"].as_array().unwrap().iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"getCurrentSelection") && tools.contains(&"openFile"), "{tools:?}");
    assert!(!tools.contains(&"getDiagnostics") && !tools.contains(&"openDiff"), "{tools:?}");
    send(&mut socket, json!({ "jsonrpc": "2.0", "id": 3, "method": "nonsense" }));
    assert_eq!(next(&mut socket)["error"]["code"], -32601);

    // a selection the app sends before Claude Code says where it runs is kept
    let file = dir.join("main.rs");
    let selection = IdeSelection { file: file.clone(), text: "fn main".into(), start: (2, 0), end: (2, 7) };
    app.notify(Request::IdeSelection { group: group.clone(), selection: Some(selection) });
    std::thread::sleep(Duration::from_millis(100));
    send(&mut socket, json!({ "jsonrpc": "2.0", "method": "ide_connected", "params": { "pid": pid.parse::<u32>().unwrap() } }));
    let told = next(&mut socket);
    assert_eq!(told["method"], "selection_changed", "{told}");
    assert_eq!(told["params"]["text"], "fn main");
    assert_eq!(told["params"]["filePath"], file.to_string_lossy().as_ref());
    assert_eq!(told["params"]["selection"]["start"], json!({ "line": 2, "character": 0 }));
    assert_eq!(told["params"]["selection"]["end"]["character"], 7);

    // then each change is told; another workspace's is not
    let other = IdeSelection { file: "/elsewhere/a.rs".into(), text: String::new(), start: (0, 0), end: (0, 0) };
    app.notify(Request::IdeSelection { group: "/elsewhere".into(), selection: Some(other) });
    let moved = IdeSelection { file: file.clone(), text: String::new(), start: (9, 4), end: (9, 4) };
    app.notify(Request::IdeSelection { group: group.clone(), selection: Some(moved) });
    let told = next(&mut socket);
    assert_eq!(told["params"]["selection"]["start"]["line"], 9, "{told}");
    assert_eq!(told["params"]["selection"]["isEmpty"], true);
    // with nothing selected, no file: Claude Code would show it in the prompt
    assert!(told["params"].get("filePath").is_none(), "{told}");

    // tools answered by the agent
    let current: Value = serde_json::from_str(&call(&mut socket, 4, "getCurrentSelection", json!({}))).unwrap();
    assert_eq!((current["success"].as_bool(), current["selection"]["start"]["line"].as_u64()), (Some(true), Some(9)));
    assert_eq!(current["filePath"], file.to_string_lossy().as_ref());
    let folders: Value = serde_json::from_str(&call(&mut socket, 5, "getWorkspaceFolders", json!({}))).unwrap();
    assert_eq!(folders["folders"][0]["path"], group.as_str());
    assert_eq!(call(&mut socket, 6, "getDiagnostics", json!({})), "[]");

    // and those the app answers, from the terminal's workspace
    let (tx, commands) = mpsc::channel();
    app.watch(move |event| {
        if let Event::Command { command, args, group, .. } = event {
            let _ = tx.send((*command, args.clone(), group.clone()));
        }
    });
    request(Request::Serve).unwrap();
    send(&mut socket, json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "getOpenEditors", "arguments": {} } }));
    let (command, args, from) = commands.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!((args[..2].to_vec(), from), (vec!["ide".to_string(), "getOpenEditors".to_string()], Some(group.clone())));
    app.notify(Request::CommandDone { command, result: Ok(r#"{"tabs":[]}"#.into()) });
    let reply = next(&mut socket);
    assert_eq!((reply["id"].as_u64(), reply["result"]["content"][0]["text"].as_str()), (Some(7), Some(r#"{"tabs":[]}"#)));

    // the lock file goes with the agent
    request(Request::Shutdown).ok();
    let start = Instant::now();
    while locks[0].path().exists() {
        assert!(start.elapsed() < Duration::from_secs(5), "the lock file stayed");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
