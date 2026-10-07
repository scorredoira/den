//! The editor's tabs and its two groups: activating, closing, splitting
//! and moving them.

use super::*;

impl Workspace {
    /// Opens another view of tab `of`'s file in `group`, with the same text
    /// and cursor, showing the source or the rendered Markdown.
    pub(super) fn new_view(&mut self, of: usize, group: usize, show_source: bool, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let path = self.tabs[of].path.clone();
        let language = language::for_path(&path);
        let mut view = self.new_tab_with(path, false, language, window, cx);
        let source = &self.tabs[of];
        view.view = true;
        view.group = group;
        view.show_source = show_source;
        view.markdown = source.markdown.clone();
        view.image = source.image.clone();
        view.grab_focus = false;
        if matches!(source.content, Content::Ready) {
            view.content = Content::Ready;
            let (text, cursor) = {
                let state = source.editor.read(cx);
                (state.value(), state.cursor_position())
            };
            let focused = window.focused(cx);
            view.editor.update(cx, |state, cx| {
                state.set_tab_size(indentation(&text, cx), cx);
                state.set_value(text, window, cx);
                state.set_cursor_position(cursor, window, cx);
            });
            if let Some(focused) = focused {
                focused.focus(window, cx);
            }
        }
        self.tabs.push(view);
        let ix = self.tabs.len() - 1;
        self.mark_shown(ix);
        ix
    }

    pub(super) fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_with(ix, true, window, cx);
    }

    pub(super) fn activate_with(&mut self, ix: usize, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_shown(Panel::Code, cx) {
            self.show_panel(Panel::Code, cx);
        }
        self.active = Some(ix);
        self.group = self.tabs[ix].group;
        self.mark_shown(ix);
        self.message = None;
        let tab = &self.tabs[ix];
        let (path, doc) = (tab.path.clone(), tab.doc);
        // The history rereads what changed while it was out of sight.
        if matches!(tab.page, Some(pages::Page::History)) {
            self.history.update(cx, |history, cx| history.shown(cx));
        }
        if focus {
            self.focus_active(window, cx);
        }
        // A page of the app's own isn't in the tree.
        if !doc {
            self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        }
        cx.notify();
    }

    /// Keys go to the source only if it's visible; with the rendered view they
    /// go to the workspace, so hidden text isn't edited.
    pub(super) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.map(|ix| &self.tabs[ix]) else {
            return;
        };
        if let Some(page) = &tab.page {
            let focus = page.focus_handle(self, cx);
            focus.focus(window, cx);
        } else if tab.rendered().is_some() || tab.image.is_some() || !matches!(tab.content, Content::Ready) {
            self.focus_handle.focus(window, cx);
        } else {
            tab.editor.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    pub(super) fn toggle_markdown_source(
        &mut self,
        _: &ToggleMarkdownSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active.map(|ix| &mut self.tabs[ix]) else {
            return;
        };
        if tab.markdown.is_none() {
            return;
        }
        tab.show_source = !tab.show_source;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Typing in a file's rendered Markdown switches it to the source, to
    /// edit it. What was typed isn't inserted: there's no cursor in the
    /// preview to say where.
    pub(super) fn type_in_preview(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.map(|ix| &self.tabs[ix]) else {
            return;
        };
        let Some(markdown) = tab.rendered() else {
            return;
        };
        let focused = self.focus_handle.is_focused(window) || markdown.read(cx).focus_handle().is_focused(window);
        let modifiers = &event.keystroke.modifiers;
        let typed = event.keystroke.key_char.as_ref().is_some_and(|text| !text.trim().is_empty());
        if focused && typed && tab.diff.is_none() && !tab.doc && !modifiers.platform && !modifiers.control && !modifiers.function {
            cx.stop_propagation();
            self.toggle_markdown_source(&ToggleMarkdownSource, window, cx);
        }
    }

    /// Tabs of renamed items follow the file.
    pub(super) fn renamed(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            if let Ok(rest) = tab.path.strip_prefix(from) {
                tab.path = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
            }
        }
        cx.notify();
    }

    /// Closes the tabs of whatever was moved to the Trash, except those with
    /// unsaved changes.
    pub(super) fn trashed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| tab.path.starts_with(path) && !tab.dirty)
        {
            self.close(ix, window, cx);
        }
        if self.tabs.iter().any(|tab| tab.path.starts_with(path)) {
            self.message = Some("A file with unsaved changes was moved to the Trash; its tab stays open".into());
        }
        cx.notify();
    }

    pub(super) fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.tabs[ix].path.clone();
        let other_view = (0..self.tabs.len()).find(|other| *other != ix && self.tabs[*other].view && self.tabs[*other].path == path);
        let tab = &mut self.tabs[ix];
        if tab.dirty && !tab.confirm_close && other_view.is_none() {
            tab.confirm_close = true;
            self.message = Some("Unsaved changes: press Cmd-W again to close without saving".into());
            cx.notify();
            return;
        }
        // The file stays open in its other view, which takes over.
        if tab.is_file()
            && let Some(view) = other_view
        {
            let (saved, dirty, blame) = (tab.saved.clone(), tab.dirty, tab.blame.clone());
            let view = &mut self.tabs[view];
            view.view = false;
            view.saved = saved;
            view.dirty = dirty;
            view.blame = blame;
        }
        let was_active = self.active == Some(ix);
        let next = self.remove_tab(ix);
        self.message = None;
        match next {
            None => {
                self.active = None;
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
            Some(next) if was_active => self.activate(next, window, cx),
            Some(next) => {
                self.active = Some(next);
                self.group = self.tabs[next].group;
                cx.notify();
            }
        }
    }

    /// Closes all tabs except `keep` (those with unsaved changes stay, with a
    /// warning).
    pub(super) fn close_others(&mut self, keep: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let keep = keep.map(|ix| self.tabs[ix].editor.clone());
        while let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| Some(&tab.editor) != keep.as_ref() && !tab.dirty)
        {
            self.close(ix, window, cx);
        }
        if let Some(ix) = keep.and_then(|editor| self.tabs.iter().position(|tab| tab.editor == editor)) {
            self.activate(ix, window, cx);
        }
        if self.tabs.iter().any(|tab| tab.dirty) {
            self.message = Some("Tabs with unsaved changes remain open".into());
        }
        cx.notify();
    }

    /// Closes the tabs after `ix` in its group, and shows it (those with
    /// unsaved changes stay, with a warning).
    pub(super) fn close_to_the_right(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (keep, group) = (self.tabs[ix].editor.clone(), self.tabs[ix].group);
        let right: Vec<_> = self.tabs[ix + 1..]
            .iter()
            .filter(|tab| tab.group == group)
            .map(|tab| tab.editor.clone())
            .collect();
        self.close_saved_of(&right, window, cx);
        if let Some(ix) = self.tab_index(&keep) {
            self.activate(ix, window, cx);
        }
        if right.iter().any(|editor| self.tab_index(editor).is_some()) {
            self.message = Some("Tabs with unsaved changes remain open".into());
        }
        cx.notify();
    }

    /// Closes the tabs of `group` without unsaved changes.
    pub(super) fn close_saved(&mut self, group: usize, window: &mut Window, cx: &mut Context<Self>) {
        let editors: Vec<_> = self.tabs.iter().filter(|tab| tab.group == group).map(|tab| tab.editor.clone()).collect();
        self.close_saved_of(&editors, window, cx);
        cx.notify();
    }

    /// Closes the tabs of these editors but those with unsaved changes.
    pub(super) fn close_saved_of(&mut self, editors: &[Entity<EditorState>], window: &mut Window, cx: &mut Context<Self>) {
        for editor in editors {
            if let Some(ix) = self.tab_index(editor).filter(|ix| !self.tabs[*ix].dirty) {
                self.close(ix, window, cx);
            }
        }
    }

    /// Keeps a preview tab open: the next preview gets a tab of its own.
    pub(super) fn keep_open(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.tabs[ix].preview = false;
        cx.notify();
    }

    /// The uncommitted changes of `path`, side by side, as the Changes panel
    /// and `den diff` show them.
    pub(super) fn open_changes(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.repo_path(path) {
            self.open_diff(DiffOf { file, commit: None, source: false }, true, window, cx);
        }
    }

    /// `path` from the task's folder with `/`, as git and a server name it:
    /// a Windows path's `\` would name nothing on a Linux server.
    pub(super) fn repo_path(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        Some(relative.to_string_lossy().replace('\\', "/"))
    }

    /// Whether `path` has uncommitted changes, as the Changes panel last read
    /// them: for a menu, built when it opens.
    pub(super) fn has_changes(&self, path: &Path) -> impl Fn(&App) -> bool + 'static {
        let changes = self.changes.downgrade();
        let file = self.repo_path(path);
        move |cx| {
            file.as_ref()
                .zip(changes.upgrade())
                .is_some_and(|(file, changes)| changes.read(cx).is_changed(file))
        }
    }

    /// A tab's index by its editor, which is unique (paths aren't: a file,
    /// its diff and its side preview share one).
    pub(super) fn tab_index(&self, editor: &Entity<EditorState>) -> Option<usize> {
        self.tabs.iter().position(|tab| &tab.editor == editor)
    }

    /// Cmd-N: a new file in the folder of the file open, or the workspace's,
    /// named in the files panel.
    pub(super) fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self
            .active_file()
            .and_then(|ix| self.tabs[ix].path.parent())
            .filter(|dir| dir.starts_with(&self.root))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        self.show_panel(Panel::Files, cx);
        self.file_tree.update(cx, |tree, cx| tree.new_file_in(dir, window, cx));
    }

    pub(super) fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.show_panel(Panel::Files, cx);
        self.file_tree.update(cx, |tree, cx| tree.reveal(path, cx));
    }

    /// The file of the active tab (which may be a view of it).
    pub(super) fn active_file(&self) -> Option<usize> {
        let active = self.active?;
        if !self.tabs[active].view {
            return Some(active);
        }
        let path = &self.tabs[active].path;
        self.tabs.iter().position(|tab| &tab.path == path && tab.is_file())
    }

    pub(super) fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.read(cx).contains_focus(window, cx) {
            self.terminals
                .update(cx, |terminals, cx| terminals.close_focused(window, cx));
            if self.terminals.read(cx).is_empty() {
                self.hide_panel(Panel::Terminals, cx);
                self.focus_ide(window, cx);
            }
            return;
        }
        if let Some(ix) = self.active {
            self.close(ix, window, cx);
        }
    }

    pub(super) fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(1, window, cx);
    }

    pub(super) fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(-1, window, cx);
    }

    /// To the next or previous tab of the focused group.
    pub(super) fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        let group: Vec<usize> = (0..self.tabs.len()).filter(|ix| self.tabs[*ix].group == self.group).collect();
        let Some(at) = group.iter().position(|ix| *ix == active) else {
            return;
        };
        let next = group[(at as isize + delta).rem_euclid(group.len() as isize) as usize];
        self.activate(next, window, cx);
    }

    /// Split Editor Right/Down: the active tab's file also opens on the
    /// other side, as in VS Code (a Markdown file, with its preview there).
    /// With the split already there, it only changes direction (Move to Other
    /// Side moves tabs).
    pub(super) fn split_editor(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_split.is_some() {
            self.editor_split = Some(axis);
            return cx.notify();
        }
        let Some(ix) = self.active else {
            return;
        };
        if self.tabs[ix].diff.is_some() {
            self.message = Some("A diff can't be split; move it with Move to Other Side".into());
            return cx.notify();
        }
        self.editor_split = Some(axis);
        if self.tabs[ix].markdown.is_some() {
            return self.open_preview_to_side(&OpenPreviewToSide, window, cx);
        }
        let group = self.tabs[ix].group;
        let view = self.new_view(ix, 1 - group, true, window, cx);
        self.activate(view, window, cx);
    }

    /// Moves a tab to the other group (creating it side by side if there's
    /// no split); if its group is left empty, the split closes.
    pub(super) fn move_to_other_group(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_split.get_or_insert(Axis::Row);
        let from = self.tabs[ix].group;
        let tab = self.tabs.remove(ix);
        let mut tab = tab;
        tab.group = 1 - from;
        self.tabs.push(tab);
        let ix = self.tabs.len() - 1;
        let editor = self.tabs[ix].editor.clone();
        // The group it left shows its most recent tab.
        self.active = None;
        self.normalize_groups();
        let ix = self.tab_index(&editor).unwrap_or(ix);
        self.activate(ix, window, cx);
    }

    /// Open to the Side: `path` in the other group, splitting the editor if
    /// it isn't; open on this side already, as a view of it.
    pub(super) fn open_to_side(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // With nothing open there's no side to put it beside.
        if self.tabs.is_empty() {
            return self.open(path, true, window, cx);
        }
        let other = 1 - self.group;
        if let Some(ix) = self.tabs.iter().position(|tab| tab.shows_file(&path) && tab.group == other) {
            self.tabs[ix].preview = false;
            return self.activate(ix, window, cx);
        }
        self.editor_split.get_or_insert(Axis::Row);
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path && tab.is_file()) {
            let view = self.new_view(ix, other, true, window, cx);
            return self.activate(view, window, cx);
        }
        self.group = other;
        self.open(path, true, window, cx);
    }

    /// Markdown: the rendered view in the other group, next to the source,
    /// updating as you type.
    pub(super) fn open_preview_to_side(&mut self, _: &OpenPreviewToSide, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        let tab = &self.tabs[ix];
        if tab.markdown.is_none() || tab.diff.is_some() {
            return;
        }
        let (path, other) = (tab.path.clone(), 1 - tab.group);
        self.tabs[ix].show_source = true;
        self.editor_split.get_or_insert(Axis::Row);
        // A view already on the other side switches to the preview.
        match self.tabs.iter().position(|tab| tab.shows_file(&path) && tab.group == other) {
            Some(view) => {
                self.tabs[view].show_source = false;
                self.mark_shown(view);
            }
            None => {
                self.new_view(ix, other, false, window, cx);
            }
        }
        self.activate(ix, window, cx);
    }

    pub(super) fn toggle_word_wrap(&mut self, _: &ToggleWordWrap, _: &mut Window, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.word_wrap = !config.word_wrap);
        crate::app_menu::set(cx);
        cx.notify();
    }

    /// Applies the config's indentation to the files that don't show their
    /// own, if it changed (in any task).
    pub(super) fn apply_tab(&mut self, cx: &mut Context<Self>) {
        let tab = Config::get(cx).tab();
        if (tab.tab_size, tab.hard_tabs) == self.tab {
            return;
        }
        self.tab = (tab.tab_size, tab.hard_tabs);
        for tab in self.tabs.iter().filter(|tab| tab.is_file()) {
            tab.editor.update(cx, |state, cx| {
                let tab = indentation(&state.value(), cx);
                state.set_tab_size(tab, cx);
            });
        }
    }

    /// Applies the config's word wrap to the tabs if it changed (in any task).
    pub(super) fn apply_word_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = Config::get(cx).word_wrap;
        if wrap == self.word_wrap {
            return;
        }
        self.word_wrap = wrap;
        // The sides of a diff don't wrap, to stay aligned.
        for tab in self.tabs.iter().filter(|tab| tab.old.is_none()) {
            tab.editor.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        }
    }
}
