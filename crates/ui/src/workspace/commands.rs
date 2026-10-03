//! What the `den` commands run in a terminal do in its workspace (see
//! `app::commands`). Paths are absolute; lines and columns, 0-based here.

use super::*;

impl Workspace {
    /// `den debug`: the workspace's debugger.
    pub fn debugger(&self) -> Entity<Debugger> {
        self.debugger.clone()
    }

    /// `den show`: `path` with the cursor at `from` or, with `to`, the range
    /// between them selected. Without `focus`, the keyboard stays where it was.
    pub fn show(&mut self, path: PathBuf, from: Position, to: Option<Position>, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_at_with(path.clone(), from, true, focus, window, cx);
        let Some(to) = to else {
            return;
        };
        let Some(tab) = self.active.map(|ix| &mut self.tabs[ix]).filter(|tab| tab.path == path) else {
            return;
        };
        match tab.content {
            Content::Ready => tab.editor.update(cx, |state, cx| select(state, from, to, cx)),
            _ => tab.select_to = Some(to),
        }
    }

    /// `den diff`: the Changes panel and, for `file` (relative to the root),
    /// its uncommitted changes.
    pub fn show_changes(&mut self, file: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(Panel::Changes, cx);
        if let Some(file) = file {
            self.terminals_maximized = false;
            self.open_diff(DiffOf { file, commit: None, source: false }, true, window, cx);
        }
        cx.notify();
    }

    /// `den selection`: the active file and each selection in it
    /// (`path:line:col-line:col`, 1-based), followed by its text.
    pub fn selection(&self, cx: &App) -> Result<String, String> {
        let tab = self
            .active
            .map(|ix| &self.tabs[ix])
            .filter(|tab| tab.is_file() && !tab.doc)
            .ok_or("no file is open in the editor")?;
        let state = tab.editor.read(cx);
        let text = state.text();
        let mut out = String::new();
        for (anchor, cursor) in state.selections() {
            let (start, end) = (anchor.min(cursor), anchor.max(cursor));
            let from = text.offset_to_position(start);
            out.push_str(&format!("{}:{}:{}", tab.path.display(), from.line + 1, from.character + 1));
            if start < end {
                let to = text.offset_to_position(end);
                out.push_str(&format!("-{}:{}\n{}", to.line + 1, to.character + 1, text.slice(start..end)));
            }
            out.push('\n');
        }
        Ok(out)
    }

    /// `den tabs`: the open files, by group; the active one with a `*` and its cursor.
    pub fn tab_list(&self, cx: &App) -> String {
        let mut out = String::new();
        for (ix, tab) in self.tabs.iter().enumerate().filter(|(_, tab)| !tab.view) {
            let active = Some(ix) == self.active;
            let name = match &tab.diff {
                _ if tab.doc => format!("doc: {}", tab.path.display()),
                Some(diff) => match &diff.commit {
                    Some((_, short)) if diff.file.is_empty() => format!("commit {short}"),
                    Some((_, short)) => format!("{} at {short}", diff.file),
                    None => format!("changes: {}", diff.file),
                },
                None if active => {
                    let cursor = tab.editor.read(cx).cursor_position();
                    format!("{}:{}:{}", tab.path.display(), cursor.line + 1, cursor.character + 1)
                }
                None => tab.path.display().to_string(),
            };
            let mark = if active { "*" } else { " " };
            let dirty = if tab.dirty { " (unsaved)" } else { "" };
            out.push_str(&format!("{mark} {name}{dirty}\n"));
        }
        out
    }

    /// `den message`: `text` in the status bar.
    pub fn set_message(&mut self, text: String, cx: &mut Context<Self>) {
        self.message = Some(text.into());
        cx.notify();
    }

    /// `den term list`: id, title and whether it's the active one.
    pub fn terminal_list(&self, cx: &App) -> Vec<(proto::TermId, String, bool)> {
        self.terminals.read(cx).list(cx)
    }

    /// `den term new`: see `TerminalArea::open_for_command`.
    pub fn open_terminal(
        &mut self,
        beside: Option<proto::TermId>,
        split: Option<Axis>,
        line: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<proto::TermId>> {
        self.show_panel(Panel::Terminals, cx);
        self.terminals
            .update(cx, |terminals, cx| terminals.open_for_command(beside, split, line, focus, window, cx))
    }

    /// `den term focus`: false if `term` isn't one of this workspace's.
    pub fn focus_terminal(&mut self, term: proto::TermId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let found = self.terminals.update(cx, |terminals, cx| terminals.focus_term(term, window, cx));
        if found {
            self.show_panel(Panel::Terminals, cx);
        }
        found
    }
}

/// Selects from `from` to `to`, with the cursor at `from` so that it's the
/// start of the range that scrolls into view.
pub(super) fn select(state: &mut EditorState, from: Position, to: Position, cx: &mut Context<EditorState>) {
    let text = state.text();
    let (from, to) = (text.position_to_offset(&from), text.position_to_offset(&to));
    state.set_selections(&[(to, from)], cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::WaitFor;
    use core::prelude::v1::test;

    /// `den debug break` sets a breakpoint once, and `den debug state` shows
    /// it as the program names it: relative, 1-based. With no session there
    /// is nothing to wait for.
    #[gpui_kit::test]
    fn debug_state_and_breakpoints(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            Workspace::new(PathBuf::from("/debug-test"), None, true, "debug-test".into(), window, cx)
        });
        let debugger = workspace.read_with(cx, |workspace, _| workspace.debugger());
        let path = PathBuf::from("/debug-test/modules/a/main.ts");
        debugger.update(cx, |debugger, cx| {
            debugger.set_breakpoint(&path, 11, cx);
            debugger.set_breakpoint(&path, 11, cx);
        });

        let state = debugger.read_with(cx, |debugger, _| debugger.state());
        assert_eq!(state["status"], "idle");
        assert_eq!(
            state["breakpoints"],
            serde_json::json!([{ "file": "modules/a/main.ts", "line": 12, "enabled": true }])
        );
        assert!(state.get("focus").is_none());
        for what in [WaitFor::Stop, WaitFor::Connected, WaitFor::Idle] {
            assert!(debugger.read_with(cx, |debugger, _| debugger.reached(what)));
        }

        debugger.update(cx, |debugger, cx| debugger.remove_breakpoint(&path, 11, cx));
        let state = debugger.read_with(cx, |debugger, _| debugger.state());
        assert_eq!(state["breakpoints"], serde_json::json!([]));
    }

    #[test]
    fn wait_words() {
        assert!(WaitFor::parse("stop") == Some(WaitFor::Stop));
        assert!(WaitFor::parse("connected") == Some(WaitFor::Connected));
        assert!(WaitFor::parse("idle") == Some(WaitFor::Idle));
        assert!(WaitFor::parse("30").is_none());
    }
}
