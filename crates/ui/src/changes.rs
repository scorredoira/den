//! The Changes and History panels: what isn't committed yet (staged and
//! unstaged), and the history, with its search, or only a file's or folder's.
//! It only reads: committing, staging, discarding and switching branches
//! are done in a terminal. The branch is in the status bar. The agent does
//! all the reading.

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
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{ChangedFile, CommitInfo, GitOp, GitStatus, Request, Response};

use crate::{
    config::{Config, UiText},
    menu,
};

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
}

/// What a panel lists: the Changes panel what isn't committed, the History
/// panel the commits.
#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Uncommitted,
    History,
}

pub struct ChangesPanel {
    client: Option<Arc<Client>>,
    /// On this machine (not on a server): Finder is available.
    local: bool,
    root: PathBuf,
    view: View,
    /// Branch, distance from the remote and what's uncommitted.
    status: GitStatus,
    /// History view.
    commits: Vec<CommitInfo>,
    /// Only the history of this file or folder (`true`: a folder).
    file: Option<(String, bool)>,
    /// There may be more commits than the ones read.
    more: bool,
    /// Selected commit, and the files of those already read.
    commit: Option<String>,
    /// The selected commit's files shown below the commits.
    files_open: bool,
    commit_files: HashMap<String, Vec<ChangedFile>>,
    /// History search: hash, message or author.
    query: Option<Entity<InputState>>,
    /// Selected row (`s:`, `u:` or `c:<hash>:` plus the path, or `h:<hash>`
    /// in a file's history).
    selected: Option<String>,
    loading: bool,
    error: Option<SharedString>,
    /// Needs rereading when the panel becomes visible.
    stale: bool,
    refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ChangesEvent> for ChangesPanel {}

impl ChangesPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, local: bool, view: View) -> Self {
        Self {
            client,
            local,
            root,
            view,
            status: GitStatus::default(),
            commits: Vec::new(),
            file: None,
            more: false,
            commit: None,
            files_open: true,
            commit_files: HashMap::new(),
            query: None,
            selected: None,
            loading: false,
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

    /// Something changed on disk: reread (after a short delay, since changes
    /// arrive in bursts) if the panel is visible, or when it's shown. Hidden,
    /// the Changes panel rereads the status, for the count on its icon.
    pub fn mark_stale(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.stale = true;
        if visible || self.view == View::Uncommitted {
            self.schedule(DEBOUNCE, cx);
        }
    }

    /// The files changed, staged or not: the count on the panel's icon.
    pub fn count(&self) -> usize {
        let staged = self.status.staged.iter().map(|file| &file.path);
        let unstaged = self.status.unstaged.iter().map(|file| &file.path);
        staged.chain(unstaged).collect::<std::collections::HashSet<_>>().len()
    }

    /// Showing the panel always rereads: while hidden, changes only mark it
    /// stale.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.schedule(Duration::ZERO, cx);
    }

    /// Rereads what the panel lists: the status, or the commits.
    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return;
        };
        let path = self.root.clone();
        let op = match self.view {
            View::Uncommitted => GitOp::Status,
            View::History => self.log_op(0, cx),
        };
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
                    Ok(Response::GitStatus(status)) => this.status = status,
                    Ok(Response::Commits(commits)) => {
                        this.more = commits.len() == PAGE;
                        // A new commit or a branch switch makes the selected commit stale.
                        if this.commits.first().map(|c| &c.hash) != commits.first().map(|c| &c.hash) {
                            this.commit = None;
                        }
                        this.commits = commits;
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

    /// The history's commits from `skip` on: all of them, those matching the
    /// search, or the file's.
    fn log_op(&self, skip: usize, cx: &App) -> GitOp {
        if let Some((file, _)) = &self.file {
            return GitOp::FileLog { file: file.clone(), skip, limit: PAGE };
        }
        let query = self.query.as_ref().map(|query| query.read(cx).value().trim().to_string()).unwrap_or_default();
        if query.is_empty() {
            GitOp::Log { skip, limit: PAGE }
        } else {
            GitOp::Search { query, skip, limit: PAGE }
        }
    }

    /// Shows the history of only `file` (relative), or of everything.
    pub fn show_history(&mut self, file: Option<(String, bool)>, cx: &mut Context<Self>) {
        if self.file != file {
            self.file = file;
            self.commits.clear();
            self.commit = None;
            self.more = false;
        }
        self.schedule(Duration::ZERO, cx);
    }

    /// The checked-out branch, as last read; `None` if detached or not read yet.
    pub fn branch(&self) -> Option<&str> {
        self.status.branch.as_deref()
    }

    fn select_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        self.commit = Some(hash.clone());
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
                this.commit = None;
                this.schedule(SEARCH_DEBOUNCE, cx);
            }
        }));
        self.query = Some(input);
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
    fn file_menu(&self, file: &ChangedFile, cx: &mut Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let panel = cx.entity().downgrade();
        let path = file.path.clone();
        let absolute = self.root.join(&path);
        let deleted = file.status == 'D';
        // Untracked or just added: no history yet.
        let new = matches!(file.status, '?' | 'A');
        let local = self.local;
        move |menu, _, _| {
            let (diff, open, copy, history) = (path.clone(), path.clone(), path.clone(), path.clone());
            let absolute = absolute.clone();
            menu.item(menu::item("Open Changes", &panel, move |_, _, cx| {
                cx.emit(ChangesEvent::OpenDiff { file: diff.clone(), pin: false })
            }))
            .item(
                menu::item("Open File", &panel, move |_, _, cx| cx.emit(ChangesEvent::OpenFile { file: open.clone() }))
                    .disabled(deleted),
            )
            .item(
                menu::item("Show File History", &panel, move |this, _, cx| this.show_history(Some((history.clone(), false)), cx))
                    .disabled(new),
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
    fn change_row(&self, key: String, file: &ChangedFile, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.selected.as_ref() == Some(&key);
        let path = file.path.clone();
        file_row(SharedString::from(format!("change-{key}")).into(), file, selected, 0., cx)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.selected = Some(key.clone());
                cx.emit(ChangesEvent::OpenDiff { file: path.clone(), pin: event.click_count() >= 2 });
                cx.notify();
            }))
            .context_menu(self.file_menu(file, cx))
            .into_any_element()
    }

    fn section_title(&self, title: String, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .pt_2()
            .pb_0p5()
            .text_ui_small(cx)
            .text_color(cx.theme().muted_foreground)
            .child(title)
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

    /// What isn't committed, as git sees it: committing, staging and
    /// discarding are done in a terminal.
    fn render_uncommitted(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let status = &self.status;
        let mut rows = Vec::new();
        for (title, prefix, files) in [("Staged Changes", "s", &status.staged), ("Changes", "u", &status.unstaged)] {
            if files.is_empty() {
                continue;
            }
            rows.push(self.section_title(format!("{title} ({})", files.len()), cx).into_any_element());
            for file in files {
                rows.push(self.change_row(format!("{prefix}:{}", file.path), file, cx));
            }
        }
        rows
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let mut rows = Vec::new();
        if let Some((file, _)) = &self.file {
            rows.push(
                h_flex()
                    .px_3()
                    .pb_1()
                    .gap_1()
                    .text_ui_small(cx)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_color(theme.muted_foreground)
                            .child(format!("History of {file}")),
                    )
                    .child(
                        link("history-all", "✕", cx).on_click(cx.listener(|this, _, _, cx| this.show_history(None, cx))),
                    )
                    .into_any_element(),
            );
        } else if let Some(query) = &self.query {
            rows.push(div().px_3().pb_1().child(Input::new(query).small().cleanable(true)).into_any_element());
        }
        // In a file's history a commit is that file's changes, not a list of files.
        let file = self.file.as_ref().filter(|(_, dir)| !dir).map(|(file, _)| file.clone());
        for (ix, commit) in self.commits.iter().enumerate() {
            let key = format!("h:{}", commit.hash);
            let selected = match file {
                Some(_) => self.selected.as_ref() == Some(&key),
                None => self.commit.as_ref() == Some(&commit.hash),
            };
            let file = file.clone();
            let menu_file = file.clone();
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
                    .when(selected, |el| el.bg(theme.sidebar_accent))
                    .when(!selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(commit.subject.clone()),
                    )
                    .child(
                        h_flex()
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
                        if let Some(file) = &file {
                            this.selected = Some(key.clone());
                            cx.emit(ChangesEvent::OpenCommitDiff { commit: hash.clone(), short: short.clone(), file: file.clone(), pin });
                            cx.notify();
                            return;
                        }
                        this.select_commit(hash.clone(), cx);
                        cx.emit(ChangesEvent::OpenCommit { commit: hash.clone(), short: short.clone(), pin });
                    }))
                    .context_menu(move |menu, _, _| {
                        let (copy_hash, copy_subject) = (copy_hash.clone(), copy_subject.clone());
                        let (show, short) = (copy_hash.clone(), commit_short.clone());
                        menu.when_some(menu_file.clone(), |menu, file| {
                            let (diff_hash, diff_short, diff_file) = (show.clone(), short.clone(), file.clone());
                            let (at_hash, at_short) = (show.clone(), short.clone());
                            menu.item(menu::item("Open Changes", &panel, move |_, _, cx| {
                                cx.emit(ChangesEvent::OpenCommitDiff {
                                    commit: diff_hash.clone(),
                                    short: diff_short.clone(),
                                    file: diff_file.clone(),
                                    pin: true,
                                })
                            }))
                            .item(menu::item("Open File at This Commit", &panel, move |_, _, cx| {
                                cx.emit(ChangesEvent::OpenFileAt { commit: at_hash.clone(), short: at_short.clone(), file: file.clone() })
                            }))
                            .separator()
                        })
                        .item(menu::item("Show Commit", &panel, move |_, _, cx| {
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

    /// The selected commit's files, under the commits while they share the
    /// history's place; none in a file's history, where a commit is that
    /// file's changes.
    fn has_commit_files(&self, cx: &App) -> bool {
        self.view == View::History && !self.file.as_ref().is_some_and(|(_, dir)| !dir) && Config::get(cx).layout.commit_in_history()
    }

    /// The bar over the selected commit's files: click to hide or show them.
    fn render_files_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let commit = self.commit.as_ref().and_then(|hash| self.commits.iter().find(|commit| commit.hash == *hash));
        let count = self.commit.as_ref().and_then(|hash| self.commit_files.get(hash)).map(Vec::len);
        h_flex()
            .id("commit-files-bar")
            .flex_none()
            .h(px(24.))
            .px_3()
            .gap_1()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .text_ui_small(cx)
            .text_color(theme.muted_foreground)
            .hover(|style| style.text_color(theme.sidebar_foreground))
            .child(div().w(px(10.)).flex_none().child(if self.files_open { "▾" } else { "▸" }))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("FILES"))
            .when_some(commit, |el, commit| el.child(commit.short.clone()))
            .when_some(count, |el, count| el.child(format!("({count})")))
            .on_click(cx.listener(|this, _, _, cx| {
                this.files_open = !this.files_open;
                cx.notify();
            }))
    }

    fn render_commit_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let note = |text: &'static str| div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(text);
        let Some(commit) = self.commit.as_ref().and_then(|hash| self.commits.iter().find(|commit| commit.hash == *hash)) else {
            return note("Select a commit").into_any_element();
        };
        let Some(files) = self.commit_files.get(&commit.hash) else {
            return note("…").into_any_element();
        };
        v_flex()
            .id("commit-files")
            .size_full()
            .overflow_y_scroll()
            .children(files.iter().enumerate().map(|(ix, file)| {
                let key = format!("c:{}:{}", commit.hash, file.path);
                let selected = self.selected.as_ref() == Some(&key);
                let (hash, short, path) = (commit.hash.clone(), commit.short.clone(), file.path.clone());
                file_row(("commit-file", ix).into(), file, selected, 0., cx)
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
            }))
            .into_any_element()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.view == View::History {
            self.ensure_query(window, cx);
        }
        let (rows, empty) = match self.view {
            View::Uncommitted => (
                self.render_uncommitted(cx),
                (self.status.staged.is_empty() && self.status.unstaged.is_empty()).then_some("No changes"),
            ),
            View::History => (self.render_history(cx), self.commits.is_empty().then_some("No commits")),
        };
        let files = self.has_commit_files(cx).then(|| (self.render_files_bar(cx).into_any_element(), self.render_commit_files(cx)));
        let theme = cx.theme();
        let list = v_flex()
            .id("changes-list")
            .size_full()
            .overflow_y_scroll()
            .children(rows)
            .when_some(empty.filter(|_| !self.loading && self.error.is_none()), |el, empty| {
                el.child(div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(empty))
            });
        let body = match files {
            None => list.into_any_element(),
            Some((bar, files)) => {
                // Hidden, the files keep their place in the split, for its size; their bar goes below.
                let (inside, below) = if self.files_open { (Some(bar), None) } else { (None, Some(bar)) };
                v_flex()
                    .size_full()
                    .child(
                        div().flex_1().min_h_0().child(
                            v_resizable("history-split").child(resizable_panel().child(list)).child(
                                resizable_panel()
                                    .visible(self.files_open)
                                    .child(v_flex().size_full().children(inside).child(div().flex_1().min_h_0().child(files))),
                            ),
                        ),
                    )
                    .children(below)
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .pt_1()
            .text_ui(cx)
            .children(self.error.clone().map(|error| {
                div()
                    .px_3()
                    .pb_1()
                    .text_ui_small(cx)
                    .text_color(theme.danger)
                    .whitespace_normal()
                    .child(error)
            }))
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// The Commit Files panel: the files of the commit selected in the history,
/// when placed apart from it.
pub struct CommitFilesPanel {
    history: Entity<ChangesPanel>,
    _subscription: Subscription,
}

impl CommitFilesPanel {
    pub fn new(history: Entity<ChangesPanel>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&history, |_, _, cx| cx.notify());
        Self { history, _subscription: subscription }
    }
}

impl Render for CommitFilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.history.update(cx, |history, cx| history.render_commit_files(cx));
        v_flex().size_full().pt_1().text_ui(cx).child(files)
    }
}
