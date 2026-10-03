//! End-to-end test: an agent that dies without a restart (killed, the Mac
//! restarted) leaves its terminals for the next one, under the same ids. Its
//! own file, so it runs in its own process (the socket is chosen with an
//! environment variable).

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
fn terminals_come_back_after_the_agent_dies() {
    let dir = std::env::temp_dir().join(format!("den-agent-killed-{}", std::process::id()));
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

    let client = Client::connect_local(agent).unwrap();
    let Response::Hello { pid, .. } = smol::block_on(client.request(Request::Hello { protocol: proto::PROTOCOL })).unwrap()
    else {
        panic!("no hello");
    };
    let Response::TermCreated { term } = smol::block_on(client.request(Request::TermCreate {
        group: "killed".into(),
        cwd: dir.clone(),
        command: None,
        cols: 50,
        rows: 12,
    }))
    .unwrap() else {
        panic!("no terminal");
    };
    // It saves its terminals every few seconds, without being asked.
    let saved = socket.with_extension("restart.json");
    wait_for("the terminal saved", || {
        std::fs::read_to_string(&saved).is_ok_and(|text| text.contains(&format!("\"term\":{term},")))
    });
    std::process::Command::new("kill").args(["-9", &pid.to_string()]).status().unwrap();
    drop(client);
    std::thread::sleep(Duration::from_millis(300));

    let client = Client::connect_local(agent).unwrap();
    let Response::TermList(terms) =
        smol::block_on(client.request(Request::TermList { group: "killed".into() })).unwrap()
    else {
        panic!("no list");
    };
    assert_eq!(terms.iter().map(|info| info.term).collect::<Vec<_>>(), vec![term]);

    client.notify(Request::Shutdown);
    std::thread::sleep(Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&dir);
}
