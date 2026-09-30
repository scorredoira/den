use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use client::Client;
use proto::{CommitInfo, GitOp, LspLocation, LspOp, PortInfo, Request, Response, SearchHit};

use gpui_kit::component::{
    ActiveTheme as _, h_flex, h_resizable, v_resizable,
    input::{self, Editor, EditorState, InputEvent, Position, RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, RopeExt as _},
    menu::ContextMenuExt as _,
    resizable_panel,
    text::{TextView, TextViewState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    CloseAllTabs, CloseTab, CollapseFileTree, MaximizeTerminals, NewTerminal, NextTab, PrevTab, Save, ShowChanges, ShowFiles,
    FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, ShowReferences, ShowSearch,
    SplitDown, SplitRight, ToggleMarkdownSource, ToggleSidePanel,
    ToggleTerminals, OpenFileFinder, NextResult, PrevResult, GoToDefinition, FindReferences, NavigateBack, NavigateForward,
    GoToLine, OpenPreviewToSide, SplitEditorDown, SplitEditorRight, ToggleWordWrap, FormatDocument,
    changes::{self, ChangesEvent, ChangesPanel},
    completion::Completions,
    editing::{self, DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, SelectNextOccurrence},
    config::{self, Config, SavedTab, Session, TextArea, UiText},
    picker::{Picker, PickerEvent},
    search::{SearchEvent, SearchPanel},
    signature::{self, SignatureHint},
    file_tree::{FileTree, FileTreeEvent},
    language, menu,
    splits::{Axis, Direction},
    terminals::{TerminalArea, TerminalAreaEvent},
};

mod tab_drag;
use tab_drag::{EditorDrop, TabDrag, TabDragPreview};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Files,
    Changes,
    Search,
    References,
}

impl Mode {
    const ALL: [Mode; 4] = [Mode::Files, Mode::Changes, Mode::Search, Mode::References];

    fn icon(self) -> &'static str {
        match self {
            Mode::Files => "icons/files.svg",
            Mode::Changes => "icons/git-branch.svg",
            Mode::Search => "icons/text-search.svg",
            Mode::References => "icons/references.svg",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Mode::Files => "Files",
            Mode::Changes => "Changes",
            Mode::Search => "Search",
            Mode::References => "References",
        }
    }

}

enum Content {
    Loading,
    Ready,
    Failed(SharedString),
}

impl FileTab {
    fn rendered(&self) -> Option<&Entity<TextViewState>> {
        self.markdown.as_ref().filter(|_| !self.show_source)
    }

    /// The tab that holds the file itself (its saved text, whether it has
    /// changes, its blame): not a diff or another view of it.
    fn is_file(&self) -> bool {
        self.diff.is_none() && !self.view
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
    /// Text as it is on disk, to tell whether there are unsaved changes.
    saved: String,
    dirty: bool,
    preview: bool,
    /// Cmd-W was already pressed once with unsaved changes.
    confirm_close: bool,
    /// Where to put the cursor once loading finishes.
    goto: Option<Position>,
    /// Once loaded, focus goes to this tab (not if it was opened as a preview
    /// from the tree, which keeps the keyboard).
    grab_focus: bool,
    /// Tab showing a file's diff (read-only), not the file itself.
    diff: Option<DiffOf>,
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
    _subscriptions: Vec<Subscription>,
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

/// Places remembered for going back.
const MAX_PLACES: usize = 100;

#[derive(Clone, PartialEq)]
struct DiffOf {
    /// Relative to the task's folder; empty for the whole commit.
    file: String,
    uncommitted: bool,
    /// What changed in a commit (hash and short hash), not in the folder.
    commit: Option<(String, String)>,
    /// The file as it was in `commit`, not its diff.
    source: bool,
}

impl DiffOf {
    fn commit(commit: String, short: String, file: String, source: bool) -> Self {
        Self { file, uncommitted: false, commit: Some((commit, short)), source }
    }
}

pub struct Workspace {
    root: PathBuf,
    /// The task's key in `config.json`, to remember what was open.
    session_key: String,
    /// Last session's tabs were already reopened (nothing is saved before that).
    restored: bool,
    focus_handle: FocusHandle,
    mode: Mode,
    side_panel_visible: bool,
    file_tree: Entity<FileTree>,
    terminals: Entity<TerminalArea>,
    terminals_visible: bool,
    terminals_maximized: bool,
    /// Side panel, code and terminals.
    split: config::Split,
    width: Pixels,
    client: Option<Arc<Client>>,
    /// On this machine (not on a server).
    local: bool,
    changes: Entity<ChangesPanel>,
    search: Entity<SearchPanel>,
    /// References panel: the latest F12 (with several targets) or Shift-F12.
    references: Entity<SearchPanel>,
    finder: Option<(Entity<Picker>, Subscription)>,
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
        let search = cx.new(|cx| SearchPanel::new(root.clone(), agent.clone(), window, cx));
        let references = cx.new(|cx| SearchPanel::references(root.clone(), window, cx));
        let subscriptions = vec![
            cx.observe_self(|this, cx| this.remember(cx)),
            cx.subscribe_in(
                &file_tree,
                window,
                |this, _, event: &FileTreeEvent, window, cx| match event {
                    FileTreeEvent::Open { path, pin } => this.open_with(path.clone(), *pin, *pin, window, cx),
                    FileTreeEvent::Renamed { from, to } => this.renamed(from, to, cx),
                    FileTreeEvent::Trashed { path } => this.trashed(path, window, cx),
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
                },
            ),
            cx.subscribe_in(&changes, window, |this, _, event: &ChangesEvent, window, cx| match event {
                ChangesEvent::OpenFile { file } => this.open(this.root.join(file), true, window, cx),
                ChangesEvent::OpenDiff { file, pin } => {
                    let uncommitted = this.changes.read(cx).uncommitted();
                    let deleted = !this.root.join(file).exists();
                    if *pin && !deleted {
                        this.open(this.root.join(file), true, window, cx);
                    } else {
                        let of = DiffOf { file: file.clone(), uncommitted, commit: None, source: false };
                        this.open_diff(of, *pin, window, cx);
                    }
                }
                ChangesEvent::OpenCommitDiff { commit, short, file, pin } => {
                    this.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), false), *pin, window, cx);
                }
                ChangesEvent::OpenCommit { commit, short, pin } => {
                    this.open_diff(DiffOf::commit(commit.clone(), short.clone(), String::new(), false), *pin, window, cx);
                }
                ChangesEvent::OpenFileAt { commit, short, file } => {
                    this.open_diff(DiffOf::commit(commit.clone(), short.clone(), file.clone(), true), true, window, cx);
                }
                ChangesEvent::ChooseBranch { branches } => this.choose_branch(branches.clone(), window, cx),
            }),
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
        let focus_handle = cx.focus_handle();
        // Disk changes are watched by the agent on the task's machine.
        if let Some(client) = &agent {
            Self::watch_fs(&root, client, window, cx);
        }
        let message = (!has_agent).then(|| "No agent: no files or terminals".into());
        if !local {
            Self::watch_ports(cx);
        }
        Self {
            root,
            session_key,
            restored: false,
            focus_handle,
            mode: Mode::Files,
            side_panel_visible: true,
            file_tree,
            terminals,
            terminals_visible: true,
            terminals_maximized: false,
            split: config::Split::new(cx),
            width: px(0.),
            client: agent,
            local,
            changes,
            search,
            references,
            finder: None,
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
        let mut session = Session { split: self.editor_split, ..Session::default() };
        for (ix, tab) in self.tabs.iter().enumerate().filter(|(_, tab)| tab.diff.is_none()) {
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
            if session.tabs.is_empty() {
                config.sessions.remove(&key);
            } else {
                config.sessions.insert(key, session);
            }
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

    /// Asks the agent to report changes inside `root`.
    fn watch_fs(root: &Path, client: &Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = smol::channel::unbounded::<Vec<PathBuf>>();
        let watched = root.to_path_buf();
        client.watch(move |event| {
            if let proto::Event::FsChanged { root, paths } = event
                && *root == watched
            {
                let _ = tx.try_send(paths.clone());
            }
        });
        client.notify(Request::Watch { path: root.to_path_buf() });
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
    }

    /// Switches to a new connection with the agent (after reconnecting): the
    /// tree, panels and terminals carry on with it, and open files are reread.
    pub fn set_client(&mut self, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        self.client = Some(client.clone());
        Self::watch_fs(&self.root, &client, window, cx);
        self.file_tree
            .update(cx, |tree, cx| tree.set_client(client.clone(), cx));
        let changes_visible = self.side_panel_visible && self.mode == Mode::Changes;
        self.changes
            .update(cx, |changes, cx| changes.set_client(client.clone(), changes_visible, cx));
        self.search.update(cx, |search, _| search.set_client(client.clone()));
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

    /// Opens (or reuses) the tab with the diff of `file`.
    /// The folder one is reused when switching between Branch and Uncommitted;
    /// a commit's one belongs to that commit only.
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
        let request = match &of.commit {
            Some((commit, _)) => {
                let (commit, file) = (commit.clone(), of.file.clone());
                let op = if of.source {
                    GitOp::FileAt { commit, file }
                } else if file.is_empty() {
                    GitOp::Show { commit }
                } else {
                    GitOp::CommitDiff { commit, file }
                };
                Request::Git { path: self.root.clone(), op }
            }
            None => Request::GitDiff {
                path: self.root.clone(),
                file: of.file.clone(),
                uncommitted: of.uncommitted,
            },
        };
        cx.spawn_in(window, async move |this, cx| {
            let response = client.request(request).await;
            this.update_in(cx, |this, window, cx| {
                let Some(tab) = this.tabs.iter_mut().find(|tab| tab.diff.as_ref() == Some(&of)) else {
                    return;
                };
                match response {
                    Ok(Response::Text(text)) => {
                        let text = if text.is_empty() && !of.source { "No changes".to_string() } else { text };
                        let focused = window.focused(cx);
                        tab.saved = text.clone();
                        tab.content = Content::Ready;
                        tab.editor.update(cx, |state, cx| state.set_value(text, window, cx));
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

    /// Branch picker for the Changes mode, in Cmd-P's spot.
    fn choose_branch(&mut self, branches: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let picker = cx.new(|cx| Picker::new(Arc::new(branches), "Switch to Branch…", false, window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            this.finder = None;
            match event {
                PickerEvent::Pick(branch) => {
                    let branch = branch.clone();
                    this.changes.update(cx, |changes, cx| changes.switch_branch(branch, cx));
                    this.focus_ide(window, cx);
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
                this.message = None;
                match (response, op) {
                    // No language server: F12 can't; Shift-F12 searches for the word.
                    (Ok(Response::Lsp { server: None, .. }), LspOp::Definition) => {
                        this.message = Some("F12: no language server for this file".into());
                    }
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
                    (Err(err), LspOp::Definition) => this.message = Some(format!("{err:#}").into()),
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
        let panel = if self.mode == Mode::References { &self.references } else { &self.search };
        panel.update(cx, |panel, cx| panel.step(delta, cx));
    }

    /// Shows the References panel without moving focus.
    fn show_references(&mut self, cx: &mut Context<Self>) {
        self.side_panel_visible = true;
        self.terminals_maximized = false;
        self.set_mode(Mode::References, cx);
    }

    /// Cmd-Shift-F: the search panel, with the editor's selection.
    fn show_search(&mut self, _: &ShowSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Search;
        self.side_panel_visible = true;
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
        self.terminals_visible = true;
        self.terminals
            .update(cx, |terminals, cx| terminals.new_terminal(window, cx));
        cx.notify();
    }

    fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals_visible = true;
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
        if self.terminals_visible && focused {
            self.terminals_visible = false;
            self.terminals_maximized = false;
            self.focus_ide(window, cx);
        } else {
            self.terminals_visible = true;
            self.terminals
                .update(cx, |terminals, cx| terminals.focus(window, cx));
        }
        cx.notify();
    }

    fn maximize_terminals(&mut self, _: &MaximizeTerminals, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals_maximized = !self.terminals_maximized;
        if self.terminals_maximized {
            self.terminals_visible = true;
            self.terminals
                .update(cx, |terminals, cx| terminals.focus(window, cx));
        }
        cx.notify();
    }

    /// Width left over by the tasks column. When it changes, the side panel and
    /// terminals keep their width and the code takes the difference.
    pub fn set_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
        if self.width != width {
            self.width = width;
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
        if self.terminals_visible && !self.terminals.read(cx).is_empty() {
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
        let editor = cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(Config::get(cx).word_wrap);
            let lsp = editor.lsp_mut();
            lsp.completion_provider = Some(Rc::new(Completions::new(workspace, cx.entity().downgrade())));
            lsp.completion_menu.max_width = px(480.);
            editor
        });
        let subscriptions = vec![
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
            saved: String::new(),
            dirty: false,
            preview,
            confirm_close: false,
            goto: None,
            grab_focus: true,
            diff: None,
            restored: false,
            blame: None,
            occurrences: None,
            occurrences_for: Vec::new(),
            group: self.group,
            shown: 0,
            view: false,
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
        let changes_visible = self.side_panel_visible && self.mode == Mode::Changes;
        self.changes
            .update(cx, |changes, cx| changes.mark_stale(changes_visible, cx));
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

    fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.side_panel_visible = true;
        self.set_mode(Mode::Files, cx);
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
                    return Err("Nothing formats this kind of file: the repo can add a .task/format".into());
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
            return Task::ready(false);
        };
        let write = client.request(Request::WriteFile {
            path: path.clone(),
            data: text.clone().into_bytes(),
        });
        cx.spawn(async move |this, cx| {
            let result = write.await;
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
                self.terminals_visible = false;
                self.terminals_maximized = false;
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
        for tab in &self.tabs {
            tab.editor.update(cx, |state, cx| state.set_soft_wrap(wrap, window, cx));
        }
    }

    fn toggle_side_panel(&mut self, _: &ToggleSidePanel, _: &mut Window, cx: &mut Context<Self>) {
        self.side_panel_visible = !self.side_panel_visible;
        cx.notify();
    }

    /// The same key that shows a mode hides it; focus doesn't move.
    fn show_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.side_panel_visible && self.mode == mode {
            self.side_panel_visible = false;
        } else {
            self.set_mode(mode, cx);
            self.side_panel_visible = true;
        }
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        self.mode = mode;
        if mode == Mode::Changes {
            self.changes.update(cx, |changes, cx| changes.shown(cx));
        }
        cx.notify();
    }

    fn render_side_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // Modes are icon tabs in the panel's own header: no separate bar,
        // and one click (or shortcut) away.
        let tabs: Vec<AnyElement> = Mode::ALL
            .into_iter()
            .map(|mode| {
                mode_button(("mode", mode as usize), mode.icon(), self.mode == mode, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
                    .into_any_element()
            })
            .collect();
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_none()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .children(tabs)
                    .child(div().flex_1())
                    .child(
                        div()
                            .pr_1()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child(self.mode.title()),
                    ),
            )
            .child(match self.mode {
                Mode::Changes => div().flex_1().min_h_0().child(self.changes.clone()),
                Mode::Search => div().flex_1().min_h_0().child(self.search.clone()),
                Mode::References => div().flex_1().min_h_0().child(self.references.clone()),
                Mode::Files => div().flex_1().min_h_0().child(self.file_tree.clone()),
            })
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
                    .context_menu({
                        let workspace = cx.entity().downgrade();
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
                    })
            }))
            .child(
                div()
                    .id(("tab-drop-end", group))
                    .h_full()
                    .flex_1()
                    .min_w(px(24.))
                    .drag_over::<TabDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary)),
            )
    }

    /// A group's tab bar and the tab it shows. A click anywhere in it gives
    /// it the focus.
    fn render_group(&self, group: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let body = match self.shown_in(group).map(|ix| &self.tabs[ix]) {
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_ui(cx)
                .text_color(theme.muted_foreground)
                .child("Open a file from the tree")
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
                    .size_full()
                    .p_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(image.clone()).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                Content::Ready => match tab.rendered() {
                    Some(markdown) => div()
                        .size_full()
                        .text_size(px(Config::get(cx).font_size(TextArea::Preview)))
                        .child(
                            TextView::new(markdown)
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
                        let readonly = tab.diff.is_some();
                        let file = self.tabs.iter().find(|file| file.path == tab.path && file.is_file());
                        let blame = file.filter(|file| !file.dirty && tab.diff.is_none()).and_then(|file| file.blame.clone());
                        let editor = Editor::new(&tab.editor)
                            .bordered(false)
                            .readonly(readonly)
                            .h_full()
                            // The right click already put the cursor where clicked. The menu
                            // is built while the editor is mid-update: it can't be
                            // read (GPUI aborts), so Cut and Copy are always
                            // enabled and do nothing without a selection.
                            .context_menu(move |menu, _, _| {
                                menu.menu_with_disabled("Go to Definition", readonly, Box::new(GoToDefinition))
                                    .menu_with_disabled("Find References", readonly, Box::new(FindReferences))
                                    .menu_with_disabled("Format Document", readonly, Box::new(FormatDocument))
                                    .separator()
                                    .menu_with_disabled("Cut", readonly, Box::new(input::Cut))
                                    .menu("Copy", Box::new(input::Copy))
                                    .menu_with_disabled("Paste", readonly, Box::new(input::Paste))
                                    .separator()
                                    .menu("Select All", Box::new(input::SelectAll))
                            });
                        div()
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
                            .children(
                                self.signature
                                    .as_ref()
                                    .filter(|hint| hint.editor == tab.editor)
                                    .and_then(|hint| signature::render(hint, cx)),
                            )
                            .into_any_element()
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
    /// other; the status bar under them.
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
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(div().flex_1().min_h_0().child(groups))
            .child(self.render_status_bar(cx))
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut left = h_flex().gap_3();
        let mut right = h_flex().gap_3();
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

fn mode_button(
    id: impl Into<ElementId>,
    icon: &'static str,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .when(active, |el| el.bg(theme.sidebar_accent))
        .hover(|style| style.bg(theme.sidebar_accent))
        .child(svg().path(icon).size(px(16.)).text_color(if active {
            theme.sidebar_accent_foreground
        } else {
            theme.muted_foreground
        }))
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
        if !cx.has_active_drag() {
            self.editor_drop = None;
        }
        h_flex()
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
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(|this, _: &CloseAllTabs, window, cx| this.close_others(None, window, cx)))
            .on_action(cx.listener(|this, _: &CollapseFileTree, _, cx| {
                this.file_tree.update(cx, |tree, cx| tree.collapse_all(cx))
            }))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::toggle_side_panel))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.show_mode(Mode::Files, cx)))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.show_mode(Mode::Changes, cx)))
            .on_action(cx.listener(Self::show_search))
            .on_action(cx.listener(Self::open_file_finder))
            // F4 steps through the visible panel's results: References or Search.
            .on_action(cx.listener(|this, _: &NextResult, _, cx| this.step_result(1, cx)))
            .on_action(cx.listener(|this, _: &PrevResult, _, cx| this.step_result(-1, cx)))
            .on_action(cx.listener(Self::go_to_definition))
            .on_action(cx.listener(Self::go_to_line))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| this.navigate(false, window, cx)))
            .on_action(cx.listener(Self::find_references))
            .on_action(
                cx.listener(|this, _: &ShowReferences, _, cx| this.show_mode(Mode::References, cx)),
            )
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
            .relative()
            .size_full()
            .font_family(cx.theme().font_family.clone())
            .text_ui(cx)
            .text_color(cx.theme().foreground)
            .child({
                let layout = Config::get(cx).layout;
                let side_visible = self.side_panel_visible && !self.terminals_maximized;
                let editor_visible = !self.terminals_maximized;
                let terminals_visible = self.terminals_visible;
                // With no saved size, the terminals take half the space left
                // by the tasks column and the side panel.
                let terminals = layout.terminals.unwrap_or_else(|| {
                    let free = f32::from(window.bounds().size.width) - layout.tasks - layout.side;
                    (free / 2.).max(400.)
                });
                h_resizable("workspace-split")
                    .with_state(self.split.state(self.width, cx))
                    .child(
                        resizable_panel()
                            .size(config::width(layout.side, 160., 600.))
                            .size_range(px(160.)..px(600.))
                            .visible(side_visible)
                            .child(self.render_side_panel(cx)),
                    )
                    .child(
                        resizable_panel()
                            .visible(editor_visible)
                            .child(self.render_editor_area(cx)),
                    )
                    .child(
                        resizable_panel()
                            .size(config::width(terminals, 240., 4000.))
                            .size_range(px(240.)..px(4000.))
                            .visible(terminals_visible)
                            .child(self.terminals.clone()),
                    )
                    .on_resize(move |state, _, cx| {
                        let sizes = state.read(cx).sizes().clone();
                        Config::update_quietly(cx, |config| {
                            if side_visible && let Some(side) = sizes.first() {
                                config.layout.side = f32::from(*side);
                            }
                            if editor_visible && terminals_visible && let Some(terminals) = sizes.get(2) {
                                config.layout.terminals = Some(f32::from(*terminals));
                            }
                        });
                    })
            })
            .children(self.finder.as_ref().map(|(finder, _)| {
                div()
                    .absolute()
                    .top(px(44.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(finder.clone())
            }))
    }
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
fn normalize(path: &Path) -> PathBuf {
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

    use super::{normalize, percent_decode, word_at};

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
