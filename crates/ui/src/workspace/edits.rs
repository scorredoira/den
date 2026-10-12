//! `den edit`: a tab with text a program passed in and no file behind it.
//! Saving hands the text back to the program, which answers whether it
//! kept it; the text is never written to disk, nor remembered with what's
//! open, nor sent to a language server.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::*;

/// What the program hears from its tab.
pub(crate) enum EditEvent {
    /// Saved: the text to keep.
    Saved(String),
    /// Closed after `--idle` without edits.
    Idle,
}

/// The tab's end of a `den edit`.
pub(super) struct External {
    id: u64,
    events: smol::channel::Sender<EditEvent>,
    last_edit: Instant,
    /// The text handed over on saving, waiting for the program's answer.
    pending: Option<String>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// An edit's tab, and its views, have a path under this, which no file has.
const EDIT_PATH: &str = "den:edit";

impl FileTab {
    /// An edit's tab or a view of it: nothing of it leaves den but to the
    /// program.
    pub(super) fn is_edit(&self) -> bool {
        self.path.starts_with(EDIT_PATH)
    }
}

impl Workspace {
    /// Opens `text` in a tab named `title`; returns its id, for the program
    /// to hear its saves with `edit_events`. With `idle`, it closes after
    /// that long without edits if nothing is unsaved.
    pub fn open_edit(&mut self, title: &str, idle: Option<Duration>, text: String, window: &mut Window, cx: &mut Context<Self>) -> u64 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (events, receiver) = smol::channel::unbounded();
        self.edits.insert(id, receiver);
        let path = PathBuf::from(format!("{EDIT_PATH}/{id}/{title}"));
        let mut tab = self.new_tab_with(path, false, "text", window, cx);
        tab.external = Some(External { id, events, last_edit: Instant::now(), pending: None });
        tab.content = Content::Ready;
        tab.saved = text.clone();
        tab.editor.update(cx, |state, cx| state.set_value(text, window, cx));
        let ix = self.place_tab(tab, true);
        self.activate(ix, window, cx);
        if let Some(idle) = idle {
            self.watch_idle(id, idle, window, cx);
        }
        cx.notify();
        id
    }

    /// What the program of edit `id` hears next: a save, or `None` once its
    /// tab is gone.
    pub fn edit_events(&mut self, id: u64) -> Result<smol::channel::Receiver<EditEvent>, String> {
        self.edits.get(&id).cloned().ok_or_else(|| format!("no edit {id}"))
    }

    /// Its program heard that the tab closed.
    pub fn forget_edit(&mut self, id: u64) {
        self.edits.remove(&id);
    }

    /// The program's answer to the last save: `None` if it kept the text.
    pub fn edit_saved(&mut self, id: u64, error: Option<String>, cx: &mut Context<Self>) -> Result<(), String> {
        let tab = self.edit_tab(id).ok_or_else(|| format!("no edit {id}"))?;
        let tab = &mut self.tabs[tab];
        let external = tab.external.as_mut().expect("an edit's tab");
        let Some(text) = external.pending.take() else {
            return Err("nothing was being saved".into());
        };
        match error {
            None => {
                tab.saved = text;
                tab.dirty = tab.editor.read(cx).text().to_string() != tab.saved;
                tab.confirm_close = false;
                self.message = None;
            }
            Some(error) => self.message = Some(format!("Couldn't save: {error}").into()),
        }
        cx.notify();
        Ok(())
    }

    /// The program ended: its tab closes, unsaved changes and all, as they
    /// would have nowhere to go.
    pub fn close_edit(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.edits.remove(&id);
        if let Some(ix) = self.edit_tab(id) {
            self.tabs[ix].dirty = false;
            self.close(ix, window, cx);
        }
    }

    /// Saving an edit's tab hands its text to the program.
    pub(super) fn save_edit(&mut self, ix: usize, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[ix];
        let text = tab.editor.read(cx).text().to_string();
        let external = tab.external.as_mut().expect("an edit's tab");
        if external.events.try_send(EditEvent::Saved(text.clone())).is_err() {
            self.message = Some("Couldn't save: the program that opened it has ended".into());
        } else {
            external.pending = Some(text);
            self.message = Some("Saving…".into());
        }
        cx.notify();
    }

    pub(super) fn edit_tab(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.external.as_ref().is_some_and(|external| external.id == id))
    }

    pub(super) fn touch_edit(&mut self, ix: usize) {
        if let Some(external) = &mut self.tabs[ix].external {
            external.last_edit = Instant::now();
        }
    }

    fn watch_idle(&mut self, id: u64, idle: Duration, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let done = this.update_in(cx, |this, window, cx| {
                    let Some(ix) = this.edit_tab(id) else {
                        return true;
                    };
                    let tab = &this.tabs[ix];
                    let external = tab.external.as_ref().expect("an edit's tab");
                    if tab.dirty || external.pending.is_some() || external.last_edit.elapsed() < idle {
                        return false;
                    }
                    external.events.try_send(EditEvent::Idle).ok();
                    this.close(ix, window, cx);
                    true
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
        })
        .detach();
    }
}
