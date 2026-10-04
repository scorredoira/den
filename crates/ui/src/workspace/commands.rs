//! What the `den` commands run in a terminal do in its workspace (see
//! `app::commands`). Paths are absolute; lines and columns, 0-based here.

use super::*;

impl Workspace {
    /// `den debug`: the workspace's debugger.
    pub fn debugger(&self) -> Entity<Debugger> {
        self.debugger.clone()
    }

    /// `den device`: the workspace's Device panel.
    pub fn device(&self) -> Entity<Device> {
        self.device.clone()
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
            let mark = if active { "*" } else { " " };
            let dirty = if tab.dirty { " (unsaved)" } else { "" };
            out.push_str(&format!("{mark} {}{dirty}\n", self.tab_name(ix, cx)));
        }
        out
    }

    /// A tab as `den tabs` and `den where` name it: the file, with the
    /// cursor for the active one; a doc, a diff or a commit said so.
    fn tab_name(&self, ix: usize, cx: &App) -> String {
        let tab = &self.tabs[ix];
        match &tab.diff {
            _ if tab.doc => format!("doc: {}", tab.path.display()),
            Some(diff) => match &diff.commit {
                Some((_, short)) if diff.file.is_empty() => format!("commit {short}"),
                Some((_, short)) => format!("{} at {short}", diff.file),
                None => format!("changes: {}", diff.file),
            },
            None if Some(ix) == self.active => {
                let cursor = tab.editor.read(cx).cursor_position();
                format!("{}:{}:{}", tab.path.display(), cursor.line + 1, cursor.character + 1)
            }
            None => tab.path.display().to_string(),
        }
    }

    /// `den where`: what the workspace shows. The tabs, the active one by
    /// itself; the panels in sight; the Device panel; the debugger's session.
    pub fn whereabouts(&self, cx: &App) -> serde_json::Value {
        let tabs: Vec<serde_json::Value> = (0..self.tabs.len())
            .filter(|ix| !self.tabs[*ix].view)
            .map(|ix| {
                let mut tab = serde_json::json!({ "tab": self.tab_name(ix, cx) });
                if self.tabs[ix].dirty {
                    tab["unsaved"] = true.into();
                }
                tab
            })
            .collect();
        let panels: Vec<Panel> = Panel::ALL.into_iter().filter(|panel| self.is_shown(*panel, cx)).collect();
        let mut device = self.device.read(cx).state();
        if device["available"] == true {
            device["shown"] = self.is_shown(Panel::Device, cx).into();
        }
        let debugger = self.debugger.read(cx).state();
        let mut session = serde_json::json!({});
        for key in ["status", "running", "stopped", "command", "page", "device", "revealed", "launchError"] {
            if let Some(value) = debugger.get(key) {
                session[key] = value.clone();
            }
        }
        if let Some(focus) = debugger.get("focus") {
            session["stoppedAt"] = serde_json::json!(format!("{}:{}", focus["file"].as_str().unwrap_or(""), focus["line"]));
        }
        let mut out = serde_json::json!({
            "root": self.root,
            "tabs": tabs,
            "panels": panels,
            "device": device,
            "debugger": session,
        });
        if let Some(branch) = &self.branch {
            out["branch"] = branch.clone().into();
        }
        if let Some(ix) = self.active {
            out["active"] = self.tab_name(ix, cx).into();
        }
        out
    }

    /// `den close`: the tabs of `path` (a file, its diffs and views), or
    /// every tab. None closes if one has unsaved changes: closing would
    /// throw them away.
    pub fn close_files(&mut self, path: Option<&Path>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let hits = |tab: &FileTab| path.is_none_or(|path| tab.path == path);
        let unsaved: Vec<String> =
            self.tabs.iter().filter(|tab| hits(tab) && tab.dirty).map(|tab| tab.path.display().to_string()).collect();
        if !unsaved.is_empty() {
            return Err(format!("unsaved changes, nothing closed: {}", unsaved.join(", ")));
        }
        if let Some(path) = path
            && !self.tabs.iter().any(|tab| hits(tab))
        {
            return Err(format!("{} isn't open", path.display()));
        }
        while let Some(ix) = self.tabs.iter().position(|tab| hits(tab)) {
            self.close(ix, window, cx);
        }
        Ok(())
    }

    /// `den panel show|hide`.
    pub fn set_panel(&mut self, panel: Panel, show: bool, cx: &mut Context<Self>) {
        if show {
            self.show_panel(panel, cx);
        } else {
            self.hide_panel(panel, cx);
        }
    }

    /// `den reveal`: `path` selected in the files panel, which shows.
    pub fn reveal_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.reveal_in_tree(path, cx);
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

    /// `den where` of a workspace with nothing open: its root, no tabs, no
    /// Device panel, the debugger idle. `den close` of a file not open says so.
    #[gpui_kit::test]
    fn whereabouts_and_close(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            Workspace::new(PathBuf::from("/where-test"), None, true, "where-test".into(), window, cx)
        });
        let place = workspace.read_with(cx, |workspace, cx| workspace.whereabouts(cx));
        assert_eq!(place["root"], "/where-test");
        assert_eq!(place["tabs"], serde_json::json!([]));
        assert!(place.get("active").is_none());
        assert_eq!(place["device"], serde_json::json!({ "available": false }));
        assert_eq!(place["debugger"]["status"], "idle");
        assert!(place["panels"].as_array().unwrap().iter().all(|panel| panel.is_string()));

        let closed = cx.update(|window, cx| {
            workspace.update(cx, |workspace, cx| workspace.close_files(Some(Path::new("/where-test/a.ts")), window, cx))
        });
        assert_eq!(closed, Err("/where-test/a.ts isn't open".to_string()));
        let all = cx.update(|window, cx| workspace.update(cx, |workspace, cx| workspace.close_files(None, window, cx)));
        assert_eq!(all, Ok(()));
    }

    /// A `reveal` from the program is in `den debug state`, the program's
    /// page and device from its `hello` too.
    #[gpui_kit::test]
    fn debug_state_says_what_the_program_said(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            Workspace::new(PathBuf::from("/reveal-test"), None, true, "reveal-test".into(), window, cx)
        });
        let debugger = workspace.read_with(cx, |workspace, _| workspace.debugger());
        debugger.update(cx, |debugger, cx| {
            debugger.receive(r#"{"event":"reveal","file":"client/home.ts","line":12}"#, cx);
            debugger.set_ran(0, "scl -d apps/padel/app.xml".into(), None);
        });
        let state = debugger.read_with(cx, |debugger, _| debugger.state());
        assert_eq!(state["revealed"], serde_json::json!({ "file": "client/home.ts", "line": 12 }));
        assert_eq!(state["command"], "scl -d apps/padel/app.xml");
    }

    #[test]
    fn wait_words() {
        assert!(WaitFor::parse("stop") == Some(WaitFor::Stop));
        assert!(WaitFor::parse("connected") == Some(WaitFor::Connected));
        assert!(WaitFor::parse("idle") == Some(WaitFor::Idle));
        assert!(WaitFor::parse("30").is_none());
    }
}
