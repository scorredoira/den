//! Files in tabs: opening, loading, reloading when they change on disk,
//! and saving.

use super::*;

impl Workspace {
    pub(super) fn new_tab(&mut self, path: PathBuf, preview: bool, window: &mut Window, cx: &mut Context<Self>) -> FileTab {
        let language = language::for_path(&path);
        self.new_tab_with(path, preview, language, window, cx)
    }

    pub(super) fn new_tab_with(
        &mut self,
        path: PathBuf,
        preview: bool,
        language: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FileTab {
        let markdown =
            (language == "markdown").then(|| cx.new(|cx| TextViewState::markdown("", cx)));
        let workspace = cx.entity().downgrade();
        let debugger = self.debugger.downgrade();
        let editor = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(Config::get(cx).word_wrap)
                .tab_size(Config::get(cx).tab());
            let lsp = editor.lsp_mut();
            lsp.completion_provider = Some(Rc::new(Completions::new(workspace.clone(), cx.entity().downgrade())));
            lsp.completion_menu.max_width = px(480.);
            lsp.hover_provider = Some(Rc::new(debug::hover::DebugHover { debugger: debugger.clone(), editor: cx.entity().downgrade() }));
            lsp.definition_provider = Some(Rc::new(definition::Definitions { workspace: workspace.clone(), editor: cx.entity().downgrade() }));
            lsp.show_document = Some(Rc::new({
                let workspace = workspace.clone();
                move |params, window, cx| definition::show_document(workspace.clone(), params, window, cx)
            }));
            editor.set_gutter_column(true, cx);
            let this_editor = cx.entity().downgrade();
            editor.on_gutter_click(Some(Rc::new(move |line, event: &MouseDownEvent, window, cx| {
                if let Some(editor) = this_editor.upgrade() {
                    workspace
                        .update(cx, |this, cx| this.gutter_clicked(&editor, line as u32, event.button, window, cx))
                        .ok();
                }
            })));
            editor
        });
        let focus_handle = editor.read(cx).focus_handle(cx);
        let subscriptions = vec![
            cx.on_blur(&focus_handle, window, {
                let editor = editor.clone();
                move |this, window, cx| this.auto_save_editor(&editor, window, cx)
            }),
            cx.subscribe_in(&editor, window, move |this, editor, event: &InputEvent, window, cx| {
                if let InputEvent::Change = event {
                    this.on_edit(editor, window, cx);
                }
            }),
            // The status bar shows the cursor position, and the blame follows it.
            cx.observe(&editor, |this, editor, cx| {
                this.highlight_occurrences(&editor, cx);
                this.follow_signature(&editor, cx);
                cx.notify()
            }),
        ];
        FileTab {
            path,
            image: None,
            editor,
            markdown,
            show_source: false,
            content: Content::Loading,
            lsp_status: LspStatus::default(),
            saved: String::new(),
            save_lock: Default::default(),
            dirty: false,
            preview,
            confirm_close: false,
            goto: None,
            select_to: None,
            grab_focus: true,
            diff: None,
            old: None,
            commit: None,
            page: None,
            restored: false,
            blame: None,
            occurrences: None,
            occurrences_for: Vec::new(),
            occurrences_task: None,
            group: self.group,
            shown: 0,
            view: false,
            doc: false,
            text: None,
            stand_in: None,
            _subscriptions: subscriptions,
        }
    }

    /// Reads a tab's file. On reload (it changed on disk) the cursor and
    /// scroll are kept, and unsaved changes aren't overwritten.
    pub(super) fn load(&mut self, path: PathBuf, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        let read = self
            .client
            .clone()
            .map(|client| client.request(Request::ReadFile { path: path.clone() }));
        let image_format = image_format(&path);
        cx.spawn_in(window, async move |this, cx| {
            let mut image = None;
            let result = match read {
                Some(read) => match read.await {
                    Ok(Response::Bytes(bytes)) if let Some(format) = image_format => {
                        image = Some(Arc::new(Image::from_bytes(format, bytes)));
                        Ok(String::new())
                    }
                    Ok(Response::Bytes(bytes)) => decode_text(bytes),
                    Ok(other) => Err(format!("Unexpected response: {other:?}")),
                    Err(err) => Err(format!("Couldn't open: {err:#}")),
                },
                None => Err("No agent".to_string()),
            };
            this.update_in(cx, |this, window, cx| {
                let Some(tab) = this.tabs.iter_mut().find(|tab| tab.path == path && tab.is_file()) else {
                    return;
                };
                let name = file_name(&path);
                if let Some(image) = image {
                    tab.image = Some(image);
                    tab.content = Content::Ready;
                    tab.restored = false;
                    return cx.notify();
                }
                match result {
                    Ok(text) if reload && text == tab.saved => return,
                    Ok(_) if reload && tab.dirty => {
                        this.message = Some(
                            format!("{name} changed on disk; your unsaved changes are kept")
                                .into(),
                        );
                    }
                    Ok(text) => {
                        if let Some(markdown) = &tab.markdown {
                            markdown.update(cx, |view, cx| view.set_text(&text, cx));
                        }
                        tab.saved = text.clone();
                        tab.content = Content::Ready;
                        tab.restored = false;
                        tab.text = Some(text.clone().into());
                        let focused = window.focused(cx);
                        tab.editor.update(cx, |state, cx| {
                            let cursor = state.cursor_position();
                            let scroll = state.scroll_offset();
                            state.set_value(text.clone(), window, cx);
                            state.set_tab_size(indentation(&text, cx), cx);
                            if reload {
                                state.set_cursor_position(cursor, window, cx);
                                state.set_scroll_offset(scroll, cx);
                            } else {
                                let goto = tab.goto.take().unwrap_or_default();
                                state.set_cursor_position(goto, window, cx);
                                if let Some(to) = tab.select_to.take() {
                                    commands::select(state, goto, to, cx);
                                }
                            }
                        });
                        if !reload {
                            let line = tab.editor.read(cx).cursor_position().line;
                            reveal_centered(&tab.editor, line, true, cx);
                        }
                        // Its other views get the same text, keeping their cursor.
                        for view in this.tabs.iter_mut().filter(|tab| tab.view && tab.path == path) {
                            view.content = Content::Ready;
                            let goto = view.goto.take();
                            view.editor.update(cx, |state, cx| {
                                let cursor = goto.unwrap_or_else(|| state.cursor_position());
                                let scroll = state.scroll_offset();
                                state.set_value(text.clone(), window, cx);
                                state.set_tab_size(indentation(&text, cx), cx);
                                state.set_cursor_position(cursor, window, cx);
                                if goto.is_none() {
                                    state.set_scroll_offset(scroll, cx);
                                }
                            });
                        }
                        this.load_blame(path.clone(), cx);
                        this.refresh_debug_marks(cx);
                        // Setting the text moves focus to the editor: it goes back to
                        // where it was, or where it belongs if this is the active tab.
                        let grab = this
                            .tabs
                            .iter()
                            .find(|tab| tab.path == path && tab.is_file())
                            .is_some_and(|tab| tab.grab_focus);
                        if !reload && grab && this.active.is_some_and(|ix| this.tabs[ix].path == path) {
                            this.focus_active(window, cx);
                        } else if let Some(focused) = focused {
                            focused.focus(window, cx);
                        }
                    }
                    // A reopened file that's gone doesn't deserve a tab (while
                    // offline there's no telling: it's reread on reconnect).
                    Err(_) if tab.restored && this.client.as_ref().is_some_and(|client| client.is_connected()) => {
                        if let Some(ix) = this.tabs.iter().position(|tab| tab.path == path && tab.is_file()) {
                            this.forget(ix, cx);
                        }
                    }
                    Err(_) if reload => {
                        this.message = Some(format!("{name} no longer exists on disk").into());
                    }
                    Err(err) => tab.content = Content::Failed(err.into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_fs_changed(
        &mut self,
        paths: HashSet<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The launch file changed: its problems and tests show.
        let den = self.root.join(".den");
        if paths.iter().any(|path| path.starts_with(&den)) {
            self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
        }
        // `root/.git`: git's state changed. If HEAD moved (a commit,
        // checkout or reset), the blame of every open file may have too;
        // staging alone (the index) doesn't change it.
        if paths.contains(&self.root.join(".git")) {
            self.check_head(cx);
        }
        self.file_tree
            .update(cx, |tree, cx| tree.invalidate(&paths, cx));
        self.changes.update(cx, |changes, cx| changes.mark_stale(cx));
        let visible = self.history_visible();
        self.history.update(cx, |history, cx| history.mark_stale(visible, cx));
        let reload: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|tab| tab.is_file() && matches!(tab.content, Content::Ready) && paths.contains(&tab.path))
            .map(|tab| tab.path.clone())
            .collect();
        for path in reload {
            self.load(path, true, window, cx);
        }
        let diffs: Vec<usize> = (0..self.tabs.len())
            .filter(|ix| self.tabs[*ix].diff.is_some() && paths.contains(&self.tabs[*ix].path))
            .collect();
        for ix in diffs {
            self.load_diff(ix, false, window, cx);
        }
    }

    /// A tab's text changed: its other views get the same edit, the rendered
    /// Markdown follows, and the file is marked as changed or not.
    pub(super) fn on_edit(&mut self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tab_index(editor) else {
            return;
        };
        if !matches!(self.tabs[ix].content, Content::Ready) || self.tabs[ix].diff.is_some() {
            return;
        }
        self.revision += 1;
        let path = self.tabs[ix].path.clone();
        let text = editor.read(cx).value();
        if editor.read(cx).focus_handle(cx).is_focused(window) {
            self.ask_signature(editor, cx);
        }
        // Each other view gets the difference; its own change event comes
        // back here, finds them equal and stops.
        for other in &self.tabs {
            if &other.editor == editor || !other.shows_file(&path) || !matches!(other.content, Content::Ready) {
                continue;
            }
            other.editor.update(cx, |state, cx| {
                if let Some((range, with)) = editing::difference(&state.value(), &text) {
                    let selections = editing::shift(&state.selections(), &range, with.len());
                    state.edit(&[(range, with)], &selections, false, window, cx);
                }
            });
        }
        if let Some(markdown) = &self.tabs[ix].markdown {
            markdown.update(cx, |view, cx| view.set_text(&text, cx));
        }
        // Breakpoints move with the lines they're on, by where the text
        // changed: an edit of another view comes to the file's tab too.
        if self.tabs[ix].is_file() {
            let before = self.tabs[ix].text.replace(text.clone());
            let marked = !self.debugger.read(cx).breakpoints.of(&path).is_empty();
            if marked
                && let Some(before) = before
                && let Some((range, with)) = editing::difference(&before, &text)
            {
                let edit = debug::LineEdit::new(&before, range, &with);
                self.debugger.update(cx, |debugger, cx| debugger.shift_breakpoints(&path, edit, cx));
            }
        }
        // Editing a preview turns it into a pinned tab.
        if self.tabs[ix].preview {
            self.tabs[ix].preview = false;
            cx.notify();
        }
        let Some(file) = self.tabs.iter_mut().find(|tab| tab.path == path && tab.is_file()) else {
            return;
        };
        let dirty = *text != file.saved;
        if dirty != file.dirty {
            file.dirty = dirty;
            file.confirm_close = false;
            cx.notify();
        }
    }

    /// A tab's file has unsaved changes (a view shows its file's).
    pub(super) fn is_dirty(&self, ix: usize) -> bool {
        let tab = &self.tabs[ix];
        if !tab.view {
            return tab.dirty;
        }
        self.tabs.iter().any(|file| file.path == tab.path && file.is_file() && file.dirty)
    }

    /// Saves the active tab's file (from a view too), formatting it first if
    /// Settings say so for its type.
    pub(super) fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_file() else {
            return;
        };
        self.save_file(ix, window, cx);
    }

    pub(super) fn auto_save_editor(&mut self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
        if !Config::get(cx).auto_save_on_focus_loss {
            return;
        }
        let Some(tab) = self.tabs.iter().find(|tab| &tab.editor == editor && tab.diff.is_none()) else {
            return;
        };
        // A split view edits the same file; save the owning tab, not the tab
        // that happens to be active after the focus change.
        let Some(ix) = self.tabs.iter().position(|file| file.path == tab.path && file.is_file()) else {
            return;
        };
        if self.tabs[ix].dirty {
            self.save_file(ix, window, cx);
        }
    }

    pub(super) fn save_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !Config::get(cx).formats_on_save(&self.tabs[ix].path) {
            self.save_tab(ix, cx).detach();
            return;
        }
        let format = self.format_tab(ix, window, cx);
        let editor = self.tabs[ix].editor.clone();
        cx.spawn(async move |this, cx| {
            let formatted = format.await;
            let Ok(Some(save)) = this.update(cx, |this, cx| this.tab_index(&editor).map(|ix| this.save_tab(ix, cx))) else {
                return;
            };
            // Saving clears the status bar: why it wasn't formatted goes after.
            if save.await
                && let Err(err) = formatted
            {
                this.update(cx, |this, cx| {
                    this.message = Some(err);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn format_document(&mut self, _: &FormatDocument, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_file() else {
            return;
        };
        let format = self.format_tab(ix, window, cx);
        cx.spawn(async move |this, cx| {
            let message = format.await.err();
            this.update(cx, |this, cx| {
                this.message = message;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Formats the tab's text in the editor (one undo step, the cursor kept
    /// on its line and column); the error says why it didn't.
    pub(super) fn format_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Task<Result<(), SharedString>> {
        let tab = &self.tabs[ix];
        if !matches!(tab.content, Content::Ready) || tab.image.is_some() || tab.diff.is_some() {
            return Task::ready(Ok(()));
        }
        let Some(client) = self.client.clone() else {
            return Task::ready(Err("Couldn't format: no agent".into()));
        };
        let editor = tab.editor.clone();
        let text = editor.read(cx).value().to_string();
        let request = Request::Format { root: self.root.clone(), path: tab.path.clone(), text: text.clone() };
        cx.spawn_in(window, async move |_, cx| {
            let formatted = match client.request(request).await {
                Ok(Response::Formatted { text: Some(formatted), .. }) => formatted,
                Ok(Response::Formatted { text: None, .. }) => {
                    return Err("Nothing formats this kind of file: the repo can add a .den/format".into());
                }
                Ok(other) => return Err(format!("Unexpected response: {other:?}").into()),
                Err(err) => return Err(format!("Couldn't format: {err:#}").into()),
            };
            editor
                .update_in(cx, |state, window, cx| {
                    if *state.value() != text {
                        return Err("Not formatted: the text changed meanwhile".into());
                    }
                    if let Some((range, with)) = editing::difference(&text, &formatted) {
                        let cursor = state.cursor_position();
                        let offset = editing::offset_at(&formatted, cursor.line, cursor.character);
                        state.edit(&[(range, with)], &[(offset, offset)], false, window, cx);
                    }
                    Ok(())
                })
                .map_err(|_| SharedString::from("Not formatted: the tab closed"))?
        })
    }

    /// Saves all tabs with changes; the result says whether it succeeded.
    pub fn save_all(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let dirty: Vec<usize> = (0..self.tabs.len()).filter(|ix| self.tabs[*ix].dirty).collect();
        let saves: Vec<Task<bool>> = dirty.into_iter().map(|ix| self.save_tab(ix, cx)).collect();
        cx.background_spawn(async move {
            let mut ok = true;
            for save in saves {
                ok &= save.await;
            }
            ok
        })
    }

    /// Writes the tab to disk; the result says whether it succeeded.
    pub(super) fn save_tab(&mut self, ix: usize, cx: &mut Context<Self>) -> Task<bool> {
        let tab = &self.tabs[ix];
        // Image tabs have an empty text editor; saving it would erase the image.
        if !matches!(tab.content, Content::Ready) || !tab.is_file() || tab.image.is_some() {
            return Task::ready(true);
        }
        let path = tab.path.clone();
        let text = tab.editor.read(cx).text().to_string();
        let editor = tab.editor.clone();
        let Some(client) = self.client.clone() else {
            self.message = Some("Couldn't save: no agent".into());
            cx.notify();
            return Task::ready(false);
        };
        let save_lock = tab.save_lock.clone();
        cx.spawn(async move |this, cx| {
            let _guard = save_lock.lock().await;
            // Read the current buffer after previous writes finish. A tab
            // closed meanwhile still has its captured text saved.
            let text = this.update(cx, |this, cx| {
                this.tabs.iter().find(|tab| tab.editor == editor && tab.path == path)
                    .map(|tab| tab.editor.read(cx).text().to_string())
            }).ok().flatten().unwrap_or(text);
            let result = client.request(Request::WriteFile {
                path: path.clone(),
                data: text.clone().into_bytes(),
            }).await;
            this.update(cx, |this, cx| {
                let ok = result.is_ok();
                match result {
                    Ok(_) => {
                        if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.editor == editor && tab.path == path) {
                            tab.saved = text;
                            // The user may have kept typing while the write was in flight.
                            tab.dirty = tab.editor.read(cx).text().to_string() != tab.saved;
                            tab.confirm_close = false;
                            tab.preview = false;
                        }
                        this.message = None;
                        this.load_blame(path.clone(), cx);
                        this.debugger.update(cx, |debugger, cx| debugger.file_saved(&path, cx));
                    }
                    Err(err) => {
                        this.message = Some(format!("Couldn't save: {err:#}").into());
                    }
                }
                cx.notify();
                ok
            })
            .unwrap_or(false)
        })
    }
}
