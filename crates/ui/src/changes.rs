//! The Changes panel: what isn't committed yet, staged and unstaged (the
//! history is a tab of the code, see `history`). It only reads: committing, staging, discarding and switching branches
//! are done in a terminal. The branch is in the status bar. The agent does
//! all the reading.

use std::{
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use crate::menu::PanelItems as _;
use proto::{ChangedFile, GitOp, GitStatus, Request, Response};

use crate::{config::UiText, menu};

/// Delay after a change on disk before asking git again.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub enum ChangesEvent {
    /// Show the file's diff (`pin` false: preview).
    OpenDiff { file: String, pin: bool },
    OpenFile { file: String },
    /// Select `file` in the Files panel.
    RevealInTree { file: String },
    /// The History tab with the commits that changed `file`.
    ShowHistory { file: String },
}

pub struct ChangesPanel {
    client: Option<Arc<Client>>,
    /// On this machine (not on a server): Finder is available.
    local: bool,
    root: PathBuf,
    /// Branch, distance from the remote and what's uncommitted.
    status: GitStatus,
    /// Selected row: `s:` or `u:` plus the path.
    selected: Option<String>,
    loading: bool,
    error: Option<SharedString>,
    /// Needs rereading when the panel becomes visible.
    stale: bool,
    refresh: Option<Task<()>>,
    /// The rows, drawn only while on screen: a long history costs nothing
    /// while scrolling or while something else in the window moves.
    list: ListState,
    rows: Rc<[Row]>,
    /// What each row was when the list was last told (see `update_rows`).
    row_keys: Vec<SharedString>,
}

/// A row of the list.
#[derive(Clone, Copy)]
enum Row {
    /// "Staged Changes (n)" (`true`) or "Changes (n)".
    Section(bool),
    /// A staged (`true`) or unstaged file.
    Change(bool, usize),
}

impl EventEmitter<ChangesEvent> for ChangesPanel {}

impl ChangesPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, local: bool) -> Self {
        Self {
            client,
            local,
            root,
            status: GitStatus::default(),
            selected: None,
            loading: false,
            error: None,
            stale: true,
            refresh: None,
            list: ListState::new(0, ListAlignment::Top, px(200.)),
            rows: Rc::new([]),
            row_keys: Vec::new(),
        }
    }

    /// Switches to a new connection with the agent.
    pub fn set_client(&mut self, client: Arc<Client>, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.mark_stale(cx);
    }

    /// Something changed on disk: reread, after a short delay since changes
    /// arrive in bursts. Hidden too, for the count on its icon.
    pub fn mark_stale(&mut self, cx: &mut Context<Self>) {
        self.stale = true;
        self.schedule(DEBOUNCE, cx);
    }

    /// The files changed, staged or not: the count on the panel's icon.
    pub fn count(&self) -> usize {
        let staged = self.status.staged.iter().map(|file| &file.path);
        let unstaged = self.status.unstaged.iter().map(|file| &file.path);
        staged.chain(unstaged).collect::<std::collections::HashSet<_>>().len()
    }

    /// Whether `file` (relative to the root) is deleted, as git last saw it:
    /// on a server, the local disk can't say.
    pub fn is_deleted(&self, file: &str) -> bool {
        self.status.staged.iter().chain(&self.status.unstaged).any(|changed| changed.path == file && changed.status == 'D')
    }

    /// Whether `file` (relative to the root) is among those changes.
    pub fn is_changed(&self, file: &str) -> bool {
        self.status.staged.iter().chain(&self.status.unstaged).any(|changed| changed.path == file)
    }

    /// Showing the panel always rereads: while hidden, changes only mark it
    /// stale.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        self.schedule(Duration::ZERO, cx);
    }

    /// Rereads the status.
    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return;
        };
        let path = self.root.clone();
        let op = GitOp::Status;
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
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The checked-out branch, as last read; `None` if detached or not read yet.
    pub fn branch(&self) -> Option<&str> {
        self.status.branch.as_deref()
    }
}

/// A file row: status, name, folder, and lines added and removed.
fn file_row(id: ElementId, file: &ChangedFile, selected: bool, indent: f32, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    // The whole path, its folder muted: in the list's order, the files of a
    // folder go together. Cut at the start, so the name always shows.
    let dir = file.path.rfind('/').map_or(0, |slash| slash + 1);
    let muted = HighlightStyle { color: Some(theme.muted_foreground), ..Default::default() };
    let path = StyledText::new(file.path.clone()).with_highlights([(0..dir, muted)]);
    let tip = SharedString::from(file.path.clone());
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
        .when(selected, |el| el.bg(crate::app::selected_row(cx)))
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
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .when(file.status == 'D', |el| el.line_through())
                .child(path),
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
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
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
        move |menu, window, cx| {
            let (diff, open, copy, history, reveal) = (path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
            let (absolute, copy_absolute) = (absolute.clone(), absolute.to_string_lossy().into_owned());
            menu.item(menu::item("Open Changes", &panel, move |_, _, cx| {
                cx.emit(ChangesEvent::OpenDiff { file: diff.clone(), pin: false })
            }))
            .item(
                menu::item("Open File", &panel, move |_, _, cx| cx.emit(ChangesEvent::OpenFile { file: open.clone() }))
                    .disabled(deleted),
            )
            .item(
                menu::item("Show File History", &panel, move |_, _, cx| cx.emit(ChangesEvent::ShowHistory { file: history.clone() }))
                    .disabled(new),
            )
            .separator()
            .item(menu::item("Copy Path", &panel, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_absolute.clone()))
            }))
            .item(menu::item("Copy Relative Path", &panel, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
            }))
            .item(
                menu::item("Reveal in File Tree", &panel, move |_, _, cx| {
                    cx.emit(ChangesEvent::RevealInTree { file: reveal.clone() })
                })
                .disabled(deleted),
            )
            .item(
                menu::item("Reveal in Finder", &panel, move |_, _, cx| cx.reveal_path(&absolute))
                    .disabled(deleted || !local),
            )
            .separator()
            .panel_items(menu::hide_panel(), window, cx)
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

    /// The rows of the list, in order: only those on screen are drawn.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (staged, files) in [(true, &self.status.staged), (false, &self.status.unstaged)] {
            if !files.is_empty() {
                rows.push(Row::Section(staged));
                rows.extend((0..files.len()).map(|ix| Row::Change(staged, ix)));
            }
        }
        rows
    }

    /// What a row is, to tell a row that changed from one that didn't.
    fn row_key(&self, row: Row) -> SharedString {
        match row {
            Row::Section(staged) => if staged { "staged" } else { "changes" }.into(),
            Row::Change(staged, ix) => {
                let files = if staged { &self.status.staged } else { &self.status.unstaged };
                format!("{}:{}", if staged { "s" } else { "u" }, files[ix].path).into()
            }
        }
    }

    /// Tells the list which rows changed: those around them keep their
    /// place, and the scroll its position.
    fn update_rows(&mut self) {
        let rows = self.rows();
        let keys: Vec<SharedString> = rows.iter().map(|row| self.row_key(*row)).collect();
        if keys != self.row_keys {
            let prefix = keys.iter().zip(&self.row_keys).take_while(|(a, b)| a == b).count();
            let rest = keys.len().min(self.row_keys.len()) - prefix;
            let suffix = keys.iter().rev().zip(self.row_keys.iter().rev()).take(rest).take_while(|(a, b)| a == b).count();
            self.list.splice(prefix..self.row_keys.len() - suffix, keys.len() - prefix - suffix);
            self.row_keys = keys;
        }
        self.rows = rows.into();
    }

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(&row) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        match row {
            Row::Section(staged) => {
                let (title, files) = if staged { ("Staged Changes", &self.status.staged) } else { ("Changes", &self.status.unstaged) };
                self.section_title(format!("{title} ({})", files.len()), cx).into_any_element()
            }
            Row::Change(staged, ix) => {
                let (prefix, files) = if staged { ("s", &self.status.staged) } else { ("u", &self.status.unstaged) };
                let file = &files[ix];
                self.change_row(format!("{prefix}:{}", file.path), file, cx)
            }
        }
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.update_rows();
        let empty = (self.status.staged.is_empty() && self.status.unstaged.is_empty()).then_some("No changes");
        let theme = cx.theme();
        let empty = empty.filter(|_| !self.loading && self.error.is_none());
        let rows = list(self.list.clone(), cx.processor(|this, ix, _, cx| this.render_row(ix, cx))).w_full();
        let rows = match empty {
            Some(_) => rows.with_sizing_behavior(ListSizingBehavior::Infer),
            None => rows.flex_1().min_h_0(),
        };
        let list = v_flex()
            .id("changes-list")
            .size_full()
            .child(rows)
            .when_some(empty, |el, empty| {
                el.child(div().px_3().pt_2().text_ui_small(cx).text_color(theme.muted_foreground).child(empty))
            });
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
            .child(div().flex_1().min_h_0().child(list))
    }
}
