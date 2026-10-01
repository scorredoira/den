//! End-to-end test: an agent terminal survives the UI disconnecting, and on
//! reconnect it comes back with its contents.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use client::Client;
use proto::{Event, Request, Response};

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(5), "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn terminal_survives_reconnect() {
    let dir = std::env::temp_dir().join(format!("sik-agent-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the test is the only thread touching the environment.
    unsafe { std::env::set_var("SIK_AGENT_SOCKET", dir.join("agent.sock")) };
    unsafe {
        std::env::set_var("SIK_STATE_DIR", dir.join("state"));
        std::env::set_var("SIK_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_sik-agent"));

    // First connection: starts the agent and creates a terminal.
    let client = Client::connect_local(agent).unwrap();
    let term = match smol::block_on(client.request(Request::TermCreate {
        group: "test".into(),
        cwd: dir.clone(),
        command: Some(if cfg!(windows) {
            vec!["cmd.exe".into(), "/Q".into(), "/K".into(), "echo hello".into()]
        } else {
            vec!["/bin/sh".into(), "-c".into(), "echo hello; exec cat".into()]
        }),
        cols: 40,
        rows: 10,
    }))
    .unwrap()
    {
        Response::TermCreated { term } => term,
        other => panic!("unexpected response: {other:?}"),
    };

    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    client.subscribe(term, {
        let output = output.clone();
        move |update| {
            if let client::TermUpdate::Event(Event::TermOutput { data, .. }) = update {
                output.lock().unwrap().extend(data);
            }
        }
    });
    let Response::TermSnapshot { .. } = smol::block_on(client.request(Request::TermAttach { term })).unwrap() else {
        panic!("no snapshot");
    };
    client.notify(Request::TermInput {
        term,
        data: b"bye\n".to_vec(),
    });
    wait_for("the input echo", || {
        String::from_utf8_lossy(&output.lock().unwrap()).contains("bye")
    });
    drop(client);

    // Second connection: the terminal is still there and the snapshot has what came before.
    let client = Client::connect_local(agent).unwrap();
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "test".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert_eq!(terms.iter().map(|info| info.term).collect::<Vec<_>>(), vec![term]);
    let Response::TermSnapshot { cols, rows, data } =
        smol::block_on(client.request(Request::TermAttach { term })).unwrap()
    else {
        panic!("no snapshot");
    };
    assert_eq!((cols, rows), (40, 10));
    let screen = String::from_utf8_lossy(&data);
    assert!(screen.contains("hello") && screen.contains("bye"), "snapshot: {screen:?}");

    // Killing it removes it from the list; then the agent shuts down.
    smol::block_on(client.request(Request::TermKill { term })).unwrap();
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "test".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert!(terms.is_empty());
    // A pasted image is saved to a file on the agent.
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let Response::Path(Some(path)) = smol::block_on(client.request(Request::SavePastedImage {
        extension: "png".into(),
        data: png.clone(),
    }))
    .unwrap() else {
        panic!("no path");
    };
    assert_eq!(path.extension().unwrap(), "png");
    assert_eq!(std::fs::read(&path).unwrap(), png);
    let _ = std::fs::remove_file(path);

    // Files through the agent: write, list and hear about changes.
    let (tx, rx) = std::sync::mpsc::channel();
    client.watch(move |event| {
        if let Event::FsChanged { paths, .. } = event {
            let _ = tx.send(paths.clone());
        }
    });
    smol::block_on(client.request(Request::Watch { path: dir.clone() })).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    smol::block_on(client.request(Request::WriteFile {
        path: dir.join("note.txt"),
        data: b"hello".to_vec(),
    }))
    .unwrap();
    let Response::Dir(entries) = smol::block_on(client.request(Request::ListDir { path: dir.clone() })).unwrap() else {
        panic!("no listing");
    };
    assert!(entries.iter().any(|entry| entry.name == "note.txt" && !entry.is_dir));
    let changed = rx.recv_timeout(Duration::from_secs(5)).expect("FsChanged never arrived");
    assert!(changed.iter().any(|path| path.ends_with("note.txt")), "{changed:?}");

    // The agent's fingerprint is its binary's (it starts from an identical copy).
    let Response::Text(id) = smol::block_on(client.request(Request::Version)).unwrap() else {
        panic!("no version");
    };
    assert_eq!(id, proto::build_id(&std::fs::read(agent).unwrap()));
    assert!(!client.outdated());

    client.notify(Request::Shutdown);
    let _ = std::fs::remove_dir_all(&dir);
}
