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
    let (tx, rx) = smol::channel::unbounded();
    client.subscribe(term, move |update| {
        let _ = match update {
            TermUpdate::Event(Event::TermOutput { data, .. }) => tx.try_send(PtyEvent::Output(data)),
            TermUpdate::Event(Event::TermExit { .. }) => tx.try_send(PtyEvent::Exit),
            TermUpdate::Disconnected => tx.try_send(PtyEvent::Disconnected),
            TermUpdate::Event(_) => Ok(()),
        };
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

/// Live terminals in a group.
pub async fn list(client: &Client, group: String) -> Result<Vec<TermId>> {
    match client.request(Request::TermList { group }).await? {
        Response::TermList(terms) => Ok(terms.into_iter().map(|info| info.term).collect()),
        other => bail!("unexpected response from the agent: {other:?}"),
    }
}
