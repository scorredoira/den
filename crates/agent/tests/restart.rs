//! End-to-end test: restarting the agent (to update it) brings its terminals
//! back under the same ids. Its own file, so it runs in its own process (the
//! socket is chosen with an environment variable).

use std::{
    path::Path,
    time::{Duration, Instant},
};

use client::Client;
use proto::{Request, Response};

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < Duration::from_secs(5), "never arrived: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn terminals_come_back_after_a_restart() {
    let dir = std::env::temp_dir().join(format!("den-agent-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let socket = dir.join("agent.sock");
    // SAFETY: the test is the only thread touching the environment.
    unsafe { std::env::set_var("DEN_AGENT_SOCKET", &socket) };
    unsafe {
        std::env::set_var("DEN_STATE_DIR", dir.join("state"));
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));

    eprintln!("connecting to the agent for restart test");
    let client = Client::connect_local(agent).unwrap();
    let Response::TermCreated { term } = smol::block_on(client.request(Request::TermCreate {
        group: "restart".into(),
        cwd: dir.clone(),
        command: None,
        cols: 50,
        rows: 12,
    }))
    .unwrap() else {
        panic!("no terminal");
    };
    eprintln!("requesting agent shutdown");
    client.notify(Request::Shutdown);
    let saved = socket.with_extension("restart.json");
    wait_for("the restart file", || saved.exists());
    drop(client);
    std::thread::sleep(Duration::from_millis(300));

    // The new agent opens it again under the same id, in the same folder and size.
    eprintln!("connecting to the agent for restart test");
    let client = Client::connect_local(agent).unwrap();
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "restart".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert_eq!(terms.iter().map(|info| info.term).collect::<Vec<_>>(), vec![term]);
    assert!(!saved.exists(), "the restart file is used once");
    let Response::TermSnapshot { cols, rows, .. } = smol::block_on(client.request(Request::TermAttach { term })).unwrap()
    else {
        panic!("no snapshot");
    };
    assert_eq!((cols, rows), (50, 12));
    wait_for("the shell's folder", || {
        matches!(
            smol::block_on(client.request(Request::TermCwd { term })),
            Ok(Response::Path(Some(cwd))) if cwd == dir
        )
    });

    eprintln!("requesting agent shutdown");
    client.notify(Request::Shutdown);
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
}
