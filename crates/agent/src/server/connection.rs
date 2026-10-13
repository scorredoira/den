//! A connection with a UI: one thread reads its requests, another writes
//! to it what its outbox holds, which never waits for a slow one.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use proto::{ClientEnvelope, ClientMessage, Decoded};

use crate::platform::Stream;
use super::*;

/// What limits a slow connection is `MAX_PENDING_OUTPUT`: a burst of small
/// reads from a terminal is many messages but few bytes.
const MAX_PENDING_MESSAGES: usize = 8192;
/// Output of one terminal waiting together is sent in one message up to this.
const MAX_JOINED_OUTPUT: usize = 256 * 1024;
const MAX_PENDING_OUTPUT: usize = 8 * 1024 * 1024;

/// Never wait for a slow connection while holding the agent's state lock.
/// Closing it lets the UI reconnect from a snapshot; the terminals live on.
#[derive(Clone)]
pub(super) struct Outbox {
    sender: mpsc::SyncSender<QueuedMessage>,
    output_bytes: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    close: Arc<dyn Fn() + Send + Sync>,
}

pub(super) struct QueuedMessage {
    pub(super) message: ServerMessage,
    bytes: usize,
    output_bytes: Arc<AtomicUsize>,
}

impl Drop for QueuedMessage {
    fn drop(&mut self) {
        self.output_bytes.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl Outbox {
    pub(super) fn new(close: Arc<dyn Fn() + Send + Sync>) -> (Self, mpsc::Receiver<QueuedMessage>) {
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_MESSAGES);
        (Self { sender, output_bytes: Arc::default(), closed: Arc::default(), close }, receiver)
    }

    fn disconnect(&self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            (self.close)();
        }
    }

    pub(super) fn send(&self, message: ServerMessage) -> Result<(), ()> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(());
        }
        let bytes = match &message {
            ServerMessage::Event(Event::TermOutput { data, .. }) => data.len(),
            _ => 0,
        };
        if self.output_bytes.try_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
            pending.checked_add(bytes).filter(|bytes| *bytes <= MAX_PENDING_OUTPUT)
        }).is_err() {
            self.disconnect();
            return Err(());
        }
        let message = QueuedMessage { message, bytes, output_bytes: self.output_bytes.clone() };
        if self.sender.try_send(message).is_err() {
            self.disconnect();
            return Err(());
        }
        Ok(())
    }
}

pub(super) fn serve(mut stream: Box<dyn Stream>, state: Shared) -> Result<()> {
    let mut writer = stream.try_clone_stream()?;
    let (tx, rx) = Outbox::new(stream.close_handle()?);
    let writer_closed = tx.closed.clone();
    let close_writer = tx.close.clone();
    let conn = {
        let mut state = state.locked();
        let conn = state.next_conn;
        state.next_conn += 1;
        state.clients.insert(conn, tx);
        state.update_idle();
        conn
    };

    std::thread::spawn(move || {
        let mut next = None;
        loop {
            let mut message = match next.take() {
                Some(message) => message,
                None => match rx.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
            };
            // A terminal's output with more of it already waiting behind:
            // one message, one write, one wake of the UI.
            if let ServerMessage::Event(Event::TermOutput { term, data }) = &mut message.message {
                while data.len() < MAX_JOINED_OUTPUT {
                    let Ok(mut more) = rx.try_recv() else {
                        break;
                    };
                    match &mut more.message {
                        ServerMessage::Event(Event::TermOutput { term: other, data: more_data }) if other == term => {
                            data.append(more_data);
                        }
                        _ => {
                            next = Some(more);
                            break;
                        }
                    }
                }
            }
            if write_message(&mut writer, &message.message).is_err() {
                if !writer_closed.swap(true, Ordering::Relaxed) {
                    close_writer();
                }
                break;
            }
        }
    });

    let result = (|| -> Result<()> {
        while let Some(decoded) = proto::read_message::<ClientMessage, ClientEnvelope>(&mut stream)? {
            let mut message = match decoded {
                Decoded::Known(message) => message,
                // From a newer UI: rejected without dropping the connection.
                Decoded::Unknown { id } => {
                    if let Some(id) = id {
                        let result = Err("this agent does not know the request: close it so a new one starts".into());
                        state.locked().send(conn, ServerMessage::Response { id, result });
                    }
                    continue;
                }
            };
            let id = message.id;
            own_separators(&mut message.request);
            // Answered when the app is done, not now.
            if let Request::Command { args, cwd, term } = message.request {
                if let Err(err) = send_command(&state, Asker::Request(conn, id), args, cwd, term) {
                    reply_to(&state, conn, id, Err(err));
                }
                continue;
            }
            match caught(|| handle(&state, conn, message.request)) {
                Ok(Reply::Now(response)) => reply_to(&state, conn, id, Ok(response)),
                Ok(Reply::Later(work)) => {
                    let state = state.clone();
                    std::thread::spawn(move || reply_to(&state, conn, id, caught(work)));
                }
                Err(err) => reply_to(&state, conn, id, Err(err)),
            }
        }
        Ok(())
    })();

    let mut state = state.locked();
    if let Some(client) = state.clients.remove(&conn) {
        client.disconnect();
    }
    state.watchers.remove(&conn);
    state.relays.remove(&conn);
    state.apps.retain(|app| *app != conn);
    // What that app was running won't be answered.
    let lost: Vec<_> = state.commands.extract_if(|_, (app, _)| *app == conn).collect();
    for (_, (_, asker)) in lost {
        asker.answer(&state, Err("the app closed before answering".into()));
    }
    for entry in state.terms.values() {
        entry.locked().subscribers.remove(&conn);
    }
    state.update_idle();
    result
}

fn reply_to(state: &Shared, conn: ConnId, id: Option<u64>, reply: Result<Response>) {
    if let Some(id) = id {
        let result = reply.map_err(|err| format!("{err:#}"));
        state.locked().send(conn, ServerMessage::Response { id, result });
    }
}

/// An oversized response fails its request without disconnecting all terminals.
fn write_message(writer: &mut impl std::io::Write, message: &ServerMessage) -> Result<()> {
    match proto::write_frame(writer, message) {
        Err(err) if err.is::<proto::FrameTooLarge>() => {
            if let ServerMessage::Response { id, .. } = message {
                proto::write_frame(writer, &ServerMessage::Response { id: *id, result: Err(err.to_string()) })
            } else {
                Err(err)
            }
        }
        result => result,
    }
}

/// A UI on Windows builds this machine's paths with its own separator
/// (`~\Downloads`, `/home/me/repo\src`): on Linux and macOS they become `/`.
/// A `\` in a file name there is rare enough to give up.
pub(super) fn own_separators(request: &mut Request) {
    if cfg!(windows) {
        return;
    }
    let fix = |path: &mut PathBuf| {
        if let Some(text) = path.to_str()
            && text.contains('\\')
        {
            *path = PathBuf::from(text.replace('\\', "/"));
        }
    };
    match request {
        Request::TermCreate { cwd: path, .. }
        | Request::RepoAdd { path }
        | Request::RepoRemove { path }
        | Request::TaskCreate { repo: path, .. }
        | Request::TaskRemove { path }
        | Request::TaskForceRemove { path }
        | Request::GitChanges { path, .. }
        | Request::GitDiff { path, .. }
        | Request::FindFiles { path }
        | Request::Search { path, .. }
        | Request::ReadFile { path }
        | Request::WriteFile { path, .. }
        | Request::ListDir { path }
        | Request::ListDirAll { path }
        | Request::CreateFile { path }
        | Request::CreateDir { path }
        | Request::Trash { path }
        | Request::Watch { path }
        | Request::Unwatch { path }
        | Request::Git { path, .. }
        | Request::Replace { path, .. }
        | Request::Command { cwd: path, .. }
        | Request::ReplaceAgent { exe: path }
        | Request::Resolve { path } => fix(path),
        Request::Rename { from, to } | Request::Copy { from, to } | Request::Untrash { item: from, to } => {
            fix(from);
            fix(to);
        }
        Request::Lsp { root, path, .. } | Request::LspResolve { root, path, .. } | Request::Format { root, path, .. } => {
            fix(root);
            fix(path);
        }
        Request::LspWorkspaceSymbols { root, path, .. } => {
            fix(root);
            if let Some(path) = path {
                fix(path);
            }
        }
        Request::Open { root, file } => {
            fix(root);
            if let Some(file) = file {
                fix(file);
            }
        }
        Request::Hello { .. }
        | Request::Shutdown
        | Request::TermList { .. }
        | Request::TermAttach { .. }
        | Request::TermDetach { .. }
        | Request::TermInput { .. }
        | Request::TermResize { .. }
        | Request::TermKill { .. }
        | Request::TermClear { .. }
        | Request::TermCwd { .. }
        | Request::SavePastedImage { .. }
        | Request::RepoList
        | Request::TaskList
        | Request::Version
        | Request::BlockedList
        | Request::AgentList
        | Request::Ports
        | Request::RelayConnect { .. }
        | Request::RelaySend { .. }
        | Request::RelayClose { .. }
        | Request::Serve
        | Request::CommandDone { .. }
        | Request::TermRead { .. }
        | Request::TermBusy { .. }
        | Request::FreePort => {}
        Request::IdeSelection { selection, .. } => {
            if let Some(selection) = selection {
                fix(&mut selection.file);
            }
        }
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn output_overflow_wakes_the_socket_threads_and_cleans_the_connection() {
        let (stream, _unread_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let state = Shared::default();
        let shared = state.clone();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || { let _ = done.send(serve(Box::new(stream), shared)); });
        let started = Instant::now();
        let outbox = loop {
            if let Some(outbox) = state.locked().clients.values().next().cloned() {
                break outbox;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        };
        // The peer doesn't read, so the writer must eventually block. The
        // next overflow has to wake both it and serve's idle socket reader.
        for _ in 0..8 {
            if outbox.send(ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT / 2] })).is_err() {
                break;
            }
        }
        finished.recv_timeout(Duration::from_secs(2)).expect("connection stayed blocked").unwrap();
        assert!(state.locked().clients.is_empty());
    }

    #[test]
    fn slow_consumers_disconnect_without_blocking_other_connections() {
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let (outbox, pending) = Outbox::new(Arc::new(move || { flag.store(true, Ordering::Relaxed); }));
        let (other, other_pending) = Outbox::new(Arc::new(|| {}));
        let output = || ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT / 2] });
        outbox.send(output()).unwrap();
        outbox.send(output()).unwrap();
        assert!(outbox.send(output()).is_err());
        assert!(closed.load(Ordering::Relaxed));
        assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), MAX_PENDING_OUTPUT);
        other.send(ServerMessage::Response { id: 1, result: Ok(Response::Ok) }).unwrap();
        assert!(other_pending.try_recv().is_ok());
        drop(pending);
        assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bounds_small_messages_and_releases_consumed_output() {
        let (outbox, pending) = Outbox::new(Arc::new(|| {}));
        for _ in 0..MAX_PENDING_MESSAGES {
            outbox.send(ServerMessage::Response { id: 1, result: Ok(Response::Ok) }).unwrap();
        }
        assert!(outbox.send(ServerMessage::Response { id: 2, result: Ok(Response::Ok) }).is_err());
        assert!(outbox.closed.load(Ordering::Relaxed));
        drop(pending);
        let (outbox, pending) = Outbox::new(Arc::new(|| {}));
        for _ in 0..3 {
            outbox.send(ServerMessage::Event(Event::TermOutput { term: 1, data: vec![0; MAX_PENDING_OUTPUT] })).unwrap();
            drop(pending.recv().unwrap());
            assert_eq!(outbox.output_bytes.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn oversized_response_returns_an_error_and_keeps_the_stream_usable() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &ServerMessage::Response {
            id: 7,
            result: Ok(Response::Text("x".repeat(64 * 1024 * 1024))),
        }).unwrap();
        write_message(&mut bytes, &ServerMessage::Response { id: 8, result: Ok(Response::Ok) }).unwrap();
        let mut reader = bytes.as_slice();
        assert!(matches!(proto::read_frame(&mut reader).unwrap(),
            Some(ServerMessage::Response { id: 7, result: Err(error) }) if error.contains("frame too large")));
        assert!(matches!(proto::read_frame(&mut reader).unwrap(),
            Some(ServerMessage::Response { id: 8, result: Ok(Response::Ok) })));
    }
}

#[cfg(all(test, not(windows)))]
mod path_tests {
    use super::*;

    #[test]
    fn paths_from_a_windows_ui_use_this_machines_separator() {
        let mut request = Request::Rename {
            from: PathBuf::from("/home/me/repo\\src\\a.rs"),
            to: PathBuf::from("~\\Downloads\\a.rs"),
        };
        own_separators(&mut request);
        let Request::Rename { from, to } = request else { unreachable!() };
        assert_eq!(from, PathBuf::from("/home/me/repo/src/a.rs"));
        assert_eq!(to, PathBuf::from("~/Downloads/a.rs"));
    }
}
