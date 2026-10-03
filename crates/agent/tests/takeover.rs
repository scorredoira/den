//! End-to-end test: an agent of the previous protocol still running when the
//! new one starts (Den was updated) hands its terminals over under the same
//! ids. Its own file, so it runs in its own process (the socket is chosen
//! with an environment variable).

use std::{
    path::Path,
    time::{Duration, Instant},
};

use client::Client;
use proto::{PROTOCOL, Request, Response};

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(5), "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_new_protocol_takes_the_terminals_of_the_old_one() {
    let dir = std::env::temp_dir().join(format!("den-agent-takeover-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let state = dir.join("state");
    let old_socket = state.join(format!("agent-{}.sock", PROTOCOL - 1));
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("DEN_AGENT_SOCKET", &old_socket);
        std::env::set_var("DEN_STATE_DIR", &state);
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));

    // The old agent, on the previous protocol's socket.
    let old = Client::connect_local(agent).unwrap();
    let Response::TermCreated { term } = smol::block_on(old.request(Request::TermCreate {
        group: "takeover".into(),
        cwd: dir.clone(),
        command: None,
        cols: 50,
        rows: 12,
    }))
    .unwrap() else {
        panic!("no terminal");
    };

    // The new one, on its own socket, shuts it down and opens its terminal.
    unsafe { std::env::remove_var("DEN_AGENT_SOCKET") };
    let client = Client::connect_local(agent).unwrap();
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "takeover".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert_eq!(terms.iter().map(|info| info.term).collect::<Vec<_>>(), vec![term]);
    wait_for("the old agent to exit", || smol::block_on(old.request(Request::Version)).is_err());
    assert!(!old_socket.with_extension("restart.json").exists(), "the restart file is used once");

    client.notify(Request::Shutdown);
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
}
