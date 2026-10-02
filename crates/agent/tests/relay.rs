//! End-to-end test: a UI talks to a program listening on the agent's
//! loopback through a relay, lines both ways, and learns when the program
//! closes it. Its own file, so it runs in its own process (the socket is
//! chosen with an environment variable).

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::Path,
    sync::mpsc,
    time::Duration,
};

use client::{Client, RelayUpdate};

#[test]
fn a_relay_carries_lines_both_ways_until_the_program_closes_it() {
    let dir = std::env::temp_dir().join(format!("sik-agent-relay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    // SAFETY: the test is the only thread touching the environment.
    unsafe {
        std::env::set_var("SIK_AGENT_SOCKET", dir.join("agent.sock"));
        std::env::set_var("SIK_STATE_DIR", dir.join("state"));
        std::env::set_var("SIK_CONFIG_DIR", dir.join("config"));
    }
    let agent = Path::new(env!("CARGO_BIN_EXE_sik-agent"));
    let client = Client::connect_local(agent).unwrap();

    // a program that answers each line upper-cased, then says goodbye and closes
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let program = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut writer = stream.try_clone().unwrap();
        // it speaks first, before being asked: the line must not be lost
        writer.write_all(b"hello\n").unwrap();
        let mut reader = BufReader::new(stream);
        for _ in 0..2 {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            writer.write_all(line.to_uppercase().as_bytes()).unwrap();
        }
    });

    let (tx, updates) = mpsc::channel();
    let relay = smol::block_on(client.connect_relay(port, move |update| {
        let _ = tx.send(match update {
            RelayUpdate::Line(line) => Some(line),
            RelayUpdate::Closed => None,
        });
    }))
    .unwrap();

    client.relay_send(relay, "one".into());
    client.relay_send(relay, "two".into());

    let wait = Duration::from_secs(5);
    assert_eq!(updates.recv_timeout(wait).unwrap().as_deref(), Some("hello"));
    assert_eq!(updates.recv_timeout(wait).unwrap().as_deref(), Some("ONE"));
    assert_eq!(updates.recv_timeout(wait).unwrap().as_deref(), Some("TWO"));
    program.join().unwrap();
    assert_eq!(updates.recv_timeout(wait).unwrap(), None, "the close arrives");

    // nothing listens there now
    assert!(smol::block_on(client.connect_relay(port, |_| {})).is_err());
}
