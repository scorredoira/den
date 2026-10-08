//! Tabs showing a file's changes: fetching the diff and laying it out,
//! in one column or side by side.

use super::*;

impl Workspace {
    /// Opens (or reuses) the tab with the diff of `file`: the folder's, or a
    /// commit's, which belongs to that commit only.
    pub(super) fn open_diff(&mut self, of: DiffOf, pin: bool, window: &mut Window, cx: &mut Context<Self>) {
        let file = of.file.clone();
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| {
                tab.diff
                    .as_ref()
                    .is_some_and(|diff| diff.file == file && diff.commit == of.commit && diff.source == of.source)
            })
        {
            if pin {
                self.tabs[ix].preview = false;
            }
            if self.tabs[ix].diff.as_ref() != Some(&of) {
                self.tabs[ix].diff = Some(of);
                self.load_diff(ix, false, window, cx);
            }
            self.activate_with(ix, false, window, cx);
            return;
        }
        // A whole commit is its own tab, not a file's.
        let path = match &of.commit {
            Some((commit, _)) if file.is_empty() => self.root.join(commit),
            _ => self.root.join(&file),
        };
        // Another commit in the preview tab (going through the history): it
        // keeps showing the one before until the new one is read, instead of
        // going blank and being laid out again from nothing.
        let group = self.group;
        let reuse = self.tabs.iter().position(|tab| {
            tab.preview
                && tab.group == group
                && matches!(tab.content, Content::Ready)
                && tab.diff.as_ref().is_some_and(|diff| {
                    diff.commit.is_some() && of.commit.is_some() && !diff.source && !of.source && diff.file == of.file
                })
        });
        if let Some(ix) = reuse.filter(|_| !pin) {
            self.tabs[ix].path = path;
            self.tabs[ix].diff = Some(of);
            self.load_diff(ix, true, window, cx);
            self.activate_with(ix, false, window, cx);
            return;
        }
        let language = if of.source { language::for_path(&path) } else { "diff" };
        let mut tab = self.new_tab_with(path, !pin, language, window, cx);
        tab.diff = Some(of);
        tab.grab_focus = false;
        let ix = self.place_tab(tab, pin);
        self.load_diff(ix, false, window, cx);
        self.activate_with(ix, false, window, cx);
    }

    /// The width last measured for `group`'s body (unknown: as wide as can be).
    pub(super) fn group_width(&self, group: usize) -> Pixels {
        let width = self.group_widths[group.min(1)].get();
        if width > px(0.) { width } else { px(f32::MAX) }
    }

    /// Keeps what a commit showed, the most recent ones only.
    pub(super) fn remember_commit_text(&mut self, of: DiffOf, text: String) {
        const KEPT: usize = 64;
        if self.commit_texts.len() >= KEPT {
            self.commit_texts.clear();
        }
        self.commit_texts.insert(of, text);
    }

    /// Asks the agent for a diff tab's text: a file's changes with the whole
    /// file, a whole commit, or a file as it was.
    pub(super) fn fetch_diff(&self, of: &DiffOf, client: Arc<Client>) -> impl std::future::Future<Output = anyhow::Result<Response>> + use<> {
        let path = self.root.clone();
        let commit = of.commit.as_ref().map(|(commit, _)| commit.clone());
        // A file's changes go side by side, with the whole file.
        let whole = (!of.source && !of.file.is_empty()).then(|| Request::Git {
            path: path.clone(),
            op: GitOp::WholeDiff { file: of.file.clone(), commit: commit.clone(), uncommitted: true },
        });
        let request = match commit {
            Some(commit) => {
                let file = of.file.clone();
                let op = if of.source {
                    GitOp::FileAt { commit, file }
                } else if file.is_empty() {
                    GitOp::Show { commit }
                } else {
                    GitOp::CommitDiff { commit, file }
                };
                Request::Git { path, op }
            }
            None => Request::GitDiff { path, file: of.file.clone(), uncommitted: true },
        };
        async move {
            if let Some(whole) = whole {
                // An agent that doesn't know `WholeDiff` fails: the plain diff then.
                if let Ok(Response::Text(text)) = client.request(whole).await {
                    return Ok(Response::Text(text));
                }
            }
            client.request(request).await
        }
    }

    /// Reads a diff tab's changes. With `reveal` (another commit in the same
    /// tab) it goes to the first change, or the top, as when first shown.
    pub(super) fn load_diff(&mut self, ix: usize, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(of)) = (self.client.clone(), self.tabs[ix].diff.clone()) else {
            return;
        };
        let cached = self.commit_texts.get(&of).cloned();
        let fetch = self.fetch_diff(&of, client);
        cx.spawn_in(window, async move |this, cx| {
            let response = match cached {
                Some(text) => Ok(Response::Text(text)),
                None => {
                    let response = fetch.await;
                    if let Ok(Response::Text(text)) = &response
                        && of.commit.is_some()
                    {
                        let (of, text) = (of.clone(), text.clone());
                        this.update(cx, |this, _| this.remember_commit_text(of, text)).ok();
                    }
                    response
                }
            };
            // A file's changes are split in two sides, and a whole commit's
            // rows highlighted, in the background.
            let mut sides = None;
            if let Ok(Response::Text(text)) = &response
                && !of.source
                && !of.file.is_empty()
            {
                let text = text.clone();
                sides = cx.background_spawn(async move { diff::split(&text) }).await;
            }
            let mut prepared = None;
            if let Ok(Response::Text(show)) = &response
                && of.commit.is_some()
                && of.file.is_empty()
                && !of.source
            {
                let show = show.clone();
                let Ok(task) = this.update(cx, |_, cx| commit_view::prepare(show, cx)) else {
                    return;
                };
                prepared = Some(task.await);
            }
            this.update_in(cx, |this, window, cx| {
                let Some(ix) = this.tabs.iter().position(|tab| tab.diff.as_ref() == Some(&of)) else {
                    return;
                };
                if let Some(sides) = sides {
                    this.show_side_by_side(ix, sides, reveal, window, cx);
                    cx.notify();
                    return;
                }
                if let Some(prepared) = prepared {
                    if let Some(view) = &this.tabs[ix].commit {
                        view.update(cx, |view, cx| view.set(prepared, cx));
                        cx.notify();
                        return;
                    }
                    let width = this.group_width(this.tabs[ix].group);
                    let view = cx.new(|_| CommitView::new(prepared, width));
                    // The tab's commit when clicked: it may show another one by then.
                    let subscription = cx.subscribe_in(&view, window, move |this, view, event: &CommitViewEvent, window, cx| {
                        let CommitViewEvent::OpenFile(file) = event;
                        let commit = this
                            .tabs
                            .iter()
                            .find(|tab| tab.commit.as_ref() == Some(view))
                            .and_then(|tab| tab.diff.as_ref()?.commit.clone());
                        if let Some((hash, short)) = commit {
                            this.open_diff(DiffOf::commit(hash, short, file.clone(), false), true, window, cx);
                        }
                    });
                    let tab = &mut this.tabs[ix];
                    tab.commit = Some(view);
                    tab._subscriptions.push(subscription);
                    tab.content = Content::Ready;
                    cx.notify();
                    return;
                }
                let tab = &mut this.tabs[ix];
                tab.old = None;
                match response {
                    Ok(Response::Text(text)) => {
                        let text = if text.is_empty() && !of.source { "No changes".to_string() } else { text };
                        let focused = window.focused(cx);
                        tab.saved = text.clone();
                        tab.content = Content::Ready;
                        let language = if of.source { language::for_path(&tab.path) } else { "diff" };
                        tab.editor.update(cx, |state, cx| {
                            if state.language_name() != language {
                                state.set_highlighter(language, cx);
                            }
                            state.set_line_styles(Vec::new(), cx);
                            state.set_scrollbar_marks(Vec::new(), cx);
                            state.set_value(text, window, cx);
                        });
                        if let Some(focused) = focused {
                            focused.focus(window, cx);
                        }
                    }
                    Ok(other) => tab.content = Content::Failed(format!("Unexpected response: {other:?}").into()),
                    Err(err) => tab.content = Content::Failed(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows a diff with the whole file, or only its changes again, each
    /// editor's cursor on the same line of the file.
    pub(super) fn toggle_whole_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let new = self.tabs[ix].editor.clone();
        let Some(old) = self.tabs[ix].old.as_mut() else {
            return;
        };
        let shown = diff::collapse(&old.sides, if old.whole { usize::MAX } else { diff::CONTEXT });
        let inline = diff::inline(&shown);
        let line_of = |editor: &Entity<EditorState>, numbers: Vec<Option<u32>>| {
            let row = editor.read(cx).cursor_position().line as usize;
            // A row without a number (left out, or only on the other side): the next one's.
            numbers.into_iter().skip(row).flatten().next()
        };
        let lines = [
            line_of(&new, shown.new.lines.iter().map(|line| line.number).collect()),
            line_of(&old.inline, inline.lines.iter().map(|line| line.new.or(line.old)).collect()),
        ];
        old.whole = !old.whole;
        let (full, inline_editor) = (std::mem::take(&mut old.sides), old.inline.clone());
        let shown = diff::collapse(&full, if old.whole { usize::MAX } else { diff::CONTEXT });
        let inline = diff::inline(&shown);
        self.show_side_by_side(ix, full, false, window, cx);
        let rows = [
            lines[0].and_then(|line| shown.new.lines.iter().position(|row| row.number >= Some(line))),
            lines[1].and_then(|line| inline.lines.iter().position(|row| row.new.or(row.old) >= Some(line))),
        ];
        for (editor, row) in [(&new, rows[0]), (&inline_editor, rows[1])] {
            if let Some(row) = row {
                editor.update(cx, |state, cx| state.set_cursor_position(Position::new(row as u32, 0), window, cx));
                reveal_centered(editor, row as u32, true, cx);
            }
        }
        cx.notify();
    }

    /// Shows a file's diff side by side: the old side in its own editor, the
    /// new one in the tab's, both highlighted as the file and scrolling
    /// together; only the changes, unless chosen otherwise. The first time
    /// it goes to the first change.
    pub(super) fn show_side_by_side(&mut self, ix: usize, full: diff::SideBySide, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        let language = language::for_path(&self.tabs[ix].path);
        let new = self.tabs[ix].editor.clone();
        let first = self.tabs[ix].old.is_none();
        if first {
            let editor = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(false));
            let inline = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(false));
            let selected: [Rc<Cell<bool>>; 3] = Default::default();
            let subscriptions = vec![
                cx.observe(&editor, {
                    let (new, selected) = (new.clone(), selected[0].clone());
                    move |_, old, cx| {
                        follow_scroll(&old, &new, cx);
                        selected.set(has_selection(&old, cx));
                    }
                }),
                cx.observe(&new, {
                    let (old, selected) = (editor.clone(), selected[1].clone());
                    move |_, new, cx| {
                        follow_scroll(&new, &old, cx);
                        selected.set(has_selection(&new, cx));
                    }
                }),
                cx.observe(&inline, {
                    let selected = selected[2].clone();
                    move |_, inline, cx| selected.set(has_selection(&inline, cx))
                }),
            ];
            self.tabs[ix].old = Some(OldSide {
                editor,
                marks: None,
                inline,
                inline_marks: None,
                width: Rc::new(Cell::new(self.group_width(self.tabs[ix].group))),
                selected,
                sides: diff::SideBySide::default(),
                whole: false,
                _subscriptions: subscriptions,
            });
        }
        let whole = self.tabs[ix].old.as_ref().is_some_and(|old| old.whole);
        let sides = diff::collapse(&full, if whole { usize::MAX } else { diff::CONTEXT });
        let theme = cx.theme();
        let removed = (theme.danger.opacity(0.14), theme.danger.opacity(0.3));
        let added = (theme.success.opacity(0.14), theme.success.opacity(0.3));
        let (removed_mark, added_mark) = (theme.danger.opacity(0.7), theme.success.opacity(0.7));
        let skipped = theme.info.opacity(0.1);
        let focused = window.focused(cx);
        let tab = &mut self.tabs[ix];
        tab.saved = sides.new.text.clone();
        tab.content = Content::Ready;
        let old = tab.old.as_mut().expect("the old side was just created");
        let mut marks = Vec::new();
        let sides_marks = [(&old.editor, &sides.old, removed, '−', removed_mark, added_mark), (&new, &sides.new, added, '+', added_mark, removed_mark)];
        for (editor, side, (line, word), marker, own, other) in sides_marks {
            let decorations = side
                .lines
                .iter()
                .filter_map(|line| line.changed.clone().filter(|range| !range.is_empty()))
                .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(word))
                .collect::<Vec<_>>();
            editor.update(cx, |state, cx| {
                if state.language_name() != language {
                    state.set_highlighter(language, cx);
                }
                state.set_soft_wrap(false, window, cx);
                state.set_value(side.text.clone(), window, cx);
                state.set_line_styles(line_styles(side, marker, line, skipped), cx);
                state.set_scrollbar_marks(
                    scrollbar_marks(side.lines.iter().map(|line| match line.kind {
                        diff::Kind::Changed => Some(own),
                        diff::Kind::Gap => Some(other),
                        _ => None,
                    })),
                    cx,
                );
            });
            marks.push(decorations);
        }
        let new_marks = marks.pop().unwrap_or_default();
        let old_marks = marks.pop().unwrap_or_default();
        match &old.marks {
            Some((old_collection, new_collection)) => {
                old_collection.set(old_marks, cx);
                new_collection.set(new_marks, cx);
            }
            None => {
                let old_collection = old.editor.update(cx, |state, cx| state.create_range_decorations_collection(old_marks, cx));
                let new_collection = new.update(cx, |state, cx| state.create_range_decorations_collection(new_marks, cx));
                old.marks = Some((old_collection, new_collection));
            }
        }
        let inline = diff::inline(&sides);
        let inline_marks: Vec<_> = inline
            .lines
            .iter()
            .filter_map(|line| {
                let range = line.changed.clone().filter(|range| !range.is_empty())?;
                let color = if line.new.is_some() { added.1 } else { removed.1 };
                Some(RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color))
            })
            .collect();
        old.inline.update(cx, |state, cx| {
            if state.language_name() != language {
                state.set_highlighter(language, cx);
            }
            state.set_soft_wrap(false, window, cx);
            state.set_value(inline.text.clone(), window, cx);
            state.set_line_styles(inline_line_styles(&inline, removed.0, added.0, skipped), cx);
            state.set_scrollbar_marks(
                scrollbar_marks(inline.lines.iter().map(|line| match (line.old, line.new) {
                    (Some(_), None) => Some(removed_mark),
                    (None, Some(_)) => Some(added_mark),
                    _ => None,
                })),
                cx,
            );
        });
        match &old.inline_marks {
            Some(collection) => collection.set(inline_marks, cx),
            None => {
                let collection = old.inline.update(cx, |state, cx| state.create_range_decorations_collection(inline_marks, cx));
                old.inline_marks = Some(collection);
            }
        }
        if first || reveal {
            if let Some(&row) = sides.changes.first() {
                let at = Position::new(row as u32, 0);
                new.update(cx, |state, cx| state.set_cursor_position(at, window, cx));
                reveal_centered(&new, row as u32, true, cx);
            }
            if let Some(&row) = inline.changes.first() {
                let editor = old.inline.clone();
                editor.update(cx, |state, cx| state.set_cursor_position(Position::new(row as u32, 0), window, cx));
                reveal_centered(&editor, row as u32, true, cx);
            }
        }
        old.sides = full;
        if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }
}
