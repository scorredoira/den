//! A slow worktree refresh must not hold up the connection's other requests.
#![cfg(unix)]

use std::{
    os::unix::{fs::PermissionsExt, net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use proto::{ClientMessage, Request, Response, ServerMessage};

struct Agent(Child);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn requests_overtake_a_slow_task_list() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("agent.sock");
    let config = dir.path().join("config");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(config.join("repos.json"), serde_json::to_vec(&[dir.path()]).unwrap()).unwrap();
    // Hold the worktree query until the test has received another response.
    let script = bin.join("git");
    std::fs::write(&script, "#!/bin/sh\n: > started\nwhile [ ! -f release ]; do sleep 0.02; done\nexit 1\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()))).unwrap();
    let _agent = Agent(Command::new(env!("CARGO_BIN_EXE_sik-agent"))
        .arg("daemon")
        .env("SIK_AGENT_SOCKET", &socket)
        .env("SIK_CONFIG_DIR", &config)
        .env("SIK_STATE_DIR", dir.path().join("state"))
        .env("PATH", path)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap());
    let start = Instant::now();
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) { break stream; }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    };
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    proto::write_frame(&mut stream, &ClientMessage { id: Some(1), request: Request::TaskList }).unwrap();
    while !dir.path().join("started").exists() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    proto::write_frame(&mut stream, &ClientMessage { id: Some(2), request: Request::Hello { protocol: proto::PROTOCOL } }).unwrap();
    let response = proto::read_frame::<ServerMessage>(&mut stream);
    // Release the fake git even when the assertion below fails.
    std::fs::write(dir.path().join("release"), "").unwrap();
    assert!(matches!(response.unwrap(), Some(ServerMessage::Response { id: 2, result: Ok(Response::Hello { .. }) })));
    assert!(matches!(proto::read_frame::<ServerMessage>(&mut stream).unwrap(),
        Some(ServerMessage::Response { id: 1, result: Ok(Response::Tasks(_)) })));
}
