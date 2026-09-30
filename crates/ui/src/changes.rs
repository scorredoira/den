//! Changes panel, in three views: what the task has touched relative to its
//! base branch, what isn't committed yet (staged and unstaged, with the
//! commit box) and the history, with its search. At the top, the branch.
//! The agent does all the reading and work.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{ChangedFile, CommitInfo, GitOp, GitStatus, Request, Response};

use crate::{config::UiText, menu};

/// Delay after a change on disk before asking git again.
const DEBOUNCE: Duration = Duration::from_millis(400);

/// Commits requested each time in the history.
const PAGE: usize = 200;

/// Delay after typing in the history search before asking again.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

pub enum ChangesEvent {
    /// Show the file's diff (`pin` false: preview).
    OpenDiff { file: String, pin: bool },
    OpenFile { file: String },
    /// Show what `file` changed in a commit.
    OpenCommitDiff { commit: String, short: String, file: String, pin: bool },
    /// Show the whole commit: header, message and diff.
    OpenCommit { commit: String, short: String, pin: bool },
    /// Show `file` as it was in a commit.
    OpenFileAt { commit: String, short: String, file: String },
    /// Choose which branch to switch to.
    ChooseBranch { branches: Vec<String> },
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Branch,
    Uncommitted,
    History,
}

pub struct ChangesPanel {
    client: Option<Arc<Client>>,
    /// On this machine (not on a server): Finder is available.
    local: bool,
    root: PathBuf,
    view: View,
    /// Branch view.
    base: Option<String>,
    files: Vec<ChangedFile>,
    /// Branch, distance from the remote and what's uncommitted.
    status: GitStatus,
    /// History view.
    commits: Vec<CommitInfo>,
    /// There may be more commits than the ones read.
    more: bool,
    /// Expanded commit, and the files of those already read.
    expanded: Option<String>,
    commit_files: HashMap<String, Vec<ChangedFile>>,
    /// History search: hash, message or author.
    query: Option<Entity<InputState>>,
    message: Option<Entity<InputState>>,
    /// The commit succeeded: the message is cleared on the next paint.
    clear_message: bool,
    /// Selected row (`s:`, `u:`, `b:` or `c:<hash>:` plus the path).
    selected: Option<String>,
    loading: bool,
    /// Operation in progress (push, pull, commit…), to display it.
    busy: Option<SharedString>,
    error: Option<SharedString>,
    /// Needs rereading when the panel becomes visible.
    stale: bool,
    refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChangesEvent> for ChangesPanel {}

impl ChangesPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, local: bool) -> Self {
        Self {
            client,
            local,
            root,
            view: View::Uncommitted,
            base: None,
            files: Vec::new(),
            status: GitStatus::default(),
            commits: Vec::new(),
            more: false,
            expanded: None,
            commit_files: HashMap::new(),
            query: None,
            message: None,
            clear_message: false,
            selected: None,
            loading: false,
            busy: None,
            error: None,
            stale: true,
            refresh: None,
            _subscriptions: Vec::new(),
        }
    }

    /// Switches to a new connection with the agent.
    pub fn set_client(&mut self, client: Arc<Client>, visible: bool, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.mark_stale(visible, cx);
    }

    /// File diffs are opened according to the current view.
    pub fn uncommitted(&self) -> bool {
        self.view == View::Uncommitted
    }

    /// Something changed on disk: reread (after a short delay, since changes
    /// arrive in bursts) if the panel is visible, or when it's shown.
    pub fn mark_stale(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.stale = true;
        if visible {
            self.schedule(DEBOUNCE, cx);
        }
    }

    /// Showing the panel always rereads: commits, pushes or branch switches
    /// made from a terminal only touch `.git`, which isn't watched.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.schedule(Duration::ZERO, cx);
    }

    /// Rereads the status and the current view's contents.
    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return;
        };
        let path = self.root.clone();
        let view = self.view;
        let log = self.log_op(0, cx);
        self.loading = true;
        self.refresh = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let status = client.request(Request::Git { path: path.clone(), op: GitOp::Status }).await;
            let content = match view {
                View::Branch => Some(client.request(Request::GitChanges { path, uncommitted: false }).await),
                View::History => Some(client.request(Request::Git { path, op: log }).await),
                View::Uncommitted => None,
            };
            this.update(cx, |this, cx| {
                this.loading = false;
                this.stale = false;
                this.error = None;
                match status {
                    Ok(Response::GitStatus(status)) => this.status = status,
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                match content {
                    Some(Ok(Response::Changes { base, files })) => {
                        this.base = base;
                        this.files = files;
                    }
                    Some(Ok(Response::Commits(commits))) => {
                        this.more = commits.len() == PAGE;
                        // A new commit or a branch switch makes the expanded commit stale.
                        if this.commits.first().map(|c| &c.hash) != commits.first().map(|c| &c.hash) {
                            this.expanded = None;
                        }
                        this.commits = commits;
                    }
                    Some(Ok(other)) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Some(Err(err)) => this.error = Some(format!("{err:#}").into()),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The history's commits from `skip` on: all of them, or those matching the search.
    fn log_op(&self, skip: usize, cx: &App) -> GitOp {
        let query = self.query.as_ref().map(|query| query.read(cx).value().trim().to_string()).unwrap_or_default();
        if query.is_empty() {
            GitOp::Log { skip, limit: PAGE }
        } else {
            GitOp::Search { query, skip, limit: PAGE }
        }
    }

    fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        if self.view != view {
            self.view = view;
            self.files.clear();
            self.schedule(Duration::ZERO, cx);
        }
    }

    /// Runs a git operation and, when it finishes, rereads everything.
    fn run(&mut self, op: GitOp, busy: Option<&'static str>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let path = self.root.clone();
        let commit = matches!(op, GitOp::Commit { .. });
        self.busy = busy.map(SharedString::from);
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = client.request(Request::Git { path, op }).await;
            this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(_) => {
                        if commit {
                            this.clear_message = true;
                        }
                        this.schedule(Duration::ZERO, cx);
                    }
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Fetches the branches and lets the workspace show the picker.
    fn choose_branch(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.clone();
        cx.spawn(async move |this, cx| {
            let result = client.request(Request::Git { path, op: GitOp::Branches }).await;
            this.update(cx, |this, cx| match result {
                Ok(Response::Branches { current, branches }) => {
                    let branches = branches.into_iter().filter(|branch| Some(branch) != current.as_ref()).collect();
                    cx.emit(ChangesEvent::ChooseBranch { branches });
                }
                Ok(_) => {}
                Err(err) => {
                    this.error = Some(format!("{err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn switch_branch(&mut self, branch: String, cx: &mut Context<Self>) {
        self.run(GitOp::Switch { branch }, Some("Switching branch…"), cx);
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some(message) = &self.message else {
            return;
        };
        let text = message.read(cx).value().trim().to_string();
        if text.is_empty() {
            self.error = Some("Commit message is missing".into());
            cx.notify();
            return;
        }
        let all = self.status.staged.is_empty();
        if all && self.status.unstaged.is_empty() {
            return;
        }
        self.run(GitOp::Commit { message: text, all }, Some("Committing…"), cx);
    }

    /// Discarding can't be undone (except new files, which go to the Trash), so
    /// it asks first.
    fn discard(&mut self, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let message = match files.as_slice() {
            [file] => format!("Discard changes to {file}?"),
            files => format!("Discard changes to {} files?", files.len()),
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some("Modified files are restored to their previous state; new files are moved to the Trash."),
            &[PromptButton::new("Cancel"), PromptButton::new("Discard")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if matches!(answer.await, Ok(1)) {
                this.update(cx, |this, cx| this.run(GitOp::Discard { files }, None, cx)).ok();
            }
        })
        .detach();
    }

    fn toggle_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        if self.expanded.as_ref() == Some(&hash) {
            self.expanded = None;
            cx.notify();
            return;
        }
        self.expanded = Some(hash.clone());
        cx.notify();
        if self.commit_files.contains_key(&hash) {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.clone();
        cx.spawn(async move |this, cx| {
            let result = client
                .request(Request::Git { path, op: GitOp::CommitFiles { commit: hash.clone() } })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Response::Changes { files, .. }) => {
                        this.commit_files.insert(hash, files);
                    }
                    Ok(_) => {}
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.clone();
        let skip = self.commits.len();
        let op = self.log_op(skip, cx);
        self.more = false;
        cx.spawn(async move |this, cx| {
            let result = client.request(Request::Git { path, op }).await;
            this.update(cx, |this, cx| {
                if let Ok(Response::Commits(commits)) = result
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

    fn ensure_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search hash, message or author"));
        self._subscriptions.push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.expanded = None;
                this.schedule(SEARCH_DEBOUNCE, cx);
            }
        }));
        self.query = Some(input);
    }

    fn ensure_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Commit message"));
            self._subscriptions.push(cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.commit(cx);
                }
            }));
            self.message = Some(input);
        }
        if std::mem::take(&mut self.clear_message)
            && let Some(input) = &self.message
        {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }
}

/// A small text button, like the ones in the header.
fn link(id: impl Into<ElementId>, label: impl Into<SharedString>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_1()
        .text_ui_small(cx)
        .rounded(theme.radius)
        .text_color(theme.muted_foreground)
        .hover(|style| style.text_color(theme.sidebar_foreground).bg(theme.sidebar_accent))
        .child(label.into())
}

/// A file row: status, name, folder, and lines added and removed.
fn file_row(id: ElementId, file: &ChangedFile, selected: bool, indent: f32, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let (name, dir) = match file.path.rsplit_once('/') {
        Some((dir, name)) => (name.to_string(), dir.to_string()),
        None => (file.path.clone(), String::new()),
    };
    let status_color = match file.status {
        'A' | '?' => theme.success,
        'D' | 'U' => theme.danger,
        _ => theme.warning,
    };
    h_flex()
        .id(id)
        .group("file-row")
        .h(px(24.))
        .pl(px(12. + indent))
        .pr_3()
        .gap_2()
        .when(selected, |el| el.bg(theme.sidebar_accent))
        .when(!selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
        .child(
            div()
                .w(px(12.))
                .flex_none()
                .text_ui_small(cx)
                .text_color(status_color)
                .child(if file.status == '?' { 'U' } else { file.status }.to_string()),
        )
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(div().flex_none().when(file.status == 'D', |el| el.line_through()).child(name))
                .child(
                    div()
                        .text_ui_small(cx)
                        .text_color(theme.muted_foreground)
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(dir),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .gap_1()
                .text_ui_small(cx)
                .when(file.added > 0, |el| el.child(div().text_color(theme.success).child(format!("+{}", file.added))))
                .when(file.removed > 0, |el| {
                    el.child(div().text_color(theme.danger).child(format!("−{}", file.removed)))
                }),
        )
}

/// A row button that only shows on hover.
fn row_action(id: impl Into<ElementId>, label: &'static str, cx: &App) -> Stateful<Div> {
    link(id, label, cx)
        .flex_none()
        .opacity(0.)
        .group_hover("file-row", |style| style.opacity(1.))
}

/// "5 min ago", "3 d ago"…
pub fn ago(time: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let secs = (now - time).max(0);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        86400..604800 => format!("{} d ago", secs / 86400),
        604800..2592000 => format!("{} wk ago", secs / 604800),
        2592000..31536000 => format!("{} mo ago", secs / 2592000),
        _ => format!("{} yr ago", secs / 31536000),
    }
}

impl ChangesPanel {
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let tab = |id: &'static str, label: &'static str, view: View| {
            let selected = self.view == view;
            div()
                .id(id)
                .px_2()
                .py_0p5()
                .text_ui_small(cx)
                .rounded(theme.radius)
                .when(selected, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_foreground))
                .when(!selected, |el| el.text_color(theme.muted_foreground))
                .hover(|style| style.text_color(theme.sidebar_foreground))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
        };
        let status = &self.status;
        let branch = status.branch.clone().unwrap_or_else(|| "Detached HEAD".into());
        v_flex()
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(tab("changes-branch", "Branch", View::Branch))
                    .child(tab("changes-uncommitted", "Uncommitted", View::Uncommitted))
                    .child(tab("changes-history", "History", View::History))
                    .child(div().flex_1())
                    .child(
                        link("changes-refresh", if self.loading { "…" } else { "↻" }, cx)
                            .on_click(cx.listener(|this, _, _, cx| this.schedule(Duration::ZERO, cx))),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .pb_1()
                    .gap_1()
                    .text_ui_small(cx)
                    .child(
                        div()
                            .id("changes-switch")
                            .flex_1()
                            .min_w_0()
                            .px_1()
                            .rounded(theme.radius)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(theme.sidebar_foreground)
                            .hover(|style| style.bg(theme.sidebar_accent))
                            .child(format!("⎇ {branch}"))
                            .on_click(cx.listener(|this, _, _, cx| this.choose_branch(cx))),
                    )
                    .when_some(self.busy.clone(), |el, busy| {
                        el.child(div().flex_none().text_color(theme.muted_foreground).child(busy))
                    }),
            )
    }

    fn file_menu(&self, file: &ChangedFile, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let panel = cx.entity().downgrade();
        let path = file.path.clone();
        let absolute = self.root.join(&path);
        let deleted = file.status == 'D';
        let local = self.local;
        move |menu, _, _| {
            let (diff, open, copy) = (path.clone(), path.clone(), path.clone());
            let absolute = absolute.clone();
            menu.item(menu::item("Open Changes", &panel, move |_, _, cx| {
                cx.emit(ChangesEvent::OpenDiff { file: diff.clone(), pin: false })
            }))
            .item(
                menu::item("Open File", &panel, move |_, _, cx| cx.emit(ChangesEvent::OpenFile { file: open.clone() }))
                    .disabled(deleted),
            )
            .separator()
            .item(menu::item("Copy Relative Path", &panel, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
            }))
            .item(
                menu::item("Reveal in Finder", &panel, move |_, _, cx| cx.reveal_path(&absolute))
                    .disabled(deleted || !local),
            )
        }
    }

    /// A file row in the Branch and Uncommitted views: click to see its
    /// changes, double-click to open it.
    fn change_row(&self, key: String, file: &ChangedFile, actions: Vec<Stateful<Div>>, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.as_ref() == Some(&key);
        let path = file.path.clone();
        file_row(SharedString::from(format!("change-{key}")).into(), file, selected, 0., cx)
            .children(actions)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.selected = Some(key.clone());
                cx.emit(ChangesEvent::OpenDiff { file: path.clone(), pin: event.click_count() >= 2 });
                cx.notify();
            }))
            .context_menu(self.file_menu(file, cx))
            .into_any_element()
    }

    fn section_title(&self, id: &'static str, title: String, action: Option<(&'static str, GitOp)>, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .px_3()
            .pt_2()
            .pb_0p5()
            .text_ui_small(cx)
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child(title))
            .when_some(action, |el, (label, op)| {
                el.child(link(id, label, cx).on_click(cx.listener(move |this, _, _, cx| this.run(op.clone(), None, cx))))
            })
    }

    fn commit_file_menu(
        panel: WeakEntity<Self>,
        commit: &CommitInfo,
        file: &ChangedFile,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let (hash, short, path) = (commit.hash.clone(), commit.short.clone(), file.path.clone());
        move |menu, _, _| {
            let (diff_hash, diff_short, diff_path) = (hash.clone(), short.clone(), path.clone());
            let (at_hash, at_short, at_path) = (hash.clone(), short.clone(), path.clone());
            let (open, copy) = (path.clone(), path.clone());
            menu.item(menu::item("Open Changes", &panel, move |_, _, cx| {
                cx.emit(ChangesEvent::OpenCommitDiff {
                    commit: diff_hash.clone(),
                    short: diff_short.clone(),
                    file: diff_path.clone(),
                    pin: true,
                })
            }))
            .item(menu::item("Open File at This Commit", &panel, move |_, _, cx| {
                cx.emit(ChangesEvent::OpenFileAt { commit: at_hash.clone(), short: at_short.clone(), file: at_path.clone() })
            }))
            .item(menu::item("Open File", &panel, move |_, _, cx| cx.emit(ChangesEvent::OpenFile { file: open.clone() })))
            .separator()
            .item(menu::item("Copy Relative Path", &panel, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
            }))
        }
    }

    fn render_branch(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let total: (u32, u32) = self.files.iter().fold((0, 0), |(a, r), file| (a + file.added, r + file.removed));
        let base = match &self.base {
            Some(base) => format!("Since branching off {base}"),
            None => "No base branch".to_string(),
        };
        let mut rows = vec![
            h_flex()
                .px_3()
                .pb_1()
                .gap_2()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
                .child(base)
                .child(div().flex_1())
                .when(!self.files.is_empty(), |el| {
                    el.child(format!("{} files", self.files.len()))
                        .child(div().text_color(theme.success).child(format!("+{}", total.0)))
                        .child(div().text_color(theme.danger).child(format!("−{}", total.1)))
                })
                .into_any_element(),
        ];
        rows.extend(
            self.files
                .iter()
                .map(|file| self.change_row(format!("b:{}", file.path), file, Vec::new(), cx)),
        );
        rows
    }

    fn render_uncommitted(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let status = &self.status;
        let nothing = status.staged.is_empty() && status.unstaged.is_empty();
        let commit_label = if status.staged.is_empty() { "Commit All" } else { "Commit" };
        let mut rows = Vec::new();
        if let Some(message) = &self.message {
            rows.push(
                v_flex()
                    .px_3()
                    .pb_1()
                    .gap_1()
                    .child(Input::new(message).small())
                    .child(
                        div()
                            .id("changes-commit")
                            .py_0p5()
                            .text_ui_small(cx)
                            .text_center()
                            .rounded(theme.radius)
                            .when(nothing || self.busy.is_some(), |el| {
                                el.bg(theme.muted).text_color(theme.muted_foreground)
                            })
                            .when(!nothing && self.busy.is_none(), |el| {
                                el.bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .hover(|style| style.bg(theme.primary_hover))
                                    .on_click(cx.listener(|this, _, _, cx| this.commit(cx)))
                            })
                            .child(commit_label),
                    )
                    .into_any_element(),
            );
        }
        if !status.staged.is_empty() {
            let files: Vec<String> = status.staged.iter().map(|file| file.path.clone()).collect();
            rows.push(
                self.section_title(
                    "unstage-all",
                    format!("Staged Changes ({})", status.staged.len()),
                    Some(("− all", GitOp::Unstage { files })),
                    cx,
                )
                .into_any_element(),
            );
            for (ix, file) in status.staged.iter().enumerate() {
                let path = file.path.clone();
                let unstage = row_action(("unstage", ix), "−", cx).on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.run(GitOp::Unstage { files: vec![path.clone()] }, None, cx);
                }));
                rows.push(self.change_row(format!("s:{}", file.path), file, vec![unstage], cx));
            }
        }
        if !status.unstaged.is_empty() {
            let files: Vec<String> = status.unstaged.iter().map(|file| file.path.clone()).collect();
            rows.push(
                self.section_title(
                    "stage-all",
                    format!("Changes ({})", status.unstaged.len()),
                    Some(("+ all", GitOp::Stage { files })),
                    cx,
                )
                .into_any_element(),
            );
            for (ix, file) in status.unstaged.iter().enumerate() {
                let (stage, discard) = (file.path.clone(), file.path.clone());
                let actions = vec![
                    row_action(("discard", ix), "↺", cx).on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.discard(vec![discard.clone()], window, cx);
                    })),
                    row_action(("stage", ix), "+", cx).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.run(GitOp::Stage { files: vec![stage.clone()] }, None, cx);
                    })),
                ];
                rows.push(self.change_row(format!("u:{}", file.path), file, actions, cx));
            }
        }
        rows
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let mut rows = Vec::new();
        if let Some(query) = &self.query {
            rows.push(div().px_3().pb_1().child(Input::new(query).small().cleanable(true)).into_any_element());
        }
        for (ix, commit) in self.commits.iter().enumerate() {
            let expanded = self.expanded.as_ref() == Some(&commit.hash);
            let refs = commit.refs.replace("HEAD -> ", "");
            let (hash, short) = (commit.hash.clone(), commit.short.clone());
            let panel = cx.entity().downgrade();
            let (copy_hash, copy_subject, commit_short) = (commit.hash.clone(), commit.subject.clone(), commit.short.clone());
            rows.push(
                v_flex()
                    .id(("commit", ix))
                    .px_3()
                    .py_1()
                    .gap_0p5()
                    .when(expanded, |el| el.bg(theme.sidebar_accent.opacity(0.5)))
                    .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
                    .child(
                        h_flex()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .w(px(10.))
                                    .flex_none()
                                    .text_ui_small(cx)
                                    .text_color(theme.muted_foreground)
                                    .child(if expanded { "▾" } else { "▸" }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(commit.subject.clone()),
                            ),
                    )
                    .child(
                        h_flex()
                            .pl(px(14.))
                            .gap_2()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(commit.short.clone())
                            .child(commit.author.clone())
                            .child(ago(commit.time))
                            .when(!refs.is_empty(), |el| {
                                el.child(div().text_color(theme.primary).overflow_hidden().text_ellipsis().child(refs))
                            }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        let pin = event.click_count() >= 2;
                        if !pin {
                            this.toggle_commit(hash.clone(), cx);
                        }
                        if pin || this.expanded.as_ref() == Some(&hash) {
                            cx.emit(ChangesEvent::OpenCommit { commit: hash.clone(), short: short.clone(), pin });
                        }
                    }))
                    .context_menu(move |menu, _, _| {
                        let (copy_hash, copy_subject) = (copy_hash.clone(), copy_subject.clone());
                        let (show, short) = (copy_hash.clone(), commit_short.clone());
                        menu.item(menu::item("Show Commit", &panel, move |_, _, cx| {
                            cx.emit(ChangesEvent::OpenCommit { commit: show.clone(), short: short.clone(), pin: true })
                        }))
                        .separator()
                        .item(menu::item("Copy Hash", &panel, move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_hash.clone()))
                        }))
                        .item(menu::item("Copy Message", &panel, move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_subject.clone()))
                        }))
                    })
                    .into_any_element(),
            );
            if !expanded {
                continue;
            }
            let Some(files) = self.commit_files.get(&commit.hash) else {
                rows.push(
                    div()
                        .pl(px(26.))
                        .py_0p5()
                        .text_ui_small(cx)
                        .text_color(theme.muted_foreground)
                        .child("…")
                        .into_any_element(),
                );
                continue;
            };
            for (file_ix, file) in files.iter().enumerate() {
                let key = format!("c:{}:{}", commit.hash, file.path);
                let selected = self.selected.as_ref() == Some(&key);
                let (hash, short, path) = (commit.hash.clone(), commit.short.clone(), file.path.clone());
                rows.push(
                    file_row(SharedString::from(format!("commit-file-{ix}-{file_ix}")).into(), file, selected, 14., cx)
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            this.selected = Some(key.clone());
                            cx.emit(ChangesEvent::OpenCommitDiff {
                                commit: hash.clone(),
                                short: short.clone(),
                                file: path.clone(),
                                pin: event.click_count() >= 2,
                            });
                            cx.notify();
                        }))
                        .context_menu(Self::commit_file_menu(cx.entity().downgrade(), commit, file))
                        .into_any_element(),
                );
            }
        }
        if self.more {
            rows.push(
                h_flex()
                    .px_3()
                    .py_1()
                    .child(link("history-more", "Load More", cx).on_click(cx.listener(|this, _, _, cx| this.load_more(cx))))
                    .into_any_element(),
            );
        }
        rows
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_message(window, cx);
        self.ensure_query(window, cx);
        let (rows, empty) = match self.view {
            View::Branch => (self.render_branch(cx), self.files.is_empty().then_some("No changes")),
            View::Uncommitted => (
                self.render_uncommitted(cx),
                (self.status.staged.is_empty() && self.status.unstaged.is_empty()).then_some("Nothing to commit"),
            ),
            View::History => (self.render_history(cx), self.commits.is_empty().then_some("No commits")),
        };
        let header = self.render_header(cx).into_any_element();
        let theme = cx.theme();
        v_flex()
            .size_full()
            .text_ui(cx)
            .child(header)
            .children(self.error.clone().map(|error| {
                div()
                    .px_3()
                    .pb_1()
                    .text_ui_small(cx)
                    .text_color(theme.danger)
                    .whitespace_normal()
                    .child(error)
            }))
            .child(
                v_flex()
                    .id("changes-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .when_some(empty.filter(|_| !self.loading && self.error.is_none()), |el, empty| {
                        el.child(div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(empty))
                    }),
            )
    }
}
