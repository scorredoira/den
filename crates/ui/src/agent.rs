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

/// Connects to the local agent, starting it if needed. Its binary sits next
/// to the app's.
pub fn connect() -> Result<Arc<Client>> {
    let exe = std::env::current_exe()?;
    let agent = exe
        .parent()
        .context("the app is not in a folder")?
        .join(proto::AGENT_BIN);
    Client::connect_local(&agent)
}

struct AgentBackend {
    client: Arc<Client>,
    term: TermId,
    cwd: Arc<Mutex<Option<PathBuf>>>,
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

/// Subscribes to an agent terminal's output and requests its snapshot.
async fn join(
    client: Arc<Client>,
    term: TermId,
) -> Result<(Rc<AgentBackend>, smol::channel::Receiver<PtyEvent>, u16, u16, Vec<u8>)> {
    // Agent output comes in chunks of at most 64 KiB: at most 8 MiB can
    // wait for this terminal's UI. Never block the connection's reader,
    // which also delivers the snapshot awaited below.
    let (tx, rx) = smol::channel::bounded(128);
    let overflow_rx = rx.downgrade();
    let weak = Arc::downgrade(&client);
    client.subscribe(term, move |update| {
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
    let response = client.request(Request::TermAttach { term }).await?;
    let Response::TermSnapshot { cols, rows, data } = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    let backend = Rc::new(AgentBackend {
        client,
        term,
        cwd: Arc::default(),
    });
    Ok((backend, rx, cols, rows, data))
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

/// Reattaches a terminal to its process over a new connection.
pub async fn reattach(client: Arc<Client>, term: TermId, terminal: &Entity<Terminal>, cx: &mut AsyncApp) -> Result<()> {
    let (backend, rx, cols, rows, data) = join(client, term).await?;
    terminal.update(cx, |terminal, cx| terminal.reconnect(backend, rx, cols, rows, &data, cx));
    Ok(())
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
