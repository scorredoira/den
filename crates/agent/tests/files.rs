//! End-to-end test: the files panel's copy goes through the agent, never over
//! anything. Its own file, so it runs in its own process (the socket is
//! chosen with an environment variable).

use std::path::Path;

use client::Client;
use proto::{Request, Response};

#[test]
fn copies_go_next_to_what_they_would_overwrite() {
    let dir = std::env::temp_dir().join(format!("den-agent-files-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work/src")).unwrap();
    let dir = dir.canonicalize().unwrap();
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("DEN_AGENT_SOCKET", dir.join("agent.sock"));
        std::env::set_var("DEN_STATE_DIR", dir.join("state"));
        std::env::set_var("DEN_CONFIG_DIR", dir.join("config"));
    }
    let work = dir.join("work");
    std::fs::write(work.join("src/a.txt"), "a").unwrap();
    let agent = Path::new(env!("CARGO_BIN_EXE_den-agent"));
    let client = Client::connect_local(agent).unwrap();
    let request = |request| smol::block_on(client.request(request));
    let copy = |from: &str, to: &str| match request(Request::Copy { from: work.join(from), to: work.join(to) }) {
        Ok(Response::Path(Some(path))) => path,
        other => panic!("{other:?}"),
    };

    assert_eq!(copy("src/a.txt", "a.txt"), work.join("a.txt"));
    assert_eq!(copy("src/a.txt", "a.txt"), work.join("a copy.txt"));
    assert_eq!(copy("src", "src"), work.join("src copy"));
    assert_eq!(std::fs::read_to_string(work.join("src copy/a.txt")).unwrap(), "a");
    assert!(request(Request::Copy { from: work.join("src"), to: work.join("src/inner") }).is_err());

    request(Request::Shutdown).ok();
    let _ = std::fs::remove_dir_all(&dir);
}
