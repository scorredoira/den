use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use client::Client;
use proto::{GitOp, LspLocation, LspOp, Request, Response, SearchHit};

use gpui_kit::component::{
    ActiveTheme as _, h_flex, h_resizable,
    input::{self, Editor, EditorState, InputEvent, Position},
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
    changes::{ChangesEvent, ChangesPanel},
    config::{self, Config, SavedTab, Session},
    picker::{Picker, PickerEvent},
    search::{SearchEvent, SearchPanel},
    file_tree::{FileTree, FileTreeEvent},
    language, menu,
    splits::{Axis, Direction},
    terminals::{TerminalArea, TerminalAreaEvent},
};

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
    _subscriptions: Vec<Subscription>,
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
    /// Relative to the task's folder.
    file: String,
    uncommitted: bool,
    /// What changed in a commit (hash and short hash), not in the folder.
    commit: Option<(String, String)>,
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
    active: Option<usize>,
    message: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

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
                        this.open_diff(DiffOf { file: file.clone(), uncommitted, commit: None }, *pin, window, cx);
                    }
                }
                ChangesEvent::OpenCommitDiff { commit, short, file, pin } => {
                    let of = DiffOf { file: file.clone(), uncommitted: false, commit: Some((commit.clone(), short.clone())) };
                    this.open_diff(of, *pin, window, cx);
                }
                ChangesEvent::ChooseBranch { branches } => this.choose_branch(branches.clone(), window, cx),
            }),
            cx.subscribe_in(&search, window, |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { file, line, column, pin } => {
                    let goto = Position::new(line.saturating_sub(1), *column);
                    this.open_at_with(this.root.join(file), goto, *pin, *pin, window, cx);
                }
            }),
            // Paths outside the task (the standard library) are absolute.
            cx.subscribe_in(&references, window, |this, _, event: &SearchEvent, window, cx| match event {
                SearchEvent::Open { file, line, column, pin } => {
                    let goto = Position::new(line.saturating_sub(1), *column);
                    this.open_at_with(this.root.join(file), goto, *pin, *pin, window, cx);
                }
            }),
        ];
        terminals.update(cx, |terminals, cx| terminals.restore(window, cx));
        let focus_handle = cx.focus_handle();
        // Disk changes are watched by the agent on the task's machine.
        if let Some(client) = &agent {
            Self::watch_fs(&root, client, window, cx);
        }
        let message = (!has_agent).then(|| "No agent: no files or terminals".into());
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
            message,
            _subscriptions: subscriptions,
        }
    }

    /// Reopens what was open last time, without stealing focus.
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = Config::get(cx)
            .sessions
            .get(&self.session_key)
            .cloned()
            .unwrap_or_default();
        for saved in session.tabs {
            let mut tab = self.new_tab(saved.path.clone(), false, window, cx);
            tab.grab_focus = false;
            tab.restored = true;
            tab.goto = Some(Position::new(saved.line, saved.column));
            self.tabs.push(tab);
            self.load(saved.path, false, window, cx);
        }
        self.active = session
            .active
            .filter(|ix| *ix < self.tabs.len())
            .or((!self.tabs.is_empty()).then_some(0));
        if let Some(ix) = self.active {
            let path = self.tabs[ix].path.clone();
            self.file_tree.update(cx, |tree, cx| tree.reveal(&path, cx));
        }
        self.restored = true;
        cx.notify();
    }

    /// What is open now (excluding diff tabs).
    fn session(&self, cx: &App) -> Session {
        let mut session = Session::default();
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
            && saved.tabs.iter().map(|tab| &tab.path).eq(session.tabs.iter().map(|tab| &tab.path));
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
        self.tabs.remove(ix);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > ix => Some(active - 1),
            Some(active) if active == ix => Some(ix.min(self.tabs.len() - 1)),
            other => other,
        };
        cx.notify();
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
            .filter(|tab| tab.diff.is_none() && (matches!(tab.content, Content::Ready) || tab.restored))
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
        if let Some(ix) = self.tabs.iter().position(|tab| tab.path == path && tab.diff.is_none()) {
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
        let reuse = self
            .tabs
            .iter()
            .position(|tab| tab.preview && !tab.dirty)
            .filter(|_| !pin);
        let ix = match reuse {
            Some(ix) => {
                self.tabs[ix] = tab;
                ix
            }
            None => {
                let ix = self.active.map_or(self.tabs.len(), |active| active + 1);
                self.tabs.insert(ix, tab);
                ix
            }
        };
        self.load(path, false, window, cx);
        self.activate_with(ix, focus, window, cx);
    }

    /// The active tab's file and cursor position.
    fn place(&self, cx: &App) -> Option<Place> {
        let tab = &self.tabs[self.active?];
        if tab.diff.is_some() {
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
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none()) else {
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
                reveal_centered(&tab.editor, goto.line, true, window, cx);
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
            .position(|tab| tab.diff.as_ref().is_some_and(|diff| diff.file == file && diff.commit == of.commit))
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
        let mut tab = self.new_tab_with(self.root.join(&file), !pin, "diff", window, cx);
        tab.diff = Some(of);
        tab.grab_focus = false;
        let reuse = self
            .tabs
            .iter()
            .position(|tab| tab.preview && !tab.dirty)
            .filter(|_| !pin);
        let ix = match reuse {
            Some(ix) => {
                self.tabs[ix] = tab;
                ix
            }
            None => {
                let ix = self.active.map_or(self.tabs.len(), |active| active + 1);
                self.tabs.insert(ix, tab);
                ix
            }
        };
        self.load_diff(ix, window, cx);
        self.activate_with(ix, false, window, cx);
    }

    fn load_diff(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(of)) = (self.client.clone(), self.tabs[ix].diff.clone()) else {
            return;
        };
        let request = match &of.commit {
            Some((commit, _)) => Request::Git {
                path: self.root.clone(),
                op: GitOp::CommitDiff { commit: commit.clone(), file: of.file.clone() },
            },
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
                        let text = if text.is_empty() { "No changes".to_string() } else { text };
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
                    (Err(err), LspOp::References) => {
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
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .soft_wrap(false)
        });
        let path_for_change = path.clone();
        let subscriptions = vec![
            cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.on_edit(&path_for_change, &editor, cx);
                }
            }),
            // The status bar shows the cursor position.
            cx.observe(&editor, |_, _, cx| cx.notify()),
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
                let Some(tab) = this.tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none()) else {
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
                            state.set_value(text, window, cx);
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
                            reveal_centered(&tab.editor, line, true, window, cx);
                        }
                        // Setting the text moves focus to the editor: it goes back to
                        // where it was, or where it belongs if this is the active tab.
                        let grab = this
                            .tabs
                            .iter()
                            .find(|tab| tab.path == path && tab.diff.is_none())
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
                        if let Some(ix) = this.tabs.iter().position(|tab| tab.path == path && tab.diff.is_none()) {
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
        self.file_tree
            .update(cx, |tree, cx| tree.invalidate(&paths, cx));
        let changes_visible = self.side_panel_visible && self.mode == Mode::Changes;
        self.changes
            .update(cx, |changes, cx| changes.mark_stale(changes_visible, cx));
        let reload: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|tab| tab.diff.is_none() && matches!(tab.content, Content::Ready) && paths.contains(&tab.path))
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

    fn on_edit(&mut self, path: &Path, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| &tab.editor == editor && tab.path == path) else {
            return;
        };
        if !matches!(tab.content, Content::Ready) {
            return;
        }
        let text = editor.read(cx).text().to_string();
        let dirty = text != tab.saved;
        if let Some(markdown) = &tab.markdown {
            markdown.update(cx, |view, cx| view.set_text(&text, cx));
        }
        if dirty != tab.dirty {
            tab.dirty = dirty;
            tab.confirm_close = false;
            cx.notify();
        }
        // Editing a preview turns it into a pinned tab.
        if dirty && tab.preview {
            tab.preview = false;
            cx.notify();
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_with(ix, true, window, cx);
    }

    fn activate_with(&mut self, ix: usize, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = Some(ix);
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
        if tab.rendered().is_some() {
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
        let tab = &mut self.tabs[ix];
        if tab.dirty && !tab.confirm_close {
            tab.confirm_close = true;
            self.message = Some("Unsaved changes: press Cmd-W again to close without saving".into());
            cx.notify();
            return;
        }
        self.tabs.remove(ix);
        self.message = None;
        match self.active {
            _ if self.tabs.is_empty() => {
                self.active = None;
                self.focus_handle.focus(window, cx);
                cx.notify();
            }
            Some(active) if active >= ix => {
                let next = active.saturating_sub(1).min(self.tabs.len() - 1);
                let next = if active == ix { ix.min(self.tabs.len() - 1) } else { next };
                self.activate(next, window, cx);
            }
            _ => cx.notify(),
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

    fn tab_index(&self, path: &Path, diff: bool) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.path == path && tab.diff.is_some() == diff)
    }

    fn reveal_in_tree(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.side_panel_visible = true;
        self.set_mode(Mode::Files, cx);
        self.file_tree.update(cx, |tree, cx| tree.reveal(path, cx));
    }

    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.save_tab(ix, cx).detach();
        }
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
        if !matches!(tab.content, Content::Ready) || tab.diff.is_some() {
            return Task::ready(true);
        }
        let path = tab.path.clone();
        let text = tab.editor.read(cx).text().to_string();
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
                        if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none()) {
                            tab.saved = text;
                            tab.dirty = false;
                            tab.confirm_close = false;
                            tab.preview = false;
                        }
                        this.message = None;
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
        if let Some(ix) = self.active {
            self.activate((ix + 1) % self.tabs.len(), window, cx);
        }
    }

    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active {
            self.activate((ix + self.tabs.len() - 1) % self.tabs.len(), window, cx);
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
                            .text_xs()
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

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id("tab-bar")
            .h(px(34.))
            .flex_none()
            .overflow_x_scroll()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let active = self.active == Some(ix);
                let name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = match &tab.diff {
                    Some(DiffOf { commit: Some((_, short)), .. }) => format!("{name} ({short})"),
                    Some(_) => format!("{name} (changes)"),
                    None => name,
                };
                let close_icon = if tab.dirty {
                    "icons/tab-dirty.svg"
                } else {
                    "icons/tab-close.svg"
                };
                h_flex()
                    .id(("tab", ix))
                    .group("tab")
                    .h_full()
                    .flex_none()
                    .gap_1()
                    .pl_3()
                    .pr_1()
                    .text_sm()
                    .border_r_1()
                    .border_color(theme.border)
                    .when(active, |el| {
                        el.bg(theme.tab_active).text_color(theme.tab_active_foreground)
                    })
                    .when(!active, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
                    .when(tab.preview, |el| el.italic())
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
                                    .size(px(if tab.dirty { 8. } else { 14. }))
                                    .text_color(theme.muted_foreground)
                                    .when(!tab.dirty && !active, |el| {
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
                        let diff = tab.diff.is_some();
                        let local = self.local;
                        let relative = tab
                            .path
                            .strip_prefix(&self.root)
                            .unwrap_or(&tab.path)
                            .to_string_lossy()
                            .into_owned();
                        move |menu, _, _| {
                            let relative = relative.clone();
                            let (close, others) = (path.clone(), path.clone());
                            menu.item(
                                menu::item("Close", &workspace, move |this, window, cx| {
                                    if let Some(ix) = this.tab_index(&close, diff) {
                                        this.close(ix, window, cx);
                                    }
                                })
                                .action(Box::new(CloseTab)),
                            )
                            .item(menu::item("Close Others", &workspace, move |this, window, cx| {
                                this.close_others(this.tab_index(&others, diff), window, cx)
                            }))
                            .item(
                                menu::item("Close All", &workspace, |this, window, cx| this.close_others(None, window, cx))
                                    .action(Box::new(CloseAllTabs)),
                            )
                            .when(diff, |menu| {
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
    }

    fn render_editor_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let body = match self.active.map(|ix| &self.tabs[ix]) {
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Open a file from the tree")
                .into_any_element(),
            Some(tab) => match &tab.content {
                Content::Loading => div().size_full().into_any_element(),
                Content::Failed(err) => div()
                    .p_4()
                    .text_sm()
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
                        Editor::new(&tab.editor)
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
                                    .separator()
                                    .menu_with_disabled("Cut", readonly, Box::new(input::Cut))
                                    .menu("Copy", Box::new(input::Copy))
                                    .menu_with_disabled("Paste", readonly, Box::new(input::Paste))
                                    .separator()
                                    .menu("Select All", Box::new(input::SelectAll))
                            })
                            .into_any_element()
                    }
                },
            },
        };
        v_flex()
            .size_full()
            .bg(theme.background)
            .when(!self.tabs.is_empty(), |el| el.child(self.render_tab_bar(cx)))
            .child(div().flex_1().min_h_0().child(body))
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
        h_flex()
            .h(px(24.))
            .flex_none()
            .px_3()
            .justify_between()
            .text_xs()
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
        h_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
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
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| this.navigate(false, window, cx)))
            .on_action(cx.listener(Self::find_references))
            .on_action(
                cx.listener(|this, _: &ShowReferences, _, cx| this.show_mode(Mode::References, cx)),
            )
            .on_action(cx.listener(Self::toggle_markdown_source))
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

/// After a jump: if `line` wasn't visible, scrolls to center it, like VS Code
/// (the editor alone only brings it in at the edge). If not laid out yet (a
/// freshly opened file), it's done on the next frame.
fn reveal_centered(editor: &Entity<EditorState>, line: u32, retry: bool, window: &mut Window, cx: &mut App) {
    let state = editor.read(cx);
    let (Some(visible), Some(line_height)) = (state.visible_row_range(), state.line_height()) else {
        if retry {
            let editor = editor.clone();
            window.on_next_frame(move |window, cx| reveal_centered(&editor, line, false, window, cx));
        }
        return;
    };
    let line = line as usize;
    // Lines at the edge may be half visible: they count as outside.
    if visible.len() > 2 && line > visible.start && line + 1 < visible.end {
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
