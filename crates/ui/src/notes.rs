//! The Notes panel: what's next in a workspace, as plain Markdown that Den
//! keeps (in `notes.json` in its config folder, by workspace), never in the
//! repo. By default a tab of the terminals'; its tab and its icon carry a dot
//! while it has something. Cmd-E's notice shows its first line, and removing
//! the worktree forgets it.

use std::{collections::HashMap, path::PathBuf, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Editor, EditorState, InputEvent},
    v_flex,
};
use gpui_kit::*;

/// How long typing pauses before it's written.
const SAVE_AFTER: Duration = Duration::from_millis(400);

/// Every workspace's notes, by its key (see `TaskKey::config`), read once:
/// all the windows share them.
#[derive(Default)]
struct Store(Option<HashMap<String, String>>);

impl Global for Store {}

fn path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    proto::config_dir().ok().map(|dir| dir.join("notes.json"))
}

fn all(cx: &mut App) -> &mut HashMap<String, String> {
    cx.default_global::<Store>().0.get_or_insert_with(|| {
        path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    })
}

fn store(cx: &mut App) {
    let Some(path) = path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(all(cx)) {
        let _ = std::fs::write(path, bytes);
    }
}

/// Workspace `key`'s notes.
pub fn get(key: &str, cx: &mut App) -> String {
    all(cx).get(key).cloned().unwrap_or_default()
}

/// The first line with something in it: what's next.
pub fn first_line(key: &str, cx: &mut App) -> Option<String> {
    let notes = get(key, cx);
    notes
        .lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', ' ']).trim())
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn set(key: &str, text: String, cx: &mut App) {
    let notes = all(cx);
    if text.trim().is_empty() {
        if notes.remove(key).is_none() {
            return;
        }
    } else if notes.get(key) == Some(&text) {
        return;
    } else {
        notes.insert(key.to_string(), text);
    }
    store(cx);
}

/// Forgets workspace `key`'s notes: it was removed.
pub fn forget(key: &str, cx: &mut App) {
    set(key, String::new(), cx);
}

pub struct NotesPanel {
    key: String,
    editor: Entity<EditorState>,
    /// Whether it has something, as last written.
    filled: bool,
    save: Task<()>,
    _subscription: Subscription,
}

impl NotesPanel {
    pub fn new(key: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = get(&key, cx);
        let filled = !text.trim().is_empty();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .soft_wrap(true)
                .placeholder("What's next here…")
                .default_value(text)
        });
        let subscription = cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.schedule_save(cx);
            }
        });
        Self { key, editor, filled, save: Task::ready(()), _subscription: subscription }
    }

    /// Whether it has something: its tab and icon say so.
    pub fn filled(&self) -> bool {
        self.filled
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.editor.read(cx).focus_handle(cx).contains_focused(window, cx)
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let filled = !self.editor.read(cx).value().trim().is_empty();
        if filled != self.filled {
            self.filled = filled;
            cx.notify();
        }
        self.save = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            this.update(cx, |this, cx| this.save_now(cx)).ok();
        });
    }

    fn save_now(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value().to_string();
        set(&self.key, text, cx);
    }
}

impl Render for NotesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .pl_3()
            .pt_2()
            .bg(cx.theme().background)
            .child(Editor::new(&self.editor).bordered(false).h_full())
    }
}
