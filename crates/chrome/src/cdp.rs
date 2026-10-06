//! A connection to Chrome's DevTools Protocol over one WebSocket, with flat
//! sessions: a page's messages carry its `sessionId`. Calls block until
//! Chrome answers; events go to a callback on the reader thread.

use std::{
    collections::HashMap,
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tungstenite::{
    Message, WebSocket,
    protocol::{Role, WebSocketConfig},
};

/// How long a call waits for Chrome by default.
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Event {
    pub method: String,
    pub params: Value,
    pub session: Option<String>,
}

type Reply = mpsc::Sender<Result<Value, String>>;

pub struct Cdp {
    writer: Mutex<WebSocket<TcpStream>>,
    stream: TcpStream,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Reply>>,
    closed: AtomicBool,
    /// `DEN_CHROME_TRACE` set: every message is printed on stderr.
    trace: bool,
}

impl Cdp {
    /// Connects to the browser endpoint `ws://127.0.0.1:{port}{path}`.
    /// `on_event` gets every event; `on_close` is called once when the
    /// connection ends, with the reason.
    pub fn connect(
        port: u16,
        path: &str,
        on_event: impl Fn(Event) + Send + 'static,
        on_close: impl FnOnce(String) + Send + 'static,
    ) -> Result<Arc<Cdp>> {
        let stream =
            TcpStream::connect(("127.0.0.1", port)).with_context(|| format!("connect to Chrome on port {port}"))?;
        stream.set_nodelay(true).context("set TCP_NODELAY")?;
        let url = format!("ws://127.0.0.1:{port}{path}");
        // inline source maps arrive inside events, and can be large
        let config = WebSocketConfig::default().max_message_size(None).max_frame_size(None);
        let (reader, _) = tungstenite::client::client_with_config(
            url.as_str(),
            stream.try_clone().context("clone the socket")?,
            Some(config),
        )
        .map_err(|err| anyhow!("WebSocket handshake with {url}: {err}"))?;
        // Reading and writing go through two WebSockets on the same socket:
        // the reader only reads, the writer only writes. Chrome sends no
        // pings, so the reader never needs to write.
        let writer =
            WebSocket::from_raw_socket(stream.try_clone().context("clone the socket")?, Role::Client, Some(config));
        let cdp = Arc::new(Cdp {
            writer: Mutex::new(writer),
            stream,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            trace: std::env::var_os("DEN_CHROME_TRACE").is_some(),
        });
        let reading = cdp.clone();
        thread::Builder::new()
            .name("cdp-reader".into())
            .spawn(move || {
                let reason = reading.read_loop(reader, &on_event);
                reading.close_all(&reason);
                on_close(reason);
            })
            .context("start the CDP reader")?;
        Ok(cdp)
    }

    fn read_loop(&self, mut socket: WebSocket<TcpStream>, on_event: &dyn Fn(Event)) -> String {
        loop {
            let message = match socket.read() {
                Ok(message) => message,
                Err(err) => return format!("the connection to Chrome ended: {err}"),
            };
            let text = match message {
                Message::Text(text) => text,
                Message::Close(_) => return "Chrome closed the connection".into(),
                _ => continue,
            };
            let value: Value = match serde_json::from_str(text.as_str()) {
                Ok(value) => value,
                Err(err) => {
                    eprintln!("chrome: a message from Chrome is not JSON: {err}");
                    continue;
                }
            };
            if self.trace {
                eprintln!("chrome <- {}", crate::fetch::short(text.as_str()));
            }
            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                let reply = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id);
                let Some(reply) = reply else { continue };
                let result = match value.get("error") {
                    Some(error) => Err(error.get("message").and_then(Value::as_str).unwrap_or("CDP error").to_string()),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                // a caller that timed out is gone; its answer has nowhere to go
                if reply.send(result).is_err() {
                    eprintln!("chrome: an answer from Chrome came after its call gave up (id {id})");
                }
                continue;
            }
            let Some(method) = value.get("method").and_then(Value::as_str) else { continue };
            on_event(Event {
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
                session: value.get("sessionId").and_then(Value::as_str).map(str::to_string),
            });
        }
    }

    fn close_all(&self, reason: &str) {
        self.closed.store(true, Ordering::SeqCst);
        let pending: Vec<Reply> = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, reply)| reply)
            .collect();
        for reply in pending {
            // the caller may have given up already: nothing waits for it then
            let _ = reply.send(Err(reason.to_string()));
        }
    }

    /// Calls a method and waits for its result, `TIMEOUT` at most.
    pub fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        self.call_timeout(session, method, params, Some(TIMEOUT))
    }

    /// Calls a method; `None` waits as long as Chrome takes (an evaluation
    /// that awaits a promise).
    pub fn call_timeout(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let receiver = self.send(session, method, params)?;
        let result = match timeout {
            Some(timeout) => {
                receiver.recv_timeout(timeout).map_err(|_| anyhow!("{method}: Chrome didn't answer in {timeout:?}"))?
            }
            None => receiver.recv().map_err(|_| anyhow!("{method}: the connection to Chrome ended"))?,
        };
        result.map_err(|error| anyhow!("{method}: {error}"))
    }

    /// Sends a call; its answer comes on the receiver.
    pub fn send(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<mpsc::Receiver<Result<Value, String>>> {
        if self.closed.load(Ordering::SeqCst) {
            bail!("{method}: the connection to Chrome is closed");
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = Map::new();
        message.insert("id".into(), json!(id));
        message.insert("method".into(), json!(method));
        message.insert("params".into(), params);
        if let Some(session) = session {
            message.insert("sessionId".into(), json!(session));
        }
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(id, sender);
        let text = Value::Object(message).to_string();
        if self.trace {
            eprintln!("chrome -> {}", crate::fetch::short(&text));
        }
        let sent = self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).send(Message::text(text));
        if let Err(err) = sent {
            self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&id);
            bail!("{method}: send to Chrome: {err}");
        }
        Ok(receiver)
    }

    /// Ends the connection; the reader thread then reports it closed.
    pub fn shutdown(&self) {
        if let Err(err) = self.stream.shutdown(Shutdown::Both)
            && err.kind() != std::io::ErrorKind::NotConnected
        {
            eprintln!("chrome: close the connection to Chrome: {err}");
        }
    }
}
