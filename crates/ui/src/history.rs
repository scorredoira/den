//! The History tab, as gitk lays it out: the commits of every branch, tag
//! and remote above, with their graph, what points at them, their author
//! and their date; under them the selected commit's files and, on their
//! right, the commit (its message and every file's changes, as a commit's
//! tab shows them), each file a click away from its changes. The three parts and the author's
//! and date's columns resize, and keep their sizes. It only reads, and the
//! agent does all the reading.

use std::{ops::Range, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    resizable_panel,
    tooltip::Tooltip,
    v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{GitOp, GraphCommit, Request, Response};

use crate::{
    commit_view::{self, CommitFile, CommitView, CommitViewEvent},
    config::{Config, Split, UiText as _},
    menu,
};

actions!(history, [SelectPrev, SelectNext]);

/// The history's shortcuts: they only apply while its commits have focus.
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("History");
    vec![KeyBinding::new("up", SelectPrev, context), KeyBinding::new("down", SelectNext, context)]
}

/// Commits read each time: more are read on scrolling near the end.
const PAGE: usize = 500;

/// Delay after a change on disk before asking git again.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Delay after typing in the search before asking again.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// The width of a lane of the graph.
const LANE: f32 = 14.;

/// Commits whose text is kept, to go back to them at once.
const MAX_TEXTS: usize = 64;

pub enum HistoryEvent {
    /// Show what `file` changed in a commit, in a tab of its own.
    OpenCommitDiff { commit: String, short: String, file: String },
    /// Show `file` as it was in a commit.
    OpenFileAt { commit: String, short: String, file: String },
    OpenFile { file: String },
    /// Select `file` in the Files panel.
    RevealInTree { file: String },
}

pub struct HistoryView {
    client: Option<Arc<Client>>,
    root: PathBuf,
    commits: Vec<GraphCommit>,
    graph: Rc<Vec<GraphRow>>,
    /// Only the history of this file or folder (`true`: a folder).
    file: Option<(String, bool)>,
    /// There may be more commits than the ones read.
    more: bool,
    loading: bool,
    error: Option<SharedString>,
    /// Needs rereading when it shows.
    stale: bool,
    query: Entity<InputState>,
    /// The selected commit, and the one its diff shows.
    selected: Option<String>,
    shown: Option<String>,
    /// What `git show` said of the commits read, the most recent last.
    texts: Vec<(String, Arc<String>)>,
    /// The shown commit's files, and the one picked among them (`None`: its
    /// message).
    files: Vec<CommitFile>,
    file_selected: Option<SharedString>,
    commit: Entity<CommitView>,
    refresh: Option<Task<()>>,
    reading: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// The files' list: its selection is outlined while it has the keyboard.
    files_focus: FocusHandle,
    /// The commits above the commit, the files beside the commit, and the
    /// commits' columns.
    rows: Split,
    bottom: Split,
    columns: Split,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for HistoryView {}

impl HistoryView {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hash, message or author"));
        let commit = cx.new(|_| CommitView::new(commit_view::Prepared::default(), px(0.)));
        let subscriptions = vec![
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.schedule(SEARCH_DEBOUNCE, cx);
                }
            }),
            cx.subscribe(&commit, |this, _, event: &CommitViewEvent, cx| {
                let CommitViewEvent::OpenFile(file) = event;
                if let Some(commit) = this.shown_commit() {
                    let (commit, short) = (commit.hash.clone(), commit.short.clone());
                    cx.emit(HistoryEvent::OpenCommitDiff { commit, short, file: file.clone() });
                }
            }),
        ];
        Self {
            client,
            root,
            commits: Vec::new(),
            graph: Rc::new(Vec::new()),
            file: None,
            more: false,
            loading: false,
            error: None,
            stale: true,
            query,
            selected: None,
            shown: None,
            texts: Vec::new(),
            files: Vec::new(),
            file_selected: None,
            commit,
            refresh: None,
            reading: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            files_focus: cx.focus_handle(),
            rows: Split::new(cx),
            bottom: Split::new(cx),
            columns: Split::new(cx),
            _subscriptions: subscriptions,
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Switches to a new connection with the agent.
    pub fn set_client(&mut self, client: Arc<Client>, visible: bool, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.mark_stale(visible, cx);
    }

    /// Something changed on disk: reread (after a short delay, since changes
    /// arrive in bursts) if it shows, or when it's shown.
    pub fn mark_stale(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.stale = true;
        if visible {
            self.schedule(DEBOUNCE, cx);
        }
    }

    /// It shows: reread if something changed since.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if self.stale {
            self.schedule(Duration::ZERO, cx);
        }
    }

    /// Only the history of `file` (relative; `true`: a folder), or of everything.
    pub fn show_file(&mut self, file: Option<(String, bool)>, cx: &mut Context<Self>) {
        if self.file != file {
            self.file = file;
            self.commits.clear();
            self.graph = Rc::new(Vec::new());
            self.selected = None;
        }
        self.schedule(Duration::ZERO, cx);
    }

    fn op(&self, skip: usize, limit: usize, cx: &App) -> GitOp {
        let query = self.query.read(cx).value().trim().to_string();
        let file = self.file.as_ref().map(|(file, _)| file.clone());
        GitOp::Graph { query, file, skip, limit }
    }

    /// Rereads the commits: as many as were read, so the list stays where it was.
    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return;
        };
        let path = self.root.clone();
        let op = self.op(0, self.commits.len().max(PAGE), cx);
        self.loading = true;
        self.refresh = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let response = client.request(Request::Git { path, op }).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                this.stale = false;
                this.error = None;
                match response {
                    Ok(Response::Graph(commits)) => {
                        this.more = commits.len() >= PAGE.max(this.commits.len());
                        this.set_commits(commits, cx);
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

    fn set_commits(&mut self, commits: Vec<GraphCommit>, cx: &mut Context<Self>) {
        self.commits = commits;
        self.graph = Rc::new(graph(&self.commits, self.linked(cx)));
        // The selected commit if it's still there, or the newest.
        let kept = self.selected.as_ref().is_some_and(|hash| self.commits.iter().any(|commit| commit.hash == *hash));
        if !kept {
            match self.commits.first().map(|commit| commit.hash.clone()) {
                Some(hash) => self.select(hash, false, cx),
                None => {
                    self.selected = None;
                    self.shown = None;
                    self.files.clear();
                    self.commit.update(cx, |view, cx| view.set(commit_view::Prepared::default(), cx));
                }
            }
        }
    }

    /// The graph joins the commits, unless a search leaves some out.
    fn linked(&self, cx: &App) -> bool {
        self.query.read(cx).value().trim().is_empty()
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.clone();
        let skip = self.commits.len();
        let op = self.op(skip, PAGE, cx);
        self.more = false;
        cx.spawn(async move |this, cx| {
            let result = client.request(Request::Git { path, op }).await;
            this.update(cx, |this, cx| {
                if let Ok(Response::Graph(commits)) = result
                    && this.commits.len() == skip
                {
                    this.more = commits.len() == PAGE;
                    this.commits.extend(commits);
                    this.graph = Rc::new(graph(&this.commits, this.linked(cx)));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn shown_commit(&self) -> Option<&GraphCommit> {
        let hash = self.shown.as_ref()?;
        self.commits.iter().find(|commit| commit.hash == *hash)
    }

    fn text(&self, hash: &str) -> Option<Arc<String>> {
        self.texts.iter().find(|(text_hash, _)| text_hash == hash).map(|(_, text)| text.clone())
    }

    fn keep_text(&mut self, hash: String, text: Arc<String>) {
        self.texts.retain(|(kept, _)| *kept != hash);
        self.texts.push((hash, text));
        if self.texts.len() > MAX_TEXTS {
            self.texts.remove(0);
        }
    }

    /// Selects a commit and shows it under the list; `reveal` scrolls the
    /// list to it.
    fn select(&mut self, hash: String, reveal: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.commits.iter().position(|commit| commit.hash == hash) else {
            return;
        };
        self.selected = Some(hash.clone());
        if reveal {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
        if self.shown.as_ref() == Some(&hash) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let cached = self.text(&hash);
        let path = self.root.clone();
        self.reading = Some(cx.spawn(async move |this, cx| {
            let text = match cached {
                Some(text) => text,
                None => match client.request(Request::Git { path, op: GitOp::Show { commit: hash.clone() } }).await {
                    Ok(Response::Text(text)) => Arc::new(text),
                    Ok(_) => return,
                    Err(err) => {
                        this.update(cx, |this, cx| {
                            this.error = Some(format!("{err:#}").into());
                            cx.notify();
                        })
                        .ok();
                        return;
                    }
                },
            };
            let Ok(prepare) = this.update(cx, |this, cx| {
                this.keep_text(hash.clone(), text.clone());
                commit_view::prepare(text.to_string(), cx)
            }) else {
                return;
            };
            let prepared = prepare.await;
            this.update(cx, |this, cx| {
                this.shown = Some(hash);
                this.files = prepared.files();
                this.file_selected = None;
                this.commit.update(cx, |view, cx| view.set(prepared, cx));
                cx.notify();
            })
            .ok();
        }));
        self.prefetch_around(ix, cx);
    }

    /// Reads ahead the commits before and after the `ix`th: stepping to
    /// them is then instant.
    fn prefetch_around(&self, ix: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        for near in [ix + 1, ix.wrapping_sub(1)] {
            let Some(commit) = self.commits.get(near) else {
                continue;
            };
            if self.text(&commit.hash).is_some() {
                continue;
            }
            let (hash, path, client) = (commit.hash.clone(), self.root.clone(), client.clone());
            cx.spawn(async move |this, cx| {
                if let Ok(Response::Text(text)) = client.request(Request::Git { path, op: GitOp::Show { commit: hash.clone() } }).await {
                    this.update(cx, |this, _| this.keep_text(hash, Arc::new(text))).ok();
                }
            })
            .detach();
        }
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
            self.select(self.commits[ix].hash.clone(), true, cx);
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(1, cx);
    }

    /// Picks one of the commit's files (`None`: its message) and scrolls its
    /// diff there.
    fn pick_file(&mut self, file: Option<SharedString>, cx: &mut Context<Self>) {
        self.commit.read(cx).scroll_to(file.as_deref());
        self.file_selected = file;
        cx.notify();
    }
}

/// A commit's row of the graph: its dot, and the lines through the row.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GraphRow {
    /// The dot's lane, and its color.
    lane: usize,
    color: usize,
    /// Lines from the row's top (a lane) to its middle (a lane), with their
    /// color.
    top: Vec<(usize, usize, usize)>,
    /// And from its middle to its bottom.
    bottom: Vec<(usize, usize, usize)>,
    /// Lanes wide.
    width: usize,
}

/// The graph of `commits`, newest first and each before its parents. Each
/// line goes down a lane of its own, towards the commit it waits for; a
/// branch gets a lane of its own and a color. Not `linked` (a search), each
/// commit is only a dot.
pub(crate) fn graph(commits: &[GraphCommit], linked: bool) -> Vec<GraphRow> {
    if !linked {
        return commits.iter().map(|_| GraphRow { width: 1, ..GraphRow::default() }).collect();
    }
    // What each lane waits for, and its color.
    let mut lanes: Vec<Option<(&str, usize)>> = Vec::new();
    let mut colors = 0;
    let mut new_color = || {
        colors += 1;
        colors - 1
    };
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let waiting = |lane: &Option<(&str, usize)>| lane.is_some_and(|(hash, _)| hash == commit.hash);
        let lane = match lanes.iter().position(waiting) {
            Some(lane) => lane,
            None => match lanes.iter().position(Option::is_none) {
                Some(free) => free,
                None => {
                    lanes.push(None);
                    lanes.len() - 1
                }
            },
        };
        let color = lanes[lane].map_or_else(&mut new_color, |(_, color)| color);
        let top: Vec<(usize, usize, usize)> = lanes
            .iter()
            .enumerate()
            .filter_map(|(ix, waits)| waits.map(|(hash, color)| (ix, if hash == commit.hash { lane } else { ix }, color)))
            .collect();
        let before = lanes.len();
        for waits in lanes.iter_mut().filter(|waits| waiting(waits)) {
            *waits = None;
        }
        // The lanes that go on past it, and those its parents add.
        let mut bottom: Vec<(usize, usize, usize)> =
            lanes.iter().enumerate().filter_map(|(ix, waits)| waits.map(|(_, color)| (ix, ix, color))).collect();
        for (nth, parent) in commit.parents.iter().enumerate() {
            if let Some(other) = lanes.iter().position(|waits| waits.is_some_and(|(hash, _)| hash == parent)) {
                let other_color = lanes[other].map_or(color, |(_, color)| color);
                if nth == 0 && other > lane {
                    // Its first parent goes on straight down: the line
                    // further right joins this one.
                    if let Some(pass) = bottom.iter_mut().find(|(from, _, _)| *from == other) {
                        pass.1 = lane;
                    }
                    lanes[other] = None;
                    lanes[lane] = Some((parent.as_str(), color));
                    bottom.push((lane, lane, color));
                } else {
                    // Another line already goes to it: this one joins it.
                    bottom.push((lane, other, other_color));
                }
                continue;
            }
            let (target, parent_color) = if nth == 0 {
                (lane, color)
            } else {
                let free = lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                    lanes.push(None);
                    lanes.len() - 1
                });
                (free, new_color())
            };
            lanes[target] = Some((parent.as_str(), parent_color));
            bottom.push((lane, target, parent_color));
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        bottom.sort_unstable();
        let width = before.max(lanes.len()).max(lane + 1);
        rows.push(GraphRow { lane, color, top, bottom, width });
    }
    rows
}

/// The colors of the lanes, in turn.
fn lane_color(color: usize, cx: &App) -> Hsla {
    let theme = cx.theme();
    [theme.blue, theme.green, theme.magenta, theme.cyan, theme.yellow, theme.red][color % 6]
}

/// A row's piece of the graph, drawn as tall as the row.
fn graph_cell(row: &GraphRow, head: bool, cx: &App) -> impl IntoElement {
    let lines: Vec<(usize, usize, bool, Hsla)> = row
        .top
        .iter()
        .map(|&(from, to, color)| (from, to, true, lane_color(color, cx)))
        .chain(row.bottom.iter().map(|&(from, to, color)| (from, to, false, lane_color(color, cx))))
        .collect();
    let (lane, dot) = (row.lane, lane_color(row.color, cx));
    let background = cx.theme().background;
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let x = |lane: usize| bounds.left() + px(LANE * lane as f32 + LANE / 2. + 2.);
            let middle = bounds.top() + bounds.size.height / 2.;
            for (from, to, upper, color) in &lines {
                let (start, end) = if *upper {
                    (point(x(*from), bounds.top()), point(x(*to), middle))
                } else {
                    (point(x(*from), middle), point(x(*to), bounds.bottom()))
                };
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(start);
                path.line_to(end);
                if let Ok(path) = path.build() {
                    window.paint_path(path, *color);
                }
            }
            let radius = px(4.);
            let center = point(x(lane), middle);
            let dot_bounds = Bounds::new(point(center.x - radius, center.y - radius), size(radius * 2., radius * 2.));
            // HEAD's commit is hollow, as in gitk.
            let fill_color = if head { background } else { dot };
            window.paint_quad(quad(dot_bounds, radius, fill_color, px(1.5), dot, BorderStyle::Solid));
        },
    )
    .flex_none()
    .w(px(LANE * row.width as f32 + 4.))
    .h_full()
}

/// What points at a commit, as labels: its branches, tags and remote
/// branches, the checked-out one in bold.
fn ref_labels(commit: &GraphCommit, cx: &App) -> Vec<AnyElement> {
    let theme = cx.theme();
    let head_branch = commit.refs.iter().position(|name| name == "HEAD").and_then(|ix| commit.refs.get(ix + 1));
    let detached = commit.refs.iter().any(|name| name == "HEAD") && !head_branch.is_some_and(|name| name.starts_with("refs/heads/"));
    let label = |text: String, color: Hsla, bold: bool| {
        div()
            .flex_none()
            .px_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(color.opacity(0.7))
            .bg(color.opacity(0.18))
            .text_ui_small(cx)
            .when(bold, |el| el.font_weight(FontWeight::BOLD))
            .child(text)
            .into_any_element()
    };
    let mut labels = Vec::new();
    if detached {
        labels.push(label("HEAD".into(), theme.danger, true));
    }
    for name in &commit.refs {
        if let Some(branch) = name.strip_prefix("refs/heads/") {
            labels.push(label(branch.to_string(), theme.success, head_branch == Some(name)));
        } else if let Some(tag) = name.strip_prefix("refs/tags/") {
            labels.push(label(tag.to_string(), theme.warning, false));
        } else if let Some(remote) = name.strip_prefix("refs/remotes/") {
            labels.push(label(remote.to_string(), theme.info, false));
        }
    }
    labels
}

impl HistoryView {
    fn commit_menu(&self, commit: &GraphCommit, cx: &mut Context<Self>) -> impl Fn(menu::PopupMenu, &mut Window, &mut Context<menu::PopupMenu>) -> menu::PopupMenu + 'static {
        let view = cx.entity().downgrade();
        let (hash, short, subject) = (commit.hash.clone(), commit.short.clone(), commit.subject.clone());
        move |menu, _, _| {
            let (hash, short, subject) = (hash.clone(), short.clone(), subject.clone());
            menu.item(menu::item("Copy Hash", &view, move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))))
                .item(menu::item("Copy Short Hash", &view, move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(short.clone()))
                }))
                .item(menu::item("Copy Message", &view, move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(subject.clone()))
                }))
        }
    }

    fn file_menu(&self, file: &CommitFile, cx: &mut Context<Self>) -> impl Fn(menu::PopupMenu, &mut Window, &mut Context<menu::PopupMenu>) -> menu::PopupMenu + 'static {
        let view = cx.entity().downgrade();
        let commit = self.shown_commit().map(|commit| (commit.hash.clone(), commit.short.clone())).unwrap_or_default();
        let path = file.path.to_string();
        let root = self.root.clone();
        move |menu, _, _| {
            let ((diff_hash, diff_short), (at_hash, at_short)) = (commit.clone(), commit.clone());
            let (diff, at, open, copy, reveal) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
            let absolute = root.join(&path).to_string_lossy().into_owned();
            menu.item(menu::item("Open Changes", &view, move |_, _, cx| {
                cx.emit(HistoryEvent::OpenCommitDiff { commit: diff_hash.clone(), short: diff_short.clone(), file: diff.clone() })
            }))
            .item(menu::item("Open File at This Commit", &view, move |_, _, cx| {
                cx.emit(HistoryEvent::OpenFileAt { commit: at_hash.clone(), short: at_short.clone(), file: at.clone() })
            }))
            .item(menu::item("Open File", &view, move |_, _, cx| cx.emit(HistoryEvent::OpenFile { file: open.clone() })))
            .separator()
            .item(menu::item("Copy Path", &view, move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(absolute.clone()))))
            .item(menu::item("Copy Relative Path", &view, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
            }))
            .item(menu::item("Reveal in File Tree", &view, move |_, _, cx| {
                cx.emit(HistoryEvent::RevealInTree { file: reveal.clone() })
            }))
        }
    }

    /// The commits' rows in `range`: graph, labels and message, author, date.
    /// `focused`: the list has the keyboard, and its selection is outlined.
    fn render_rows(
        &mut self,
        range: Range<usize>,
        widths: (Pixels, Pixels),
        height: Pixels,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        // Near the end: read the next ones.
        if self.more && range.end + 100 >= self.commits.len() {
            self.load_more(cx);
        }
        let graph = self.graph.clone();
        range
            .filter_map(|ix| Some((ix, self.commits.get(ix)?.clone())))
            .map(|(ix, commit)| {
                let theme = cx.theme();
                let selected = self.selected.as_ref() == Some(&commit.hash);
                let head = commit.refs.iter().any(|name| name == "HEAD");
                let row = graph.get(ix).cloned().unwrap_or_default();
                let cell = |width: Pixels| div().flex_none().w(width).px_2().overflow_hidden().whitespace_nowrap().text_ellipsis();
                let hash = commit.hash.clone();
                h_flex()
                    .id(("commit", ix))
                    .h(height)
                    .w_full()
                    .border_1()
                    .border_color(transparent_black())
                    .when(selected, |el| el.bg(crate::app::selected_row(cx)))
                    .when(selected && focused, |el| el.border_color(theme.list_active_border))
                    .when(!selected, |el| el.hover(|style| style.bg(theme.list_hover)))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .pl_1()
                            .overflow_hidden()
                            .child(graph_cell(&row, head, cx))
                            .child(h_flex().flex_none().gap_1().pr_1().children(ref_labels(&commit, cx)))
                            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(commit.subject.clone())),
                    )
                    .child(
                        cell(widths.0)
                            .child(commit.author.clone())
                            .when(!commit.email.is_empty(), |el| {
                                el.child(div().flex_none().text_color(theme.muted_foreground).child(format!(" <{}>", commit.email)))
                            })
                            .flex(),
                    )
                    .child(cell(widths.1).text_color(theme.muted_foreground).child(commit.date.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.focus_handle.focus(window, cx);
                        this.select(hash.clone(), false, cx);
                    }))
                    .context_menu(self.commit_menu(&commit, cx))
                    .into_any_element()
            })
            .collect()
    }

    /// The search, and the file whose history it is, if it is one.
    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().w(px(320.)).flex_none().child(Input::new(&self.query).small().cleanable(true)))
            .when_some(self.file.as_ref().map(|(file, _)| file.clone()), |el, file| {
                el.child(
                    h_flex()
                        .min_w_0()
                        .gap_1()
                        .text_ui_small(cx)
                        .text_color(theme.muted_foreground)
                        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(format!("History of {file}")))
                        .child(
                            div()
                                .id("history-all")
                                .px_1()
                                .rounded(theme.radius)
                                .hover(|style| style.bg(theme.secondary_hover))
                                .child("✕")
                                .tooltip(|window, cx| Tooltip::new("Every File").build(window, cx))
                                .on_click(cx.listener(|this, _, _, cx| this.show_file(None, cx))),
                        ),
                )
            })
            .child(div().flex_1())
            .when(self.loading, |el| el.child(div().text_ui_small(cx).text_color(theme.muted_foreground).child("…")))
            .when_some(self.error.clone(), |el, error| {
                el.child(div().min_w_0().overflow_hidden().text_ellipsis().text_ui_small(cx).text_color(theme.danger).child(error))
            })
    }

    /// The commits: their columns' titles, which resize them, and the rows.
    fn render_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sizes = Config::get(cx).history;
        let state = self.columns.state(window.viewport_size().width, (), cx).clone();
        let widths = match state.read(cx).sizes().as_slice() {
            [_, author, date] => (*author, *date),
            _ => (px(sizes.author), px(sizes.date)),
        };
        let theme = cx.theme();
        let title = |text: &'static str| div().px_2().h_full().flex().items_center().overflow_hidden().whitespace_nowrap().child(text);
        let header = h_resizable("history-columns")
            .with_state(&state)
            .child(resizable_panel().child(title("Commit").pl(px(LANE + 6.))))
            .child(resizable_panel().size(px(sizes.author)).size_range(px(60.)..px(1000.)).child(title("Author")))
            .child(resizable_panel().size(px(sizes.date)).size_range(px(60.)..px(600.)).child(title("Date")))
            .on_resize(|state, _, cx| {
                if let [_, author, date] = state.read(cx).sizes().as_slice() {
                    let (author, date) = (f32::from(*author), f32::from(*date));
                    Config::update_quietly(cx, |config| {
                        config.history.author = author;
                        config.history.date = date;
                    });
                }
            });
        let size = Config::get(cx).font_size(crate::config::TextArea::Interface);
        let height = px((size * 1.75).round());
        let count = self.commits.len();
        let empty = (count == 0 && !self.loading && self.error.is_none()).then_some("No commits");
        v_flex()
            .size_full()
            .child(
                div()
                    .flex_none()
                    .h(px((size * 1.6).round()))
                    .border_b_1()
                    .border_color(theme.border)
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(header),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(cfg!(test), |el| el.debug_selector(|| "history-commits".into()))
                    .key_context("History")
                    .track_focus(&self.focus_handle)
                    .on_action(cx.listener(Self::select_prev))
                    .on_action(cx.listener(Self::select_next))
                    .when_some(empty, |el, empty| {
                        el.child(div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(empty))
                    })
                    .child(
                        uniform_list(
                            "history-commits",
                            count,
                            cx.processor(move |this, range: Range<usize>, window, cx| {
                                let focused = this.focus_handle.is_focused(window);
                                this.render_rows(range, widths, height, focused, cx)
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
            )
            .into_any_element()
    }

    /// The shown commit's files: its message first, as in gitk.
    fn render_files(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let focused = self.files_focus.is_focused(window);
        let message = h_flex()
            .id("commit-message")
            .h(px(24.))
            .px_3()
            .border_1()
            .border_color(transparent_black())
            .text_color(theme.muted_foreground)
            .when(self.file_selected.is_none(), |el| el.bg(crate::app::selected_row(cx)).text_color(theme.foreground))
            .when(self.file_selected.is_none() && focused, |el| el.border_color(theme.list_active_border))
            .when(self.file_selected.is_some(), |el| el.hover(|style| style.bg(theme.list_hover)))
            .child("Message")
            .on_click(cx.listener(|this, _, window, cx| {
                this.files_focus.focus(window, cx);
                this.pick_file(None, cx)
            }));
        let count = self.files.len();
        v_flex()
            .size_full()
            .when(cfg!(test), |el| el.debug_selector(|| "commit-files".into()))
            .track_focus(&self.files_focus)
            .text_ui(cx)
            .child(message)
            .child(
                uniform_list(
                    "commit-files",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        let focused = this.files_focus.is_focused(window);
                        range
                            .filter_map(|ix| Some((ix, this.files.get(ix)?.clone())))
                            .map(|(ix, file)| this.file_row(ix, &file, focused, cx))
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    /// A file row: name, folder, and lines added and removed. A click goes
    /// to its changes in the diff; a double click opens them in a tab.
    fn file_row(&self, ix: usize, file: &CommitFile, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selected = self.file_selected.as_ref() == Some(&file.path);
        let dir = file.path.rfind('/').map_or(0, |slash| slash + 1);
        let muted = HighlightStyle { color: Some(theme.muted_foreground), ..Default::default() };
        let path = StyledText::new(file.path.clone()).with_highlights([(0..dir, muted)]);
        let tip = file.path.clone();
        let picked = file.path.clone();
        h_flex()
            .id(("commit-file", ix))
            .h(px(24.))
            .px_3()
            .gap_2()
            .border_1()
            .border_color(transparent_black())
            .when(selected, |el| el.bg(crate::app::selected_row(cx)))
            .when(selected && focused, |el| el.border_color(theme.list_active_border))
            .when(!selected, |el| el.hover(|style| style.bg(theme.list_hover)))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().child(path))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .text_ui_small(cx)
                    .when(file.added > 0, |el| el.child(div().text_color(theme.success).child(format!("+{}", file.added))))
                    .when(file.removed > 0, |el| el.child(div().text_color(theme.danger).child(format!("−{}", file.removed)))),
            )
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.files_focus.focus(window, cx);
                if event.click_count() >= 2
                    && let Some(commit) = this.shown_commit()
                {
                    let (commit, short) = (commit.hash.clone(), commit.short.clone());
                    cx.emit(HistoryEvent::OpenCommitDiff { commit, short, file: picked.to_string() });
                }
                this.pick_file(Some(picked.clone()), cx);
            }))
            .context_menu(self.file_menu(file, cx))
            .into_any_element()
    }
}

impl Focusable for HistoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sizes = Config::get(cx).history;
        let viewport = window.viewport_size();
        let rows = self.rows.state(viewport.height, (), cx).clone();
        let bottom = self.bottom.state(viewport.width, (), cx).clone();
        let bar = self.render_bar(cx).into_any_element();
        let commits = self.render_commits(window, cx);
        let files = self.render_files(window, cx);
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .text_ui(cx)
            .child(bar)
            .child(
                div().flex_1().min_h_0().child(
                    v_resizable("history-rows")
                        .with_state(&rows)
                        .child(resizable_panel().size(px(sizes.commits)).size_range(px(80.)..px(4000.)).child(commits))
                        .child(
                            resizable_panel().child(
                                // The files at the left, the commit beside them.
                                h_resizable("history-bottom")
                                    .with_state(&bottom)
                                    .child(
                                        resizable_panel()
                                            .size(px(sizes.files))
                                            .size_range(px(120.)..px(2000.))
                                            .child(div().size_full().border_t_1().border_r_1().border_color(theme.border).child(files)),
                                    )
                                    .child(resizable_panel().child(div().size_full().border_t_1().border_color(theme.border).child(self.commit.clone())))
                                    .on_resize(|state, _, cx| {
                                        if let Some(files) = state.read(cx).sizes().first().copied() {
                                            Config::update_quietly(cx, |config| config.history.files = f32::from(files));
                                        }
                                    }),
                            ),
                        )
                        .on_resize(|state, _, cx| {
                            if let Some(commits) = state.read(cx).sizes().first().copied() {
                                Config::update_quietly(cx, |config| config.history.commits = f32::from(commits));
                            }
                        }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn commit(hash: &str, parents: &[&str]) -> GraphCommit {
        GraphCommit {
            hash: hash.into(),
            short: hash.into(),
            author: String::new(),
            email: String::new(),
            date: String::new(),
            parents: parents.iter().map(|parent| parent.to_string()).collect(),
            refs: Vec::new(),
            subject: String::new(),
        }
    }

    #[test]
    fn a_straight_history_is_one_lane() {
        let rows = graph(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])], true);
        assert!(rows.iter().all(|row| row.lane == 0 && row.width == 1));
        assert_eq!(rows[0].top, []);
        assert_eq!(rows[0].bottom, [(0, 0, 0)]);
        assert_eq!(rows[1].top, [(0, 0, 0)]);
        assert_eq!(rows[2].bottom, []);
    }

    #[test]
    fn a_merge_opens_a_lane_that_closes_at_the_fork() {
        // m merges x (a branch) into b; both come from a.
        let rows = graph(
            &[commit("m", &["b", "x"]), commit("x", &["a"]), commit("b", &["a"]), commit("a", &[])],
            true,
        );
        // The merge: its first parent below it, the branch in a lane of its own.
        assert_eq!(rows[0].bottom, [(0, 0, 0), (0, 1, 1)]);
        assert_eq!((rows[1].lane, rows[1].color), (1, 1));
        // Under the first parent the branch's lane joins the first one.
        assert_eq!(rows[2].lane, 0);
        assert_eq!(rows[2].bottom, [(0, 0, 0), (1, 0, 1)]);
        assert_eq!(rows[3].top, [(0, 0, 0)]);
        assert_eq!(rows[3].lane, 0);
        assert!(rows.iter().all(|row| row.width <= 2));
    }

    #[test]
    fn two_branch_tips_take_two_lanes() {
        let rows = graph(&[commit("t1", &["a"]), commit("t2", &["a"]), commit("a", &[])], true);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 1);
        // The second tip joins the first's lane, which waits for the same parent.
        assert_eq!(rows[1].bottom, [(0, 0, 0), (1, 0, 0)]);
        assert_eq!(rows[2].top, [(0, 0, 0)]);
    }

    #[test]
    fn a_search_is_only_dots() {
        let rows = graph(&[commit("c", &["b"]), commit("a", &[])], false);
        assert!(rows.iter().all(|row| row.top.is_empty() && row.bottom.is_empty() && row.lane == 0));
    }
}
