//! The Graph tab, as `git log --graph --oneline` and then `git show`: the
//! commits of the current branch (or, with All Branches, of every branch, tag
//! and remote), one line each across the whole tab, with their graph, hash,
//! what points at them, subject, author and date; Enter or a click opens one
//! in their place, its message, files and changes as a commit's tab shows
//! them, and Esc goes back to the list where it was. Up and down step through
//! the commits in both, and Page Up and Page Down scroll an open one. A
//! file's or a folder's history is the same list, of the commits that
//! changed it, each opened with only its changes. The
//! author's and date's columns resize and keep their sizes. It only reads,
//! and the agent does all the reading.

use std::{cell::Cell, ops::Range, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    resizable_panel,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{GitOp, GraphCommit, GraphScope, Request, Response};

use crate::{
    commit_view::{self, CommitView, CommitViewEvent},
    config::{Config, Split, UiText as _},
    menu,
};

actions!(history, [SelectPrev, SelectNext, PageUp, PageDown, OpenCommit, CloseCommit]);

/// The history's shortcuts: they only apply while it has focus.
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("History");
    vec![
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("pageup", PageUp, context),
        KeyBinding::new("pagedown", PageDown, context),
        KeyBinding::new("enter", OpenCommit, context),
        KeyBinding::new("escape", CloseCommit, context),
    ]
}

/// Commits read each time: more are read on scrolling near the end.
const PAGE: usize = 500;

/// Delay after a change on disk before asking git again.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Delay after typing in the search before asking again.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// The width of a lane of the graph.
const LANE: f32 = 14.;

/// The room the commits keep for their subject, author and date, in widths
/// of the interface's font. Short of it, the author's and then the date's
/// columns give up theirs down to their own.
const SUBJECT_ROOM: f32 = 28.;
const AUTHOR_ROOM: f32 = 9.;
const DATE_ROOM: f32 = 7.;

/// Commits whose text is kept, to go back to them at once.
const MAX_TEXTS: usize = 64;

/// What the open commit asks of one of its files.
pub struct HistoryEvent {
    pub commit: String,
    pub short: String,
    pub event: CommitViewEvent,
}

pub struct HistoryView {
    client: Option<Arc<Client>>,
    root: PathBuf,
    commits: Vec<GraphCommit>,
    graph: Rc<Vec<GraphRow>>,
    /// There may be more commits than the ones read.
    more: bool,
    loading: bool,
    error: Option<SharedString>,
    /// Needs rereading when it shows.
    stale: bool,
    query: Entity<InputState>,
    /// Where the search looks: the message, the paths or the content.
    scope: GraphScope,
    /// Only the history of this file or folder (`true`: a folder).
    file: Option<(String, bool)>,
    /// The selected commit, and the one its view shows.
    selected: Option<String>,
    shown: Option<String>,
    /// The selected commit shows in the list's place.
    open: bool,
    /// What `git show` said of the commits read, the most recent last.
    texts: Vec<(String, Arc<String>)>,
    commit: Entity<CommitView>,
    refresh: Option<Task<()>>,
    /// Counts the reads of the list from the top: a page more read for one
    /// isn't added to another's (another search, or the same with other
    /// commits), even if it happens to have as many.
    listing: u64,
    reading: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// The commits' columns.
    columns: Split,
    /// How wide the commits are, as last painted.
    commits_width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<HistoryEvent> for HistoryView {}

impl HistoryView {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search hash, message or author"));
        let commit = cx.new(|cx| CommitView::new(commit_view::Prepared::default(), cx));
        let subscriptions = vec![
            cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.schedule(SEARCH_DEBOUNCE, cx);
                }
            }),
            cx.subscribe(&commit, |this, _, event: &CommitViewEvent, cx| {
                if let Some(commit) = this.shown_commit() {
                    let (commit, short) = (commit.hash.clone(), commit.short.clone());
                    cx.emit(HistoryEvent { commit, short, event: event.clone() });
                }
            }),
        ];
        Self {
            client,
            root,
            commits: Vec::new(),
            graph: Rc::new(Vec::new()),
            more: false,
            loading: false,
            error: None,
            stale: true,
            query,
            scope: GraphScope::Message,
            file: None,
            selected: None,
            shown: None,
            open: false,
            texts: Vec::new(),
            commit,
            refresh: None,
            listing: 0,
            reading: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            columns: Split::new(cx),
            commits_width: Rc::default(),
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

    /// Only the history of `file` (relative; `true`: a folder), or of
    /// everything: the list, from the top.
    pub fn show_file(&mut self, file: Option<(String, bool)>, cx: &mut Context<Self>) {
        if self.file == file {
            return;
        }
        self.file = file;
        self.commits.clear();
        self.selected = None;
        self.shown = None;
        self.open = false;
        self.scroll = UniformListScrollHandle::new();
        self.schedule(Duration::ZERO, cx);
    }

    fn op(&self, skip: usize, limit: usize, cx: &App) -> GitOp {
        let query = self.query.read(cx).value().trim().to_string();
        let all = Config::get(cx).history_all_branches;
        let file = self.file.as_ref().map(|(file, _)| file.clone());
        GitOp::FindGraph { query, scope: self.scope, file, all, skip, limit }
    }

    /// Searches somewhere else, the placeholder saying where.
    fn set_scope(&mut self, scope: GraphScope, window: &mut Window, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        let placeholder = match scope {
            GraphScope::Message => "Search hash, message or author",
            GraphScope::Paths => "Search the paths of the files changed",
            GraphScope::Content => "Search what was added or removed",
        };
        self.query.update(cx, |query, cx| query.set_placeholder(placeholder, window, cx));
        if !self.linked(cx) {
            self.commits.clear();
            self.schedule(Duration::ZERO, cx);
        }
        cx.notify();
    }

    /// Shows every branch, or only the current one.
    fn set_all_branches(&mut self, all: bool, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.history_all_branches = all);
        self.commits.clear();
        self.schedule(Duration::ZERO, cx);
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
                    self.open = false;
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

    /// Selects a commit and reads it, for when it opens; `reveal` scrolls
    /// the list to it.
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
        let only = self.file.clone();
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
                commit_view::prepare(text.to_string(), only, cx)
            }) else {
                return;
            };
            let prepared = prepare.await;
            this.update(cx, |this, cx| {
                this.shown = Some(hash);
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

    /// A page up or down the open commit's changes.
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_commit(-1., cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_commit(1., cx);
    }

    fn scroll_commit(&mut self, pages: f32, cx: &mut Context<Self>) {
        if !self.open {
            return cx.propagate();
        }
        self.commit.update(cx, |view, cx| view.scroll_pages(pages, cx));
    }

    /// Shows the selected commit in the list's place.
    fn open_commit(&mut self, _: &OpenCommit, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_some() && !self.open {
            self.open = true;
            cx.notify();
        }
    }

    /// Back to the list, at the selected commit.
    fn close_commit(&mut self, _: &CloseCommit, _: &mut Window, cx: &mut Context<Self>) {
        if !self.open {
            return cx.propagate();
        }
        self.open = false;
        if let Some(ix) = self.selected.as_ref().and_then(|hash| self.commits.iter().position(|commit| commit.hash == *hash)) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }
}

/// Reads the graph. An agent from before `FindGraph` (until it's restarted)
/// only knows `Graph`, every branch's: the current branch is then picked out
/// here; a search or a file's history are every branch's, as they were.
pub(crate) async fn read_graph(client: &Client, path: PathBuf, op: GitOp) -> anyhow::Result<Response> {
    let response = client.request(Request::Git { path: path.clone(), op: op.clone() }).await;
    match (response, op) {
        (Err(_), GitOp::FindGraph { query, scope: GraphScope::Message, file, all, skip, limit }) => {
            // A file's history may not have HEAD's commit to start from.
            if all || !query.trim().is_empty() || file.is_some() {
                return client.request(Request::Git { path, op: GitOp::Graph { query, file, skip, limit } }).await;
            }
            head_graph(client, path, file, skip, limit).await
        }
        (response, _) => response,
    }
}

/// `HEAD`'s commits, from every branch's read a page at a time: those its
/// parents lead to, from the commit `HEAD` is on, until there are enough.
async fn head_graph(client: &Client, path: PathBuf, file: Option<String>, skip: usize, limit: usize) -> anyhow::Result<Response> {
    const PAGE: usize = 500;
    let mut walk = HeadWalk::default();
    let mut read = 0;
    loop {
        let op = GitOp::Graph { query: String::new(), file: file.clone(), skip: read, limit: PAGE };
        let Response::Graph(page) = client.request(Request::Git { path: path.clone(), op }).await? else {
            anyhow::bail!("unexpected response from the agent");
        };
        read += page.len();
        let last = page.len() < PAGE;
        walk.take(page);
        if last || walk.kept.len() >= skip + limit || walk.done() {
            return Ok(Response::Graph(walk.kept.into_iter().skip(skip).take(limit).collect()));
        }
    }
}

/// Picks `HEAD`'s commits out of every branch's, newest first.
#[derive(Default)]
struct HeadWalk {
    kept: Vec<GraphCommit>,
    /// The parents of those kept, not reached yet.
    wanted: std::collections::HashSet<String>,
    /// `HEAD`'s commit was reached.
    started: bool,
}

impl HeadWalk {
    fn take(&mut self, commits: Vec<GraphCommit>) {
        for commit in commits {
            let head = commit.refs.iter().any(|name| name == "HEAD");
            if head || self.wanted.remove(&commit.hash) {
                self.started = true;
                self.wanted.extend(commit.parents.iter().cloned());
                self.kept.push(commit);
            }
        }
    }

    /// Down to the first commit: nothing further is `HEAD`'s.
    fn done(&self) -> bool {
        self.started && self.wanted.is_empty()
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
pub(crate) fn ref_labels(commit: &GraphCommit, cx: &App) -> Vec<AnyElement> {
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

    /// The commits' rows in `range`: graph, hash, labels and subject, author,
    /// date. `focused`: the list has the keyboard, and its selection is
    /// outlined.
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
                            // in the editor's font: every hash as wide, the subjects in line
                            .child(div().flex_none().pr_2().font_family(theme.mono_font_family.clone()).text_color(theme.muted_foreground).child(commit.short.clone()))
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
                        this.open_commit(&OpenCommit, window, cx);
                    }))
                    .context_menu(self.commit_menu(&commit, cx))
                    .into_any_element()
            })
            .collect()
    }

    /// Where the search looks, as gitk's "containing" / "touching paths" /
    /// "adding/removing string": one of three, the chosen one marked.
    fn render_scopes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let chosen = self.scope;
        h_flex().flex_none().gap_0p5().text_ui_small(cx).children(
            [(GraphScope::Message, "Message"), (GraphScope::Paths, "Paths"), (GraphScope::Content, "Content")].map(|(scope, label)| {
                let selected = scope == chosen;
                div()
                    .id(label)
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .text_color(if selected { theme.foreground } else { theme.muted_foreground })
                    .when(selected, |el| el.bg(crate::app::selected_row(cx)))
                    .when(!selected, |el| el.hover(|style| style.bg(theme.secondary_hover)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| this.set_scope(scope, window, cx)))
            }),
        )
    }

    /// All Branches: every branch, tag and remote, or (unchecked) only the
    /// current branch, as gitk shows it.
    fn render_branches_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let all = Config::get(cx).history_all_branches;
        toggle("history-all-branches", "All Branches", all, cx)
            .tooltip(|window, cx| Tooltip::new("Every branch, tag and remote, not only the current branch").build(window, cx))
            .on_click(cx.listener(move |this, _, _, cx| this.set_all_branches(!all, cx)))
    }

    /// A file's or folder's history: its path, and a cross to see every
    /// commit's again.
    fn render_file(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (file, _) = self.file.as_ref()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .id("history-file")
                .when(cfg!(test), |el| el.debug_selector(|| "history-file".into()))
                .flex_none()
                .min_w_0()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded(theme.radius)
                .bg(crate::app::selected_row(cx))
                .text_ui_small(cx)
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis_start().child(format!("Only {file}")))
                .child(div().text_color(theme.muted_foreground).hover(|style| style.text_color(theme.foreground)).child("✕"))
                .tooltip(|window, cx| Tooltip::new("Every commit's history").build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| this.show_file(None, cx)))
                .into_any_element(),
        )
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let branches = self.render_branches_toggle(cx).into_any_element();
        let scopes = self.render_scopes(cx).into_any_element();
        let file = self.render_file(cx);
        let theme = cx.theme();
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .child(div().w(px(320.)).flex_none().child(Input::new(&self.query).small().cleanable(true)))
            .child(scopes)
            .children(file)
            .child(div().flex_1())
            .child(branches)
            .when(self.loading, |el| el.child(div().text_ui_small(cx).text_color(theme.muted_foreground).child("…")))
            .when_some(self.error.clone(), |el, error| {
                el.child(div().min_w_0().overflow_hidden().text_ellipsis().text_ui_small(cx).text_color(theme.danger).child(error))
            })
    }

    /// The commits: their columns' titles, which resize them, and the rows.
    fn render_commits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sizes = Config::get(cx).history;
        let size = Config::get(cx).font_size(crate::config::TextArea::Interface);
        let (author, date) = fit_columns(f32::from(self.commits_width.get()), sizes.author, sizes.date, size);
        // Squeezed, they start again from their room whenever it changes.
        let squeezed = (author != sizes.author || date != sizes.date).then(|| f32::from(self.commits_width.get()).to_bits());
        let state = self.columns.state(window.viewport_size().width, squeezed, cx).clone();
        let widths = match state.read(cx).sizes().as_slice() {
            [_, author, date] => (*author, *date),
            _ => (px(author), px(date)),
        };
        let theme = cx.theme();
        let title = |text: &'static str| div().px_2().h_full().flex().items_center().overflow_hidden().whitespace_nowrap().child(text);
        let header = h_resizable("history-columns")
            .with_state(&state)
            .child(resizable_panel().child(title("Commit").pl(px(LANE + 6.))))
            .child(resizable_panel().size(px(author)).size_range(px(60.)..px(1000.)).child(title("Author")))
            .child(resizable_panel().size(px(date)).size_range(px(60.)..px(600.)).child(title("Date")))
            .on_resize(|state, _, cx| {
                if let [_, author, date] = state.read(cx).sizes().as_slice() {
                    let (author, date) = (f32::from(*author), f32::from(*date));
                    Config::update_quietly(cx, |config| {
                        config.history.author = author;
                        config.history.date = date;
                    });
                }
            });
        let height = px((size * 1.75).round());
        let count = self.commits.len();
        let empty = (count == 0 && !self.loading && self.error.is_none()).then_some("No commits");
        v_flex()
            .size_full()
            .relative()
            .child(measure(&self.commits_width))
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
}

impl Focusable for HistoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.open {
            v_flex()
                .size_full()
                .child(self.render_commit_bar(cx))
                .child(div().flex_1().min_h_0().child(self.commit.clone()))
                .into_any_element()
        } else {
            let bar = self.render_bar(cx).into_any_element();
            v_flex().size_full().child(bar).child(div().flex_1().min_h_0().child(self.render_commits(window, cx))).into_any_element()
        };
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.background)
            .text_ui(cx)
            .key_context("History")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::open_commit))
            .on_action(cx.listener(Self::close_commit))
            .child(body)
    }
}

impl HistoryView {
    /// Over an open commit: back to the commits, and where it is among them.
    fn render_commit_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let at = self.selected.as_ref().and_then(|hash| self.commits.iter().position(|commit| commit.hash == *hash));
        let place = at.map(|ix| format!("{} of {}{}", ix + 1, self.commits.len(), if self.more { "+" } else { "" }));
        h_flex()
            .flex_none()
            .px_2()
            .py_1()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .text_ui_small(cx)
            .child(
                div()
                    .id("history-back")
                    .when(cfg!(test), |el| el.debug_selector(|| "history-back".into()))
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.secondary_hover))
                    .child("← Commits")
                    .tooltip(|window, cx| Tooltip::new("Back to the commits (Esc)").build(window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.close_commit(&CloseCommit, window, cx))),
            )
            .when_some(place, |el, place| el.child(div().text_color(theme.muted_foreground).child(place)))
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).child("↑ ↓ the previous or next commit"))
    }
}

/// The author's and date's widths in commits `width` wide, at `font`: they
/// give up theirs, the author's first, before the subject's room.
fn fit_columns(width: f32, author: f32, date: f32, font: f32) -> (f32, f32) {
    if width <= 0. {
        return (author, date);
    }
    let mut short = author + date + SUBJECT_ROOM * font - width;
    let mut give = |own: f32, room: f32| {
        let given = short.clamp(0., (own - room * font).max(0.));
        short -= given;
        own - given
    };
    let author = give(author, AUTHOR_ROOM);
    let date = give(date, DATE_ROOM);
    (author, date)
}

/// Keeps in `width` how wide the element it's in is, and redraws when it
/// changes.
fn measure(width: &Rc<Cell<Pixels>>) -> impl IntoElement {
    let width = width.clone();
    canvas(
        move |bounds, window, _| {
            if width.replace(bounds.size.width) != bounds.size.width {
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

/// A checkbox in the bar, as gitk's.
fn toggle(id: &'static str, label: &'static str, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .flex_none()
        .gap_1()
        .px_1()
        .rounded(theme.radius)
        .text_ui_small(cx)
        .text_color(if on { theme.foreground } else { theme.muted_foreground })
        .hover(|style| style.bg(theme.secondary_hover))
        .child(if on { "☑" } else { "☐" })
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn the_author_and_then_the_date_give_their_room_to_the_subject() {
        // Font 10: the subject keeps 280, the author 90, the date 70.
        assert_eq!(fit_columns(1000., 300., 150., 10.), (300., 150.));
        assert_eq!(fit_columns(600., 300., 150., 10.), (170., 150.));
        assert_eq!(fit_columns(400., 300., 150., 10.), (90., 70.));
        assert_eq!(fit_columns(100., 300., 150., 10.), (90., 70.));
        // Already narrower than their room, they keep what they have.
        assert_eq!(fit_columns(300., 60., 60., 10.), (60., 60.));
    }

    /// Enter opens the selected commit in the list's place, the arrows go
    /// on through the commits, and Esc brings the list back.
    #[gpui_kit::test]
    fn a_commit_opens_in_the_list_s_place(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
            cx.bind_keys(keymap());
        });
        let (view, cx) = cx.add_window_view(|window, cx| HistoryView::new(PathBuf::from("/history-test"), None, window, cx));
        view.update_in(cx, |view, window, cx| {
            view.set_commits(vec![commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])], cx);
            view.focus_handle.focus(window, cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("history-back").is_none());
        cx.simulate_keystrokes("down enter");
        assert!(cx.debug_bounds("history-commits").is_none());
        assert!(cx.debug_bounds("history-back").is_some());
        cx.simulate_keystrokes("down");
        view.read_with(cx, |view, _| assert_eq!((view.selected.as_deref(), view.open), (Some("a"), true)));
        cx.simulate_keystrokes("escape");
        assert!(cx.debug_bounds("history-commits").is_some());
        view.read_with(cx, |view, _| assert_eq!((view.selected.as_deref(), view.open), (Some("a"), false)));
    }

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

    /// With an older agent, the current branch is picked out of all of them,
    /// across pages.
    #[test]
    fn head_s_commits_are_picked_out_of_every_branch() {
        let mut head = commit("h", &["b"]);
        head.refs = vec!["HEAD".into(), "refs/heads/master".into()];
        let mut walk = HeadWalk::default();
        // Another branch's tip comes before HEAD, and its own commit between.
        walk.take(vec![commit("t", &["x"]), head, commit("x", &["a"])]);
        assert!(!walk.done());
        walk.take(vec![commit("b", &["a"]), commit("a", &[])]);
        let kept: Vec<&str> = walk.kept.iter().map(|commit| commit.hash.as_str()).collect();
        assert_eq!(kept, ["h", "b", "a"]);
        assert!(walk.done());
    }

    #[test]
    fn a_search_is_only_dots() {
        let rows = graph(&[commit("c", &["b"]), commit("a", &[])], false);
        assert!(rows.iter().all(|row| row.top.is_empty() && row.bottom.is_empty() && row.lane == 0));
    }
}
