//! Going places in the code: Go to Line, Go to File, symbols, and what
//! the language servers answer (definitions, references, signatures).

use super::*;

impl Workspace {
    /// Ctrl-G: asks for `line` or `line:column` and goes there.
    pub(super) fn go_to_line(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_editor().is_none() {
            return;
        }
        self.symbols = None;
        let picker = cx.new(|cx| Picker::free_text("Go to Line (line or line:column)…", window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            this.finder = None;
            match event {
                PickerEvent::Pick(text) => {
                    let mut parts = text.trim().splitn(2, [':', ',']);
                    let line = parts.next().and_then(|line| line.trim().parse::<u32>().ok());
                    let column = parts.next().and_then(|column| column.trim().parse::<u32>().ok()).unwrap_or(1);
                    match (line, this.active_editor()) {
                        (Some(line), Some(editor)) => {
                            this.remember_place(cx);
                            let lines = editor.read(cx).text().lines_len() as u32;
                            let goto = Position::new(line.clamp(1, lines.max(1)) - 1, column.saturating_sub(1));
                            editor.update(cx, |state, cx| state.set_cursor_position(goto, window, cx));
                            reveal_centered(&editor, goto.line, false, cx);
                        }
                        _ => this.focus_ide(window, cx),
                    }
                }
                PickerEvent::Dismiss => this.focus_ide(window, cx),
                PickerEvent::Close => {}
            }
            cx.notify();
        });
        self.finder = Some((picker, subscription));
        cx.notify();
    }

    /// Cmd-P: find a file by name.
    pub(super) fn open_file_finder(&mut self, _: &OpenFileFinder, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((finder, _)) = &self.finder {
            finder.update(cx, |finder, cx| finder.set_files(self.files.clone(), cx));
            return;
        }
        self.symbols = None;
        let finder = cx.new(|cx| Picker::new(self.files.clone(), "Go to File…", true, window, cx));
        let subscription = cx.subscribe_in(&finder, window, |this, _, event: &PickerEvent, window, cx| {
            this.finder = None;
            match event {
                PickerEvent::Pick(file) => {
                    this.remember_place(cx);
                    this.open(this.root.join(file), true, window, cx)
                }
                PickerEvent::Dismiss => this.focus_ide(window, cx),
                PickerEvent::Close => {}
            }
            cx.notify();
        });
        self.finder = Some((finder, subscription));
        // The list is refreshed on every open; meanwhile, the previous one is used.
        self.read_files(cx);
        cx.notify();
    }

    /// Asks for the list of files Cmd-P goes through.
    pub(super) fn read_files(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.files_asked = true;
        let path = self.root.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Response::Files(files)) = client.request(Request::FindFiles { path }).await {
                this.update(cx, |this, cx| {
                    this.files = Arc::new(files);
                    if let Some((finder, _)) = &this.finder {
                        finder.update(cx, |finder, cx| finder.set_files(this.files.clone(), cx));
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Cmd-Shift-O: to a symbol of the file, from its language server (in
    /// Markdown, its headings).
    pub(super) fn go_to_symbol(&mut self, _: &GoToSymbol, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active.filter(|_| self.symbols.is_none()) else {
            return;
        };
        let tab = &mut self.tabs[ix];
        if !matches!(tab.content, Content::Ready) || tab.diff.is_some() || tab.image.is_some() {
            return;
        }
        // The headings are found in the source, and shown there.
        let markdown = tab.markdown.is_some();
        tab.show_source |= markdown;
        let (path, editor) = (tab.path.clone(), tab.editor.clone());
        let text = editor.read(cx).text().to_string();
        let picker = self.open_symbols(None, window, cx);
        if markdown {
            let symbols = symbol_picker::markdown_symbols(&path, &text);
            picker.update(cx, |picker, cx| picker.set_symbols(None, Ok(symbols), cx));
            return;
        }
        let Some(client) = self.client.clone() else {
            picker.update(cx, |picker, cx| picker.set_symbols(None, Err("Not connected to the agent".into()), cx));
            return;
        };
        let request = Request::Lsp { root: self.root.clone(), path, text, line: 0, column: 0, op: LspOp::Symbols };
        cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                this.report_lsp(&editor, &response, cx);
                picker.update(cx, |picker, cx| picker.set_symbols(None, symbols_of(response, "No language server for this file"), cx));
            })
            .ok();
        })
        .detach();
    }

    /// Cmd-Shift-T: to a symbol of the workspace, from the language servers
    /// running for it and that of the file in front.
    pub(super) fn go_to_workspace_symbol(&mut self, _: &GoToWorkspaceSymbol, window: &mut Window, cx: &mut Context<Self>) {
        if self.symbols.is_none() {
            self.open_symbols(Some(self.root.clone()), window, cx);
        }
    }

    /// Keeps the outline on the file in front and its cursor: its symbols are
    /// asked again a moment after it's edited, at once for another file.
    pub(super) fn sync_outline(&mut self, cx: &mut Context<Self>) {
        let tab = self
            .active
            .map(|ix| &self.tabs[ix])
            .filter(|tab| tab.diff.is_none() && tab.image.is_none() && matches!(tab.content, Content::Ready));
        let Some(tab) = tab else {
            self.outline_of = None;
            self.outline_task = Task::ready(());
            self.outline.update(cx, |outline, cx| outline.clear(cx));
            return;
        };
        let (path, editor, markdown, doc) = (tab.path.clone(), tab.editor.clone(), tab.markdown.is_some(), tab.doc);
        let state = editor.read(cx);
        let (cursor, length) = (state.cursor_position().line, state.text().len());
        let key = (editor.entity_id(), self.revision, length);
        self.outline.update(cx, |outline, cx| outline.set_cursor(Some(cursor), cx));
        if self.outline_of == Some(key) {
            return;
        }
        let edited = self.outline_of.is_some_and(|(id, ..)| id == key.0);
        self.outline_of = Some(key);
        self.outline.update(cx, |outline, cx| outline.loading(&path, cx));
        let client = self.client.clone();
        if markdown || doc || client.is_none() {
            let symbols = match client {
                _ if markdown => Ok(symbol_picker::markdown_symbols(&path, &editor.read(cx).text().to_string())),
                // Shown with `den doc`: no file for a server to read.
                _ if doc => Ok(Vec::new()),
                _ => Err("Not connected to the agent".into()),
            };
            self.outline_task = Task::ready(());
            self.outline.update(cx, |outline, cx| outline.set_symbols(&path, symbols, cx));
            return;
        }
        let (client, root, outline) = (client.expect("checked"), self.root.clone(), self.outline.downgrade());
        self.outline_task = cx.spawn(async move |_, cx| {
            if edited {
                cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            }
            let text = editor.read_with(cx, |state, _| state.text().to_string());
            let request = Request::Lsp { root, path: path.clone(), text, line: 0, column: 0, op: LspOp::Symbols };
            let symbols = symbols_of(client.request(request).await, "No language server for this file");
            outline.update(cx, |outline, cx| outline.set_symbols(&path, symbols, cx)).ok();
        });
    }

    pub(super) fn on_outline(&mut self, _: &Entity<OutlinePanel>, event: &OutlineEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            OutlineEvent::Pick(symbol) => self.open_at(symbol.path.clone(), Position::new(symbol.line, symbol.column), window, cx),
        }
    }

    /// Opens the symbol picker: of the workspace at `workspace`, or of the
    /// file in front, which shows each one as it's selected.
    pub(super) fn open_symbols(&mut self, workspace: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Entity<SymbolPicker> {
        self.finder = None;
        let origin = self.active_editor().map(|editor| {
            let state = editor.read(cx);
            let (selections, scroll) = (state.selections(), state.scroll_offset());
            (editor, selections, scroll)
        });
        let picker = cx.new(|cx| SymbolPicker::new(workspace, window, cx));
        let subscription = cx.subscribe_in(&picker, window, Self::on_symbol_picker);
        self.symbols = Some(SymbolSearch { picker: picker.clone(), origin, request: Task::ready(()), _subscription: subscription });
        cx.notify();
        picker
    }

    pub(super) fn on_symbol_picker(&mut self, picker: &Entity<SymbolPicker>, event: &SymbolPickerEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = &self.symbols else {
            return;
        };
        match event {
            // Its name selected, without taking the focus from what's typed.
            SymbolPickerEvent::Preview(symbol) => {
                if let Some((editor, ..)) = &search.origin {
                    editor.update(cx, |state, cx| {
                        let text = state.text();
                        let start = text.position_to_offset(&Position::new(symbol.line, symbol.column));
                        let end = (start + symbol.name.len()).min(text.len());
                        let named = text.slice(start..end) == symbol.name.as_str();
                        state.set_selections(&[(start, if named { end } else { start })], cx);
                    });
                    reveal_centered(editor, symbol.line, false, cx);
                }
                return;
            }
            // Asked a moment after the last key, so typing doesn't send a request per key.
            SymbolPickerEvent::Query(query) => {
                let (picker, query) = (picker.clone(), query.clone());
                let (Some(client), false) = (self.client.clone(), query.is_empty()) else {
                    return;
                };
                let root = self.root.clone();
                let request = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                    // The file in front, as the editor has it.
                    let Ok(file) = this.update(cx, |this, cx| {
                        let editor = this.active_editor()?;
                        let (_, _, path) = this.completion_target(&editor)?;
                        Some((path, editor.read(cx).text().to_string()))
                    }) else {
                        return;
                    };
                    let (path, text) = file.map_or((None, String::new()), |(path, text)| (Some(path), text));
                    let request = Request::LspWorkspaceSymbols { root, path, text, query: query.clone() };
                    let response = client.request(request).await;
                    let symbols = symbols_of(response, "No language server is running for this workspace: open one of its files");
                    picker.update(cx, |picker, cx| picker.set_symbols(Some(&query), symbols, cx));
                });
                if let Some(search) = &mut self.symbols {
                    search.request = request;
                }
                return;
            }
            _ => {}
        }
        let Some(SymbolSearch { origin, .. }) = self.symbols.take() else {
            return;
        };
        // Back to where it was: it's where Go Back returns after a pick.
        let restore = |cx: &mut Context<Self>| {
            if let Some((editor, selections, scroll)) = origin {
                editor.update(cx, |state, cx| {
                    state.set_selections(&selections, cx);
                    state.set_scroll_offset(scroll, cx);
                });
            }
        };
        match event {
            SymbolPickerEvent::Pick(symbol) => {
                restore(cx);
                self.open_at(symbol.path.clone(), Position::new(symbol.line, symbol.column), window, cx);
            }
            SymbolPickerEvent::Dismiss => {
                restore(cx);
                self.focus_ide(window, cx);
            }
            // Clicked elsewhere: the editor stays on what it shows.
            _ => {}
        }
        cx.notify();
    }

    /// F12: to the definition of what's under the cursor. With one target it
    /// jumps; with several, they're listed in References.
    pub(super) fn go_to_definition(&mut self, _: &GoToDefinition, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_lsp(LspOp::Definition, window, cx);
    }

    /// Shift-F12: the references of what's under the cursor, in their panel.
    pub(super) fn find_references(&mut self, _: &FindReferences, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_lsp(LspOp::References, window, cx);
    }

    /// Who to ask for completions in `editor`: the agent, the task and the
    /// file, if it's a text tab (not a diff).
    pub fn completion_target(&self, editor: &Entity<EditorState>) -> Option<(Arc<Client>, PathBuf, PathBuf)> {
        let tab = self.tabs.iter().find(|tab| &tab.editor == editor)?;
        if !matches!(tab.content, Content::Ready) || tab.diff.is_some() {
            return None;
        }
        Some((self.client.clone()?, self.root.clone(), tab.path.clone()))
    }

    /// Keep failures on the tab that made the request, even if focus moved
    /// while the server was answering. Saving doesn't clear this status.
    pub fn report_lsp(&mut self, editor: &Entity<EditorState>, response: &anyhow::Result<Response>, cx: &mut Context<Self>) {
        if let Some(ix) = self.tab_index(editor) {
            self.tabs[ix].lsp_status.observe(response);
            cx.notify();
        }
    }

    /// Asks for the signature of the call at the cursor of `editor` when
    /// `(` or `,` was just typed, or if it's already shown there.
    pub(super) fn ask_signature(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let state = editor.read(cx);
        let cursor = state.cursor_position();
        let shown = self.signature.as_ref().is_some_and(|hint| &hint.editor == editor);
        let text = state.value();
        if state.selections().len() != 1 || !(shown || signature::opens(&text, cursor)) {
            return;
        }
        let Some((client, root, path)) = self.completion_target(editor) else {
            return;
        };
        self.signature_at = Some(cursor);
        let request = Request::Lsp {
            root,
            path,
            text: text.to_string(),
            line: cursor.line,
            column: cursor.character,
            op: LspOp::SignatureHelp,
        };
        let editor = editor.clone();
        self.signature_task = cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                this.report_lsp(&editor, &response, cx);
                this.signature = match response {
                    Ok(Response::Signature(Some(signature))) => Some(SignatureHint { editor, signature }),
                    _ => None,
                };
                cx.notify();
            })
            .ok();
        });
    }

    /// The cursor moved while the signature is shown: along the line it's
    /// asked again (the parameter may be another); off it, it closes.
    pub(super) fn follow_signature(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        if !self.signature.as_ref().is_some_and(|hint| &hint.editor == editor) {
            return;
        }
        let cursor = editor.read(cx).cursor_position();
        match self.signature_at {
            Some(at) if at == cursor => {}
            Some(at) if at.line == cursor.line => self.ask_signature(editor, cx),
            _ => self.close_signature(),
        }
    }

    pub(super) fn close_signature(&mut self) {
        self.signature = None;
        self.signature_at = None;
        self.signature_task = Task::ready(());
    }

    pub(super) fn ask_lsp(&mut self, op: LspOp, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        let tab = &self.tabs[ix];
        if !matches!(tab.content, Content::Ready) || tab.diff.is_some() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let state = tab.editor.read(cx);
        let editor = tab.editor.clone();
        let text = state.text().to_string();
        let cursor = state.cursor_position();
        let path = tab.path.clone();
        let word = word_at(&text, cursor.line, cursor.character);
        let name = if word.is_empty() { "this".to_string() } else { format!("“{word}”") };
        if op == LspOp::References {
            self.references
                .update(cx, |references, cx| references.set_loading(format!("References to {name}"), cx));
            self.show_references(cx);
        } else {
            self.message = Some(format!("Looking for the definition of {name}…").into());
        }
        cx.notify();
        let request = Request::Lsp {
            root: self.root.clone(),
            path,
            text,
            line: cursor.line,
            column: cursor.character,
            op,
        };
        cx.spawn_in(window, async move |this, cx| {
            let response = client.request(request).await;
            this.update_in(cx, |this, window, cx| {
                this.report_lsp(&editor, &response, cx);
                this.message = None;
                match (response, op) {
                    // No language server: F12 can't; Shift-F12 searches for the word.
                    (Ok(Response::Lsp { server: None, .. }), LspOp::Definition) => {}
                    (Ok(Response::Lsp { server: None, .. }), LspOp::References) => {
                        this.word_references(word, cx);
                    }
                    (Ok(Response::Lsp { locations, .. }), LspOp::Definition) => match locations.as_slice() {
                        [] => this.message = Some(format!("No definition found for {name}").into()),
                        [location] => {
                            let goto = Position::new(location.line, location.column);
                            this.open_at(location.path.clone(), goto, window, cx);
                        }
                        _ => {
                            let hits = this.hits(locations);
                            let title = format!("{} definitions of {name}", hits.len());
                            this.references.update(cx, |references, cx| references.set_results(title, Ok(hits), cx));
                            this.show_references(cx);
                        }
                    },
                    (Ok(Response::Lsp { locations, .. }), LspOp::References) => {
                        let hits = this.hits(locations);
                        let title = match hits.len() {
                            1 => format!("1 reference to {name}"),
                            n => format!("{n} references to {name}"),
                        };
                        this.references.update(cx, |references, cx| references.set_results(title, Ok(hits), cx));
                    }
                    (Ok(other), _) => this.message = Some(format!("Unexpected response: {other:?}").into()),
                    (Err(_), LspOp::Definition) => {}
                    (Err(err), _) => {
                        let title = format!("References to {name}");
                        this.references
                            .update(cx, |references, cx| references.set_results(title, Err(format!("{err:#}").into()), cx));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Without a language server, the references are the places where the
    /// whole word appears (case-sensitive), and the panel says so.
    pub(super) fn word_references(&mut self, word: String, cx: &mut Context<Self>) {
        let title = format!("“{word}” in text (no language server for this file)");
        let Some(client) = self.client.clone().filter(|_| !word.is_empty()) else {
            let error = "No symbol under the cursor".into();
            self.references.update(cx, |references, cx| references.set_results(title, Err(error), cx));
            return;
        };
        let request = Request::Search {
            path: self.root.clone(),
            query: proto::SearchQuery { text: word, regex: false, case_sensitive: true, whole_word: true },
            include: Vec::new(),
            exclude: Vec::new(),
            max_hits: 5_000,
        };
        cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                let result = match response {
                    Ok(Response::SearchResults { hits, .. }) => Ok(hits),
                    Ok(other) => Err(format!("Unexpected response: {other:?}").into()),
                    Err(err) => Err(format!("{err:#}").into()),
                };
                this.references.update(cx, |references, cx| references.set_results(title, result, cx));
            })
            .ok();
        })
        .detach();
    }

    /// The language server's locations as panel rows: relative to the task
    /// for those inside, absolute for those outside.
    pub(super) fn hits(&self, locations: Vec<LspLocation>) -> Vec<SearchHit> {
        locations
            .into_iter()
            .map(|location| SearchHit {
                path: location
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&location.path)
                    .to_string_lossy()
                    .into_owned(),
                line: location.line + 1,
                column: location.column,
                length: location.length,
                text: location.text,
            })
            .collect()
    }

    pub(super) fn step_result(&mut self, delta: isize, cx: &mut Context<Self>) {
        let references = self.panels.results == Panel::References;
        let panel = if references { &self.references } else { &self.search };
        panel.update(cx, |panel, cx| panel.step(delta, cx));
    }

    /// Shows the References panel without moving focus.
    pub(super) fn show_references(&mut self, cx: &mut Context<Self>) {
        self.terminals_maximized = false;
        self.show_panel(Panel::References, cx);
    }

    /// Cmd-Shift-F: the search panel, with the editor's selection.
    pub(super) fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(Panel::Search, cx);
        let selection = self
            .active
            .map(|ix| self.tabs[ix].editor.read(cx).selected_text().to_string())
            .filter(|text| !text.is_empty() && !text.contains('\n') && text.len() < 200);
        self.search.update(cx, |search, cx| {
            if let Some(selection) = &selection {
                search.set_query(selection, window, cx);
            }
            search.focus(window, cx);
        });
        cx.notify();
    }

    /// Find in Folder: the search panel, searching only under `dir` (all of
    /// the task for its folder).
    pub(super) fn find_in_folder(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let scope = self.repo_path(dir).filter(|dir| !dir.is_empty());
        self.show_panel(Panel::Search, cx);
        self.search.update(cx, |search, cx| {
            search.set_scope(scope, cx);
            search.focus(window, cx);
        });
        cx.notify();
    }
}
