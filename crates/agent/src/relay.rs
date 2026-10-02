//! Relays lines of text between a UI and a TCP port on this machine's
//! loopback: how the UI talks to a debugger listening on localhost, the same
//! locally as over SSH.

use std::{
    io::{BufRead, BufReader, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    time::Duration,
};

use anyhow::{Context as _, Result};

pub struct Relay {
    stream: TcpStream,
}

impl Relay {
    /// Connects to `port` and calls `on_line` with each line read (from
    /// another thread), then `on_close` when the other end closes it.
    pub fn connect(
        port: u16,
        mut on_line: impl FnMut(String) + Send + 'static,
        on_close: impl FnOnce() + Send + 'static,
    ) -> Result<Relay> {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(1))
            .with_context(|| format!("nothing listens on port {port}"))?;
        stream.set_nodelay(true)?;

        let reader = stream.try_clone()?;
        std::thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                match line {
                    Ok(line) => on_line(line),
                    Err(_) => break,
                }
            }
            on_close();
        });

        Ok(Relay { stream })
    }

    pub fn send(&mut self, line: &str) -> Result<()> {
        let mut data = Vec::with_capacity(line.len() + 1);
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
        self.stream.write_all(&data)?;
        Ok(())
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc};

    #[test]
    fn relays_lines_both_ways_and_reports_the_close() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let (lines_tx, lines) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let mut relay = Relay::connect(
            port,
            move |line| lines_tx.send(line).unwrap(),
            move || closed_tx.send(()).unwrap(),
        )
        .unwrap();

        // the other end echoes each line upper-cased, then closes
        let (server, _) = listener.accept().unwrap();
        relay.send("hello").unwrap();
        relay.send("world").unwrap();
        let mut writer = server.try_clone().unwrap();
        let mut input = BufReader::new(server);
        for _ in 0..2 {
            let mut line = String::new();
            input.read_line(&mut line).unwrap();
            writer.write_all(line.to_uppercase().as_bytes()).unwrap();
        }
        drop(writer);
        drop(input);

        let wait = Duration::from_secs(5);
        assert_eq!(lines.recv_timeout(wait).unwrap(), "HELLO");
        assert_eq!(lines.recv_timeout(wait).unwrap(), "WORLD");
        closed.recv_timeout(wait).unwrap();
    }

    #[test]
    fn connecting_where_nothing_listens_fails() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(Relay::connect(port, |_| {}, || {}).is_err());
    }
}
