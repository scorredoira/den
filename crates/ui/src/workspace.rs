use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use client::Client;
use gpui_base::input::{ExecutionLine, GutterMark, LineStyle};
use proto::{CommitInfo, GitOp, LspLocation, LspOp, PortInfo, Request, Response, SearchHit};

use gpui_kit::component::{
    ActiveTheme as _, h_flex, h_resizable, v_resizable,
    input::{self, Editor, EditorState, InputEvent, Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _},
    menu::{ContextMenuExt as _, PopupMenu},
    resizable_panel,
    text::{TextView, TextViewState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    CloseAllTabs, CloseTab, CollapseFileTree, MaximizeTerminals, MoveTerminals, NewTerminal, NextTab, PrevTab, Save, ShowChanges, ShowFiles, ShowHistory, ToggleCommitFiles,
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, ShowOutline, ShowReferences, ShowSearch,
    SplitDown, SplitRight, ToggleMarkdownSource, ToggleSidePanel,
    ToggleTerminals, OpenFileFinder, NewFile, NextResult, PrevResult, GoToDefinition, FindReferences, NavigateBack, NavigateForward,
    GoToLine, GoToSymbol, GoToWorkspaceSymbol, OpenPreviewToSide, SplitEditorDown, SplitEditorRight, ToggleWordWrap, FormatDocument,
    changes::{self, ChangesEvent, ChangesPanel},
    commit_view::{CommitView, CommitViewEvent},
    completion::Completions,
    editing::{self, DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, SelectNextOccurrence},
    config::{self, Config, Panel, SavedTab, Session, TextArea, UiText},
    debug::{self, DebugEvent, DebugView, Debugger, EditKind},
    device::{Device, DeviceEvent},
    notes::NotesPanel,
    DebugContinue, DebugPause, DebugRestart, DebugStop, RunToCursor, SetNextStatement, StepInto, StepOut, StepOver,
    AddConditionalBreakpoint, AddLogpoint, AddToWatch, EvaluateInConsole, ToggleBreakpoint, ToggleDebugPanel, ToggleNotes,
    diff,
    picker::{Picker, PickerEvent},
    search::{SearchEvent, SearchPanel},
    signature::{self, SignatureHint},
    outline::{OutlineEvent, OutlinePanel},
    symbol_picker::{self, SymbolPicker, SymbolPickerEvent},
    file_tree::{FileTree, FileTreeEvent},
    language, menu,
    splits::{Axis, Direction},
    terminals::{PanelTab, TerminalArea, TerminalAreaEvent},
};

mod tab_drag;
mod layout;
mod activity;
mod markdown_images;
mod commands;
#[cfg(test)]
pub(crate) mod autosave_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod new_file_tests;
use tab_drag::{EditorDrop, TabDrag, TabDragPreview};
use layout::Panels;
pub(crate) use layout::{WorkspacesPanel, title as panel_title};
pub(crate) use activity::{ACTIVITY_WIDTH, Badge, Item, OnActivity, TaskBadges, activity_bar};

enum Content {
    Loading,
    Ready,
    Failed(SharedString),
}

/// An empty result is normal (no definition, no suggestions, outside a call).
/// Only a missing server or a failed request makes the LSP unavailable.
#[derive(Default)]
struct LspStatus {
    problem: Option<SharedString>,
}

impl LspStatus {
    fn observe(&mut self, response: &anyhow::Result<Response>) {
        match response {
            Err(error) => self.problem = Some(format!("{error:#}").into()),
            Ok(
                Response::Lsp { server, .. }
                | Response::Completions { server, .. }
                | Response::Symbols { server, .. },
            ) => {
                self.problem = server.is_none().then(|| "No language server is available for this file.".into());
            }
            Ok(Response::Signature(Some(_))) => self.problem = None,
            Ok(Response::Resolved { detail, documentation }) if detail.is_some() || documentation.is_some() => {
                self.problem = None;
            }
            // Signature(None) and an unresolved item don't say whether a
            // server exists, so they mustn't hide a previous failure.
            _ => {}
        }
    }
}

impl FileTab {
    fn rendered(&self) -> Option<&Entity<TextViewState>> {
        self.markdown.as_ref().filter(|_| !self.show_source)
    }

    /// The tab that holds the file itself (its saved text, whether it has
    /// changes, its blame): not a diff or another view of it.
    fn is_file(&self) -> bool {
        self.diff.is_none() && !self.view && !self.doc
    }

    /// The file or one of its views, in either group.
    fn shows_file(&self, path: &Path) -> bool {
        self.diff.is_none() && self.path == path
    }
}

struct FileTab {
    path: PathBuf,
    /// An image is shown instead of the text.
    image: Option<Arc<Image>>,
    editor: Entity<EditorState>,
    /// Rendered view of a Markdown file.
    markdown: Option<Entity<TextViewState>>,
    /// For Markdown, the source is shown instead of the rendered view.
    show_source: bool,
    content: Content,
    lsp_status: LspStatus,
    /// Text as it is on disk, to tell whether there are unsaved changes.
    saved: String,
    /// Serialize writes so rapid focus changes cannot save an older version last.
    save_lock: Arc<smol::lock::Mutex<()>>,
    dirty: bool,
    preview: bool,
    /// Cmd-W was already pressed once with unsaved changes.
    confirm_close: bool,
    /// Where to put the cursor once loading finishes.
    goto: Option<Position>,
    /// And, with it, the other end of a range to select (`den show`).
    select_to: Option<Position>,
    /// Once loaded, focus goes to this tab (not if it was opened as a preview
    /// from the tree, which keeps the keyboard).
    grab_focus: bool,
    /// Tab showing a file's diff (read-only), not the file itself.
    diff: Option<DiffOf>,
    /// A file's diff shown side by side: `editor` has the new side.
    old: Option<OldSide>,
    /// A whole commit: its message and every file's changes.
    commit: Option<Entity<CommitView>>,
    /// Reopened on returning to the task: if the file is gone, it closes itself.
    restored: bool,
    /// Who last changed each line, as it was on disk when read or saved.
    blame: Option<Arc<Blame>>,
    /// Highlight of the occurrences of the word under the cursor, and the
    /// selections it was computed for.
    occurrences: Option<RangeDecorationCollection>,
    occurrences_for: Vec<editing::Selection>,
    /// The editor group it's in: 0, or 1 for the second one of a split.
    group: usize,
    /// When it was last shown: a group shows its most recent tab.
    shown: u64,
    /// Another view of a file open in another tab (the other group): its own
    /// editor, kept in sync with the file's, and the same rendered Markdown.
    view: bool,
    /// A page of the app's own (the shortcuts guide), with no file behind
    /// it: read-only, never saved, not reopened with the session.
    doc: bool,
    /// The text as of its last change, once read: what an edit changed
    /// moves the breakpoints.
    text: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

/// The old side of a side-by-side diff, which scrolls with the new one, and
/// the changes within lines of both.
struct OldSide {
    editor: Entity<EditorState>,
    marks: Option<(RangeDecorationCollection, RangeDecorationCollection)>,
    /// Both sides in one column, shown instead when there's no room for two.
    inline: Entity<EditorState>,
    inline_marks: Option<RangeDecorationCollection>,
    /// The width the diff had when last drawn.
    width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

/// Narrower than this, a diff shows in one column.
pub(crate) const SIDE_BY_SIDE_WIDTH: f32 = 1200.;

/// `git blame` of a file: `lines[i]` indexes `commits`, `None` if uncommitted.
struct Blame {
    commits: Vec<CommitInfo>,
    lines: Vec<Option<u32>>,
}

/// A place in the jump history.
#[derive(Clone, PartialEq)]
struct Place {
    path: PathBuf,
    position: Position,
}

/// Cmd-Shift-O or Cmd-Shift-T, while open.
struct SymbolSearch {
    picker: Entity<SymbolPicker>,
    /// The editor in front, its selections and scroll: what Esc goes back to.
    origin: Option<(Entity<EditorState>, Vec<(usize, usize)>, Point<Pixels>)>,
    /// The workspace's symbols being asked for.
    request: Task<()>,
    _subscription: Subscription,
}

/// Places remembered for going back.
const MAX_PLACES: usize = 100;

#[derive(Clone, PartialEq)]
struct DiffOf {
    /// Relative to the task's folder; empty for the whole commit.
    file: String,
    /// What changed in a commit (hash and short hash), not in the folder.
    commit: Option<(String, String)>,
    /// The file as it was in `commit`, not its diff.
    source: bool,
}

impl DiffOf {
    fn commit(commit: String, short: String, file: String, source: bool) -> Self {
        Self { file, commit: Some((commit, short)), source }
    }
}

pub struct Workspace {
    root: PathBuf,
    /// What of it shows (see `layout::Panels`).
    panels: Panels,
    /// The task's key in `config.json`, to remember what was open.
    session_key: String,
    /// Last session's tabs were already reopened (nothing is saved before that).
    restored: bool,
    focus_handle: FocusHandle,
    /// The app's workspaces column.
    workspaces: Option<Entity<WorkspacesPanel>>,
    /// The app's agents panel (see `set_agents`).
    agents: Option<Entity<WorkspacesPanel>>,
    /// The app's tasks' state, on the activity bar's icons.
    badges: TaskBadges,
    file_tree: Entity<FileTree>,
    terminals: Entity<TerminalArea>,
    terminals_maximized: bool,
    /// The columns.
    split: config::Split,
    /// The code and, while it's their place, the terminals under it.
    rows: config::Split,
    width: Pixels,
    /// The checked-out branch, from the workspaces list (see `set_branch`).
    branch: Option<String>,
    client: Option<Arc<Client>>,
    /// The agent reports the root's changes while this lives.
    fs_watch: Option<client::Watch>,
    /// On this machine (not on a server).
    local: bool,
    changes: Entity<ChangesPanel>,
    history: Entity<ChangesPanel>,
    search: Entity<SearchPanel>,
    /// References panel: the latest F12 (with several targets) or Shift-F12.
    references: Entity<SearchPanel>,
    outline: Entity<OutlinePanel>,
    /// What the outline's symbols were last asked for: the editor, its
    /// `revision` and length (which a load changes).
    outline_of: Option<(EntityId, u64, usize)>,
    outline_task: Task<()>,
    /// Counts the edits of every tab, for the outline to know it's out of date.
    revision: u64,
    finder: Option<(Entity<Picker>, Subscription)>,
    /// Cmd-Shift-O or Cmd-Shift-T, while open.
    symbols: Option<SymbolSearch>,
    /// Where you were before each jump (F12, results, Cmd-P…), to go back
    /// with Ctrl-Opt-←; and what was undone, to go forward with Ctrl-Opt-→.
    back: Vec<Place>,
    forward: Vec<Place>,
    /// Going back or forward: that jump isn't recorded.
    navigating: bool,
    /// The task's file list for Cmd-P (refreshed each time it opens).
    files: Arc<Vec<String>>,
    tabs: Vec<FileTab>,
    /// The active tab of the focused group (the one keys and commands act on).
    active: Option<usize>,
    /// The code area split in two groups of tabs, and which one has the focus.
    editor_split: Option<Axis>,
    /// Preview of a tab drop over an editor group's content.
    editor_drop: Option<(usize, EditorDrop)>,
    group: usize,
    /// Counter for `FileTab::shown`.
    shown: u64,
    /// Word wrap as applied to the tabs (it follows the config).
    word_wrap: bool,
    message: Option<SharedString>,
    /// On a server, ports the task's terminals are listening on.
    ports: Vec<PortInfo>,
    /// The signature of the call being typed, and where it was last asked for.
    signature: Option<SignatureHint>,
    signature_at: Option<Position>,
    signature_task: Task<()>,
    debugger: Entity<Debugger>,
    /// Its parts with a panel or a tab of their own.
    debug_views: HashMap<Panel, Entity<DebugView>>,
    debug_hover: Entity<debug::hover::HoverCard>,
    device: Entity<Device>,
    notes: Entity<NotesPanel>,
    /// The terminal the tests run in, reused by the next one.
    test_term: Option<proto::TermId>,
    _subscriptions: Vec<Subscription>,
}

/// How often a task on a server checks which ports its terminals opened.
const PORTS_REFRESH: std::time::Duration = std::time::Duration::from_secs(3);

impl Workspace {
    pub fn new(
        root: PathBuf,
        agent: Option<Arc<Client>>,
        local: bool,
        session_key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let file_tree = cx.new(|cx| FileTree::new(root.clone(), agent.clone(), local, cx));
        let has_agent = agent.is_some();
        let terminals = cx.new(|cx| TerminalArea::new(root.clone(), agent.clone(), local, cx));
        let changes = cx.new(|cx| ChangesPanel::new(root.clone(), agent.clone(), local, changes::View::Uncommitted, cx));
        let history = cx.new(|cx| ChangesPanel::new(root.clone(), agent.clone(), local, changes::View::History, cx));
        let search = cx.new(|cx| SearchPanel::new(root.clone(), agent.clone(), window, cx));
        let references = cx.new(|cx| SearchPanel::references(root.clone(), window, cx));
        let outline = cx.new(|_| OutlinePanel::new());
        let debugger = cx.new(|cx| Debugger::new(root.clone(), agent.clone(), session_key.clone(), window, cx));
        let debug_hover = cx.new(|cx| debug::hover::HoverCard::new(debugger.clone(), cx));
        let debug_views = layout::debug_views(&debugger, cx);
        let device = cx.new(|cx| Device::new(root.clone(), local, cx));
        let notes = cx.new(|cx| NotesPanel::new(session_key.clone(), window, cx));
        // The tests' Run and Debug come from the launch file.
        debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
        let subscriptions = vec![
            cx.subscribe_in(&debugger, window, Self::on_debug_event),
            cx.subscribe_in(&device, window, Self::on_device_event),
            cx.subscribe_in(&outline, window, Self::on_outline),
            // The Device panel's play and stop are the session's, and it shows how it starts.
            cx.observe(&debugger, |this, debugger, cx| {
                let (on, starting) = debugger.read(cx).session();
                this.device.update(cx, |device, cx| device.set_session(on, starting, cx));
                cx.notify();
            }),
            // Its icon, when the device file comes or goes.
            cx.observe(&device, |_, _, cx| cx.notify()),
            // The dot on its tab and icon, when it fills or empties.
            cx.observe(&notes, |_, _, cx| cx.notify()),
            // The count on the changes' icon.
            cx.observe(&changes, {
                let mut last = 0;
                move |_, changes, cx| {
                    let count = changes.read(cx).count();
                    if count != last {
                        last = count;
                        cx.notify();
                    }
                }
            }),
            cx.observe_self(|this, cx| this.remember(cx)),
            cx.subscribe_in(
                &file_tree,
                window,
                |this, _, event: &FileTreeEvent, window, cx| match event {
                    FileTreeEvent::Open { path, pin } => this.open_with(path.clone(), *pin, *pin, window, cx),
                    FileTreeEvent::Renamed { from, to } => this.renamed(from, to, cx),
                    FileTreeEvent::Trashed { path } => this.trashed(path, window, cx),
                    FileTreeEvent::ShowHistory { path, dir } => this.show_history(path, *dir, cx),
                    FileTreeEvent::OpenTerminal { dir } => {
                        this.show_panel(Panel::Terminals, cx);
                        this.terminals.update(cx, |terminals, cx| terminals.new_terminal_in(dir.clone(), window, cx));
                    }
                    FileTreeEvent::Error(message) => {
                        this.message = Some(message.clone());
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &terminals,
                window,
                |this, _, event: &TerminalAreaEvent, window, cx| match event {
                    TerminalAreaEvent::Hide => this.set_terminals_visible(false, window, cx),
                    TerminalAreaEvent::OpenPath { path, line, column } => {
                        let goto = Position::new(
                            line.unwrap_or(1).saturating_sub(1),
                            column.unwrap_or(1).saturating_sub(1),
                        );
                        this.open_at(path.clone(), goto, window, cx);
                    }
                    TerminalAreaEvent::Message(message) => {
                        this.message = Some(message.clone());
                        cx.notify();
                    }
                    TerminalAreaEvent::ShowPanel(Some(panel)) => this.show_panel(*panel, cx),
                    TerminalAreaEvent::ShowPanel(None) => this.show_panel(Panel::Terminals, cx),
                    TerminalAreaEvent::ClosePanel(panel) => this.hide_panel(*panel, cx),
                },
            ),
            cx.subscribe_in(&changes, window, Self::on_git_event),
            cx.subscribe_in(&history, window, Self::on_git_event),
            cx.subscribe_in(&search, window, |this, search, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { file, line, column, pin } => {
                    let goto = Position::new(line.saturating_sub(1), *column);
                    this.open_at_with(this.root.join(file), goto, *pin, *pin, window, cx);
                }
                SearchEvent::Replace { files } => {
                    let dirty: HashSet<PathBuf> = this.tabs.iter().filter(|tab| tab.dirty).map(|tab| tab.path.clone()).collect();
                    let (skipped, files): (Vec<String>, Vec<String>) =
                        files.iter().cloned().partition(|file| dirty.contains(&this.root.join(file)));
                    search.update(cx, |search, cx| search.replace(files, skipped.len(), cx));
                }
            }),
            // Paths outside the task (the standard library) are absolute.
            cx.subscribe_in(&references, window, |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { file, line, column, pin } => {
                    let goto = Position::new(line.saturating_sub(1), *column);
                    this.open_at_with(this.root.join(file), goto, *pin, *pin, window, cx);
                }
                SearchEvent::Replace { .. } => {}
            }),
        ];
        terminals.update(cx, |terminals, cx| terminals.restore(window, cx));
        if has_agent {
            changes.update(cx, |changes, cx| changes.mark_stale(false, cx));
        }
        let focus_handle = cx.focus_handle();
        // Disk changes are watched by the agent on the task's machine.
        let fs_watch = agent.as_ref().map(|client| Self::watch_fs(&root, client, window, cx));
        let message = (!has_agent).then(|| "No agent: no files or terminals".into());
        if !local {
            Self::watch_ports(cx);
        }
        Self {
            root,
            panels: Panels::new(),
            session_key,
            restored: false,
            focus_handle,
            workspaces: None,
            agents: None,
            badges: TaskBadges::default(),
            file_tree,
            terminals,
            terminals_maximized: false,
            split: config::Split::new(cx),
            rows: config::Split::new(cx),
            width: px(0.),
            branch: None,
            client: agent,
            fs_watch,
            local,
            changes,
            history,
            search,
            references,
            outline,
            outline_of: None,
            outline_task: Task::ready(()),
            revision: 0,
            finder: None,
            symbols: None,
            back: Vec::new(),
            forward: Vec::new(),
            navigating: false,
            files: Arc::default(),
            tabs: Vec::new(),
            active: None,
            editor_split: None,
            editor_drop: None,
            group: 0,
            shown: 0,
            word_wrap: Config::get(cx).word_wrap,
            message,
            ports: Vec::new(),
            signature: None,
            signature_at: None,
            signature_task: Task::ready(()),
            debugger,
            debug_views,
            debug_hover,
            device,
            notes,
            test_term: None,
            _subscriptions: subscriptions,
        }
    }

    /// Keeps `ports` up to date with what the task's terminals listen on,
    /// for as long as the workspace lives.
    fn watch_ports(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok((client, group)) = this.update(cx, |this, _| {
                    (this.client.clone(), this.root.to_string_lossy().into_owned())
                }) else {
                    break;
                };
                if let Some(client) = client
                    && let Ok(Response::Ports(ports)) = client.request(Request::Ports).await
                {
                    let ports: Vec<PortInfo> = ports.into_iter().filter(|info| info.group == group).collect();
                    let alive = this.update(cx, |this, cx| {
                        if this.ports != ports {
                            this.ports = ports;
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
                cx.background_executor().timer(PORTS_REFRESH).await;
            }
        })
        .detach();
    }

    /// Opens a port of the server in the browser, forwarded over SSH.
    fn open_port(&mut self, port: u16, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let url = format!("http://localhost:{port}/");
            let result = cx.background_spawn(async move { client.local_url(&url) }).await;
            this.update(cx, |this, cx| match result {
                Ok(url) => cx.open_url(&url),
                Err(err) => {
                    this.message = Some(format!("{err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reopens what was open last time, without stealing focus.
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = Config::get(cx)
            .sessions
            .get(&self.session_key)
            .cloned()
            .unwrap_or_default();
        self.editor_split = session.split;
        self.restore_panels(session.shows);
        // A git panel that shows from the start reads now, not when shown.
        for (panel, entity) in [(Panel::Changes, self.changes.clone()), (Panel::History, self.history.clone())] {
            if self.client.is_some() && self.is_shown(panel, cx) {
                entity.update(cx, |entity, cx| entity.shown(cx));
            }
        }
        for saved in session.tabs {
            // The same file twice: the second is a view of the first.
            if let Some(file) = self.tabs.iter().position(|tab| tab.path == saved.path) {
                let view = self.new_view(file, saved.group.min(1), true, window, cx);
                self.tabs[view].goto = Some(Position::new(saved.line, saved.column));
                continue;
            }
            let mut tab = self.new_tab(saved.path.clone(), false, window, cx);
            tab.grab_focus = false;
            tab.restored = true;
            tab.goto = Some(Position::new(saved.line, saved.column));
            tab.group = saved.group.min(1);
            self.tabs.push(tab);
            self.load(saved.path, false, window, cx);
        }
        self.normalize_groups();
        self.active = session
            .active
            .filter(|ix| *ix < self.tabs.len())
            .or((!self.tabs.is_empty()).then_some(0));
        if let Some(ix) = self.active {
            self.group = self.tabs[ix].group;
            self.mark_shown(ix);
            let path = self.tabs[ix].path.clone();
            self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        }
        self.restored = true;
        cx.notify();
    }

    /// What is open now (excluding diff tabs).
    fn session(&self, cx: &App) -> Session {
        let mut session = Session {
            split: self.editor_split,
            shows: Some(self.panels.saved()),
            ..Session::default()
        };
        for (ix, tab) in self.tabs.iter().enumerate().filter(|(_, tab)| tab.diff.is_none() && !tab.doc) {
            if self.active == Some(ix) {
                session.active = Some(session.tabs.len());
            }
            // If it hasn't loaded yet, the right cursor is the one it's headed to.
            let cursor = tab.goto.unwrap_or_else(|| tab.editor.read(cx).cursor_position());
            session.tabs.push(SavedTab {
                path: tab.path.clone(),
                line: cursor.line,
                column: cursor.character,
                group: tab.group,
            });
        }
        session
    }

    /// Saves what's open if it changed. Opening, closing or switching tabs is
    /// saved right away; cursor moves, with the next save.
    fn remember(&mut self, cx: &mut Context<Self>) {
        if !self.restored {
            return;
        }
        let session = self.session(cx);
        let saved = Config::get(cx).sessions.get(&self.session_key).cloned().unwrap_or_default();
        if saved == session {
            return;
        }
        let moved_only = saved.active == session.active
            && saved.split == session.split
            && saved.tabs.iter().map(|tab| (&tab.path, tab.group)).eq(session.tabs.iter().map(|tab| (&tab.path, tab.group)));
        let key = self.session_key.clone();
        let change = move |config: &mut Config| {
            config.sessions.insert(key, session);
        };
        if moved_only {
            Config::update_quietly(cx, change);
        } else {
            Config::update(cx, change);
        }
    }

    /// Removes a tab without moving focus.
    fn forget(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.active = self.remove_tab(ix);
        if let Some(ix) = self.active {
            self.mark_shown(ix);
        }
        cx.notify();
    }

    /// Removes a tab and returns the tab to make active: the same one if it
    /// was another (its index may have moved), or the neighbor in its group,
    /// or the other group's if its group was left empty (the split closes).
    fn remove_tab(&mut self, ix: usize) -> Option<usize> {
        let group = self.tabs[ix].group;
        let active = self.active;
        self.tabs.remove(ix);
        let groups_before = self.editor_split.is_some();
        self.normalize_groups();
        let collapsed = groups_before && self.editor_split.is_none();
        match active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > ix => Some(active - 1),
            Some(active) if active < ix => Some(active),
            _ if collapsed => self.shown_in(0),
            _ => {
                let same = |i: &usize| self.tabs[*i].group == group;
                (ix..self.tabs.len()).find(same).or_else(|| (0..ix).rev().find(same)).or(self.shown_in(self.group))
            }
        }
    }

    /// With a group left empty the split closes and everything goes to group 0.
    fn normalize_groups(&mut self) {
        let split = self.editor_split.is_some()
            && self.tabs.iter().any(|tab| tab.group == 0)
            && self.tabs.iter().any(|tab| tab.group == 1);
        if !split {
            self.editor_split = None;
            self.group = 0;
            for tab in &mut self.tabs {
                tab.group = 0;
            }
        }
    }

    /// The tab a group shows: the active one for the focused group, the
    /// most recently shown for the other.
    fn shown_in(&self, group: usize) -> Option<usize> {
        if group == self.group && self.active.is_some_and(|ix| self.tabs.get(ix).is_some_and(|tab| tab.group == group)) {
            return self.active;
        }
        (0..self.tabs.len()).filter(|ix| self.tabs[*ix].group == group).max_by_key(|ix| self.tabs[*ix].shown)
    }

    fn mark_shown(&mut self, ix: usize) {
        self.shown += 1;
        self.tabs[ix].shown = self.shown;
    }

    /// Asks the agent to report changes inside `root`, until the returned
    /// `Watch` is dropped (with the workspace, or for another connection's).
    fn watch_fs(root: &Path, client: &Arc<Client>, window: &mut Window, cx: &mut Context<Self>) -> client::Watch {
        let (tx, rx) = smol::channel::unbounded::<Vec<PathBuf>>();
        let watched = root.to_path_buf();
        let watch = client.watch_fs(root, move |event| {
            if let proto::Event::FsChanged { root, paths } = event
                && *root == watched
            {
                let _ = tx.try_send(paths.clone());
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(paths) = rx.recv().await {
                let paths: HashSet<PathBuf> = paths.into_iter().collect();
                if this
                    .update_in(cx, |this, window, cx| this.on_fs_changed(paths, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        watch
    }

    /// Switches to a new connection with the agent (after reconnecting): the
    /// tree, panels and terminals carry on with it, and open files are reread.
    pub fn set_client(&mut self, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        self.client = Some(client.clone());
        // Whatever failed with the lost connection ("Couldn't open terminal")
        // is retried with this one.
        self.message = None;
        self.fs_watch = Some(Self::watch_fs(&self.root, &client, window, cx));
        self.file_tree
            .update(cx, |tree, cx| tree.set_client(client.clone(), cx));
        for (panel, entity) in self.git_panels() {
            let visible = self.is_shown(panel, cx);
            entity.update(cx, |entity, cx| entity.set_client(client.clone(), visible, cx));
        }
        self.search.update(cx, |search, _| search.set_client(client.clone()));
        self.debugger.update(cx, |debugger, cx| {
            debugger.set_client(client.clone(), cx);
            debugger.refresh_launches(cx);
        });
        self.terminals
            .update(cx, |terminals, cx| terminals.set_client(client, window, cx));
        // Reopened files that couldn't be read while offline are read now for the first time.
        let reload: Vec<(PathBuf, bool)> = self
            .tabs
            .iter()
            .filter(|tab| tab.is_file() && (matches!(tab.content, Content::Ready) || tab.restored))
            .map(|tab| (tab.path.clone(), !tab.restored))
            .collect();
        for (path, reload) in reload {
            self.load(path, reload, window, cx);
        }
        cx.notify();
    }

    pub fn open(&mut self, path: PathBuf, pin: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_with(path, pin, true, window, cx);
    }

    /// A link clicked in rendered Markdown: URLs go to the browser; paths
    /// (relative to the file's folder, or to the task's root if they start with
    /// `/`) open in a tab. Anchors (`#section`) are ignored.
    fn follow_link(&mut self, url: &str, dir: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if url.contains("://") || url.starts_with("mailto:") {
            cx.open_url(url);
            return;
        }
        let path = url.split(['#', '?']).next().unwrap_or_default();
        if path.is_empty() {
            return;
        }
        let path = percent_decode(path);
        let path = match path.strip_prefix('/') {
            Some(rest) => self.root.join(rest),
            None => dir.join(path),
        };
        self.open(normalize(&path), true, window, cx);
    }

    /// Shows `text`, Markdown, in a tab called `title` of its own (the
    /// shortcuts guide): rendered, read-only, with no file behind it. Open
    /// already, it gets the new text.
    pub fn open_doc(&mut self, title: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let path = PathBuf::from(title);
        let ix = match self.tabs.iter().position(|tab| tab.doc && tab.path == path) {
            Some(ix) => ix,
            None => {
                let mut tab = self.new_tab_with(path, false, "markdown", window, cx);
                tab.doc = true;
                tab.grab_focus = false;
                self.place_tab(tab, true)
            }
        };
        let tab = &mut self.tabs[ix];
        tab.content = Content::Ready;
        tab.saved = text.clone();
        tab.show_source = false;
        if let Some(markdown) = &tab.markdown {
            markdown.update(cx, |view, cx| view.set_text(&text, cx));
        }
        tab.editor.update(cx, |state, cx| state.set_value(text, window, cx));
        self.activate_with(ix, false, window, cx);
        cx.notify();
    }

    /// Opens `path`; with `focus`, the keyboard goes to the editor.
    fn open_with(&mut self, path: PathBuf, pin: bool, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let group = self.group;
        let found = self
            .tabs
            .iter()
            .position(|tab| tab.shows_file(&path) && tab.group == group)
            .or_else(|| self.tabs.iter().position(|tab| tab.path == path && tab.is_file()));
        if let Some(ix) = found {
            if pin {
                self.tabs[ix].preview = false;
            }
            self.tabs[ix].grab_focus = focus;
            self.activate_with(ix, focus, window, cx);
            return;
        }

        let mut tab = self.new_tab(path.clone(), !pin, window, cx);
        tab.grab_focus = focus;
        // A preview reuses the previous preview's tab.
        let ix = self.place_tab(tab, pin);
        self.load(path, false, window, cx);
        self.activate_with(ix, focus, window, cx);
    }

    /// Puts a new tab in the focused group: over that group's preview tab
    /// (unless `pin`), or after the active one.
    fn place_tab(&mut self, tab: FileTab, pin: bool) -> usize {
        let group = self.group;
        let reuse = self
            .tabs
            .iter()
            .position(|tab| tab.preview && !tab.dirty && tab.group == group)
            .filter(|_| !pin);
        match reuse {
            Some(ix) => {
                self.tabs[ix] = tab;
                ix
            }
            None => {
                let ix = self.active.map_or(self.tabs.len(), |active| active + 1);
                self.tabs.insert(ix, tab);
                ix
            }
        }
    }

    /// The active tab's file and cursor position.
    fn place(&self, cx: &App) -> Option<Place> {
        let tab = &self.tabs[self.active?];
        if !tab.is_file() {
            return None;
        }
        let position = tab.goto.unwrap_or_else(|| tab.editor.read(cx).cursor_position());
        Some(Place { path: tab.path.clone(), position })
    }

    /// Before a jump: records where you were (unless it's the last one recorded).
    fn remember_place(&mut self, cx: &App) {
        let Some(place) = self.place(cx) else {
            return;
        };
        if self.back.last() != Some(&place) {
            self.back.push(place);
            if self.back.len() > MAX_PLACES {
                self.back.remove(0);
            }
        }
        self.forward.clear();
    }

    /// Ctrl-Opt-← and Ctrl-Opt-→: to the last place on one side; the current one moves to the other.
    fn navigate(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let place = if backward { self.back.pop() } else { self.forward.pop() };
        let Some(place) = place else {
            return;
        };
        let current = self.place(cx);
        if backward {
            self.forward.extend(current);
        } else {
            self.back.extend(current);
        }
        self.navigating = true;
        self.open_at_with(place.path, place.position, true, true, window, cx);
        self.navigating = false;
    }

    /// Opens a pinned file with the cursor at `goto` (Cmd-click in a terminal).
    fn open_at(&mut self, path: PathBuf, goto: Position, window: &mut Window, cx: &mut Context<Self>) {
        self.open_at_with(path, goto, true, true, window, cx);
    }

    /// Opens `path` with the cursor at `goto`; without `focus`, the keyboard
    /// stays where it was (a search result in preview, F4).
    fn open_at_with(
        &mut self,
        path: PathBuf,
        goto: Position,
        pin: bool,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.navigating {
            self.remember_place(cx);
        }
        self.terminals_maximized = false;
        let focused = window.focused(cx);
        self.open_with(path.clone(), pin, focus, window, cx);
        let Some(tab) = self.active.map(|ix| &mut self.tabs[ix]).filter(|tab| tab.path == path) else {
            return;
        };
        // Going to a line makes sense in the source, not in the rendered view.
        if tab.markdown.is_some() {
            tab.show_source = true;
        }
        match tab.content {
            Content::Ready => {
                tab.editor
                    .update(cx, |state, cx| state.set_cursor_position(goto, window, cx));
                reveal_centered(&tab.editor, goto.line, false, 10, window, cx);
            }
            _ => tab.goto = Some(goto),
        }
        if focus {
            self.focus_active(window, cx);
        } else if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }

    /// Opens (or reuses) the tab with the diff of `file`: the folder's, or a
    /// commit's, which belongs to that commit only.
    fn open_diff(&mut self, of: DiffOf, pin: bool, window: &mut Window, cx: &mut Context<Self>) {
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
                self.load_diff(ix, window, cx);
            }
            self.activate_with(ix, false, window, cx);
            return;
        }
        // A whole commit is its own tab, not a file's.
        let path = match &of.commit {
            Some((commit, _)) if file.is_empty() => self.root.join(commit),
            _ => self.root.join(&file),
        };
        let language = if of.source { language::for_path(&path) } else { "diff" };
        let mut tab = self.new_tab_with(path, !pin, language, window, cx);
        tab.diff = Some(of);
        tab.grab_focus = false;
        let ix = self.place_tab(tab, pin);
        self.load_diff(ix, window, cx);
        self.activate_with(ix, false, window, cx);
    }

    fn load_diff(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(of)) = (self.client.clone(), self.tabs[ix].diff.clone()) else {
            return;
        };
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
        cx.spawn_in(window, async move |this, cx| {
            let mut response = None;
            if let Some(whole) = whole {
                // An agent that doesn't know `WholeDiff` fails: the plain diff then.
                match client.request(whole).await {
                    Ok(Response::Text(text)) => response = Some(Ok(Response::Text(text))),
                    _ => {}
                }
            }
            let response = match response {
                Some(response) => response,
                None => client.request(request).await,
            };
            this.update_in(cx, |this, window, cx| {
                let Some(ix) = this.tabs.iter().position(|tab| tab.diff.as_ref() == Some(&of)) else {
                    return;
                };
                let sides = match &response {
                    Ok(Response::Text(text)) if !of.source && !of.file.is_empty() => diff::split(text),
                    _ => None,
                };
                if let Some(sides) = sides {
                    this.show_side_by_side(ix, sides, window, cx);
                    cx.notify();
                    return;
                }
                if let (Ok(Response::Text(show)), Some((hash, short))) = (&response, &of.commit)
                    && of.file.is_empty()
                    && !of.source
                {
                    let view = cx.new(|cx| CommitView::new(show, cx));
                    let (hash, short) = (hash.clone(), short.clone());
                    let subscription = cx.subscribe_in(&view, window, move |this, _, event: &CommitViewEvent, window, cx| {
                        let CommitViewEvent::OpenFile(file) = event;
                        this.open_diff(DiffOf::commit(hash.clone(), short.clone(), file.clone(), false), true, window, cx);
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

    /// Shows a file's diff side by side: the old side in its own editor, the
    /// new one in the tab's, both highlighted as the file and scrolling
    /// together. The first time it goes to the first change.
    fn show_side_by_side(&mut self, ix: usize, sides: diff::SideBySide, window: &mut Window, cx: &mut Context<Self>) {
        let language = language::for_path(&self.tabs[ix].path);
        let new = self.tabs[ix].editor.clone();
        let first = self.tabs[ix].old.is_none();
        if first {
            let editor = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(false));
            let subscriptions = vec![
                cx.observe(&editor, {
                    let new = new.clone();
                    move |_, old, cx| follow_scroll(&old, &new, cx)
                }),
                cx.observe(&new, {
                    let old = editor.clone();
                    move |_, new, cx| follow_scroll(&new, &old, cx)
                }),
            ];
            let inline = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(false));
            self.tabs[ix].old = Some(OldSide {
                editor,
                marks: None,
                inline,
                inline_marks: None,
                width: Rc::new(Cell::new(px(f32::MAX))),
                _subscriptions: subscriptions,
            });
        }
        let theme = cx.theme();
        let removed = (theme.danger.opacity(0.14), theme.danger.opacity(0.3));
        let added = (theme.success.opacity(0.14), theme.success.opacity(0.3));
        let focused = window.focused(cx);
        let tab = &mut self.tabs[ix];
        tab.saved = sides.new.text.clone();
        tab.content = Content::Ready;
        let old = tab.old.as_mut().expect("the old side was just created");
        let mut marks = Vec::new();
        for (editor, side, (line, word), marker) in [(&old.editor, &sides.old, removed, '−'), (&new, &sides.new, added, '+')] {
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
                state.set_line_styles(line_styles(side, marker, line), cx);
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
            state.set_line_styles(inline_line_styles(&inline, removed.0, added.0), cx);
        });
        match &old.inline_marks {
            Some(collection) => collection.set(inline_marks, cx),
            None => {
                let collection = old.inline.update(cx, |state, cx| state.create_range_decorations_collection(inline_marks, cx));
                old.inline_marks = Some(collection);
            }
        }
        if first {
            if let Some(&row) = sides.changes.first() {
                let at = Position::new(row as u32, 0);
                new.update(cx, |state, cx| state.set_cursor_position(at, window, cx));
                reveal_centered(&new, row as u32, true, 10, window, cx);
            }
            if let Some(&row) = inline.changes.first() {
                let editor = old.inline.clone();
                editor.update(cx, |state, cx| state.set_cursor_position(Position::new(row as u32, 0), window, cx));
                reveal_centered(&editor, row as u32, true, 10, window, cx);
            }
        }
        if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }

    /// Reads the file's blame from the agent (silently: outside a repo, or
    /// with an agent that doesn't know `Blame`, there's none).
    fn load_blame(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let (Some(client), Ok(file)) = (self.client.clone(), path.strip_prefix(&self.root)) else {
            return;
        };
        let request = Request::Git { path: self.root.clone(), op: GitOp::Blame { file: file.to_string_lossy().into_owned() } };
        cx.spawn(async move |this, cx| {
            let blame = match client.request(request).await {
                Ok(Response::Blame { commits, lines }) => Some(Arc::new(Blame { commits, lines })),
                _ => None,
            };
            this.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.path == path && tab.is_file()) {
                    tab.blame = blame;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Highlights the other occurrences of the word under the cursor, when
    /// the selections changed.
    fn highlight_occurrences(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.editor == editor) else {
            return;
        };
        let state = editor.read(cx);
        let selections = state.selections();
        if selections == tab.occurrences_for {
            return;
        }
        let ranges = editing::occurrences(&state.value(), &selections);
        tab.occurrences_for = selections;
        let color = cx.theme().selection.opacity(0.45);
        let decorations: Vec<RangeDecoration> = ranges
            .into_iter()
            .map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color))
            .collect();
        match &tab.occurrences {
            Some(collection) => collection.set(decorations, cx),
            None if decorations.is_empty() => {}
            None => {
                let collection = editor.update(cx, |state, cx| state.create_range_decorations_collection(decorations, cx));
                tab.occurrences = Some(collection);
            }
        }
    }

    /// The editor of the active tab, if it shows text (not an image or rendered Markdown).
    fn active_editor(&self) -> Option<Entity<EditorState>> {
        let tab = &self.tabs[self.active?];
        (tab.image.is_none() && tab.rendered().is_none()).then(|| tab.editor.clone())
    }

    fn select_next_occurrence(&mut self, _: &SelectNextOccurrence, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        editor.update(cx, |state, cx| {
            let case_insensitive = state.search_session().case_insensitive;
            if let Some(selections) = editing::select_next_occurrence(&state.value(), &state.selections(), case_insensitive) {
                let reveal = selections[0].1;
                state.set_selections(&selections, cx);
                state.reveal_offset(reveal, cx);
            }
        });
    }

    /// Moves (`duplicate` false) or duplicates the selected lines.
    fn edit_lines(&mut self, up: bool, duplicate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        editor.update(cx, |state, cx| {
            let (text, selections) = (state.value(), state.selections());
            let edit = if duplicate {
                Some(editing::duplicate_lines(&text, &selections, up))
            } else {
                editing::move_lines(&text, &selections, up)
            };
            if let Some(edit) = edit {
                state.edit(&edit.edits, &edit.selections, true, window, cx);
            }
        });
    }

    /// Ctrl-G: asks for `line` or `line:column` and goes there.
    fn go_to_line(&mut self, _: &GoToLine, window: &mut Window, cx: &mut Context<Self>) {
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
                            reveal_centered(&editor, goto.line, false, 10, window, cx);
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
    fn open_file_finder(&mut self, _: &OpenFileFinder, window: &mut Window, cx: &mut Context<Self>) {
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
        if let Some(client) = self.client.clone() {
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
        cx.notify();
    }

    /// Cmd-Shift-O: to a symbol of the file, from its language server (in
    /// Markdown, its headings).
    fn go_to_symbol(&mut self, _: &GoToSymbol, window: &mut Window, cx: &mut Context<Self>) {
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
    fn go_to_workspace_symbol(&mut self, _: &GoToWorkspaceSymbol, window: &mut Window, cx: &mut Context<Self>) {
        if self.symbols.is_none() {
            self.open_symbols(Some(self.root.clone()), window, cx);
        }
    }

    /// Keeps the outline on the file in front and its cursor: its symbols are
    /// asked again a moment after it's edited, at once for another file.
    fn sync_outline(&mut self, cx: &mut Context<Self>) {
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

    fn on_outline(&mut self, _: &Entity<OutlinePanel>, event: &OutlineEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            OutlineEvent::Pick(symbol) => self.open_at(symbol.path.clone(), Position::new(symbol.line, symbol.column), window, cx),
        }
    }

    /// Opens the symbol picker: of the workspace at `workspace`, or of the
    /// file in front, which shows each one as it's selected.
    fn open_symbols(&mut self, workspace: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Entity<SymbolPicker> {
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

    fn on_symbol_picker(&mut self, picker: &Entity<SymbolPicker>, event: &SymbolPickerEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                    reveal_centered(editor, symbol.line, false, 10, window, cx);
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
    fn go_to_definition(&mut self, _: &GoToDefinition, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_lsp(LspOp::Definition, window, cx);
    }

    /// Shift-F12: the references of what's under the cursor, in their panel.
    fn find_references(&mut self, _: &FindReferences, window: &mut Window, cx: &mut Context<Self>) {
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
    fn ask_signature(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
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
    fn follow_signature(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
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

    fn close_signature(&mut self) {
        self.signature = None;
        self.signature_at = None;
        self.signature_task = Task::ready(());
    }

    fn ask_lsp(&mut self, op: LspOp, window: &mut Window, cx: &mut Context<Self>) {
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
    fn word_references(&mut self, word: String, cx: &mut Context<Self>) {
        let title = format!("“{word}” in text (no language server for this file)");
        let Some(client) = self.client.clone().filter(|_| !word.is_empty()) else {
            let error = "No symbol under the cursor".into();
            self.references.update(cx, |references, cx| references.set_results(title, Err(error), cx));
            return;
        };
        let request = Request::Search {
            path: self.root.clone(),
            query: format!(r"\b{word}\b"),
            regex: true,
            case_sensitive: true,
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
    fn hits(&self, locations: Vec<LspLocation>) -> Vec<SearchHit> {
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

    fn step_result(&mut self, delta: isize, cx: &mut Context<Self>) {
        let references = self.panels.results == Panel::References;
        let panel = if references { &self.references } else { &self.search };
        panel.update(cx, |panel, cx| panel.step(delta, cx));
    }

    /// Shows the References panel without moving focus.
    fn show_references(&mut self, cx: &mut Context<Self>) {
        self.terminals_maximized = false;
        self.show_panel(Panel::References, cx);
    }

    /// Cmd-Shift-F: the search panel, with the editor's selection.
    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
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

    fn new_terminal(&mut self, _: &NewTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(Panel::Terminals, cx);
        self.terminals
            .update(cx, |terminals, cx| terminals.new_terminal(window, cx));
        cx.notify();
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(Panel::Terminals, cx);
        self.terminals
            .update(cx, |terminals, cx| terminals.split(axis, window, cx));
        cx.notify();
    }

    /// Cmd-Option-arrows: only when focus is in a terminal.
    fn focus_pane(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.read(cx).contains_focus(window, cx) {
            self.terminals
                .update(cx, |terminals, cx| terminals.focus_neighbor(direction, window, cx));
        }
    }

    /// Shows the terminals and focuses them; if they already have focus, hides
    /// them and focus returns to the IDE.
    fn toggle_terminals(&mut self, _: &ToggleTerminals, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.terminals.read(cx).contains_focus(window, cx);
        self.set_terminals_visible(!(self.is_shown(Panel::Terminals, cx) && focused), window, cx);
    }

    /// Shows the terminals and focuses them, or hides them and focus returns to
    /// the IDE. The activity bar's icon, which ignores where the focus is.
    pub fn set_terminals_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if visible {
            self.show_panel(Panel::Terminals, cx);
            self.terminals
                .update(cx, |terminals, cx| terminals.focus(window, cx));
        } else {
            self.hide_panel(Panel::Terminals, cx);
            self.focus_ide(window, cx);
        }
        cx.notify();
    }

    fn maximize_terminals(&mut self, _: &MaximizeTerminals, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals_maximized = !self.terminals_maximized;
        if self.terminals_maximized {
            self.show_panel(Panel::Terminals, cx);
            self.terminals
                .update(cx, |terminals, cx| terminals.focus(window, cx));
        }
        cx.notify();
    }

    /// The window's width. When it changes, the other columns keep their
    /// width and the code's takes the difference.
    pub fn set_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
            cx.notify();
        }
    }

    /// The branch the workspaces list knows for this folder (refreshed every
    /// few seconds); `None` for a folder outside the known repos, which
    /// shows the one the Changes panel last read.
    pub fn set_branch(&mut self, branch: Option<String>, cx: &mut Context<Self>) {
        if self.branch != branch {
            self.branch = branch;
            cx.notify();
        }
    }

    /// Files with unsaved changes, relative to the task's folder.
    pub fn unsaved(&self) -> Vec<PathBuf> {
        self.tabs
            .iter()
            .filter(|tab| tab.dirty)
            .map(|tab| tab.path.strip_prefix(&self.root).unwrap_or(&tab.path).to_path_buf())
            .collect()
    }

    /// Focus on entering the task: the terminal if there is one, otherwise the IDE.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_shown(Panel::Terminals, cx) && !self.terminals.read(cx).is_empty() {
            self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        } else {
            self.focus_ide(window, cx);
        }
    }

    fn focus_ide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active.is_some() {
            self.focus_active(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }

    /// Inspect on the phone goes to the program being debugged.
    fn on_device_event(&mut self, device: &Entity<Device>, event: &DeviceEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DeviceEvent::Debug => self.debugger.update(cx, |debugger, cx| debugger.start_or_continue(window, cx)),
            DeviceEvent::Stop => self.debugger.update(cx, |debugger, cx| debugger.stop(cx)),
            DeviceEvent::Inspect => {
                let asked = self.debugger.update(cx, |debugger, _| debugger.inspect());
                if !asked {
                    device.update(cx, |device, cx| device.warn("Debug the app (F5) to inspect it.", cx));
                }
            }
        }
    }

    fn on_debug_event(&mut self, debugger: &Entity<Debugger>, event: &DebugEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DebugEvent::Show { path, line, focus } => {
                let goto = Position::new(*line, 0);
                let open = self.tabs.iter().position(|tab| tab.path == *path && tab.is_file());
                self.open_at_with(path.clone(), goto, true, *focus, window, cx);
                // a file already open only scrolls if the line isn't in view
                if let Some(ix) = open {
                    let editor = self.tabs[ix].editor.clone();
                    reveal_centered(&editor, *line, false, 10, window, cx);
                }
                if *focus && debugger.read(cx).is_stopped() {
                    window.activate_window();
                }
                self.refresh_debug_marks(cx);
            }
            DebugEvent::Marks => self.refresh_debug_marks(cx),
            DebugEvent::Run { session, term, line } => {
                let session = *session;
                let file = self.active_file().map(|ix| {
                    let path = &self.tabs[ix].path;
                    path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
                });
                let line = debug::command_line(line, file.as_deref());
                debugger.update(cx, |debugger, _| debugger.set_ran(session, line.clone(), file));
                self.show_panel(Panel::Terminals, cx);
                let run = self.terminals.update(cx, |terminals, cx| terminals.run_line(*term, line, window, cx));
                let debugger = debugger.downgrade();
                cx.spawn(async move |_, cx| {
                    let term = run.await;
                    debugger.update(cx, |debugger, cx| debugger.set_terminal(session, term, cx)).ok();
                })
                .detach();
                cx.notify();
            }
            DebugEvent::Interrupt { term } => {
                self.terminals.update(cx, |terminals, cx| terminals.interrupt(*term, cx));
            }
            DebugEvent::Refocus => self.focus_active(window, cx),
            DebugEvent::Reveal => self.reveal_debugger(cx),
            DebugEvent::Hide => self.hide_panel(Panel::Debugger, cx),
            DebugEvent::Device(id) => {
                self.show_panel(Panel::Device, cx);
                self.device.update(cx, |device, cx| device.show_device(id.clone(), cx));
            }
        }
    }

    /// Redraws the breakpoints and the line stopped at in every editor.
    fn refresh_debug_marks(&mut self, cx: &mut Context<Self>) {
        let debugger = self.debugger.read(cx);
        let execution = debugger.execution();
        let theme = cx.theme();
        let (stop_line, stop_arrow) = (theme.warning.opacity(0.22), theme.warning);
        let (frame_line, frame_arrow) = (theme.info.opacity(0.14), theme.info);
        let (red, orange, gray) = (debug::panel::breakpoint_color(cx), theme.warning, theme.muted_foreground);
        let mut updates = Vec::new();
        for tab in &self.tabs {
            if tab.diff.is_some() || tab.doc {
                continue;
            }
            let marks: Vec<GutterMark> = debugger
                .breakpoints
                .of(&tab.path)
                .iter()
                .map(|bp| GutterMark {
                    line: bp.line as usize,
                    color: if !bp.enabled {
                        gray
                    } else if bp.error.is_some() || bp.is_special() {
                        orange
                    } else {
                        red
                    },
                    hollow: !bp.enabled || !bp.log.is_empty(),
                })
                .collect();
            let line = execution.as_ref().filter(|(path, _, _)| *path == tab.path).map(|(_, line, top)| ExecutionLine {
                line: *line as usize,
                background: if *top { stop_line } else { frame_line },
                arrow: if *top { stop_arrow } else { frame_arrow },
            });
            updates.push((tab.editor.clone(), marks, line));
        }
        for (editor, marks, line) in updates {
            editor.update(cx, |state, cx| {
                state.set_gutter_marks(marks, cx);
                state.set_execution_line(line, cx);
            });
        }
    }

    /// A click in an editor's gutter: left toggles a breakpoint, right edits
    /// its condition.
    fn gutter_clicked(&mut self, editor: &Entity<EditorState>, line: u32, button: MouseButton, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tab_index(editor) else {
            return;
        };
        let tab = &self.tabs[ix];
        if tab.diff.is_some() || tab.doc || tab.image.is_some() {
            return;
        }
        let path = tab.path.clone();
        match button {
            MouseButton::Left => self.debugger.update(cx, |debugger, cx| debugger.toggle_breakpoint(&path, line, cx)),
            MouseButton::Right => {
                self.debugger.update(cx, |debugger, cx| debugger.edit_breakpoint(path, line, EditKind::Condition, window, cx))
            }
            _ => {}
        }
    }

    /// The file and line (0-based) of the active editor's cursor.
    fn cursor_place(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let ix = self.active?;
        let tab = &self.tabs[ix];
        if tab.diff.is_some() || tab.doc {
            return None;
        }
        Some((tab.path.clone(), tab.editor.read(cx).cursor_position().line))
    }

    fn toggle_breakpoint(&mut self, _: &ToggleBreakpoint, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.toggle_breakpoint(&path, line, cx));
        }
    }

    /// The breakpoint editor at the cursor's line, with its condition or message.
    fn edit_breakpoint_at_cursor(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.edit_breakpoint(path, line, kind, window, cx));
        }
    }

    /// The editor's selection, else the name or member chain (`a.b.c`) at the cursor.
    fn expression_at_cursor(&self, cx: &App) -> Option<String> {
        let state = self.tabs[self.active?].editor.read(cx);
        let selected = state.selected_text().to_string().trim().to_string();
        if !selected.is_empty() {
            return (!selected.contains('\n')).then_some(selected);
        }
        let cursor = state.cursor_position();
        let line = state.text().to_string().lines().nth(cursor.line as usize)?.to_string();
        let offset = line.char_indices().nth(cursor.character as usize).map_or(line.len(), |(byte, _)| byte);
        // A cursor just past the name counts too.
        let span = debug::expression_span(&line, offset)
            .or_else(|| offset.checked_sub(1).and_then(|before| debug::expression_span(&line, before)))?;
        Some(line[span].to_string())
    }

    fn add_to_watch(&mut self, _: &AddToWatch, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(expr) = self.expression_at_cursor(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.add_watch(expr, cx));
            self.show_panel(Panel::Watch, cx);
        }
    }

    fn evaluate_in_console(&mut self, _: &EvaluateInConsole, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(expr) = self.expression_at_cursor(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.evaluate_in_console(expr, cx));
            self.show_panel(Panel::Console, cx);
        }
    }

    fn run_to_cursor(&mut self, _: &RunToCursor, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, _| debugger.run_to(&path, line));
        }
    }

    fn set_next_statement(&mut self, _: &SetNextStatement, _: &mut Window, cx: &mut Context<Self>) {
        let Some((path, line)) = self.cursor_place(cx) else {
            return;
        };
        let here = self.debugger.read(cx).execution().is_some_and(|(at, _, top)| at == path && top);
        if here {
            self.debugger.update(cx, |debugger, _| debugger.jump(line));
        } else {
            self.message = Some("The next statement must be in the function stopped at".into());
            cx.notify();
        }
    }

    fn toggle_debug_panel(&mut self, _: &ToggleDebugPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(Panel::Debugger, cx);
    }

    pub fn notes(&self) -> Entity<NotesPanel> {
        self.notes.clone()
    }

    /// The notes, with the focus to write in them.
    fn show_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_panel(Panel::Notes, cx);
        self.notes.update(cx, |notes, cx| notes.focus(window, cx));
    }

    /// Cmd-Alt-N: the notes, to write in them; written in, back to the code.
    fn toggle_notes(&mut self, _: &ToggleNotes, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_shown(Panel::Notes, cx) {
            self.hide_panel(Panel::Notes, cx);
            self.focus_ide(window, cx);
        } else {
            self.show_notes(window, cx);
        }
    }

    fn new_tab(&mut self, path: PathBuf, preview: bool, window: &mut Window, cx: &mut Context<Self>) -> FileTab {
        let language = language::for_path(&path);
        self.new_tab_with(path, preview, language, window, cx)
    }

    fn new_tab_with(
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
                .soft_wrap(Config::get(cx).word_wrap);
            let lsp = editor.lsp_mut();
            lsp.completion_provider = Some(Rc::new(Completions::new(workspace.clone(), cx.entity().downgrade())));
            lsp.completion_menu.max_width = px(480.);
            lsp.hover_provider = Some(Rc::new(debug::hover::DebugHover { debugger: debugger.clone(), editor: cx.entity().downgrade() }));
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
            restored: false,
            blame: None,
            occurrences: None,
            occurrences_for: Vec::new(),
            group: self.group,
            shown: 0,
            view: false,
            doc: false,
            text: None,
            _subscriptions: subscriptions,
        }
    }

    /// Reads a tab's file. On reload (it changed on disk) the cursor and
    /// scroll are kept, and unsaved changes aren't overwritten.
    fn load(&mut self, path: PathBuf, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
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
                            reveal_centered(&tab.editor, line, true, 10, window, cx);
                        }
                        // Its other views get the same text, keeping their cursor.
                        for view in this.tabs.iter_mut().filter(|tab| tab.view && tab.path == path) {
                            view.content = Content::Ready;
                            let goto = view.goto.take();
                            view.editor.update(cx, |state, cx| {
                                let cursor = goto.unwrap_or_else(|| state.cursor_position());
                                let scroll = state.scroll_offset();
                                state.set_value(text.clone(), window, cx);
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

    fn on_fs_changed(
        &mut self,
        paths: HashSet<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The launch file changed: its problems and tests show.
        let den = self.root.join(".den");
        if paths.iter().any(|path| path.starts_with(&den)) {
            self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
            self.device.update(cx, |device, cx| device.load(cx));
        }
        // `root/.git`: a commit, checkout or reset (HEAD moved): the blame of
        // every open file may have changed.
        if paths.contains(&self.root.join(".git")) {
            let files: Vec<PathBuf> = self.tabs.iter().filter(|tab| tab.is_file()).map(|tab| tab.path.clone()).collect();
            for path in files {
                self.load_blame(path, cx);
            }
        }
        self.file_tree
            .update(cx, |tree, cx| tree.invalidate(&paths, cx));
        for (panel, entity) in self.git_panels() {
            let visible = self.is_shown(panel, cx);
            entity.update(cx, |entity, cx| entity.mark_stale(visible, cx));
        }
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
            self.load_diff(ix, window, cx);
        }
    }

    /// A tab's text changed: its other views get the same edit, the rendered
    /// Markdown follows, and the file is marked as changed or not.
    fn on_edit(&mut self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
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
    fn is_dirty(&self, ix: usize) -> bool {
        let tab = &self.tabs[ix];
        if !tab.view {
            return tab.dirty;
        }
        self.tabs.iter().any(|file| file.path == tab.path && file.is_file() && file.dirty)
    }

    /// Opens another view of tab `of`'s file in `group`, with the same text
    /// and cursor, showing the source or the rendered Markdown.
    fn new_view(&mut self, of: usize, group: usize, show_source: bool, window: &mut Window, cx: &mut Context<Self>) -> usize {
        let path = self.tabs[of].path.clone();
        let language = language::for_path(&path);
        let mut view = self.new_tab_with(path, false, language, window, cx);
        let source = &self.tabs[of];
        view.view = true;
        view.group = group;
        view.show_source = show_source;
        view.markdown = source.markdown.clone();
        view.image = source.image.clone();
        view.grab_focus = false;
        if matches!(source.content, Content::Ready) {
            view.content = Content::Ready;
            let (text, cursor) = {
                let state = source.editor.read(cx);
                (state.value(), state.cursor_position())
            };
            let focused = window.focused(cx);
            view.editor.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.set_cursor_position(cursor, window, cx);
            });
            if let Some(focused) = focused {
                focused.focus(window, cx);
            }
        }
        self.tabs.push(view);
        let ix = self.tabs.len() - 1;
        self.mark_shown(ix);
        ix
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_with(ix, true, window, cx);
    }

    fn activate_with(&mut self, ix: usize, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_shown(Panel::Code, cx) {
            self.show_panel(Panel::Code, cx);
        }
        self.active = Some(ix);
        self.group = self.tabs[ix].group;
        self.mark_shown(ix);
        self.message = None;
        let tab = &self.tabs[ix];
        let path = tab.path.clone();
        if focus {
            self.focus_active(window, cx);
        }
        self.file_tree
            .update(cx, |tree, cx| tree.reveal(&path, cx));
        cx.notify();
    }

    /// Keys go to the source only if it's visible; with the rendered view they
    /// go to the workspace, so hidden text isn't edited.
    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.map(|ix| &self.tabs[ix]) else {
            return;
        };
        if tab.rendered().is_some() || tab.image.is_some() || !matches!(tab.content, Content::Ready) {
            self.focus_handle.focus(window, cx);
        } else {
            tab.editor.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    fn toggle_markdown_source(
        &mut self,
        _: &ToggleMarkdownSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.active.map(|ix| &mut self.tabs[ix]) else {
            return;
        };
        if tab.markdown.is_none() {
            return;
        }
        tab.show_source = !tab.show_source;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Typing in a file's rendered Markdown switches it to the source, to
    /// edit it. What was typed isn't inserted: there's no cursor in the
    /// preview to say where.
    fn type_in_preview(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active.map(|ix| &self.tabs[ix]) else {
            return;
        };
        let Some(markdown) = tab.rendered() else {
            return;
        };
        let focused = self.focus_handle.is_focused(window) || markdown.read(cx).focus_handle().is_focused(window);
        let modifiers = &event.keystroke.modifiers;
        let typed = event.keystroke.key_char.as_ref().is_some_and(|text| !text.trim().is_empty());
        if focused && typed && tab.diff.is_none() && !tab.doc && !modifiers.platform && !modifiers.control && !modifiers.function {
            cx.stop_propagation();
            self.toggle_markdown_source(&ToggleMarkdownSource, window, cx);
        }
    }

    /// Tabs of renamed items follow the file.
    fn renamed(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for tab in &mut self.tabs {
            if let Ok(rest) = tab.path.strip_prefix(from) {
                tab.path = if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) };
            }
        }
        cx.notify();
    }

    /// Closes the tabs of whatever was moved to the Trash, except those with
    /// unsaved changes.
    fn trashed(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| tab.path.starts_with(path) && !tab.dirty)
        {
            self.close(ix, window, cx);
        }
        if self.tabs.iter().any(|tab| tab.path.starts_with(path)) {
            self.message = Some("A file with unsaved changes was moved to the Trash; its tab stays open".into());
        }
        cx.notify();
    }

    fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.tabs[ix].path.clone();
        let other_view = (0..self.tabs.len()).find(|other| *other != ix && self.tabs[*other].view && self.tabs[*other].path == path);
        let tab = &mut self.tabs[ix];
        if tab.dirty && !tab.confirm_close && other_view.is_none() {
            tab.confirm_close = true;
            self.message = Some("Unsaved changes: press Cmd-W again to close without saving".into());
            cx.notify();
            return;
        }
        // The file stays open in its other view, which takes over.
        if tab.is_file()
            && let Some(view) = other_view
        {
            let (saved, dirty, blame) = (tab.saved.clone(), tab.dirty, tab.blame.clone());
            let view = &mut self.tabs[view];
            view.view = false;
            view.saved = saved;
            view.dirty = dirty;
            view.blame = blame;
        }
        let was_active = self.active == Some(ix);
        let next = self.remove_tab(ix);
        self.message = None;
        match next {
            None => {
                self.active = None;
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
            Some(next) if was_active => self.activate(next, window, cx),
            Some(next) => {
                self.active = Some(next);
                self.group = self.tabs[next].group;
                cx.notify();
            }
        }
    }

    /// Closes all tabs except `keep` (those with unsaved changes stay, with a
    /// warning).
    fn close_others(&mut self, keep: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let keep = keep.map(|ix| self.tabs[ix].editor.clone());
        while let Some(ix) = self
            .tabs
            .iter()
            .position(|tab| Some(&tab.editor) != keep.as_ref() && !tab.dirty)
        {
            self.close(ix, window, cx);
        }
        if let Some(ix) = keep.and_then(|editor| self.tabs.iter().position(|tab| tab.editor == editor)) {
            self.activate(ix, window, cx);
        }
        if self.tabs.iter().any(|tab| tab.dirty) {
            self.message = Some("Tabs with unsaved changes remain open".into());
        }
        cx.notify();
    }

    /// A tab's index by its editor, which is unique (paths aren't: a file,
    /// its diff and its side preview share one).
    fn tab_index(&self, editor: &Entity<EditorState>) -> Option<usize> {
        self.tabs.iter().position(|tab| &tab.editor == editor)
    }

    /// Cmd-N: a new file in the folder of the file open, or the workspace's,
    /// named in the files panel.
    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self
            .active_file()
            .and_then(|ix| self.tabs[ix].path.parent())
            .filter(|dir| dir.starts_with(&self.root))
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone());
        self.show_panel(Panel::Files, cx);
        self.file_tree.update(cx, |tree, cx| tree.new_file_in(dir, window, cx));
    }

    fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.show_panel(Panel::Files, cx);
        self.file_tree.update(cx, |tree, cx| tree.reveal(path, cx));
    }

    /// The file of the active tab (which may be a view of it).
    fn active_file(&self) -> Option<usize> {
        let active = self.active?;
        if !self.tabs[active].view {
            return Some(active);
        }
        let path = &self.tabs[active].path;
        self.tabs.iter().position(|tab| &tab.path == path && tab.is_file())
    }

    /// Saves the active tab's file (from a view too), formatting it first if
    /// Settings say so for its type.
    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_file() else {
            return;
        };
        self.save_file(ix, window, cx);
    }

    fn auto_save_editor(&mut self, editor: &Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) {
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

    fn save_file(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
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

    fn format_document(&mut self, _: &FormatDocument, window: &mut Window, cx: &mut Context<Self>) {
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
    fn format_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Task<Result<(), SharedString>> {
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
    fn save_tab(&mut self, ix: usize, cx: &mut Context<Self>) -> Task<bool> {
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

    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminals.read(cx).contains_focus(window, cx) {
            self.terminals
                .update(cx, |terminals, cx| terminals.close_focused(window, cx));
            if self.terminals.read(cx).is_empty() {
                self.hide_panel(Panel::Terminals, cx);
                self.focus_ide(window, cx);
            }
            return;
        }
        if let Some(ix) = self.active {
            self.close(ix, window, cx);
        }
    }

    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(1, window, cx);
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(-1, window, cx);
    }

    /// To the next or previous tab of the focused group.
    fn step_tab(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.active else {
            return;
        };
        let group: Vec<usize> = (0..self.tabs.len()).filter(|ix| self.tabs[*ix].group == self.group).collect();
        let Some(at) = group.iter().position(|ix| *ix == active) else {
            return;
        };
        let next = group[(at as isize + delta).rem_euclid(group.len() as isize) as usize];
        self.activate(next, window, cx);
    }

    /// Split Editor Right/Down: the active tab's file also opens on the
    /// other side, as in VS Code (a Markdown file, with its preview there).
    /// With the split already there, it only changes direction (Move to Other
    /// Side moves tabs).
    fn split_editor(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor_split.is_some() {
            self.editor_split = Some(axis);
            return cx.notify();
        }
        let Some(ix) = self.active else {
            return;
        };
        if self.tabs[ix].diff.is_some() {
            self.message = Some("A diff can't be split; move it with Move to Other Side".into());
            return cx.notify();
        }
        self.editor_split = Some(axis);
        if self.tabs[ix].markdown.is_some() {
            return self.open_preview_to_side(&OpenPreviewToSide, window, cx);
        }
        let group = self.tabs[ix].group;
        let view = self.new_view(ix, 1 - group, true, window, cx);
        self.activate(view, window, cx);
    }

    /// Moves a tab to the other group (creating it side by side if there's
    /// no split); if its group is left empty, the split closes.
    fn move_to_other_group(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_split.get_or_insert(Axis::Row);
        let from = self.tabs[ix].group;
        let tab = self.tabs.remove(ix);
        let mut tab = tab;
        tab.group = 1 - from;
        self.tabs.push(tab);
        let ix = self.tabs.len() - 1;
        let editor = self.tabs[ix].editor.clone();
        // The group it left shows its most recent tab.
        self.active = None;
        self.normalize_groups();
        let ix = self.tab_index(&editor).unwrap_or(ix);
        self.activate(ix, window, cx);
    }

    /// Markdown: the rendered view in the other group, next to the source,
    /// updating as you type.
    fn open_preview_to_side(&mut self, _: &OpenPreviewToSide, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else {
            return;
        };
        let tab = &self.tabs[ix];
        if tab.markdown.is_none() || tab.diff.is_some() {
            return;
        }
        let (path, other) = (tab.path.clone(), 1 - tab.group);
        self.tabs[ix].show_source = true;
        self.editor_split.get_or_insert(Axis::Row);
        // A view already on the other side switches to the preview.
        match self.tabs.iter().position(|tab| tab.shows_file(&path) && tab.group == other) {
            Some(view) => {
                self.tabs[view].show_source = false;
                self.mark_shown(view);
            }
            None => {
                self.new_view(ix, other, false, window, cx);
            }
        }
        self.activate(ix, window, cx);
    }

    fn toggle_word_wrap(&mut self, _: &ToggleWordWrap, _: &mut Window, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.word_wrap = !config.word_wrap);
        crate::app_menu::set(cx);
        cx.notify();
    }

    /// Applies the config's word wrap to the tabs if it changed (in any task).
    fn apply_word_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wrap = Config::get(cx).word_wrap;
        if wrap == self.word_wrap {
            return;
        }
        self.word_wrap = wrap;
        // The sides of a diff don't wrap, to stay aligned.
        for tab in self.tabs.iter().filter(|tab| tab.old.is_none()) {
            tab.editor.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        }
    }


    /// What the Changes and History panels ask to open.
    fn on_git_event(&mut self, _: &Entity<ChangesPanel>, event: &ChangesEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ChangesEvent::OpenFile { file } => self.open(self.root.join(file), true, window, cx),
            ChangesEvent::OpenDiff { file, pin } => {
                let deleted = !self.root.join(file).exists();
                if *pin && !deleted {
                    self.open(self.root.join(file), true, window, cx);
                } else {
                    let of = DiffOf { file: file.clone(), commit: None, source: false };
                    self.open_diff(of, *pin, window, cx);
                }
            }
            ChangesEvent::OpenCommitDiff { commit, short, file, pin } => {
                self.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), false), *pin, window, cx);
            }
            ChangesEvent::OpenCommit { commit, short, pin } => {
                self.open_diff(DiffOf::commit(commit.clone(), short.clone(), String::new(), false), *pin, window, cx);
            }
            ChangesEvent::OpenFileAt { commit, short, file } => {
                self.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), true), true, window, cx);
            }
            ChangesEvent::ToggleCommitFiles => self.toggle_commit_files_now(cx),
        }
    }

    /// The panels that read git, each with its place in the layout.
    fn git_panels(&self) -> [(Panel, &Entity<ChangesPanel>); 2] {
        [(Panel::Changes, &self.changes), (Panel::History, &self.history)]
    }

    /// The History panel with the commits that changed `path`.
    fn show_history(&mut self, path: &Path, dir: bool, cx: &mut Context<Self>) {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return;
        };
        let file = relative.to_string_lossy().into_owned();
        self.show_panel(Panel::History, cx);
        self.history.update(cx, |history, cx| history.show_history(Some((file, dir)), cx));
        cx.notify();
    }

    fn render_tab_bar(&self, group: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shown = self.shown_in(group);
        let focused = self.editor_split.is_none() || group == self.group;
        let split = self.editor_split.is_some();
        h_flex()
            .id(("tab-bar", group))
            .h(px(34.))
            .flex_none()
            .overflow_x_scroll()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                this.drop_tab(drag, group, Some(this.tabs.len()), EditorDrop::Center, window, cx);
            }))
            .children(self.tabs.iter().enumerate().filter(|(_, tab)| tab.group == group).map(|(ix, tab)| {
                let active = shown == Some(ix);
                let name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = match &tab.diff {
                    Some(DiffOf { commit: Some((_, short)), file, .. }) if file.is_empty() => format!("Commit {short}"),
                    Some(DiffOf { commit: Some((_, short)), source: true, .. }) => format!("{name} @ {short}"),
                    Some(DiffOf { commit: Some((_, short)), .. }) => format!("{name} ({short})"),
                    Some(_) => format!("{name} (changes)"),
                    None if tab.view && tab.rendered().is_some() => format!("Preview {name}"),
                    None => name,
                };
                let dirty = self.is_dirty(ix);
                let close_icon = if dirty {
                    "icons/tab-dirty.svg"
                } else {
                    "icons/tab-close.svg"
                };
                h_flex()
                    .id(("tab", ix))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("editor-tab-{ix}")))
                    .group("tab")
                    .h_full()
                    .flex_none()
                    .gap_1()
                    .pl_3()
                    .pr_1()
                    .text_ui(cx)
                    .border_r_1()
                    .border_color(theme.border)
                    .when(active, |el| {
                        el.bg(theme.tab_active).text_color(if focused {
                            theme.tab_active_foreground
                        } else {
                            theme.tab_foreground
                        })
                    })
                    .when(!active, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
                    .when(tab.preview, |el| el.italic())
                    .on_drag(
                        TabDrag { editor: tab.editor.clone(), label: name.clone().into() },
                        {
                            let workspace = cx.entity().downgrade();
                            move |drag, _, window, cx| {
                                workspace.update(cx, |this, cx| {
                                    this.start_tab_drag(drag, window, cx);
                                }).ok();
                                cx.new(|_| TabDragPreview(drag.label.clone()))
                            }
                        },
                    )
                    .drag_over::<TabDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .on_drop(cx.listener({
                        let before = tab.editor.clone();
                        move |this, drag: &TabDrag, window, cx| {
                            let before = this.tab_index(&before);
                            this.drop_tab(drag, group, before, EditorDrop::Center, window, cx);
                        }
                    }))
                    .child(name)
                    .child(
                        div()
                            .id(("tab-close", ix))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .hover(|style| style.bg(theme.muted))
                            .child(
                                svg()
                                    .path(close_icon)
                                    .size(px(if dirty { 8. } else { 14. }))
                                    .text_color(theme.muted_foreground)
                                    .when(!dirty && !active, |el| {
                                        el.invisible().group_hover("tab", |s| s.visible())
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.tabs[ix].preview = false;
                        }
                        this.activate(ix, window, cx);
                    }))
                    .context_menu(self.tab_menu(tab, split, cx.entity().downgrade()))
            }))
            .child(
                div()
                    .id(("tab-drop-end", group))
                    .h_full()
                    .flex_1()
                    .min_w(px(24.))
                    .drag_over::<TabDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .context_menu({
                        let workspace = cx.entity().downgrade();
                        move |menu, _, _| {
                            menu.item(
                                menu::item("New File", &workspace, |this, window, cx| this.new_file(&NewFile, window, cx))
                                    .action(Box::new(NewFile)),
                            )
                            .item(
                                menu::item("Close All", &workspace, |this, window, cx| this.close_others(None, window, cx))
                                    .action(Box::new(CloseAllTabs)),
                            )
                        }
                    }),
            )
    }

    /// A tab's right-click menu, also on what it shows where that has
    /// none of its own (an image, a whole commit).
    fn tab_menu(
        &self,
        tab: &FileTab,
        split: bool,
        workspace: WeakEntity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let path = tab.path.clone();
        let editor = tab.editor.clone();
        let diff = tab.diff.is_some();
        let markdown = tab.markdown.is_some() && tab.is_file();
        let whole_commit = tab.diff.as_ref().is_some_and(|diff| diff.file.is_empty());
        let local = self.local;
        let relative = tab
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&tab.path)
            .to_string_lossy()
            .into_owned();
        move |menu, _, _| {
            let relative = relative.clone();
            let (close, others, moved, right, down, preview) =
                (editor.clone(), editor.clone(), editor.clone(), editor.clone(), editor.clone(), editor.clone());
            menu.item(
                menu::item("Close", &workspace, move |this, window, cx| {
                    if let Some(ix) = this.tab_index(&close) {
                        this.close(ix, window, cx);
                    }
                })
                .action(Box::new(CloseTab)),
            )
            .item(menu::item("Close Others", &workspace, move |this, window, cx| {
                this.close_others(this.tab_index(&others), window, cx)
            }))
            .item(
                menu::item("Close All", &workspace, |this, window, cx| this.close_others(None, window, cx))
                    .action(Box::new(CloseAllTabs)),
            )
            .separator()
            .when(!split, |menu| {
                menu.item(
                    menu::item("Split Right", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&right) {
                            this.activate(ix, window, cx);
                            this.split_editor(Axis::Row, window, cx);
                        }
                    })
                    .action(Box::new(SplitEditorRight)),
                )
                .item(
                    menu::item("Split Down", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&down) {
                            this.activate(ix, window, cx);
                            this.split_editor(Axis::Column, window, cx);
                        }
                    })
                    .action(Box::new(SplitEditorDown)),
                )
            })
            .when(split, |menu| {
                menu.item(menu::item("Move to Other Side", &workspace, move |this, window, cx| {
                    if let Some(ix) = this.tab_index(&moved) {
                        this.move_to_other_group(ix, window, cx);
                    }
                }))
            })
            .when(markdown, |menu| {
                menu.item(
                    menu::item("Open Preview to the Side", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&preview) {
                            this.activate(ix, window, cx);
                            this.open_preview_to_side(&OpenPreviewToSide, window, cx);
                        }
                    })
                    .action(Box::new(OpenPreviewToSide)),
                )
            })
            .when(diff && !whole_commit, |menu| {
                let path = path.clone();
                menu.item(menu::item("Open File", &workspace, move |this, window, cx| {
                    this.open(path.clone(), true, window, cx)
                }))
            })
            .when(!whole_commit, |menu| {
                let path = path.clone();
                menu.item(menu::item("Show File History", &workspace, move |this, _, cx| {
                    this.show_history(&path, false, cx)
                }))
            })
            .separator()
            .item(menu::item("Copy Path", &workspace, {
                let path = path.clone();
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()))
                }
            }))
            .item(menu::item("Copy Relative Path", &workspace, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()))
            }))
            .item(menu::item("Reveal in File Tree", &workspace, {
                let path = path.clone();
                move |this, _, cx| this.reveal_in_tree(&path, cx)
            }))
            .when(local, |menu| {
                let path = path.clone();
                menu.item(menu::item("Reveal in Finder", &workspace, move |_, _, cx| {
                    cx.reveal_path(&path)
                }))
            })
        }
    }

    /// A group's tab bar and the tab it shows. A click anywhere in it gives
    /// it the focus.
    fn render_group(&self, group: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let body = match self.shown_in(group).map(|ix| &self.tabs[ix]) {
            None => div()
                .id("code-empty")
                .size_full()
                .overflow_hidden()
                .p_6()
                .flex()
                .items_center()
                .justify_center()
                .context_menu({
                    let workspace = cx.entity().downgrade();
                    move |menu, _, _| {
                        menu.item(
                            menu::item("New File", &workspace, |this, window, cx| this.new_file(&NewFile, window, cx))
                                .action(Box::new(NewFile)),
                        )
                    }
                })
                .child(
                    svg()
                        .path("icons/den-empty.svg")
                        .size(px(360.))
                        .max_w_full()
                        .max_h_full()
                        .text_color(theme.muted_foreground.opacity(0.22)),
                )
                .into_any_element(),
            Some(tab) => match &tab.content {
                Content::Loading => div().size_full().into_any_element(),
                Content::Failed(err) => div()
                    .p_4()
                    .text_ui(cx)
                    .text_color(theme.danger)
                    .child(err.clone())
                    .into_any_element(),
                Content::Ready if let Some(image) = &tab.image => div()
                    .id("image-view")
                    .size_full()
                    .context_menu(self.tab_menu(tab, self.editor_split.is_some(), cx.entity().downgrade()))
                    .p_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(image.clone()).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                Content::Ready if let Some(commit) = &tab.commit => {
                    let tab_menu = self.tab_menu(tab, self.editor_split.is_some(), cx.entity().downgrade());
                    let hash = tab.diff.as_ref().and_then(|diff| diff.commit.as_ref()).map(|(hash, _)| hash.clone()).unwrap_or_default();
                    // As in the history's menus: Show Files or Hide Files.
                    let files = self.history.read(cx).files_in_menu(cx);
                    div()
                        .id("commit-view")
                        .size_full()
                        .context_menu(move |menu, window, cx| {
                            let hash = hash.clone();
                            let menu = menu
                                .item(menu::PopupMenuItem::new("Copy Hash").on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))
                                }))
                                .when_some(files, |menu, shown| {
                                    menu.menu(if shown { "Hide Files" } else { "Show Files" }, Box::new(ToggleCommitFiles))
                                })
                                .separator();
                            tab_menu(menu, window, cx)
                        })
                        .child(commit.clone())
                        .into_any_element()
                }
                Content::Ready => match tab.rendered() {
                    Some(markdown) => div()
                        .id("markdown-preview")
                        .size_full()
                        .context_menu({
                            let readonly = tab.diff.is_some() || tab.doc;
                            move |menu, _, _| menu.menu_with_disabled("Edit", Box::new(ToggleMarkdownSource), readonly)
                        })
                        .text_size(px(Config::get(cx).font_size(TextArea::Preview)))
                        .child(
                            TextView::new(markdown)
                                .resolve_image_source(markdown_images::resolver(
                                    self.client.clone(), self.root.clone(), tab.path.clone(),
                                ))
                                .on_link_click({
                                    let workspace = cx.entity().downgrade();
                                    let dir = tab.path.parent().map(Path::to_path_buf).unwrap_or_default();
                                    move |url, _, window, cx| {
                                        workspace
                                            .update(cx, |this, cx| this.follow_link(url, &dir, window, cx))
                                            .ok();
                                    }
                                })
                                .selectable(true)
                                .scrollable(true)
                                .size_full()
                                .px_8()
                                .py_6(),
                        )
                        .into_any_element(),
                    None => {
                        let readonly = tab.diff.is_some() || tab.doc;
                        let markdown = tab.markdown.is_some() && !readonly;
                        let file = self.tabs.iter().find(|file| file.path == tab.path && file.is_file());
                        // Stopped here, the ends of the lines show the debugger's values.
                        let execution = self.debugger.read(cx).execution();
                        let stopped_here = execution.as_ref().is_some_and(|(at, _, _)| *at == tab.path);
                        // The debugger stopped: run to a line of any file; set the next
                        // statement only in the function stopped at.
                        let stopped = execution.is_some() && !readonly;
                        let debugging = self.debugger.read(cx).is_active() && !readonly;
                        let jumpable = execution.as_ref().is_some_and(|(at, _, top)| *at == tab.path && *top);
                        let blame = file
                            .filter(|file| !file.dirty && tab.diff.is_none() && !stopped_here)
                            .and_then(|file| file.blame.clone());
                        let editor = Editor::new(&tab.editor)
                            .bordered(false)
                            .readonly(readonly)
                            .h_full()
                            // The right click already put the cursor where clicked. The menu
                            // is built while the editor is mid-update: it can't be
                            // read (GPUI aborts), so Cut and Copy are always
                            // enabled and do nothing without a selection.
                            .context_menu(move |menu, _, _| {
                                let menu = if markdown {
                                    menu.menu("Show Preview", Box::new(ToggleMarkdownSource)).separator()
                                } else {
                                    menu
                                };
                                let menu = if stopped {
                                    menu.menu("Run to Cursor", Box::new(RunToCursor))
                                        .menu_with_disabled("Set Next Statement", !jumpable, Box::new(SetNextStatement))
                                        .menu("Add to Watch", Box::new(AddToWatch))
                                        .menu("Evaluate in Console", Box::new(EvaluateInConsole))
                                        .separator()
                                } else {
                                    menu
                                };
                                let menu = menu
                                    .menu_with_disabled("Go to Definition", readonly, Box::new(GoToDefinition))
                                    .menu_with_disabled("Find References", readonly, Box::new(FindReferences))
                                    .menu_with_disabled("Format Document", readonly, Box::new(FormatDocument));
                                let menu = if !debugging {
                                    menu
                                } else {
                                    menu.separator()
                                        .menu("Toggle Breakpoint", Box::new(ToggleBreakpoint))
                                        .menu("Add Conditional Breakpoint…", Box::new(AddConditionalBreakpoint))
                                        .menu("Add Logpoint…", Box::new(AddLogpoint))
                                };
                                menu.separator()
                                    .menu_with_disabled("Cut", readonly, Box::new(input::Cut))
                                    .menu("Copy", Box::new(input::Copy))
                                    .menu_with_disabled("Paste", readonly, Box::new(input::Paste))
                                    .separator()
                                    .menu("Select All", Box::new(input::SelectAll))
                            });
                        let code = div()
                            .key_context("CodeEditor")
                            .size_full()
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" && this.signature.is_some() {
                                    this.close_signature();
                                    cx.notify();
                                }
                            }))
                            .on_action(cx.listener(Self::select_next_occurrence))
                            .on_action(cx.listener(|this, _: &MoveLineUp, window, cx| this.edit_lines(true, false, window, cx)))
                            .on_action(cx.listener(|this, _: &MoveLineDown, window, cx| this.edit_lines(false, false, window, cx)))
                            .on_action(cx.listener(|this, _: &DuplicateLineUp, window, cx| this.edit_lines(true, true, window, cx)))
                            .on_action(cx.listener(|this, _: &DuplicateLineDown, window, cx| this.edit_lines(false, true, window, cx)))
                            .child(editor)
                            .children(blame.and_then(|blame| inline_blame(&tab.editor, &blame, cx)))
                            .children(debug_inline_values(&tab.editor, &tab.path, &self.debugger, cx))
                            .children(self.test_lenses(&tab.editor, &tab.path, cx.entity().downgrade(), cx))
                            .children(breakpoint_edit_box(&tab.editor, &tab.path, &self.debugger, cx))
                            .children(
                                self.signature
                                    .as_ref()
                                    .filter(|hint| hint.editor == tab.editor)
                                    .and_then(|hint| signature::render(hint, cx)),
                            );
                        match &tab.old {
                            // No room for two sides: one column, VS Code's inline diff.
                            Some(old) if old.width.get() < px(SIDE_BY_SIDE_WIDTH) => div()
                                .size_full()
                                .relative()
                                .child(measure_width(&old.width))
                                .child(Editor::new(&old.inline).bordered(false).readonly(true).h_full())
                                .into_any_element(),
                            Some(old) => h_flex()
                                .size_full()
                                .relative()
                                .child(measure_width(&old.width))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .border_r_1()
                                        .border_color(cx.theme().border)
                                        .child(Editor::new(&old.editor).bordered(false).readonly(true).h_full()),
                                )
                                .child(div().flex_1().min_w_0().h_full().child(code))
                                .into_any_element(),
                            None => code.into_any_element(),
                        }
                    }
                },
            },
        };
        v_flex()
            .id(("editor-group", group))
            .size_full()
            .bg(theme.background)
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.group != group
                    && let Some(ix) = this.shown_in(group)
                {
                    this.activate_with(ix, false, window, cx);
                }
            }))
            .when(!self.tabs.is_empty(), |el| el.child(self.render_tab_bar(group, cx)))
            .child(
                div()
                    .id(("editor-drop-area", group))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("editor-body-{group}")))
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| {
                        this.track_tab_drop(group, event, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                        let placement = this.editor_drop.filter(|(target, _)| *target == group)
                            .map_or(EditorDrop::Center, |(_, placement)| placement);
                        this.drop_tab(drag, group, None, placement, window, cx);
                    }))
                    .child(body)
                    .when_some(self.editor_drop.filter(|(target, _)| *target == group && cx.has_active_drag()), |el, (_, placement)| {
                        el.child(placement.indicator(cx))
                    }),
            )
            .into_any_element()
    }

    /// The code area: one group, or two split side by side or one above the
    /// other.
    fn render_editor_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = match self.editor_split {
            None => self.render_group(0, cx),
            Some(axis) => {
                let (first, second) = (self.render_group(0, cx), self.render_group(1, cx));
                let split = match axis {
                    Axis::Row => h_resizable("editor-groups-row"),
                    Axis::Column => v_resizable("editor-groups-column"),
                };
                split
                    .child(resizable_panel().child(first))
                    .child(resizable_panel().child(second))
                    .into_any_element()
            }
        };
        v_flex().size_full().bg(cx.theme().background).child(div().flex_1().min_h_0().child(groups))
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut left = h_flex().gap_3().min_w_0().overflow_hidden();
        let mut right = h_flex().gap_3().flex_none().whitespace_nowrap();
        let branch = self.branch.clone().or_else(|| self.changes.read(cx).branch().map(str::to_string));
        if let Some(branch) = branch {
            left = left.child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(svg().path("icons/git-branch.svg").size(px(12.)).text_color(theme.muted_foreground))
                    .child(branch),
            );
        }
        if let Some(tab) = self.active.map(|ix| &self.tabs[ix]) {
            let relative = tab.path.strip_prefix(&self.root).unwrap_or(&tab.path);
            left = left.child(relative.display().to_string());
            let state = tab.editor.read(cx);
            if tab.image.is_some() {
                right = right.child("Image");
            } else {
                if tab.rendered().is_none() {
                    let pos = state.cursor_position();
                    right = right.child(format!("Ln {}, Col {}", pos.line + 1, pos.character + 1));
                }
                right = right.child(state.language_name());
            }
            if matches!(tab.content, Content::Ready) && tab.image.is_none() && tab.diff.is_none() {
                let problem = if !self.client.as_ref().is_some_and(|client| client.is_connected()) {
                    Some(SharedString::from("The agent is disconnected. Language features are unavailable."))
                } else {
                    tab.lsp_status.problem.clone()
                };
                if let Some(problem) = problem {
                    right = right.child(
                        div()
                            .id("lsp-status")
                            .text_color(theme.warning)
                            .child("LSP unavailable")
                            .tooltip(move |window, cx| Tooltip::new(problem.clone()).max_w(px(480.)).build(window, cx)),
                    );
                }
            }
            if tab.markdown.is_some() {
                let label = if tab.show_source { "Show Preview" } else { "Show Source" };
                right = right.child(
                    div()
                        .id("toggle-markdown")
                        .text_color(theme.link)
                        .hover(|style| style.underline())
                        .child(label)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_markdown_source(&ToggleMarkdownSource, window, cx)
                        })),
                );
            }
        }
        if let Some(message) = &self.message {
            left = left.child(div().text_color(theme.warning).child(message.clone()));
        }
        for info in &self.ports {
            let port = info.port;
            right = right.child(
                div()
                    .id(("port", port as usize))
                    .text_color(theme.link)
                    .hover(|style| style.underline())
                    .child(format!("{} :{port}", info.process))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_port(port, cx))),
            );
        }
        h_flex()
            .h(px(24.))
            .flex_none()
            .px_3()
            .justify_between()
            .text_ui_small(cx)
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_color(theme.muted_foreground)
            .child(left)
            .child(right.child("local"))
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A file's image format, by its extension.
fn image_format(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "svg" => ImageFormat::Svg,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

fn decode_text(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err("Binary file: not shown.".into());
    }
    String::from_utf8(bytes).map_err(|_| "The file isn't UTF-8: not shown.".into())
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_word_wrap(window, cx);
        if self.is_shown(Panel::Outline, cx) {
            self.sync_outline(cx);
        }
        if !cx.has_active_drag() {
            self.editor_drop = None;
        }
        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.stop_active_drag(window) {
                    this.editor_drop = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(Self::type_in_preview))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(|this, _: &CloseAllTabs, window, cx| this.close_others(None, window, cx)))
            .on_action(cx.listener(|this, _: &CollapseFileTree, _, cx| {
                this.file_tree.update(cx, |tree, cx| tree.collapse_all(cx))
            }))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::toggle_side_panel))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.toggle_panel(Panel::Files, cx)))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.toggle_panel(Panel::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, _, cx| this.toggle_panel(Panel::History, cx)))
            .on_action(cx.listener(Self::toggle_commit_files))
            .on_action(cx.listener(Self::show_search))
            .on_action(cx.listener(Self::open_file_finder))
            // F4 steps through the visible panel's results: References or Search.
            .on_action(cx.listener(|this, _: &NextResult, _, cx| this.step_result(1, cx)))
            .on_action(cx.listener(|this, _: &PrevResult, _, cx| this.step_result(-1, cx)))
            .on_action(cx.listener(Self::go_to_definition))
            .on_action(cx.listener(Self::go_to_line))
            .on_action(cx.listener(Self::go_to_symbol))
            .on_action(cx.listener(Self::go_to_workspace_symbol))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| this.navigate(false, window, cx)))
            .on_action(cx.listener(Self::find_references))
            .on_action(
                cx.listener(|this, _: &ShowReferences, _, cx| this.toggle_panel(Panel::References, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowOutline, _, cx| this.toggle_panel(Panel::Outline, cx)))
            .on_action(cx.listener(Self::toggle_markdown_source))
            .on_action(cx.listener(Self::open_preview_to_side))
            .on_action(cx.listener(Self::toggle_word_wrap))
            .on_action(cx.listener(Self::format_document))
            .on_action(cx.listener(|this, _: &SplitEditorRight, window, cx| this.split_editor(Axis::Row, window, cx)))
            .on_action(cx.listener(|this, _: &SplitEditorDown, window, cx| this.split_editor(Axis::Column, window, cx)))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| this.split(Axis::Row, window, cx)))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| this.split(Axis::Column, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| this.focus_pane(Direction::Left, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| this.focus_pane(Direction::Right, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| this.focus_pane(Direction::Up, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| this.focus_pane(Direction::Down, window, cx)))
            .on_action(cx.listener(Self::toggle_terminals))
            .on_action(cx.listener(Self::maximize_terminals))
            .on_action(cx.listener(|this, _: &MoveTerminals, _, cx| this.move_terminals(cx)))
            .on_action(cx.listener(Self::toggle_breakpoint))
            .on_action(cx.listener(Self::run_to_cursor))
            .on_action(cx.listener(|this, _: &AddConditionalBreakpoint, window, cx| {
                this.edit_breakpoint_at_cursor(EditKind::Condition, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddLogpoint, window, cx| this.edit_breakpoint_at_cursor(EditKind::Log, window, cx)))
            .on_action(cx.listener(Self::add_to_watch))
            .on_action(cx.listener(Self::evaluate_in_console))
            .on_action(cx.listener(Self::set_next_statement))
            .on_action(cx.listener(Self::toggle_debug_panel))
            .on_action(cx.listener(Self::toggle_notes))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(|this, _: &DebugContinue, window, cx| {
                this.debugger.update(cx, |debugger, cx| debugger.start_or_continue(window, cx))
            }))
            .on_action(cx.listener(|this, _: &DebugStop, _, cx| this.debugger.update(cx, |debugger, cx| debugger.stop(cx))))
            .on_action(cx.listener(|this, _: &DebugRestart, window, cx| {
                this.debugger.update(cx, |debugger, cx| debugger.restart(window, cx))
            }))
            .on_action(cx.listener(|this, _: &DebugPause, _, cx| this.debugger.update(cx, |debugger, cx| debugger.pause(cx))))
            .on_action(cx.listener(|this, _: &StepOver, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_over(cx))))
            .on_action(cx.listener(|this, _: &StepInto, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_in(cx))))
            .on_action(cx.listener(|this, _: &StepOut, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_out(cx))))
            .relative()
            .size_full()
            .font_family(cx.theme().font_family.clone())
            .text_ui(cx)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_activity_bar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // A right-click forgets the panel the last one was in; the
                            // side panels and the device, inside, set theirs after.
                            .capture_any_mouse_down(|event: &MouseDownEvent, _, cx| {
                                if event.button == MouseButton::Right {
                                    menu::set_panel_under(None, None, cx);
                                }
                            })
                            .child(self.render_layout(window, cx)),
                    ),
            )
            // Across the whole window, as VS Code's.
            .child(self.render_status_bar(cx))
            .children(
                self.finder
                    .as_ref()
                    .map(|(finder, _)| finder.clone().into_any_element())
                    .or_else(|| self.symbols.as_ref().map(|search| search.picker.clone().into_any_element()))
                    .map(|picker| div().absolute().top(px(44.)).left_0().right_0().flex().justify_center().child(picker)),
            )
            .child(self.debug_hover.clone())
            .children(self.render_notes(cx))
    }
}

impl Workspace {
    /// Run and Debug at the end of each test's line, as the launch file's
    /// `tests` finds them.
    fn test_lenses(&self, editor: &Entity<EditorState>, path: &Path, this: WeakEntity<Self>, cx: &App) -> Vec<AnyElement> {
        let Some(tests) = self.debugger.read(cx).tests.clone() else {
            return Vec::new();
        };
        let state = editor.read(cx);
        let Some(visible) = state.visible_row_range() else {
            return Vec::new();
        };
        let text = state.text();
        let area = state.input_bounds();
        let theme = cx.theme();
        let debugger = self.debugger.read(cx);
        let mut lenses = Vec::new();
        for row in visible.start..visible.end.min(text.lines_len()) {
            let line = text.slice_line(row).to_string();
            let Some(test) = tests.name_in(&line) else {
                continue;
            };
            let end = text.line_start_offset(row) + line.trim_end_matches(['\n', '\r']).len();
            let Some(bounds) = state.range_to_bounds(&(end..end)) else {
                continue;
            };
            let origin = point(bounds.origin.x + px(24.), bounds.origin.y);
            if bounds.origin.y < area.top() || bounds.bottom() > area.bottom() || area.right() - origin.x < px(120.) {
                continue;
            }
            // Debugging it: the bug turns until its program connects.
            let launching = debugger.is_launching(path, &test);
            let lens = |icon: &'static str, tip: &'static str, debug: bool| {
                let test = test.clone();
                let path = path.to_path_buf();
                let this = this.clone();
                div()
                    .id(SharedString::from(format!("test-{debug}-{row}")))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("test-lens-{row}-{debug}")))
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .map(|el| {
                        if debug && launching {
                            el.child(crate::app::spinner(theme.muted_foreground))
                        } else {
                            el.child(svg().path(icon).size(px(13.)).text_color(theme.muted_foreground))
                        }
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| this.run_test(&path, &test, debug, window, cx)).ok();
                    })
            };
            lenses.push(
                anchored()
                    .position(origin)
                    .child(
                        h_flex()
                            .h(bounds.size.height)
                            .gap_1()
                            .items_center()
                            .occlude()
                            .child(lens("icons/play.svg", "Run Test", false))
                            .child(lens("icons/bug.svg", "Debug Test", true)),
                    )
                    .into_any_element(),
            );
        }
        lenses
    }

    /// Runs the test `test` of `path` in a terminal, or debugs it.
    fn run_test(&mut self, path: &Path, test: &str, debug: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tests) = self.debugger.read(cx).tests.clone() else {
            return;
        };
        let file = path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let command = tests.command(debug, test);
        let line = debug::command_line(&command, Some(&file));
        if debug {
            self.debugger.update(cx, |debugger, cx| {
                debugger.launch_command(line, tests.port, path.to_path_buf(), test.to_string(), window, cx)
            });
            return;
        }
        self.show_panel(Panel::Terminals, cx);
        let run = self.terminals.update(cx, |terminals, cx| terminals.run_line(self.test_term, line, window, cx));
        cx.spawn(async move |this, cx| {
            let term = run.await;
            this.update(cx, |this, _| this.test_term = term).ok();
        })
        .detach();
        cx.notify();
    }
}

/// The values of the variables of the frame stopped at, at the end of the
/// lines of its function above where it stopped, like Visual Studio's
/// inline values; and the exception, at the line that raised it.
fn debug_inline_values(editor: &Entity<EditorState>, path: &Path, debugger: &Entity<Debugger>, cx: &App) -> Vec<AnyElement> {
    let debugger = debugger.read(cx);
    let Some((at, stop_line, _)) = debugger.execution() else {
        return Vec::new();
    };
    if at != path {
        return Vec::new();
    }
    let locals = debugger.frame_locals();
    let state = editor.read(cx);
    let Some(visible) = state.visible_row_range() else {
        return Vec::new();
    };
    let text = state.text();
    let stop_line = stop_line as usize;
    if stop_line >= text.lines_len() {
        return Vec::new();
    }

    // the function: up from the line stopped at to its declaration
    let mut first = stop_line;
    while first > 0 && stop_line - first < 200 {
        let line = text.slice_line(first).to_string();
        if debug::starts_function(&line) {
            break;
        }
        first -= 1;
    }

    let theme = cx.theme();
    let area = state.input_bounds();
    let mut labels = Vec::new();
    for row in first.max(visible.start)..=stop_line.min(visible.end.saturating_sub(1)) {
        let line = text.slice_line(row).to_string();
        let mut parts = Vec::new();
        for name in debug::names_in(&line) {
            if let Some(var) = locals.iter().find(|var| var.name == name) {
                let mut value = var.value.clone();
                if value.chars().count() > 60 {
                    value = value.chars().take(60).collect::<String>() + "…";
                }
                parts.push(format!("{name} = {value}"));
            }
        }
        let exception = (row == stop_line).then(|| debugger.exception()).flatten();
        if parts.is_empty() && exception.is_none() {
            continue;
        }
        let end = text.line_start_offset(row) + line.trim_end_matches(['\n', '\r']).len();
        let Some(bounds) = state.range_to_bounds(&(end..end)) else {
            continue;
        };
        let origin = point(bounds.origin.x + px(24.), bounds.origin.y);
        let width = area.right() - origin.x;
        if bounds.origin.y < area.top() || bounds.bottom() > area.bottom() || width < px(40.) {
            continue;
        }
        let (text, color) = match exception {
            Some(message) => (format!("⚠ {message}"), theme.danger),
            None => (parts.join("   "), theme.info.opacity(0.85)),
        };
        labels.push(
            anchored()
                .position(origin)
                .child(
                    div()
                        .h(bounds.size.height)
                        .max_w(width)
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_color(color)
                        .font_family(theme.mono_font_family.clone())
                        .child(text),
                )
                .into_any_element(),
        );
    }
    labels
}

/// The editor of a breakpoint's condition, under its line.
fn breakpoint_edit_box(editor: &Entity<EditorState>, path: &Path, debugger: &Entity<Debugger>, cx: &App) -> Option<AnyElement> {
    let edit = debugger.read(cx).edit.as_ref()?;
    if edit.path != path {
        return None;
    }
    let state = editor.read(cx);
    let text = state.text();
    let line = edit.line as usize;
    if line >= text.lines_len() {
        return None;
    }
    let start = text.line_start_offset(line);
    let bounds = state.range_to_bounds(&(start..start))?;
    let origin = point(state.input_bounds().left() + px(8.), bounds.bottom() + px(2.));
    let body = debug::panel::breakpoint_editor(debugger, cx)?;
    Some(anchored().position(origin).child(body).into_any_element())
}

/// At the end of the cursor's line, in gray: the commit that last changed it.
/// Positioned from the editor's last layout; nothing if the line isn't visible.
fn inline_blame(editor: &Entity<EditorState>, blame: &Blame, cx: &App) -> Option<AnyElement> {
    let state = editor.read(cx);
    if state.selections().len() != 1 {
        return None;
    }
    let line = state.cursor_position().line as usize;
    let commit = &blame.commits[(*blame.lines.get(line)?)? as usize];
    let text = state.text();
    let end = text.line_start_offset(line) + text.slice_line(line).to_string().trim_end_matches('\r').len();
    let at = state.range_to_bounds(&(end..end))?;
    let area = state.input_bounds();
    let origin = point(at.origin.x + px(48.), at.origin.y);
    let width = area.right() - origin.x;
    if at.origin.y < area.top() || at.bottom() > area.bottom() || width < px(40.) {
        return None;
    }
    let theme = cx.theme();
    let label = div()
        .h(at.size.height)
        .max_w(width)
        .flex()
        .items_center()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .text_color(theme.muted_foreground.opacity(0.8))
        .font_family(theme.mono_font_family.clone())
        .child(format!("{}, {} ({})", commit.subject, commit.author, changes::ago(commit.time)));
    // In window coordinates, like the editor's layout.
    Some(anchored().position(origin).child(label).into_any_element())
}

/// The symbols in a `Symbols` response, or why there are none: `no_server`
/// if no language server answered.
fn symbols_of(response: anyhow::Result<Response>, no_server: &'static str) -> Result<Vec<proto::LspSymbol>, SharedString> {
    match response {
        Ok(Response::Symbols { server: None, .. }) => Err(no_server.into()),
        Ok(Response::Symbols { symbols, .. }) => Ok(symbols),
        Ok(other) => Err(format!("Unexpected response: {other:?}").into()),
        Err(err) => Err(format!("{err:#}").into()),
    }
}

/// After a jump: if `line` wasn't visible, scrolls to center it, like VS Code
/// (the editor alone only brings it in at the edge). With `always`, it centers
/// even if it's visible: a freshly loaded file already scrolled the cursor in
/// at the edge on its own, so being visible there doesn't mean it was. If not
/// laid out yet (a freshly opened tab takes a few frames), it's tried again
/// on the next ones, `retries` times at most.
fn reveal_centered(editor: &Entity<EditorState>, line: u32, always: bool, retries: u8, window: &mut Window, cx: &mut App) {
    let state = editor.read(cx);
    let (Some(visible), Some(line_height)) = (state.visible_row_range(), state.line_height()) else {
        if retries > 0 {
            let editor = editor.clone();
            window.on_next_frame(move |window, cx| reveal_centered(&editor, line, true, retries - 1, window, cx));
        }
        return;
    };
    let line = line as usize;
    // Lines at the edge may be half visible: they count as outside.
    if !always && visible.len() > 2 && line > visible.start && line + 1 < visible.end {
        return;
    }
    let rows = visible.len().max(1) as f32;
    let top = (line as f32 - (rows - 1.) / 2.).max(0.);
    let x = state.scroll_offset().x;
    editor.update(cx, |state, cx| state.set_scroll_offset(point(x, -(line_height * top)), cx));
}

/// Keeps the other side of a diff at the same height.
fn follow_scroll(from: &Entity<EditorState>, to: &Entity<EditorState>, cx: &mut App) {
    let y = from.read(cx).target_scroll_offset().y;
    let offset = to.read(cx).target_scroll_offset();
    if offset.y != y {
        to.update(cx, |state, cx| state.set_scroll_offset(point(offset.x, y), cx));
    }
}

/// Keeps in `width` how wide the diff is, and redraws when that changes
/// whether its sides fit.
pub(crate) fn measure_width(width: &Rc<Cell<Pixels>>) -> impl IntoElement {
    let width = width.clone();
    canvas(
        move |bounds, window, _| {
            let fits = |width: Pixels| width >= px(SIDE_BY_SIDE_WIDTH);
            if fits(width.replace(bounds.size.width)) != fits(bounds.size.width) {
                window.refresh();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

/// How a diff in one column shows each line: both numbers, removed lines in
/// `removed` and with `−`, added ones in `added` and with `+`.
fn inline_line_styles(inline: &diff::Inline, removed: Hsla, added: Hsla) -> Vec<LineStyle> {
    let digits = inline.lines.iter().filter_map(|line| line.old.max(line.new)).max().unwrap_or(1).to_string().len();
    let number = |number: Option<u32>| number.map_or_else(|| " ".repeat(digits), |number| format!("{number:>digits$}"));
    inline
        .lines
        .iter()
        .map(|line| {
            let (background, marker) = match (line.old, line.new) {
                (Some(_), None) => (Some(removed), '−'),
                (None, Some(_)) => (Some(added), '+'),
                _ => (None, ' '),
            };
            LineStyle { background, hatched: false, number: Some(format!("{} {}{marker}", number(line.old), number(line.new)).into()) }
        })
        .collect()
}

/// How a side of a diff shows each line: changed ones in `color` and with
/// `marker` after the number, gaps hatched.
fn line_styles(side: &diff::Side, marker: char, color: Hsla) -> Vec<LineStyle> {
    side.lines
        .iter()
        .map(|line| {
            let changed = line.kind == diff::Kind::Changed;
            LineStyle {
                background: changed.then_some(color),
                hatched: line.kind == diff::Kind::Gap,
                number: line.number.map(|number| format!("{number}{}", if changed { marker } else { ' ' }).into()),
            }
        })
        .collect()
}

/// The name (letters, digits and `_`) at `column` of `line`, or just before
/// it: a cursor at the end of a word counts too.
fn word_at(text: &str, line: u32, column: u32) -> String {
    let Some(line) = text.lines().nth(line as usize) else {
        return String::new();
    };
    let chars: Vec<char> = line.chars().collect();
    let is_word = |ch: &char| ch.is_alphanumeric() || *ch == '_';
    let column = (column as usize).min(chars.len());
    let at = if chars.get(column).is_some_and(is_word) {
        column
    } else if column > 0 && is_word(&chars[column - 1]) {
        column - 1
    } else {
        return String::new();
    };
    let start = chars[..at].iter().rposition(|ch| !is_word(ch)).map_or(0, |ix| ix + 1);
    let end = chars[at..].iter().position(|ch| !is_word(ch)).map_or(chars.len(), |ix| at + ix);
    chars[start..end].iter().collect()
}

/// `%20` and friends back to their characters; invalid sequences stay as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| text.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolves `.` and `..` without touching the disk (the file may be on a server).
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{LspStatus, normalize, percent_decode, word_at};
    use proto::Response;

    #[test]
    fn lsp_failure_persists_until_a_server_responds() {
        let mut status = LspStatus::default();
        let failure = anyhow::anyhow!("env: node: No such file or directory").context("typescript did not start");
        status.observe(&Err(failure));
        assert_eq!(status.problem.as_deref(), Some("typescript did not start: env: node: No such file or directory"));

        // Ambiguous replies mustn't clear this failure.
        status.observe(&Ok(Response::Signature(None)));
        status.observe(&Ok(Response::Resolved { detail: None, documentation: None }));
        assert!(status.problem.is_some());

        // A working server can legitimately return no suggestions.
        status.observe(&Ok(Response::Completions {
            server: Some("typescript".into()), list: 1, items: Vec::new(), incomplete: false,
        }));
        assert!(status.problem.is_none());
    }

    #[test]
    fn lsp_missing_server_differs_from_no_definition() {
        let mut status = LspStatus::default();
        status.observe(&Ok(Response::Lsp { server: None, locations: Vec::new() }));
        assert!(status.problem.is_some());
        status.observe(&Ok(Response::Lsp { server: Some("typescript".into()), locations: Vec::new() }));
        assert!(status.problem.is_none());
        status.observe(&Ok(Response::Completions {
            server: None, list: 0, items: Vec::new(), incomplete: false,
        }));
        assert!(status.problem.is_some());
    }

    #[test]
    fn word_under_cursor() {
        let text = "fn main() {\n    let año = twice(1);\n}";
        assert_eq!(word_at(text, 1, 9), "año");
        assert_eq!(word_at(text, 1, 11), "año");
        // After a space there's no word; after `twice`, just before the `(`, there is.
        assert_eq!(word_at(text, 1, 12), "");
        assert_eq!(word_at(text, 1, 16), "twice");
        assert_eq!(word_at(text, 1, 19), "twice");
        assert_eq!(word_at(text, 1, 22), "");
        assert_eq!(word_at(text, 9, 0), "");
    }

    #[test]
    fn resolves_relative_links() {
        assert_eq!(percent_decode("my%20notes.md"), "my notes.md");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(
            normalize(&Path::new("/repo/docs").join("../README.md")),
            PathBuf::from("/repo/README.md")
        );
        assert_eq!(normalize(&Path::new("/repo/docs").join("./a/b.md")), PathBuf::from("/repo/docs/a/b.md"));
    }
}
