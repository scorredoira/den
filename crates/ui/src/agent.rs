//! Terminals that live in the local agent.

use std::{
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use client::{Client, TermUpdate};
use gpui_kit::{AppContext as _, AsyncApp, Entity};
use proto::{Event, Request, Response, TermId};
use ui_term::{PtyEvent, Terminal, TerminalBackend};

/// The folder with this build's agents (`den-agent`, and those uploaded to
/// servers), read once: an update installed while it runs puts the next
/// build's where this one's were, and keeps this one's aside until the
/// restart (see `update`).
static AGENTS: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where this build's agents are: next to the app, as it was when it started.
pub fn agents_dir() -> Result<PathBuf> {
    let mut agents = AGENTS.lock().unwrap();
    if agents.is_none() {
        let exe = std::env::current_exe()?;
        *agents = Some(exe.parent().context("the app is not in a folder")?.to_path_buf());
    }
    Ok(agents.clone().unwrap_or_default())
}

/// An update took this build's place: its agents are now in `dir`.
pub fn agents_moved(dir: PathBuf) {
    *AGENTS.lock().unwrap() = Some(dir);
}

/// Connects to the local agent, starting it if needed.
pub fn connect() -> Result<Arc<Client>> {
    Client::connect_local(&agents_dir()?.join(proto::AGENT_BIN))
}

struct AgentBackend {
    client: Arc<Client>,
    term: TermId,
    /// This terminal's subscription to the process's output (`Client::subscribe`).
    subscription: u64,
    cwd: Arc<Mutex<Option<PathBuf>>>,
}

/// The terminal is gone (or moved to another connection): the output stops
/// coming here, without touching another view of the same process.
impl Drop for AgentBackend {
    fn drop(&mut self) {
        self.client.unsubscribe(self.term, self.subscription);
    }
}

impl TerminalBackend for AgentBackend {
    fn write(&self, bytes: Vec<u8>) {
        self.client.notify(Request::TermInput {
            term: self.term,
            data: bytes,
        });
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.client.notify(Request::TermResize {
            term: self.term,
            cols,
            rows,
        });
    }

    fn kill(&self) {
        self.client.notify(Request::TermKill { term: self.term });
    }

    fn save_image(
        &self,
        extension: &str,
        data: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<PathBuf>>>> {
        let response = self.client.request(Request::SavePastedImage {
            extension: extension.to_string(),
            data,
        });
        Box::pin(async move {
            match response.await? {
                Response::Path(Some(path)) => Ok(path),
                other => bail!("unexpected response from the agent: {other:?}"),
            }
        })
    }

    /// An agent from before `TermClear` answers it with an error.
    fn clear(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<()>>>> {
        let response = self.client.request(Request::TermClear { term: self.term });
        Box::pin(async move {
            match response.await? {
                Response::Ok => Ok(()),
                other => bail!("unexpected response from the agent: {other:?}"),
            }
        })
    }

    /// Returns the last known directory and asks the agent for the current one.
    fn cwd(&self) -> Option<PathBuf> {
        let cache = self.cwd.clone();
        self.client
            .request_with(Request::TermCwd { term: self.term }, move |response| {
                if let Ok(Response::Path(path)) = response {
                    *cache.lock().unwrap() = path;
                }
            });
        self.cwd.lock().unwrap().clone()
    }
}

/// Creates a terminal in the agent and attaches to it.
pub async fn create(
    client: Arc<Client>,
    group: String,
    cwd: PathBuf,
    (cols, rows): (u16, u16),
    cx: &mut AsyncApp,
) -> Result<(TermId, Entity<Terminal>)> {
    let response = client
        .request(Request::TermCreate {
            group,
            cwd,
            command: None,
            cols,
            rows,
        })
        .await?;
    let Response::TermCreated { term } = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    Ok((term, attach(client, term, cx).await?))
}

type Joined = (Rc<AgentBackend>, smol::channel::Receiver<PtyEvent>, u16, u16, Vec<u8>);

/// Subscribes to an agent terminal's output and requests its snapshot, right
/// away (several are asked for at once): the snapshot comes in the future.
fn join(client: Arc<Client>, term: TermId) -> impl std::future::Future<Output = Result<Joined>> {
    // Agent output comes in chunks of at most 256 KiB, usually far less: a
    // UI busy for a moment doesn't lose the terminal. Never block the
    // connection's reader, which also delivers the snapshot awaited below.
    let (tx, rx) = smol::channel::bounded(1024);
    let overflow_rx = rx.downgrade();
    let weak = Arc::downgrade(&client);
    let subscription = client.subscribe(term, move |update| {
        let Some(overflow_rx) = overflow_rx.upgrade() else { return };
        let event = match update {
            TermUpdate::Event(Event::TermOutput { data, .. }) => PtyEvent::Output(data),
            TermUpdate::Event(Event::TermExit { .. }) => PtyEvent::Exit,
            TermUpdate::Disconnected => PtyEvent::Disconnected,
            TermUpdate::Event(_) => return,
        };
        if !queue_output(&tx, &overflow_rx, event)
            && let Some(client) = weak.upgrade()
        {
            client.disconnect();
        }
    });
    // Made first: if attaching fails, dropping it ends the subscription.
    let backend = Rc::new(AgentBackend {
        client: client.clone(),
        term,
        subscription,
        cwd: Arc::default(),
    });
    let response = client.request(Request::TermAttach { term });
    async move {
        let response = response.await?;
        let Response::TermSnapshot { cols, rows, data } = response else {
            bail!("unexpected response from the agent: {response:?}");
        };
        Ok((backend, rx, cols, rows, data))
    }
}

/// On overflow the partial screen is no longer trustworthy. Tell the
/// terminal it's disconnected and request a fresh snapshot on reconnect.
fn queue_output(tx: &smol::channel::Sender<PtyEvent>, rx: &smol::channel::Receiver<PtyEvent>, event: PtyEvent) -> bool {
    match tx.try_send(event) {
        Ok(()) | Err(smol::channel::TrySendError::Closed(_)) => true,
        Err(smol::channel::TrySendError::Full(_)) => {
            while rx.try_recv().is_ok() {}
            let _ = tx.try_send(PtyEvent::Disconnected);
            tx.close();
            false
        }
    }
}

/// Attaches to an agent terminal: receives its snapshot and then its output.
pub async fn attach(client: Arc<Client>, term: TermId, cx: &mut AsyncApp) -> Result<Entity<Terminal>> {
    let (backend, rx, cols, rows, data) = join(client, term).await?;
    Ok(cx.new(|cx| Terminal::new(backend, rx, cols, rows, &data, cx)))
}

/// Attaches to several terminals at once: all are asked for together, so
/// they take one round trip, not one each.
pub async fn attach_all(client: &Arc<Client>, terms: &[TermId], cx: &mut AsyncApp) -> Vec<(TermId, Result<Entity<Terminal>>)> {
    let pending: Vec<_> = terms.iter().map(|&term| (term, join(client.clone(), term))).collect();
    let mut attached = Vec::new();
    for (term, joined) in pending {
        let terminal = match joined.await {
            Ok((backend, rx, cols, rows, data)) => Ok(cx.new(|cx| Terminal::new(backend, rx, cols, rows, &data, cx))),
            Err(err) => Err(err),
        };
        attached.push((term, terminal));
    }
    attached
}

/// Reattaches a terminal to its process over a new connection.
/// Reattaches terminals to their processes after reconnecting, all asked
/// for at once. Returns those that couldn't be.
pub async fn reattach_all(client: &Arc<Client>, terminals: Vec<(TermId, Entity<Terminal>)>, cx: &mut AsyncApp) -> Vec<TermId> {
    let pending: Vec<_> = terminals.into_iter().map(|(term, terminal)| (term, terminal, join(client.clone(), term))).collect();
    let mut gone = Vec::new();
    for (term, terminal, joined) in pending {
        match joined.await {
            Ok((backend, rx, cols, rows, data)) => {
                terminal.update(cx, |terminal, cx| terminal.reconnect(backend, rx, cols, rows, &data, cx));
            }
            Err(_) => gone.push(term),
        }
    }
    gone
}

/// The directory a terminal's shell (or what runs in it) is in.
pub async fn cwd(client: &Client, term: TermId) -> Result<Option<PathBuf>> {
    match client.request(Request::TermCwd { term }).await? {
        Response::Path(path) => Ok(path),
        other => bail!("unexpected response from the agent: {other:?}"),
    }
}

/// Live terminals in a group.
pub async fn list(client: &Client, group: String) -> Result<Vec<TermId>> {
    match client.request(Request::TermList { group }).await? {
        Response::TermList(terms) => Ok(terms.into_iter().map(|info| info.term).collect()),
        other => bail!("unexpected response from the agent: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_overflow_requests_a_snapshot_instead_of_losing_bytes_silently() {
        let (tx, rx) = smol::channel::bounded(2);
        assert!(queue_output(&tx, &rx, PtyEvent::Output(vec![1])));
        assert!(queue_output(&tx, &rx, PtyEvent::Output(vec![2])));
        assert!(!queue_output(&tx, &rx, PtyEvent::Output(vec![3])));
        assert!(matches!(rx.try_recv(), Ok(PtyEvent::Disconnected)));
        assert!(rx.is_closed());
        assert!(rx.try_recv().is_err());
    }
}
