//! The notes: what's next in a workspace, as plain Markdown that Den keeps
//! (in `notes.json` in its config folder, by workspace), never in the repo.
//! They're a tab of the code of their own, which their icon on the activity
//! bar or Cmd-Alt-N opens; the icon is a sticky note written on while they
//! have something. Cmd-E's notice shows their first line, and removing the
//! worktree forgets them. `den notes` reads and writes them from a terminal.

use std::{collections::HashMap, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{self, Editor, EditorState, InputEvent},
    native_menu::NativeMenu,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::config::store::JsonFile;

/// How long typing pauses before it's written.
const SAVE_AFTER: Duration = Duration::from_millis(400);

/// Every workspace's notes, by its key (see `TaskKey::config`), read once:
/// all the windows share them.
#[derive(Default)]
struct Store(Option<HashMap<String, String>>);

impl Global for Store {}

static FILE: JsonFile = JsonFile::new("notes.json");

fn all(cx: &mut App) -> &mut HashMap<String, String> {
    cx.default_global::<Store>().0.get_or_insert_with(|| FILE.load())
}

fn store(cx: &mut App) {
    FILE.save_later(all(cx));
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

/// Writes workspace `key`'s notes (none, if blank).
pub fn set(key: &str, text: String, cx: &mut App) {
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
    /// The save waiting for the typing to pause, if there's one.
    save: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl NotesPanel {
    pub fn new(key: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = get(&key, cx);
        let filled = !text.trim().is_empty();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .line_number(false)
                .folding(false)
                .soft_wrap(true)
                .placeholder("What's next here…")
                .default_value(text)
        });
        let subscriptions = vec![
            cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.schedule_save(cx);
                }
            }),
            // What was typed last isn't lost with the panel, or the app.
            cx.on_release(|this, cx| this.save_pending(cx)),
            cx.on_app_quit(|this, cx| {
                this.save_pending(cx);
                async {}
            }),
        ];
        Self { key, editor, filled, save: None, _subscriptions: subscriptions }
    }

    /// Whether it has something: its tab and icon say so.
    pub fn filled(&self) -> bool {
        self.filled
    }

    /// Replaces what it shows: `den notes` wrote them.
    pub fn set_text(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.save = None;
        self.filled = !text.trim().is_empty();
        self.editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
        cx.notify();
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.editor.update(cx, |editor, cx| editor.focus(window, cx));
    }

    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        let filled = !self.editor.read(cx).value().trim().is_empty();
        if filled != self.filled {
            self.filled = filled;
            cx.notify();
        }
        self.save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            this.update(cx, |this, cx| this.save_pending(cx)).ok();
        }));
    }

    /// Writes what was typed, if it's still to be: only then, as another
    /// window's notes may have been written since.
    fn save_pending(&mut self, cx: &mut App) {
        if self.save.take().is_some() {
            let text = self.editor.read(cx).value().to_string();
            set(&self.key, text, cx);
        }
    }
}

impl Render for NotesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .when(cfg!(test), |el| el.debug_selector(|| "notes".into()))
            .pl_3()
            .pt_2()
            .bg(cx.theme().background)
            .child(Editor::new(&self.editor).bordered(false).h_full().context_menu(|menu, _, _| notes_menu(menu)))
    }
}

/// The text's right-click menu. It's built while the editor is mid-update
/// and can't read whether it has text selected: Cut and Copy are always
/// enabled.
fn notes_menu(menu: NativeMenu) -> NativeMenu {
    menu.menu("Cut", Box::new(input::Cut))
        .menu("Copy", Box::new(input::Copy))
        .menu("Paste", Box::new(input::Paste))
        .separator()
        .menu("Select All", Box::new(input::SelectAll))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    /// Closed before the typing paused long enough to be written, the notes
    /// are written as they close.
    #[gpui_kit::test]
    fn what_was_just_typed_is_kept_when_the_notes_close(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::config::Config::default());
        });
        let (notes, cx) = cx.add_window_view(|window, cx| NotesPanel::new("closing-test".into(), window, cx));
        notes.update_in(cx, |notes, window, cx| notes.focus(window, cx));
        cx.simulate_input("ship it");
        cx.update(|_, cx| assert_eq!(get("closing-test", cx), ""));
        drop(notes);
        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();
        cx.cx.update(|cx| assert_eq!(get("closing-test", cx), "ship it"));
    }
}
