//! End-to-end test: an agent replaced by a new build keeps its terminals
//! running, with their screens, in the same process. Its own file, so it
//! runs in its own process (the socket is chosen with an environment
//! variable).
#![cfg(unix)]

use std::{
    path::Path,
    time::{Duration, Instant},
};

use client::Client;
use proto::{PROTOCOL, Request, Response, TermId};

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(5), "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn text(client: &Client, term: TermId) -> String {
    match smol::block_on(client.request(Request::TermRead { term, lines: 50 })) {
        Ok(Response::Text(text)) => text,
        _ => String::new(),
    }
}

fn pid(client: &Client) -> u32 {
    match smol::block_on(client.request(Request::Hello { protocol: PROTOCOL })).unwrap() {
        Response::Hello { pid, .. } => pid,
        other => panic!("no hello: {other:?}"),
    }
}

#[test]
fn a_replaced_agent_keeps_its_terminals_running() {
    let dir = std::env::temp_dir().join(format!("den-agent-replace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let socket = dir.join("agent.sock");
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("DEN_AGENT_SOCKET", &socket);
        std::env::set_var("DEN_STATE_DIR", dir.join("state"));
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));

    let client = Client::connect_local(agent).unwrap();
    let before = pid(&client);
    let Response::TermCreated { term } = smol::block_on(client.request(Request::TermCreate {
        group: "replace".into(),
        cwd: dir.clone(),
        command: Some(vec!["/bin/sh".into()]),
        cols: 60,
        rows: 12,
    }))
    .unwrap() else {
        panic!("no terminal");
    };
    // Only the shell that ran this knows the value.
    client.notify(Request::TermInput { term, data: b"KEPT=yes; echo before-$KEPT\r".to_vec() });
    wait_for("the shell's output", || text(&client, term).contains("before-yes"));

    let replaced = client.request(Request::ReplaceAgent { exe: agent.to_path_buf() });
    assert!(smol::block_on(replaced).is_err(), "the connection closes");
    drop(client);

    let client = Client::connect_local(agent).unwrap();
    assert_eq!(pid(&client), before, "the same process");
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "replace".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert_eq!(terms.iter().map(|info| info.term).collect::<Vec<_>>(), vec![term]);
    assert!(text(&client, term).contains("before-yes"), "the screen came along");
    client.notify(Request::TermInput { term, data: b"echo after-$KEPT\r".to_vec() });
    wait_for("the same shell", || text(&client, term).contains("after-yes"));
    // Not reopened from the restart file as well.
    let Response::TermCreated { term: next } = smol::block_on(client.request(Request::TermCreate {
        group: "replace".into(),
        cwd: dir.clone(),
        command: Some(vec!["/bin/sh".into()]),
        cols: 60,
        rows: 12,
    }))
    .unwrap() else {
        panic!("no terminal");
    };
    assert!(next > term, "ids go on");

    client.notify(Request::Shutdown);
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
}
