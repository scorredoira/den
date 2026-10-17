//! The History panel of the side column: the current branch's commits,
//! newest first, each its subject over its author and date (hovered, all of
//! it); or only those that changed a file or a folder. The commit picked
//! shows in the code, in the preview tab: its message and its changes, of a
//! file's or folder's history only that file's or folder's. Under the
//! commits, sized by dragging the line between or folded to their header,
//! the commit's files (not in a file's history: there is only the one).
//! How the branches join is the Graph tab's (see `history`). It only reads, and the agent does all the reading.

use std::{ops::Range, path::PathBuf, sync::Arc, time::Duration};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    resizable_panel,
    tooltip::Tooltip,
    v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{ChangedFile, GitOp, GraphCommit, GraphScope, Request, Response};

use crate::{
    config::{Config, Split, UiText as _},
    history::{SelectNext, SelectPrev, read_graph, ref_labels},
    menu,
};

/// Commits read each time: more are read on scrolling near the end.
const PAGE: usize = 500;

/// Delay after a change on disk before asking git again.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Delay after typing in the search before asking again.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// The commit files' height until dragged.
const FILES_HEIGHT: f32 = 200.;

pub enum HistoryPanelEvent {
    /// Show a commit in the preview tab (`pin`: a tab of its own), only
    /// `only`'s changes if given: a file or (`true`) a folder.
    OpenCommit { commit: String, short: String, only: Option<(String, bool)>, pin: bool },
    /// Scroll the commit's tab to `file`'s changes.
    ShowFile { commit: String, short: String, only: Option<(String, bool)>, file: String },
    /// Show what `file` changed in a commit, in a tab of its own.
    OpenCommitDiff { commit: String, short: String, file: String },
}

pub struct HistoryPanel {
    client: Option<Arc<Client>>,
    root: PathBuf,
    commits: Vec<GraphCommit>,
    /// Only the history of this file or folder (`true`: a folder).
    file: Option<(String, bool)>,
    /// There may be more commits than the ones read.
    more: bool,
    loading: bool,
    error: Option<SharedString>,
    /// Needs rereading when it shows.
    stale: bool,
    /// The newest commit shows once read: the history of a file was asked for.
    show_first: bool,
    query: Entity<InputState>,
    selected: Option<String>,
    /// The selected commit's files, and the one picked among them.
    files: Vec<ChangedFile>,
    file_selected: Option<String>,
    refresh: Option<Task<()>>,
    reading_files: Option<Task<()>>,
    /// Counts the reads of the list from the top: a page more read for one
    /// isn't added to another's.
    listing: u64,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// The commits over their files, its line dragged to size them.
    split: Split,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryPanelEvent> for HistoryPanel {}

impl HistoryPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hash, message or author"));
        let subscriptions = vec![cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.commits.clear();
                this.schedule(SEARCH_DEBOUNCE, cx);
            }
        })];
        Self {
            client,
            root,
            commits: Vec::new(),
            file: None,
            more: false,
            loading: false,
            error: None,
            stale: true,
            show_first: false,
            query,
            selected: None,
            files: Vec::new(),
            file_selected: None,
            refresh: None,
            reading_files: None,
            listing: 0,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            split: Split::new(cx),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_client(&mut self, client: Arc<Client>, visible: bool, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.mark_stale(visible, cx);
    }

    /// Something changed on disk: reread now if it shows, or when it does.
    pub fn mark_stale(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.stale = true;
        if visible {
            self.schedule(DEBOUNCE, cx);
        }
    }

    /// It came into sight: rereads if something changed meanwhile.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if self.stale {
            self.schedule(Duration::ZERO, cx);
        }
    }

    /// Only the history of `file` (relative; `true`: a folder), or of
    /// everything; its newest commit shows once read.
    pub fn show_file(&mut self, file: Option<(String, bool)>, cx: &mut Context<Self>) {
        if self.file != file {
            self.file = file;
            self.commits.clear();
            self.selected = None;
            self.files.clear();
            self.scroll = UniformListScrollHandle::new();
        }
        self.show_first = true;
        self.schedule(Duration::ZERO, cx);
    }

    fn op(&self, skip: usize, limit: usize, cx: &App) -> GitOp {
        let query = self.query.read(cx).value().trim().to_string();
        let file = self.file.as_ref().map(|(file, _)| file.clone());
        GitOp::FindGraph { query, scope: GraphScope::Message, file, all: false, skip, limit }
    }

    /// Rereads the commits: as many as were read, so the list stays where it was.
    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return;
        };
        let path = self.root.clone();
        let op = self.op(0, self.commits.len().max(PAGE), cx);
        self.listing += 1;
        self.loading = true;
        self.refresh = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let response = read_graph(&client, path, op).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.stale = false;
                this.error = None;
                match response {
                    Ok(Response::Graph(commits)) => {
                        this.more = commits.len() >= PAGE.max(this.commits.len());
                        this.commits = commits;
                        if !this.selected.as_ref().is_some_and(|hash| this.commits.iter().any(|commit| commit.hash == *hash)) {
                            this.selected = None;
                            this.files.clear();
                        }
                        if std::mem::take(&mut this.show_first)
                            && let Some(hash) = this.commits.first().map(|commit| commit.hash.clone())
                        {
                            this.select(hash, false, cx);
                        }
                    }
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.clone();
        let skip = self.commits.len();
        let op = self.op(skip, PAGE, cx);
        let listing = self.listing;
        self.more = false;
        cx.spawn(async move |this, cx| {
            let result = read_graph(&client, path, op).await;
            this.update(cx, |this, cx| {
                if let Ok(Response::Graph(commits)) = result
                    && this.listing == listing
                    && this.commits.len() == skip
                {
                    this.more = commits.len() == PAGE;
                    this.commits.extend(commits);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The commit's files show under the commits: not in a file's history.
    fn has_files(&self) -> bool {
        !self.file.as_ref().is_some_and(|(_, folder)| !folder)
    }

    /// Selects a commit and shows it in the code (`pin`: in a tab of its
    /// own), its files under the commits.
    fn select(&mut self, hash: String, pin: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.commits.iter().position(|commit| commit.hash == hash) else {
            return;
        };
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        let short = self.commits[ix].short.clone();
        cx.emit(HistoryPanelEvent::OpenCommit { commit: hash.clone(), short, only: self.file.clone(), pin });
        cx.notify();
        if self.selected.as_ref() == Some(&hash) {
            return;
        }
        self.selected = Some(hash.clone());
        self.files.clear();
        self.file_selected = None;
        let Some(client) = self.client.clone().filter(|_| self.has_files()) else {
            return;
        };
        let path = self.root.clone();
        self.reading_files = Some(cx.spawn(async move |this, cx| {
            let response = client.request(Request::Git { path, op: GitOp::CommitFiles { commit: hash.clone() } }).await;
            this.update(cx, |this, cx| {
                if let Ok(Response::Changes { files, .. }) = response
                    && this.selected.as_ref() == Some(&hash)
                {
                    this.files = match &this.file {
                        Some((folder, true)) => {
                            let folder = folder.trim_end_matches('/');
                            files.into_iter().filter(|file| file.path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))).collect()
                        }
                        _ => files,
                    };
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn select_offset(&mut self, offset: isize, cx: &mut Context<Self>) {
        if self.commits.is_empty() {
            return;
        }
        let current = self.selected.as_ref().and_then(|hash| self.commits.iter().position(|commit| commit.hash == *hash));
        let ix = match current {
            Some(ix) => (ix as isize + offset).clamp(0, self.commits.len() as isize - 1) as usize,
            None => 0,
        };
        if current != Some(ix) {
            self.select(self.commits[ix].hash.clone(), false, cx);
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(1, cx);
    }

    fn selected_commit(&self) -> Option<&GraphCommit> {
        let hash = self.selected.as_ref()?;
        self.commits.iter().find(|commit| commit.hash == *hash)
    }

    fn commit_menu(&self, commit: &GraphCommit, cx: &mut Context<Self>) -> impl Fn(menu::PopupMenu, &mut Window, &mut Context<menu::PopupMenu>) -> menu::PopupMenu + 'static {
        let view = cx.entity().downgrade();
        let (hash, short, subject) = (commit.hash.clone(), commit.short.clone(), commit.subject.clone());
        move |menu, _, _| {
            let (pinned, hash, short, subject) = (hash.clone(), hash.clone(), short.clone(), subject.clone());
            menu.item(menu::item("Open in a Tab", &view, move |this, _, cx| this.select(pinned.clone(), true, cx)))
                .separator()
                .item(menu::item("Copy Hash", &view, move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))))
                .item(menu::item("Copy Short Hash", &view, move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(short.clone()))
                }))
                .item(menu::item("Copy Message", &view, move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(subject.clone()))
                }))
        }
    }

    /// The commits in `range`: the subject (after what points at the
    /// commit), and under it the author and the date; hovered, all of it.
    fn render_rows(&mut self, range: Range<usize>, height: Pixels, focused: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        // Near the end: read the next ones.
        if self.more && range.end + 100 >= self.commits.len() {
            self.load_more(cx);
        }
        range
            .filter_map(|ix| Some((ix, self.commits.get(ix)?.clone())))
            .map(|(ix, commit)| {
                let theme = cx.theme();
                let selected = self.selected.as_ref() == Some(&commit.hash);
                let hash = commit.hash.clone();
                let tip: SharedString = format!("{}\n\n{} <{}>\n{} · {}", commit.subject, commit.author, commit.email, commit.date, commit.short).into();
                v_flex()
                    .id(("history-commit", ix))
                    .h(height)
                    .w_full()
                    .justify_center()
                    .px_3()
                    .border_1()
                    .border_color(transparent_black())
                    .when(selected, |el| el.bg(crate::app::selected_row(cx)))
                    .when(selected && focused, |el| el.border_color(theme.list_active_border))
                    .when(!selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .children(ref_labels(&commit, cx))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(commit.subject.clone())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {}", commit.author, commit.date)),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.select(hash.clone(), event.click_count() >= 2, cx);
                    }))
                    .context_menu(self.commit_menu(&commit, cx))
                    .into_any_element()
            })
            .collect()
    }

    /// The search, and the file or folder whose history it is.
    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .flex_none()
            .px_2()
            .pb_1()
            .gap_1()
            .child(Input::new(&self.query).small().cleanable(true))
            .when_some(self.file.clone(), |el, (file, _)| {
                el.child(
                    h_flex()
                        .id("history-panel-file")
                        .pl_1()
                        .gap_1()
                        .text_ui_small(cx)
                        .text_color(theme.muted_foreground)
                        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().child(file.clone()))
                        .child(
                            div()
                                .id("history-panel-all")
                                .flex_none()
                                .px_1()
                                .rounded(theme.radius)
                                .hover(|style| style.bg(theme.secondary_hover))
                                .child("✕")
                                .tooltip(|window, cx| Tooltip::new("Every File").build(window, cx))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_file(None, cx);
                                    // Every file's commits, the one in the code left as it is.
                                    this.show_first = false;
                                })),
                        )
                        .tooltip(move |window, cx| Tooltip::new(format!("History of {file}")).build(window, cx)),
                )
            })
            .when_some(self.error.clone(), |el, error| {
                el.child(div().px_1().text_ui_small(cx).text_color(theme.danger).whitespace_normal().child(error))
            })
    }

    /// The selected commit's files under a header that folds them.
    fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let folded = Config::get(cx).history_files_folded;
        let theme = cx.theme();
        let count = self.files.len();
        let header = h_flex()
            .id("history-panel-files-header")
            .flex_none()
            .h(px(24.))
            .pl_1()
            .gap_1()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .text_ui_small(cx)
            .text_color(theme.muted_foreground)
            .child(
                svg()
                    .path(if folded { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" })
                    .size(px(14.))
                    .flex_none()
                    .text_color(theme.muted_foreground),
            )
            .child(if count > 0 { format!("FILES ({count})") } else { "FILES".to_string() })
            .on_click(|_, _, cx| Config::update(cx, |config| config.history_files_folded = !config.history_files_folded));
        v_flex()
            .size_full()
            .child(header)
            .when(!folded, |el| {
                el.child(
                    uniform_list(
                        "history-panel-files",
                        count,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            range.filter_map(|ix| Some((ix, this.files.get(ix)?.clone()))).map(|(ix, file)| this.file_row(ix, &file, cx)).collect()
                        }),
                    )
                    .flex_1()
                    .min_h_0(),
                )
            })
    }

    /// A file of the commit: a click goes to its changes in the commit's
    /// tab, a double click shows them in a tab of their own.
    fn file_row(&self, ix: usize, file: &ChangedFile, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.file_selected.as_ref() == Some(&file.path);
        let path = file.path.clone();
        crate::changes::file_row(("history-panel-file", ix).into(), file, selected, 0., cx)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                let Some(commit) = this.selected_commit() else {
                    return;
                };
                let (commit, short) = (commit.hash.clone(), commit.short.clone());
                this.file_selected = Some(path.clone());
                let file = path.clone();
                cx.emit(if event.click_count() >= 2 {
                    HistoryPanelEvent::OpenCommitDiff { commit, short, file }
                } else {
                    HistoryPanelEvent::ShowFile { commit, short, only: this.file.clone(), file }
                });
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = Config::get(cx).font_size(crate::config::TextArea::Interface);
        // Two lines, with room above and below to tell one commit from the next.
        let height = px((size * 4.).round());
        let count = self.commits.len();
        let bar = self.render_bar(cx).into_any_element();
        let files = self.has_files().then(|| self.render_files(cx).into_any_element());
        let folded = Config::get(cx).history_files_folded;
        let theme = cx.theme();
        let empty = (count == 0 && !self.loading && self.error.is_none()).then_some("No commits");
        let commits = div()
            .size_full()
            .key_context("History")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .when_some(empty, |el, empty| {
                el.child(div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(empty))
            })
            .child(
                uniform_list(
                    "history-panel-commits",
                    count,
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        let focused = this.focus_handle.is_focused(window);
                        this.render_rows(range, height, focused, cx)
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full(),
            );
        // Unfolded, the files take the height they were dragged to, under
        // the line between them and the commits; folded, only their header.
        let body = match files {
            Some(files) if !folded => {
                let height = Config::get(cx).history_files_height.unwrap_or(FILES_HEIGHT);
                let state = self.split.state(window.viewport_size().height, (), cx).clone();
                v_resizable("history-panel-split")
                    .with_state(&state)
                    .child(resizable_panel().size_range(px(60.)..px(4000.)).child(commits))
                    .child(resizable_panel().size(px(height)).size_range(px(48.)..px(4000.)).child(files))
                    .on_resize(|state, _, cx| {
                        if let Some(files) = state.read(cx).sizes().get(1).copied() {
                            Config::update_quietly(cx, |config| config.history_files_height = Some(f32::from(files)));
                        }
                    })
                    .into_any_element()
            }
            files => v_flex().size_full().child(div().flex_1().min_h_0().child(commits)).children(files).into_any_element(),
        };
        v_flex().size_full().text_ui(cx).child(bar).child(div().flex_1().min_h_0().child(body))
    }
}
