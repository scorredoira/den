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
            // The status bar shows the cursor position, and the blame follows
            // it. The editor also redraws for itself (its cursor blinks): only
            // a change in what's drawn around it concerns the workspace.
            cx.observe(&editor, |this, editor, cx| {
                if this.look_changed(&editor, cx) {
                    this.highlight_occurrences(&editor, cx);
                    this.follow_signature(&editor, cx);
                    cx.notify()
                }
            }),
        ];
        FileTab {
            path,
            image: None,
            previewed: cx.new(markdown_images::PreviewImages::new),
            editor,
            markdown,
            show_source: false,
            zoom: Rc::new(Cell::new(1.0)),
            fit: Rc::new(Cell::new(false)),
            column: Rc::new(Cell::new(0.0)),
            content: Content::Loading,
            lsp_status: LspStatus::default(),
            saved: String::new(),
            on_disk: None,
            save_lock: Default::default(),
            dirty: false,
            preview,
            confirm_close: false,
            goto: None,
            select_to: None,
            grab_focus: true,
            diff: None,
            old: None,
            page: None,
            restored: false,
            blame: None,
            occurrences: None,
            occurrences_for: Vec::new(),
            occurrences_task: None,
            reading: 0,
            blaming: 0,
            look: EditorLook::default(),
            group: self.group,
            shown: 0,
            view: false,
            doc: false,
            kept: false,
            external: None,
            text: None,
            stand_in: None,
            _subscriptions: subscriptions,
        }
    }

    /// Whether what the workspace draws from `editor` changed since it was
    /// last looked at (see `EditorLook`).
    pub(super) fn look_changed(&mut self, editor: &Entity<EditorState>, cx: &App) -> bool {
        let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.editor == editor) else {
            return false;
        };
        let look = EditorLook::of(editor.read(cx));
        if tab.look == look {
            return false;
        }
        tab.look = look;
        true
    }

    /// Reads a tab's file. On reload (it changed on disk) the cursor and
    /// scroll are kept, and unsaved changes aren't overwritten.
    pub(super) fn load(&mut self, path: PathBuf, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        let read = self
            .client
            .clone()
            .map(|client| client.request(Request::ReadFile { path: path.clone() }));
        let image_format = image_format(&path);
        let renderer = cx.svg_renderer();
        let generation = self.begin_read(&path);
        self.watch_file(&path, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let read = match read {
                Some(read) => match read.await {
                    // Decoded here and held by its tab, which frees it when
                    // it goes.
                    Ok(Response::Bytes(bytes)) if let Some(format) = image_format => cx
                        .background_spawn(async move { Image::from_bytes(content_format(format, &bytes), bytes).to_image_data(renderer) })
                        .await
                        .map(Read::Image)
                        .map_err(|err| format!("Couldn't open: {err:#}")),
                    Ok(Response::Bytes(bytes)) => decode_text(bytes).map(Read::Text),
                    Ok(other) => Err(format!("Unexpected response: {other:?}")),
                    Err(err) => Err(format!("Couldn't open: {err:#}")),
                },
                None => Err("No agent".to_string()),
            };
            this.update_in(cx, |this, window, cx| this.loaded(&path, reload, generation, read, window, cx)).ok();
        })
        .detach();
    }

    /// Counts a read of `path`'s tab, whose answer is what it shows unless
    /// another one is asked for meanwhile: each request is answered on a
    /// thread of its own, and an earlier read may come back last.
    pub(super) fn begin_read(&mut self, path: &Path) -> u64 {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == *path && tab.is_file()) else {
            return 0;
        };
        tab.reading += 1;
        tab.reading
    }

    /// What `load` read of a tab's file, the `generation`th read of it.
    pub(super) fn loaded(
        &mut self,
        path: &Path,
        reload: bool,
        generation: u64,
        read: Result<Read, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == *path && tab.is_file() && tab.reading == generation) else {
            return;
        };
        let name = file_name(path);
        let result = match read {
            Ok(Read::Image(image)) => {
                let replaced = tab.image.replace(image);
                tab.content = Content::Ready;
                tab.restored = false;
                if let Some(replaced) = replaced {
                    self.release_image(replaced, window, cx);
                }
                return cx.notify();
            }
            Ok(Read::Text(text)) => Ok(text),
            Err(err) => Err(err),
        };
        match result {
            // Back to what it was: what's unsaved is unsaved changes again.
            Ok(text) if reload && text == tab.saved => {
                if tab.on_disk.take().is_some() {
                    self.message = None;
                }
            }
            // Already asked about this version.
            Ok(text) if reload && tab.dirty && tab.on_disk.as_ref() == Some(&text) => {}
            // It asks what to do; until answered, saving won't overwrite it.
            Ok(text) if reload && tab.dirty => {
                tab.on_disk = Some(text);
                self.message = Some(format!("{name} changed on disk; your unsaved changes are kept").into());
                if let Some(ix) = self.tabs.iter().position(|tab| tab.path == *path && tab.is_file()) {
                    self.ask_overwrite(ix, "Keep My Changes", window, cx);
                }
            }
            Ok(text) => {
                tab.preview(&text, window, cx);
                tab.on_disk = None;
                tab.saved = text.clone();
                tab.content = Content::Ready;
                tab.restored = false;
                tab.text = Some(text.as_str().into());
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
                for view in self.tabs.iter_mut().filter(|tab| tab.view && tab.path == *path) {
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
                self.load_blame(path.to_path_buf(), cx);
                self.refresh_debug_marks(cx);
                // Setting the text moves focus to the editor: it goes back to
                // where it was, or where it belongs if this is the active tab.
                let grab = self
                    .tabs
                    .iter()
                    .find(|tab| tab.path == *path && tab.is_file())
                    .is_some_and(|tab| tab.grab_focus);
                if !reload && grab && self.active.is_some_and(|ix| self.tabs[ix].path == *path) {
                    self.focus_active(window, cx);
                } else if let Some(focused) = focused {
                    focused.focus(window, cx);
                }
            }
            // A reopened file that's gone doesn't deserve a tab (while
            // offline there's no telling: it's reread on reconnect).
            Err(_) if tab.restored && self.client.as_ref().is_some_and(|client| client.is_connected()) => {
                if let Some(ix) = self.tabs.iter().position(|tab| tab.path == *path && tab.is_file()) {
                    self.forget(ix, cx);
                }
            }
            Err(_) if reload => {
                self.message = Some(format!("{name} no longer exists on disk").into());
            }
            Err(err) => tab.content = Content::Failed(err.into()),
        }
        cx.notify();
    }

    /// Rereads every open file: what changed on disk shows, and what didn't
    /// is left as it is.
    pub(crate) fn reread_open_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|tab| tab.is_file() && !tab.has_no_file() && matches!(tab.content, Content::Ready))
            .map(|tab| tab.path.clone())
            .collect();
        for path in paths {
            self.load(path, true, window, cx);
        }
    }

    /// Rereads the open files among `paths`.
    pub(super) fn reload_changed(&mut self, paths: HashSet<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let reload: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|tab| tab.is_file() && matches!(tab.content, Content::Ready) && paths.contains(&tab.path))
            .map(|tab| tab.path.clone())
            .collect();
        for path in reload {
            self.load(path, true, window, cx);
        }
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
        let visible = self.graph_visible();
        self.graph.update(cx, |graph, cx| graph.mark_stale(visible, cx));
        self.reload_changed(paths.clone(), window, cx);
        let diffs: Vec<usize> = (0..self.tabs.len())
            .filter(|ix| self.tabs[*ix].diff.is_some() && paths.contains(&self.tabs[*ix].path))
            .collect();
        for ix in diffs {
            self.load_diff(ix, window, cx);
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
        self.touch_edit(ix);
        let path = self.tabs[ix].path.clone();
        // The rope is shared, not copied: the text is made a string only
        // for what needs one, once.
        let text = editor.read(cx).text().clone();
        let string = std::cell::OnceCell::new();
        let string = || string.get_or_init(|| text.to_string()).as_str();
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
                if same_text(state.text(), &text) {
                    return;
                }
                if let Some((range, with)) = editing::difference(&state.value(), string()) {
                    let selections = editing::shift(&state.selections(), &range, with.len());
                    state.edit(&[(range, with)], &selections, false, window, cx);
                }
            });
        }
        self.tabs[ix].preview(string(), window, cx);
        // Breakpoints move with the lines they're on, by where the text
        // changed: an edit of another view comes to the file's tab too.
        if self.tabs[ix].is_file() {
            let before = self.tabs[ix].text.replace(text.clone());
            let marked = !self.debugger.read(cx).breakpoints.of(&path).is_empty();
            if marked
                && let Some(before) = before.map(|before| before.to_string())
                && let Some((range, with)) = editing::difference(&before, string())
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
        let dirty = text.len() != file.saved.len() || text != file.saved.as_str();
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

    /// Open File: files from anywhere on this machine, chosen in the
    /// system's dialog. A server's are found with Go to File.
    pub(super) fn open_file(&mut self, _: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        if !self.local {
            self.message = Some("On a server, Go to File opens its files".into());
            return cx.notify();
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open_files(paths, window, cx)).ok();
        })
        .detach();
    }

    /// Files from anywhere on this machine (Open File, dropped from Finder),
    /// each in a tab, the last in front; one Den can't show (a PDF) in its
    /// app. Folders are left alone, and a server can't read them.
    pub(super) fn open_files(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_drop = None;
        if !self.local {
            self.message = Some("Files from this computer open in its workspaces, not a server's".into());
            return cx.notify();
        }
        for path in paths.iter().map(|path| normalize(path)).filter(|path| !path.is_dir()) {
            if shown_in_den(&path) {
                self.open_at(path, Position::new(0, 0), window, cx);
            } else {
                cx.open_with_system(&path);
            }
        }
        cx.notify();
    }

    /// Saves the active tab's file (from a view too), formatting it first if
    /// Settings say so for its type.
    pub(super) fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_file() else {
            return;
        };
        if self.tabs[ix].is_untitled() {
            return self.save_untitled(ix, window, cx);
        }
        if self.tabs[ix].on_disk.is_some() {
            return self.ask_overwrite(ix, "Cancel", window, cx);
        }
        self.save_file(ix, window, cx);
    }

    /// The file changed on disk while its tab had unsaved changes: saving
    /// overwrites that only if told to; or the tab reads it, dropping its
    /// changes. `keep` names the answer that does neither.
    fn ask_overwrite(&mut self, ix: usize, keep: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (path, editor) = (self.tabs[ix].path.clone(), self.tabs[ix].editor.clone());
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("{} changed on disk", file_name(&path)),
            Some("Another program changed it while you had unsaved changes here."),
            &[PromptButton::new(keep), PromptButton::new("Reload, Losing My Changes"), PromptButton::new("Overwrite")],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            this.update_in(cx, |this, window, cx| {
                let Some(ix) = this.tab_index(&editor).filter(|ix| this.tabs[*ix].is_file()) else {
                    return;
                };
                match answer {
                    // What's on disk was read already.
                    Ok(1) => {
                        let tab = &mut this.tabs[ix];
                        let Some(text) = tab.on_disk.take() else {
                            return;
                        };
                        tab.dirty = false;
                        this.message = None;
                        let generation = this.begin_read(&path);
                        this.loaded(&path, true, generation, Ok(Read::Text(text)), window, cx);
                    }
                    // What's on disk is what it saves over.
                    Ok(2) => {
                        let tab = &mut this.tabs[ix];
                        if let Some(text) = tab.on_disk.take() {
                            tab.saved = text;
                        }
                        this.save_file(ix, window, cx);
                    }
                    _ => {}
                }
            })
            .ok();
        })
        .detach();
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
        // Changed on disk too, or untitled: only Cmd-S, which asks, saves it.
        if self.tabs[ix].dirty && self.tabs[ix].on_disk.is_none() && !self.tabs[ix].is_untitled() {
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
        if self.tabs[ix].external.is_some() {
            self.save_edit(ix, cx);
            return Task::ready(true);
        }
        let tab = &self.tabs[ix];
        if tab.is_untitled() {
            self.message = Some(format!("Not saved: {} has no name yet; Save asks for one", file_name(&tab.path)).into());
            cx.notify();
            return Task::ready(false);
        }
        // Image tabs have an empty text editor; saving it would erase the image.
        if !matches!(tab.content, Content::Ready) || !tab.is_file() || tab.image.is_some() {
            return Task::ready(true);
        }
        if tab.on_disk.is_some() {
            self.refuse_overwrite(ix, cx);
            return Task::ready(false);
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
            // closed meanwhile still has its captured text saved. It's what's
            // on disk from now on (unless the write fails): the change the
            // write makes there isn't another program's.
            let written = this.update(cx, |this, cx| {
                let ix = this.tabs.iter().position(|tab| tab.editor == editor && tab.path == path)?;
                if this.tabs[ix].on_disk.is_some() {
                    this.refuse_overwrite(ix, cx);
                    return Some(Err(()));
                }
                let tab = &mut this.tabs[ix];
                let text = tab.editor.read(cx).text().to_string();
                Some(Ok((std::mem::replace(&mut tab.saved, text.clone()), text)))
            });
            let (before, text) = match written {
                Ok(Some(Ok((before, text)))) => (Some(before), text),
                Ok(Some(Err(()))) => return false,
                _ => (None, text),
            };
            // What's on disk is checked as it saves: another program may have
            // changed it before the watch said so.
            if let Some(before) = &before
                && let Ok(Response::Bytes(bytes)) = client.request(Request::ReadFile { path: path.clone() }).await
                && let Ok(disk) = decode_text(bytes)
                && disk != *before
            {
                let window = this.read_with(cx, |this, _| this.window).ok();
                let changed = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    let Some(tab) = this.tabs.iter_mut().find(|tab| tab.editor == editor && tab.path == path) else {
                        return;
                    };
                    tab.saved = before.clone();
                    let generation = this.begin_read(&path);
                    this.loaded(&path, true, generation, Ok(Read::Text(disk)), window, cx);
                };
                if let Some(window) = window {
                    let this = this.clone();
                    window.update(cx, |_, window, cx| this.update(cx, |this, cx| changed(this, window, cx))).ok();
                }
                return false;
            }
            let result = client.request(Request::WriteFile {
                path: path.clone(),
                data: text.clone().into_bytes(),
            }).await;
            this.update(cx, |this, cx| {
                let ok = result.is_ok();
                let tab = this.tabs.iter_mut().find(|tab| tab.editor == editor && tab.path == path);
                match result {
                    Ok(_) => {
                        if let Some(tab) = tab {
                            // The user may have kept typing while the write was in flight.
                            tab.dirty = *tab.editor.read(cx).text() != tab.saved;
                            tab.confirm_close = false;
                            tab.preview = false;
                        }
                        this.message = None;
                        this.load_blame(path.clone(), cx);
                        this.debugger.update(cx, |debugger, cx| debugger.file_saved(&path, cx));
                    }
                    Err(err) => {
                        if let Some(tab) = tab
                            && let Some(before) = before
                            && tab.saved == text
                        {
                            tab.saved = before;
                        }
                        this.message = Some(format!("Couldn't save: {err:#}").into());
                    }
                }
                cx.notify();
                ok
            })
            .unwrap_or(false)
        })
    }

    /// The tab's file changed on disk since it was read: it isn't saved
    /// over without asking (see `ask_overwrite`).
    fn refuse_overwrite(&mut self, ix: usize, cx: &mut Context<Self>) {
        let name = file_name(&self.tabs[ix].path);
        self.message = Some(format!("Not saved: {name} changed on disk; Save asks what to do").into());
        cx.notify();
    }
}
