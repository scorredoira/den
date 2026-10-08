use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use client::Client;
use gpui_base::input::{ExecutionLine, GutterMark, LineStyle, ScrollbarMark, TabSize};
use proto::{CommitInfo, GitOp, LspLocation, LspOp, PortInfo, Request, Response, SearchHit};

use gpui_kit::component::{
    ActiveTheme as _, h_flex, h_resizable, v_resizable,
    input::{self, Editor, EditorState, InputEvent, Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _},
    menu::{ContextMenuExt as _, PopupMenu},
    native_menu::NativeMenu,
    resizable_panel,
    text::{TextView, TextViewState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    CloseAllTabs, CloseTab, CollapseFileTree, RefreshFiles, MaximizeTerminals, MoveTerminals, NewTerminal, NextTab, PrevTab, Save, ShowChanges, ShowFiles, ShowHistory,
    OpenChanges, ShowFileHistory,
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, ShowOutline, ShowReferences, ShowSearch,
    SplitDown, SplitRight, ToggleMarkdownSource, ToggleSidePanel,
    ToggleTerminals, OpenFileFinder, NewFile, NextResult, PrevResult, GoToDefinition, FindReferences, NavigateBack, NavigateForward,
    GoToLine, GoToSymbol, GoToWorkspaceSymbol, OpenPreviewToSide, SplitEditorDown, SplitEditorRight, ToggleWordWrap, FormatDocument,
    DiffLayoutAutomatic, DiffLayoutOneColumn, DiffLayoutSideBySide, OpenDiffFile, ToggleWholeFile,
    changes::{self, ChangesEvent, ChangesPanel},
    history::{HistoryEvent, HistoryView},
    commit_view::{self, CommitView, CommitViewEvent},
    definition,
    completion::Completions,
    editing::{self, DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, SelectNextOccurrence},
    config::{self, Config, DiffLayout, Panel, SavedTab, Session, TextArea, UiText},
    debug::{self, DebugEvent, DebugView, Debugger, EditKind},
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
mod pages;
mod debugging;
mod diffs;
mod files;
mod navigation;
mod render;
mod tabs;
#[cfg(test)]
pub(crate) mod autosave_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod new_file_tests;
#[cfg(test)]
mod tab_menu_tests;
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
    /// A view of its own instead of a file (it's a `doc` too).
    page: Option<pages::Page>,
    /// Reopened on returning to the task: if the file is gone, it closes itself.
    restored: bool,
    /// Who last changed each line, as it was on disk when read or saved.
    blame: Option<Arc<Blame>>,
    /// Highlight of the occurrences of the word under the cursor, and the
    /// selections it was computed for.
    occurrences: Option<RangeDecorationCollection>,
    occurrences_for: Vec<editing::Selection>,
    /// Looking for them, in the background: a big file doesn't slow typing.
    occurrences_task: Option<Task<()>>,
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
    /// The preview it took the place of, still drawn while this one loads:
    /// the area doesn't go blank between one file and the next.
    stand_in: Option<Box<FileTab>>,
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
    /// Whether the old side, the new one and the column have text selected,
    /// for their menus' Copy (see `diff_menu`).
    selected: [Rc<Cell<bool>>; 3],
    /// The diff with the whole file, and whether it shows so: by default
    /// only the changes, with what's far from them folded.
    sides: diff::SideBySide,
    whole: bool,
    _subscriptions: Vec<Subscription>,
}

/// Shows every diff as `layout` from now on, those open too.
pub(crate) fn set_diff_layout(layout: DiffLayout, cx: &mut App) {
    Config::update(cx, |config| config.diff_layout = layout);
    cx.refresh_windows();
}

/// How diffs show, checked as chosen: their menus start with this.
fn diff_layouts(cx: &App) -> [(&'static str, bool, Box<dyn Action>); 3] {
    let layout = Config::get(cx).diff_layout;
    [
        ("Automatic", layout == DiffLayout::Automatic, Box::new(DiffLayoutAutomatic)),
        ("Side by Side", layout == DiffLayout::SideBySide, Box::new(DiffLayoutSideBySide)),
        ("One Column", layout == DiffLayout::OneColumn, Box::new(DiffLayoutOneColumn)),
    ]
}

/// The right-click menu of a diff's text: how diffs show (`layouts`, if
/// it has two sides, and `whole`, the file around the changes), Open File
/// (`open`) and what reads it; nothing that edits. The menu is built while the editor is mid-update and can't read
/// it: whether it has text selected comes in `selected`, or Copy is always
/// enabled.
fn diff_menu(mut menu: NativeMenu, layouts: Option<bool>, open: bool, selected: Option<&Cell<bool>>, cx: &App) -> NativeMenu {
    if let Some(whole) = layouts {
        for (label, checked, action) in diff_layouts(cx) {
            menu = menu.menu_with_check(label, checked, action);
        }
        menu = menu.separator().menu_with_check("Show Whole File", whole, Box::new(ToggleWholeFile)).separator();
    }
    if open {
        menu = menu.menu("Open File", Box::new(OpenDiffFile)).separator();
    }
    menu.menu_with_disabled("Copy", selected.is_some_and(|selected| !selected.get()), Box::new(input::Copy))
        .menu("Select All", Box::new(input::SelectAll))
}

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

/// How faint the logo of an empty editor (and of the welcome) is.
pub(crate) const EMPTY_LOGO_OPACITY: f32 = 0.3;

/// What an empty editor offers below the logo, as VS Code does: a few
/// shortcuts, with the keys they have now.
fn empty_hints(cx: &App) -> impl IntoElement {
    const HINTS: &[(&str, &str)] = &[
        ("OpenCommandPalette", "Show All Commands"),
        ("OpenFileFinder", "Go to File"),
        ("ShowSearch", "Find in Files"),
        ("NewFile", "New File"),
        ("NewTerminal", "New Terminal"),
    ];
    let theme = cx.theme();
    v_flex().w(px(320.)).max_w_full().gap_2().text_ui(cx).text_color(theme.muted_foreground).children(
        HINTS.iter().filter_map(|(id, label)| {
            let shortcut = crate::shortcuts::SHORTCUTS.iter().find(|shortcut| shortcut.id == *id)?;
            let keys = crate::shortcuts::keys(shortcut, cx)?;
            Some(h_flex().justify_between().gap_4().child(*label).child(component::kbd::Kbd::new(keys)))
        }),
    )
}

#[derive(Clone, PartialEq, Eq, Hash)]
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
    /// How wide each editor group's body is, measured every frame: a new
    /// diff is laid out at that width from its first frame, not corrected
    /// on the next one.
    group_widths: [Rc<Cell<Pixels>>; 2],
    /// What was read of commits (they don't change): going back to one, or
    /// to one read ahead, shows it at once.
    commit_texts: HashMap<DiffOf, String>,
    /// The commit HEAD was at when last looked: the blames are of it.
    head: Option<String>,
    /// What of it shows (see `layout::Panels`).
    panels: Panels,
    /// A debug session runs (connecting or connected): the debugging layout
    /// and panels are in use.
    debugging: bool,
    /// What the panels are shown for: editing, debugging or the history.
    mode: layout::Mode,
    /// The panels of the modes not in use, as they left them.
    kept_shown: HashMap<layout::Mode, layout::Shown>,
    /// The task's key in `config.json`, to remember what was open.
    session_key: String,
    /// Last session's tabs were already reopened (nothing is saved before that).
    restored: bool,
    focus_handle: FocusHandle,
    /// The app's workspaces panel.
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
    /// The file and selections Claude Code in this workspace's terminals
    /// was last told of, and the telling, a moment after they change (see
    /// `report_ide_selection`).
    ide_reported: Option<(PathBuf, Vec<(usize, usize)>)>,
    ide_report: Option<Task<()>>,
    /// The agent reports the root's changes while this lives.
    fs_watch: Option<client::Watch>,
    /// On this machine (not on a server).
    local: bool,
    changes: Entity<ChangesPanel>,
    history: Entity<HistoryView>,
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
    /// The list was asked for once already: the first Cmd-P finds it ready.
    files_asked: bool,
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
    /// The indentation applied to files that don't show their own (see
    /// `apply_tab`): its size and whether it's tabs.
    tab: (usize, bool),
    message: Option<SharedString>,
    /// On a server, ports the task's terminals are listening on.
    ports: Vec<PortInfo>,
    /// The signature of the call being typed, and where it was last asked for.
    signature: Option<SignatureHint>,
    signature_at: Option<Position>,
    signature_task: Task<()>,
    debugger: Entity<Debugger>,
    /// Its parts with a panel or a tab of their own.
    debug_view: Entity<DebugView>,
    debug_hover: Entity<debug::hover::HoverCard>,
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
        let changes = cx.new(|_| ChangesPanel::new(root.clone(), agent.clone(), local));
        let history = cx.new(|cx| HistoryView::new(root.clone(), agent.clone(), window, cx));
        let search = cx.new(|cx| SearchPanel::new(root.clone(), agent.clone(), window, cx));
        let references = cx.new(|cx| SearchPanel::references(root.clone(), window, cx));
        let outline = cx.new(|_| OutlinePanel::new());
        let debugger = cx.new(|cx| Debugger::new(root.clone(), agent.clone(), session_key.clone(), window, cx));
        let debug_hover = cx.new(|cx| debug::hover::HoverCard::new(debugger.clone(), cx));
        let debug_view = layout::debug_view(&debugger, cx);
        let notes = cx.new(|cx| NotesPanel::new(session_key.clone(), window, cx));
        // The tests' Run and Debug come from the launch file.
        debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
        let subscriptions = vec![
            cx.subscribe_in(&debugger, window, Self::on_debug_event),
            cx.subscribe_in(&outline, window, Self::on_outline),
            cx.observe(&debugger, |this, debugger, cx| {
                let debugging = debugger.read(cx).is_shown();
                this.debug_changed(debugging, cx);
                cx.notify();
            }),
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
                    FileTreeEvent::ShowHistory { path, dir } => this.show_history(path, *dir, window, cx),
                    FileTreeEvent::OpenTerminal { dir } => {
                        this.show_panel(Panel::Terminals, cx);
                        this.terminals.update(cx, |terminals, cx| terminals.new_terminal_in(dir.clone(), window, cx));
                    }
                    FileTreeEvent::OpenToSide { path } => this.open_to_side(path.clone(), window, cx),
                    FileTreeEvent::FindInFolder { dir } => this.find_in_folder(dir, window, cx),
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
                    TerminalAreaEvent::ShowPanel(Some(Panel::Notes)) => this.show_notes(window, cx),
                    TerminalAreaEvent::ShowPanel(Some(panel)) => this.show_panel(*panel, cx),
                    TerminalAreaEvent::ToEditorTab(Panel::Notes) => this.notes_to_tab(window, cx),
                    TerminalAreaEvent::ToEditorTab(_) => {}
                    TerminalAreaEvent::ShowPanel(None) => this.show_panel(Panel::Terminals, cx),
                    TerminalAreaEvent::ClosePanel(Panel::Notes) => this.toggle_notes(&ToggleNotes, window, cx),
                    TerminalAreaEvent::ClosePanel(panel) => this.hide_panel(*panel, cx),
                    TerminalAreaEvent::DebugTerminal(view) => {
                        this.debugger.update(cx, |debugger, cx| debugger.set_terminal_view(view.clone(), cx));
                    }
                },
            ),
            cx.subscribe_in(&changes, window, Self::on_git_event),
            cx.subscribe_in(&history, window, Self::on_history_event),
            cx.subscribe_in(&search, window, |this, search, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { file, line, column, pin } => {
                    let goto = Position::new(line.saturating_sub(1), *column);
                    this.open_at_with(this.root.join(file), goto, *pin, *pin, window, cx);
                }
                SearchEvent::Reveal { file } => this.reveal_in_tree(&this.root.join(file), cx),
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
                SearchEvent::Reveal { file } => this.reveal_in_tree(&this.root.join(file), cx),
                SearchEvent::Replace { .. } => {}
            }),
        ];
        // the debugger's terminal goes to its console, not to a tab
        let debug_term = debugger.read(cx).terminal();
        terminals.update(cx, |terminals, cx| {
            terminals.set_debug_term(debug_term);
            terminals.restore(window, cx);
        });
        if has_agent {
            changes.update(cx, |changes, cx| changes.mark_stale(cx));
        }
        let focus_handle = cx.focus_handle();
        // Disk changes are watched by the agent on the task's machine.
        let fs_watch = agent.as_ref().map(|client| Self::watch_fs(&root, client, window, cx));
        let message = (!has_agent).then(|| "No agent: no files or terminals".into());
        if !local {
            Self::watch_ports(cx);
        }
        Self {
            group_widths: Default::default(),
            commit_texts: HashMap::new(),
            head: None,
            root,
            panels: Panels::new(),
            debugging: false,
            mode: layout::Mode::Editing,
            kept_shown: HashMap::new(),
            session_key,
            restored: false,
            ide_reported: None,
            ide_report: None,
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
            files_asked: false,
            tabs: Vec::new(),
            active: None,
            editor_split: None,
            editor_drop: None,
            group: 0,
            shown: 0,
            word_wrap: Config::get(cx).word_wrap,
            tab: (Config::get(cx).tab().tab_size, Config::get(cx).tab().hard_tabs),
            message,
            ports: Vec::new(),
            signature: None,
            signature_at: None,
            signature_task: Task::ready(()),
            debugger,
            debug_view,
            debug_hover,
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
        // The changes, if they show from the start, read now, not when shown.
        if self.client.is_some() && self.is_shown(Panel::Changes, cx) {
            self.changes.update(cx, |changes, cx| changes.shown(cx));
        }
        for saved in session.tabs {
            if self.restore_page(&saved, window, cx) {
                continue;
            }
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
            let (path, doc) = (self.tabs[ix].path.clone(), self.tabs[ix].doc);
            if !doc {
                self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
            }
        }
        // The history, if it reopened in front, reads now.
        if self.history_visible() {
            self.history.update(cx, |history, cx| history.shown(cx));
        }
        self.restored = true;
        cx.notify();
    }

    /// What is open now (excluding diff tabs).
    fn session(&self, cx: &App) -> Session {
        let mut session = Session {
            split: self.editor_split,
            shows: Some(self.saved_panels()),
            ..Session::default()
        };
        // A page is a doc that opens again; the shortcuts guide isn't.
        for (ix, tab) in self.tabs.iter().enumerate().filter(|(_, tab)| tab.diff.is_none() && (!tab.doc || tab.page.is_some())) {
            if self.active == Some(ix) {
                session.active = Some(session.tabs.len());
            }
            if let Some(page) = &tab.page {
                session.tabs.push(SavedTab { path: page.saved_path(), line: 0, column: 0, group: tab.group });
                continue;
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
        self.changes.update(cx, |changes, cx| changes.set_client(client.clone(), cx));
        let visible = self.history_visible();
        self.history.update(cx, |history, cx| history.set_client(client.clone(), visible, cx));
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
        // Cmd-click on text that isn't a link: a `path[:line[:column]]` or a
        // URL, like in a terminal, from the task's folder or the document's.
        if let Some(word) = url.strip_prefix("word:") {
            match self.link_in(word, 0, dir) {
                Some((ui_term::links::Link::Url(url), _)) => cx.open_url(&url),
                Some((ui_term::links::Link::Path { path, line, column }, _)) => {
                    let goto = Position::new(line.unwrap_or(1).saturating_sub(1), column.unwrap_or(1).saturating_sub(1));
                    self.open_at(normalize(&path), goto, window, cx);
                }
                None => {}
            }
            return;
        }
        if url.contains("://") || url.starts_with("mailto:") {
            cx.open_url(url);
            return;
        }
        let (path, fragment) = url.split_once('#').unwrap_or((url, ""));
        let path = path.split('?').next().unwrap_or_default();
        if path.is_empty() {
            return;
        }
        let path = percent_decode(path);
        // `file.rs:12` or `file.rs#L12`: at that line.
        let (path, line) = match path.rsplit_once(':').map(|(path, line)| (path, line.parse::<u32>())) {
            Some((path, Ok(line))) => (path.to_string(), Some(line)),
            _ => (path.clone(), fragment.strip_prefix('L').and_then(|line| line.parse::<u32>().ok())),
        };
        let path = match path.strip_prefix('/') {
            Some(rest) => self.root.join(rest),
            None => dir.join(path),
        };
        match line {
            Some(line) => self.open_at(normalize(&path), Position::new(line.saturating_sub(1), 0), window, cx),
            None => self.open(normalize(&path), true, window, cx),
        }
    }

    /// The path (`path[:line[:column]]`) or URL around character `column`
    /// of `line`, and the characters it spans, as a terminal finds them: a
    /// relative path from the task's folder or from `dir`.
    fn link_in(&self, line: &str, column: usize, dir: &Path) -> Option<(ui_term::links::Link, std::ops::Range<usize>)> {
        [&self.root, dir].into_iter().find_map(|from| ui_term::links::link_at(line, column, Some(from), self.local))
    }

    /// `link_in` for the text of `editor`'s tab, from its file's folder.
    pub(crate) fn link_at(
        &self,
        editor: &Entity<EditorState>,
        line: &str,
        column: usize,
    ) -> Option<(ui_term::links::Link, std::ops::Range<usize>)> {
        let tab = self.tabs.iter().find(|tab| &tab.editor == editor)?;
        let dir = tab.path.parent().unwrap_or(&self.root);
        let (link, chars) = self.link_in(line, column, dir)?;
        // On a server nothing says whether it exists: in code, `config.save`
        // would pass for a file. A path there has a folder or a line.
        if let ui_term::links::Link::Path { line: number, .. } = &link
            && !self.local
        {
            let word: String = line.chars().skip(chars.start).take(chars.len()).collect();
            if number.is_none() && !word.contains('/') {
                return None;
            }
        }
        Some((link, chars))
    }

    /// Opens `path` at `goto` from a Cmd-click in the editor: Navigate Back
    /// returns to where it was clicked.
    pub(crate) fn open_at_place(&mut self, path: PathBuf, goto: Position, window: &mut Window, cx: &mut Context<Self>) {
        self.remember_place(cx);
        self.open_at(normalize(&path), goto, window, cx);
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
                let mut old = std::mem::replace(&mut self.tabs[ix], tab);
                // A preview replaced before it loaded: what it stood in for.
                let old = match old.stand_in.take() {
                    Some(stand_in) if !matches!(old.content, Content::Ready) => stand_in,
                    _ => Box::new(old),
                };
                if matches!(old.content, Content::Ready) {
                    self.tabs[ix].stand_in = Some(old);
                }
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
                reveal_centered(&tab.editor, goto.line, false, cx);
            }
            _ => tab.goto = Some(goto),
        }
        if focus {
            self.focus_active(window, cx);
        } else if let Some(focused) = focused {
            focused.focus(window, cx);
        }
    }

    /// Reloads every open file's blame if HEAD isn't the commit it was.
    fn check_head(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let request = Request::Git { path: self.root.clone(), op: GitOp::Log { skip: 0, limit: 1 } };
        cx.spawn(async move |this, cx| {
            let head = match client.request(request).await {
                Ok(Response::Commits(commits)) => commits.into_iter().next().map(|commit| commit.hash),
                _ => None,
            };
            this.update(cx, |this, cx| {
                if head.is_some() && this.head == head {
                    return;
                }
                this.head = head;
                let files: Vec<PathBuf> = this.tabs.iter().filter(|tab| tab.is_file()).map(|tab| tab.path.clone()).collect();
                for path in files {
                    this.load_blame(path, cx);
                }
            })
            .ok();
        })
        .detach();
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
        /// Larger texts are searched in the background, so typing doesn't wait.
        const AT_ONCE: usize = 256 * 1024;
        let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.editor == editor) else {
            return;
        };
        let state = editor.read(cx);
        let selections = state.selections();
        if selections == tab.occurrences_for {
            return;
        }
        tab.occurrences_for = selections.clone();
        // A copy of the text costs nothing: it's shared.
        let text = state.text().clone();
        if text.len() <= AT_ONCE {
            tab.occurrences_task = None;
            let ranges = editing::occurrences(&text.to_string(), &selections);
            Self::show_occurrences(tab, editor, ranges, cx);
            return;
        }
        let editor = editor.clone();
        tab.occurrences_task = Some(cx.spawn(async move |this, cx| {
            let found = selections.clone();
            let ranges = cx.background_spawn(async move { editing::occurrences(&text.to_string(), &found) }).await;
            this.update(cx, |this, cx| {
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.editor == editor && tab.occurrences_for == selections) {
                    Self::show_occurrences(tab, &editor, ranges, cx);
                }
            })
            .ok();
        }));
    }

    fn show_occurrences(tab: &mut FileTab, editor: &Entity<EditorState>, ranges: Vec<std::ops::Range<usize>>, cx: &mut App) {
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
            let case_insensitive = state.search_session().options.case_insensitive;
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

    pub fn notes(&self) -> Entity<NotesPanel> {
        self.notes.clone()
    }

    /// The notes where they are (their tab of the code, or in front of the
    /// terminals), with the focus to write in them.
    pub(crate) fn show_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.notes_tab() {
            Some(ix) => self.activate_with(ix, false, window, cx),
            None => self.show_panel(Panel::Notes, cx),
        }
        self.notes.update(cx, |notes, cx| notes.focus(window, cx));
    }

    /// Cmd-Alt-N: the notes, to write in them; written in, back to the code
    /// (and the terminals, where they were).
    fn toggle_notes(&mut self, _: &ToggleNotes, window: &mut Window, cx: &mut Context<Self>) {
        let in_front = match self.notes_tab() {
            Some(ix) => self.active == Some(ix),
            None => self.is_shown(Panel::Notes, cx),
        };
        if !in_front {
            return self.show_notes(window, cx);
        }
        match self.notes_tab() {
            None => self.hide_panel(Panel::Notes, cx),
            // In a tab of the code: the tab shown before it comes back in
            // front; with none, the tab closes and they're the terminals'.
            Some(ix) => {
                let group = self.tabs[ix].group;
                let before = (0..self.tabs.len())
                    .filter(|other| *other != ix && self.tabs[*other].group == group)
                    .max_by_key(|other| self.tabs[*other].shown);
                match before {
                    Some(before) => self.activate_with(before, false, window, cx),
                    None => {
                        self.close(ix, window, cx);
                        self.hide_panel(Panel::Notes, cx);
                    }
                }
            }
        }
        self.focus_ide(window, cx);
    }

    /// What the Changes panel asks to open.
    fn on_git_event(&mut self, _: &Entity<ChangesPanel>, event: &ChangesEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            ChangesEvent::OpenFile { file } => self.open(self.root.join(file), true, window, cx),
            ChangesEvent::OpenDiff { file, pin } => {
                let deleted = self.changes.read(cx).is_deleted(file);
                if *pin && !deleted {
                    self.open(self.root.join(file), true, window, cx);
                } else {
                    let of = DiffOf { file: file.clone(), commit: None, source: false };
                    self.open_diff(of, *pin, window, cx);
                }
            }
            ChangesEvent::RevealInTree { file } => self.reveal_in_tree(&self.root.join(file), cx),
            ChangesEvent::ShowHistory { file } => self.open_history(Some((file.clone(), false)), window, cx),
        }
    }

    /// What the History tab asks to open.
    fn on_history_event(&mut self, _: &Entity<HistoryView>, event: &HistoryEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            HistoryEvent::OpenCommitDiff { commit, short, file } => {
                self.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), false), true, window, cx);
            }
            HistoryEvent::OpenFileAt { commit, short, file } => {
                self.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), true), true, window, cx);
            }
            HistoryEvent::OpenFile { file } => self.open(self.root.join(file), true, window, cx),
            HistoryEvent::RevealInTree { file } => self.reveal_in_tree(&self.root.join(file), cx),
        }
    }

    /// The History tab with the commits that changed `path`.
    fn show_history(&mut self, path: &Path, dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.repo_path(path) {
            self.open_history(Some((file, dir)), window, cx);
        }
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
/// How a file's text indents: its own, else the settings'.
fn indentation(text: &str, cx: &App) -> TabSize {
    editing::indentation(text).unwrap_or_else(|| Config::get(cx).tab())
}

fn inline_blame(editor: &Entity<EditorState>, blame: &Blame, cx: &App) -> Option<AnyElement> {
    let state = editor.read(cx);
    if state.selections().len() != 1 {
        return None;
    }
    let line = state.cursor_position().line as usize;
    let commit = &blame.commits[(*blame.lines.get(line)?)? as usize];
    let text = state.text();
    let content = text.slice_line(line).to_string();
    // A blank line says nothing worth blaming, like VS Code.
    if content.trim().is_empty() {
        return None;
    }
    let end = text.line_start_offset(line) + content.trim_end_matches('\r').len();
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
        .text_color(theme.muted_foreground.opacity(0.65))
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
/// laid out yet, it's centered on its first layout.
fn reveal_centered(editor: &Entity<EditorState>, line: u32, always: bool, cx: &mut App) {
    // Lines at the edge may be half visible: they count as outside.
    reveal_in_center(editor, line, (!always).then_some(1), cx);
}

/// How far inside the view a line where the debugger stopped is left
/// where it is: closer to an edge, it goes to the middle, so a step that
/// moves a line down isn't missed at the bottom (VS Code's
/// revealLineInCenterIfOutsideViewport, with a margin).
const STOP_MARGIN: usize = 5;

/// Scrolls `line` to the middle of the view, unless it's `margin` lines or
/// more inside it (None: always).
fn reveal_in_center(editor: &Entity<EditorState>, line: u32, margin: Option<usize>, cx: &mut App) {
    let state = editor.read(cx);
    let (Some(visible), Some(line_height)) = (state.visible_row_range(), state.line_height()) else {
        // Not laid out yet (a freshly opened tab): it lays itself out
        // there, with no frame anywhere else first.
        editor.update(cx, |state, cx| state.center_row(line as usize, cx));
        return;
    };
    let line = line as usize;
    if let Some(margin) = margin {
        // a short view keeps a third of it as margin at most, and the edge
        // lines, which may be half visible, count as outside
        let margin = margin.min((visible.len().saturating_sub(1) / 3).max(1));
        if visible.len() > 2 && line >= visible.start + margin && line + margin < visible.end {
            return;
        }
    }
    let rows = visible.len().max(1) as f32;
    let top = (line as f32 - (rows - 1.) / 2.).max(0.);
    let x = state.scroll_offset().x;
    editor.update(cx, |state, cx| state.set_scroll_offset(point(x, -(line_height * top)), cx));
}

/// Whether `editor` has text selected.
fn has_selection(editor: &Entity<EditorState>, cx: &App) -> bool {
    editor.read(cx).selections().iter().any(|(anchor, cursor)| anchor != cursor)
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
        move |bounds, window, cx| {
            let fits = |width: Pixels| Config::get(cx).diff_side_by_side(width);
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
/// `removed` and with `−`, added ones in `added` and with `+`, the rows of
/// lines left out as bands in `skipped`.
fn inline_line_styles(inline: &diff::Inline, removed: Hsla, added: Hsla, skipped: Hsla) -> Vec<LineStyle> {
    let digits = inline.lines.iter().filter_map(|line| line.old.max(line.new)).max().unwrap_or(1).to_string().len();
    let number = |number: Option<u32>| number.map_or_else(|| " ".repeat(digits), |number| format!("{number:>digits$}"));
    inline
        .lines
        .iter()
        .map(|line| {
            if let Some(count) = line.skipped {
                return LineStyle { background: Some(skipped), band: Some(skipped_label(count)), ..Default::default() };
            }
            let (background, marker) = match (line.old, line.new) {
                (Some(_), None) => (Some(removed), '−'),
                (None, Some(_)) => (Some(added), '+'),
                _ => (None, ' '),
            };
            let number = Some(format!("{} {}{marker}", number(line.old), number(line.new)).into());
            LineStyle { background, number, ..Default::default() }
        })
        .collect()
}

/// What the row of `count` lines a diff leaves out says.
fn skipped_label(count: u32) -> SharedString {
    format!("⋯  {count} unchanged lines").into()
}

/// The scrollbar marks of a diff: each run of lines of one color, by line.
fn scrollbar_marks(colors: impl Iterator<Item = Option<Hsla>>) -> Vec<ScrollbarMark> {
    let mut marks: Vec<ScrollbarMark> = Vec::new();
    for (line, color) in colors.enumerate() {
        let Some(color) = color else {
            continue;
        };
        match marks.last_mut() {
            Some(mark) if mark.lines.end == line && mark.color == color => mark.lines.end += 1,
            _ => marks.push(ScrollbarMark { lines: line..line + 1, color }),
        }
    }
    marks
}

/// How a side of a diff shows each line: changed ones in `color` and with
/// `marker` after the number, gaps hatched, the rows of lines left out as
/// bands in `skipped`.
fn line_styles(side: &diff::Side, marker: char, color: Hsla, skipped: Hsla) -> Vec<LineStyle> {
    side.lines
        .iter()
        .map(|line| {
            let changed = line.kind == diff::Kind::Changed;
            LineStyle {
                background: match line.kind {
                    diff::Kind::Changed => Some(color),
                    diff::Kind::Skipped(_) => Some(skipped),
                    _ => None,
                },
                band: match line.kind {
                    diff::Kind::Skipped(count) => Some(skipped_label(count)),
                    _ => None,
                },
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
pub(crate) fn percent_decode(text: &str) -> String {
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

    use super::{LspStatus, normalize, percent_decode, scrollbar_marks, word_at};
    use gpui_kit::{blue, red};
    use proto::Response;

    #[test]
    fn scrollbar_marks_join_runs_of_one_color() {
        let (r, b) = (Some(red()), Some(blue()));
        let marks = scrollbar_marks([None, r, r, b, None, r].into_iter());
        let runs: Vec<_> = marks.iter().map(|mark| (mark.lines.clone(), mark.color)).collect();
        assert_eq!(runs, [(1..3, red()), (3..4, blue()), (5..6, red())]);
    }

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
