//! An agent of an earlier protocol that doesn't shut down when asked must
//! not keep the new one from starting.
#![cfg(unix)]

use std::{
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use proto::{ClientMessage, PROTOCOL, Request, Response, ServerMessage};

struct Agent(Child);
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_hung_older_agent_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // Takes the request to shut down, and never answers or closes.
    let old = UnixListener::bind(state.join(format!("agent-{}.sock", PROTOCOL - 1))).unwrap();
    std::thread::spawn(move || {
        let held: Vec<UnixStream> = old.incoming().flatten().collect();
        drop(held);
    });
    let _agent = Agent(Command::new(env!("CARGO_BIN_EXE_den-agent"))
        .arg("daemon")
        .env("DEN_STATE_DIR", &state)
        .env("DEN_CONFIG_DIR", dir.path().join("config"))
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().unwrap());
    let start = Instant::now();
    let socket = state.join(format!("agent-{PROTOCOL}.sock"));
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket) { break stream; }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    };
    stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    proto::write_frame(&mut stream, &ClientMessage { id: Some(1), request: Request::Hello { protocol: PROTOCOL } }).unwrap();
    assert!(matches!(proto::read_frame::<ServerMessage>(&mut stream).unwrap(),
        Some(ServerMessage::Response { id: 1, result: Ok(Response::Hello { .. }) })));
}
