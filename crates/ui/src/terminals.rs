//! The task's terminal area: tabs, and in each tab terminals split into
//! resizable rows and columns. The processes live in the agent; here we only
//! store which terminal goes where, to restore it when the app is reopened.

mod drag;

use drag::{Source, TerminalDrag};
use crate::drag_drop::DropPlacement;
use crate::menu::PanelItems as _;

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex, h_resizable,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::TermId;
use serde::{Deserialize, Serialize};
use ui_term::{Terminal, TerminalView, TerminalViewEvent, grid_for, links::Link};

use crate::{
    config::{Config, Panel, UiText, store::JsonFile},
    notes::NotesPanel,
    CloseTab, NewTerminal, SplitDown, SplitRight, agent, menu,
    splits::{Axis, Direction, Tree},
};

pub enum TerminalAreaEvent {
    Hide,
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// A message for the status bar.
    Message(SharedString),
    /// A panel's tab after the terminals' (the debugger's console):
    /// show it, or (`None`) go back to the terminals.
    ShowPanel(Option<Panel>),
    /// A panel's tab closed.
    ClosePanel(Panel),
    /// Open in Editor Tab, from a panel tab's menu (the notes').
    ToEditorTab(Panel),
    /// The notes went into a split or out of one: their tab goes or comes back.
    NotesMoved,
    /// The debugger's terminal, drawn in its console: a new one, or (`None`)
    /// it's gone.
    DebugTerminal(Option<Entity<TerminalView>>),
    /// The pane with the keyboard closed and none is left to take it: the
    /// keyboard must go somewhere, or the window's shortcuts stop working.
    FocusLeft,
}

/// A panel in the terminals' place: a tab after theirs.
pub struct PanelTab {
    pub panel: Panel,
    pub view: AnyView,
    pub icon: &'static str,
    pub title: &'static str,
    /// The one showing, rather than the terminals.
    pub showing: bool,
    /// With a close button, or always there.
    pub closable: bool,
    /// A dot of this color on it: the debugger's state.
    pub dot: Option<Hsla>,
}

impl PanelTab {
    fn key(&self) -> (Panel, EntityId, &'static str, bool, bool, Option<Hsla>) {
        (self.panel, self.view.entity_id(), self.icon, self.showing, self.closable, self.dot)
    }
}

/// What a split's leaf shows: a terminal, or the workspace's notes (in one
/// split at most, dragged there from their tab like a terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
enum Pane {
    Term(TermId),
    Notes,
}

impl Pane {
    fn term(self) -> Option<TermId> {
        match self {
            Pane::Term(term) => Some(term),
            Pane::Notes => None,
        }
    }
}

/// A name being typed, in its place: Enter gives it, Escape or a click
/// elsewhere leaves it as it was.
struct Renaming {
    named: Named,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// What a name is given to.
#[derive(Clone, Copy, PartialEq)]
enum Named {
    /// A split tab, by its id.
    Tab(usize),
    Term(TermId),
}

struct TerminalTab {
    id: usize,
    tree: Tree<Pane>,
    /// The tab's pane that last had focus.
    active: Pane,
    /// The name given to it when split (Rename Tab), rather than the
    /// workspace's folder; one pane alone is named itself.
    name: Option<String>,
}

impl TerminalTab {
    fn terms(&self) -> Vec<TermId> {
        self.tree.leaves().into_iter().filter_map(Pane::term).collect()
    }

    fn contains(&self, pane: Pane) -> bool {
        self.tree.leaves().contains(&pane)
    }

    /// Only the notes are left: the tab goes, and they're their tab again.
    fn notes_only(&self) -> bool {
        self.tree == Tree::Leaf(Pane::Notes)
    }
}

/// Where a new terminal goes.
#[derive(Clone, Copy)]
enum Place {
    NewTab,
    /// Next to the active terminal: to the right (`Row`) or below (`Column`).
    Split(Axis),
}

/// What `restore` brings back: the tabs with their names, the terminals'
/// names, and the terminals attached.
type Restored = (Vec<(Tree<Pane>, Option<String>)>, HashMap<TermId, String>, Vec<(TermId, Entity<Terminal>)>);

pub struct TerminalArea {
    client: Option<Arc<Client>>,
    /// Terminal group in the agent (the task's folder).
    group: String,
    cwd: PathBuf,
    tabs: Vec<TerminalTab>,
    views: HashMap<TermId, Entity<TerminalView>>,
    active: usize,
    next_id: usize,
    terminal_drop: Option<(Pane, DropPlacement)>,
    drag_origin: Option<usize>,
    /// Size of the terminal area at the last paint, so each shell is created
    /// at its final size (otherwise it redraws the prompt when resized).
    body_size: Rc<Cell<Option<Size<Pixels>>>>,
    /// Terminals on this machine (not on a server).
    local: bool,
    /// The panels in the terminals' place, a tab each after theirs.
    panel_tabs: Vec<PanelTab>,
    /// The terminal the debugger runs its command in: in `views`, but in no
    /// tab, as the debugger's console draws it.
    debug_term: Option<TermId>,
    /// The workspace's notes, for a split to show them.
    notes: Option<Entity<NotesPanel>>,
    /// The names given to terminals, rather than their programs' titles.
    names: HashMap<TermId, String>,
    /// The name being typed, in its place.
    renaming: Option<Renaming>,
    /// For right-click menus.
    weak: WeakEntity<Self>,
    /// What each terminal's view says, heard while it's here.
    view_subscriptions: HashMap<TermId, Subscription>,
    /// The notes getting the focus.
    _notes_focus: Option<Subscription>,
}

impl EventEmitter<TerminalAreaEvent> for TerminalArea {}

impl TerminalArea {
    /// Its terminals' group in the agent: the workspace's folder.
    pub fn group(&self) -> &str {
        &self.group
    }

    pub fn new(cwd: PathBuf, client: Option<Arc<Client>>, local: bool, cx: &mut Context<Self>) -> Self {
        Self {
            client,
            group: cwd.to_string_lossy().into_owned(),
            cwd,
            tabs: Vec::new(),
            views: HashMap::new(),
            active: 0,
            next_id: 0,
            terminal_drop: None,
            drag_origin: None,
            body_size: Rc::default(),
            local,
            panel_tabs: Vec::new(),
            debug_term: None,
            notes: None,
            names: HashMap::new(),
            renaming: None,
            weak: cx.entity().downgrade(),
            view_subscriptions: HashMap::new(),
            _notes_focus: None,
        }
    }

    /// The debugger's terminal from before (saved by the debugger), for
    /// `restore` to leave out of the tabs.
    pub fn set_debug_term(&mut self, term: Option<TermId>) {
        self.debug_term = term;
    }

    /// Restores the agent's live terminals with the saved layout; if there are
    /// none, opens one. The debugger's goes to its console.
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let group = self.group.clone();
        let debug_term = self.debug_term;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Restored> = async {
                let mut alive: HashSet<TermId> = agent::list(&client, group.clone()).await?.into_iter().collect();
                let debug = debug_term.filter(|term| alive.remove(term));
                let saved = with_layouts(|layouts| layouts.groups.get(&group).cloned()).unwrap_or_default();
                let mut names = saved.names;
                let tabs: Vec<(Tree<Pane>, Option<String>)> = saved
                    .tabs
                    .into_iter()
                    .filter_map(SavedTab::into_tab)
                    // A tab of one terminal named before names were theirs.
                    .map(|(tree, name)| match (&tree, name) {
                        (Tree::Leaf(Pane::Term(term)), Some(name)) => {
                            names.entry(*term).or_insert(name);
                            (tree, None)
                        }
                        (_, name) => (tree, name),
                    })
                    .collect();
                let mut tabs = keep_terms(tabs, &alive);
                let placed: HashSet<TermId> = tabs.iter().flat_map(|(tree, _)| tree.leaves()).filter_map(Pane::term).collect();
                // Those still alive but not saved, each in its own tab.
                let mut orphans: Vec<TermId> = alive.difference(&placed).copied().collect();
                orphans.sort();
                tabs.extend(orphans.into_iter().map(|term| (Tree::Leaf(Pane::Term(term)), None)));

                let terms: Vec<TermId> =
                    debug.into_iter().chain(tabs.iter().flat_map(|(tree, _)| tree.leaves()).filter_map(Pane::term)).collect();
                // One that ended since it was listed is left out, not the rest.
                let terminals: Vec<(TermId, Entity<Terminal>)> = agent::attach_all(&client, &terms, cx)
                    .await
                    .into_iter()
                    .filter_map(|(term, terminal)| Some((term, terminal.ok()?)))
                    .collect();
                let attached: HashSet<TermId> = terminals.iter().map(|(term, _)| *term).collect();
                let tabs = keep_terms(tabs, &attached);
                names.retain(|term, _| attached.contains(term));
                Ok((tabs, names, terminals))
            }
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok((tabs, names, terminals)) => {
                    this.names = names;
                    for (term, terminal) in terminals {
                        let view = this.add_view(term, terminal, window, cx);
                        if Some(term) == this.debug_term {
                            cx.emit(TerminalAreaEvent::DebugTerminal(Some(view)));
                        }
                    }
                    // gone with the agent's restart
                    if debug_term.is_some_and(|term| !this.views.contains_key(&term)) {
                        this.debug_term = None;
                    }
                    if tabs.is_empty() {
                        this.open(Place::NewTab, window, cx);
                        return;
                    }
                    for (tree, name) in tabs {
                        let id = this.next_tab_id();
                        let active = tree.leaves()[0];
                        this.tabs.push(TerminalTab { id, tree, active, name });
                    }
                    this.active = 0;
                    this.save();
                    cx.notify();
                }
                Err(err) => this.error(format!("Couldn't restore terminals: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Switches to a new connection with the agent (after reconnecting): each
    /// terminal reattaches to its process; those that no longer exist are
    /// removed, and those the agent has that aren't here (an agent restarted
    /// took them from one of an older protocol) open each in its own tab.
    pub fn set_client(&mut self, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        self.client = Some(client.clone());
        let group = self.group.clone();
        let terminals: Vec<(TermId, Entity<Terminal>)> = self
            .views
            .iter()
            .map(|(term, view)| (*term, view.read(cx).terminal().clone()))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            // Not knowing which are alive isn't none being so: they stay as
            // they are, with their layout, for the next connection to find.
            let alive: HashSet<TermId> = match agent::list(&client, group).await {
                Ok(terms) => terms.into_iter().collect(),
                Err(err) => {
                    this.update(cx, |this, cx| this.error(format!("Couldn't reattach terminals: {err:#}"), cx)).ok();
                    return;
                }
            };
            let (living, dead): (Vec<_>, Vec<_>) = terminals.into_iter().partition(|(term, _)| alive.contains(term));
            let mut gone: Vec<TermId> = dead.into_iter().map(|(term, _)| term).collect();
            gone.extend(agent::reattach_all(&client, living, cx).await);
            // Read now: one opened here meanwhile is already in `views`.
            let known: HashSet<TermId> = this.update(cx, |this, _| this.views.keys().copied().collect()).unwrap_or_default();
            let mut new: Vec<TermId> = alive.difference(&known).copied().collect();
            new.sort();
            let added: Vec<(TermId, Entity<Terminal>)> = agent::attach_all(&client, &new, cx)
                .await
                .into_iter()
                .filter_map(|(term, terminal)| Some((term, terminal.ok()?)))
                .collect();
            this.update_in(cx, |this, window, cx| {
                for term in gone {
                    this.remove(Pane::Term(term), window, cx);
                }
                for (term, terminal) in added {
                    // Opened here while this attached it too: the duplicate is
                    // dropped, which ends only its own subscription.
                    if this.views.contains_key(&term) {
                        continue;
                    }
                    this.add_view(term, terminal, window, cx);
                    let id = this.next_tab_id();
                    this.tabs.push(TerminalTab { id, tree: Tree::Leaf(Pane::Term(term)), active: Pane::Term(term), name: None });
                }
                this.save();
                if this.tabs.is_empty() {
                    this.open(Place::NewTab, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn next_tab_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn error(&mut self, message: String, cx: &mut Context<Self>) {
        cx.emit(TerminalAreaEvent::Message(message.into()));
    }

    /// Opens in the browser a URL from a terminal, forwarding its port if it
    /// points at the server.
    fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return cx.open_url(&url);
        };
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { client.local_url(&url) }).await;
            this.update(cx, |this, cx| match result {
                Ok(url) => cx.open_url(&url),
                Err(err) => this.error(format!("{err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    fn add_view(&mut self, term: TermId, terminal: Entity<Terminal>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TerminalView> {
        let local = self.local;
        let view = cx.new(|cx| TerminalView::new(terminal, local, window, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event, window, cx| match event {
            TerminalViewEvent::TitleChanged => cx.notify(),
            TerminalViewEvent::Exited => this.remove(Pane::Term(term), window, cx),
            TerminalViewEvent::Focused => this.select(Pane::Term(term), cx),
            TerminalViewEvent::OpenPath { path, line, column } => cx.emit(TerminalAreaEvent::OpenPath {
                path: path.clone(),
                line: *line,
                column: *column,
            }),
            TerminalViewEvent::OpenUrl(url) => this.open_url(url.clone(), cx),
        });
        self.view_subscriptions.insert(term, subscription);
        self.views.insert(term, view.clone());
        view
    }

    /// Creates a terminal in the agent and places it; once it arrives, it gets focus.
    fn open(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        self.open_running(place, None, None, true, window, cx).detach();
    }

    /// Like `open`, typing `line` and Enter into its shell once it exists.
    /// Resolves to the new terminal.
    fn open_running(
        &mut self,
        place: Place,
        dir: Option<PathBuf>,
        line: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<TermId>> {
        let Some(client) = self.client.clone() else {
            self.error("No agent: can't open terminals".into(), cx);
            return Task::ready(None);
        };
        let place = if self.tabs.is_empty() { Place::NewTab } else { place };
        let group = self.group.clone();
        let cwd = self.cwd.clone();
        let size = self.body_size.get().map_or((80, 24), |size| {
            // A terminal to the side or below takes half the space.
            let size = match place {
                Place::Split(Axis::Row) => gpui_kit::size(size.width / 2., size.height),
                Place::Split(Axis::Column) => gpui_kit::size(size.width, size.height / 2.),
                Place::NewTab => size,
            };
            grid_for(size, window, cx)
        });
        // A split opens where the terminal it splits is; a new tab, in the task's folder.
        let beside = match place {
            Place::Split(_) => self.tabs.get(self.active).and_then(|tab| tab.active.term()),
            Place::NewTab => None,
        };
        cx.spawn_in(window, async move |this, cx| {
            let inherited = match (dir, beside) {
                (Some(dir), _) => Some(dir).filter(|dir| *dir != cwd),
                (None, Some(term)) => agent::cwd(&client, term).await.ok().flatten().filter(|dir| *dir != cwd),
                (None, None) => None,
            };
            let result = match inherited {
                // The directory may be gone: then the task's folder.
                Some(dir) => match agent::create(client.clone(), group.clone(), dir, size, cx).await {
                    Ok(created) => Ok(created),
                    Err(_) => agent::create(client, group, cwd, size, cx).await,
                },
                None => agent::create(client, group, cwd, size, cx).await,
            };
            this.update_in(cx, |this, window, cx| {
                let (term, terminal) = match result {
                    Ok(created) => created,
                    Err(err) => {
                        this.error(format!("Couldn't open terminal: {err:#}"), cx);
                        return None;
                    }
                };
                if let Some(line) = line {
                    terminal.update(cx, |terminal, cx| terminal.input(format!("{line}\r").into_bytes(), cx));
                }
                let view = this.add_view(term, terminal, window, cx);
                match place {
                    Place::Split(axis) if !this.tabs.is_empty() => {
                        let tab = &mut this.tabs[this.active];
                        tab.tree.split(tab.active, Pane::Term(term), axis);
                        tab.active = Pane::Term(term);
                    }
                    _ => {
                        let id = this.next_tab_id();
                        this.tabs.push(TerminalTab {
                            id,
                            tree: Tree::Leaf(Pane::Term(term)),
                            active: Pane::Term(term),
                            name: None,
                        });
                        this.active = this.tabs.len() - 1;
                    }
                }
                if focus {
                    view.read(cx).focus_handle(cx).focus(window, cx);
                }
                this.save();
                cx.notify();
                Some(term)
            })
            .ok()
            .flatten()
        })
    }

    /// Runs `line` in the shell of terminal `term` when it's still open, or
    /// of a new tab, and shows it without taking the focus. Resolves to the
    /// terminal it runs in.
    pub fn run_line(
        &mut self,
        term: Option<TermId>,
        line: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<TermId>> {
        if let Some(term) = term
            && self.type_line(term, &line, cx)
        {
            self.select(Pane::Term(term), cx);
            return Task::ready(Some(term));
        }
        self.open_running(Place::NewTab, None, Some(line), false, window, cx)
    }

    /// Types `line` and Enter in the shell of terminal `term`, if it's here
    /// and still open; false if it isn't.
    fn type_line(&self, term: TermId, line: &str, cx: &mut App) -> bool {
        let Some(terminal) = self.views.get(&term).map(|view| view.read(cx).terminal().clone()) else {
            return false;
        };
        if terminal.read(cx).exited() {
            return false;
        }
        terminal.update(cx, |terminal, cx| terminal.input(format!("{line}\r").into_bytes(), cx));
        true
    }

    /// Runs `line` in the shell of the debugger's terminal, which its console
    /// draws: `term` when it's that one and still open, or a new one. The one
    /// before, still running something, is closed with it. Resolves to the
    /// terminal it runs in.
    pub fn run_debug(
        &mut self,
        term: Option<TermId>,
        line: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<TermId>> {
        if let Some(term) = term.filter(|term| Some(*term) == self.debug_term)
            && self.type_line(term, &line, cx)
        {
            return Task::ready(Some(term));
        }
        if let Some(before) = self.debug_term {
            self.close_term(before, window, cx);
        }
        let Some(client) = self.client.clone() else {
            self.error("No agent: can't open terminals".into(), cx);
            return Task::ready(None);
        };
        let (group, cwd) = (self.group.clone(), self.cwd.clone());
        let size = self.body_size.get().map_or((80, 24), |size| grid_for(size, window, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = agent::create(client, group, cwd, size, cx).await;
            this.update_in(cx, |this, window, cx| {
                let (term, terminal) = match result {
                    Ok(created) => created,
                    Err(err) => {
                        this.error(format!("Couldn't open terminal: {err:#}"), cx);
                        return None;
                    }
                };
                terminal.update(cx, |terminal, cx| terminal.input(format!("{line}\r").into_bytes(), cx));
                // two launches at once: the later one's
                if let Some(before) = this.debug_term {
                    this.close_term(before, window, cx);
                }
                let view = this.add_view(term, terminal, window, cx);
                this.debug_term = Some(term);
                cx.emit(TerminalAreaEvent::DebugTerminal(Some(view)));
                Some(term)
            })
            .ok()
            .flatten()
        })
    }

    /// `den term new`: a terminal in a new tab, or split from `beside` (the
    /// one the command ran in, if it's here), typing `line` in its shell.
    pub fn open_for_command(
        &mut self,
        beside: Option<TermId>,
        split: Option<Axis>,
        line: Option<String>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<TermId>> {
        if let Some(term) = beside.filter(|term| self.views.contains_key(term)) {
            self.select(Pane::Term(term), cx);
        }
        let place = split.map_or(Place::NewTab, Place::Split);
        self.open_running(place, None, line, focus, window, cx)
    }

    /// The terminals by tab, with their titles and whether each is the active one.
    pub fn list(&self, cx: &App) -> Vec<(TermId, String, bool)> {
        let active = self.tabs.get(self.active).and_then(|tab| tab.active.term());
        self.tabs
            .iter()
            .flat_map(TerminalTab::terms)
            .filter_map(|term| {
                let title = self.views.get(&term)?.read(cx).title(cx);
                Some((term, title, Some(term) == active))
            })
            .collect()
    }

    /// Shows terminal `term` and gives it the keyboard; false if it isn't here.
    pub fn focus_term(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.views.get(&term).cloned() else {
            return false;
        };
        self.select(Pane::Term(term), cx);
        view.read(cx).focus_handle(cx).focus(window, cx);
        true
    }

    /// Sends Ctrl-C to terminal `term`, which stops what runs in its shell.
    pub fn interrupt(&mut self, term: TermId, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get(&term).cloned() {
            let terminal = view.read(cx).terminal().clone();
            terminal.update(cx, |terminal, cx| terminal.input(vec![0x03], cx));
        }
    }

    pub fn set_panel_tabs(&mut self, tabs: Vec<PanelTab>, cx: &mut Context<Self>) {
        if !tabs.iter().map(PanelTab::key).eq(self.panel_tabs.iter().map(PanelTab::key)) {
            self.panel_tabs = tabs;
            cx.notify();
        }
    }

    /// The panel's tab showing rather than the terminals, if one is.
    fn panel_showing(&self) -> Option<&PanelTab> {
        self.panel_tabs.iter().find(|tab| tab.showing)
    }

    /// Opens a terminal in a new tab and focuses it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Place::NewTab, window, cx);
    }

    /// Opens a terminal in a new tab in `dir` (Open in Terminal), and focuses it.
    pub fn new_terminal_in(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_running(Place::NewTab, Some(dir), None, true, window, cx).detach();
    }

    /// Splits the active terminal: to the right (`Row`) or below (`Column`).
    pub fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Place::Split(axis), window, cx);
    }

    /// Moves focus to the neighboring terminal in `direction`.
    pub fn focus_neighbor(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if let Some(handle) = tab.tree.neighbor(tab.active, direction).and_then(|pane| self.focus_handle(pane, cx)) {
            handle.focus(window, cx);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The panes in sight, in their splits' order (left to right, top to
    /// bottom), for focus to go through; none while a panel's tab is in front.
    pub fn panes_in_sight(&self, cx: &App) -> Vec<FocusHandle> {
        let Some(tab) = self.tabs.get(self.active).filter(|_| self.panel_showing().is_none()) else {
            return Vec::new();
        };
        tab.tree.leaves().into_iter().filter_map(|pane| self.focus_handle(pane, cx)).collect()
    }

    /// Whether any terminal, or the notes in a split, has focus.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.views.values().any(|view| view.read(cx).focus_handle(cx).is_focused(window))
            || (self.notes_split().is_some() && self.focus_handle(Pane::Notes, cx).is_some_and(|handle| handle.is_focused(window)))
    }

    /// Focuses the active pane; if there is none, opens a terminal.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active).and_then(|tab| self.focus_handle(tab.active, cx)) {
            Some(handle) => handle.focus(window, cx),
            None => self.new_terminal(window, cx),
        }
    }

    fn focus_handle(&self, pane: Pane, cx: &App) -> Option<FocusHandle> {
        match pane {
            Pane::Term(term) => Some(self.views.get(&term)?.read(cx).focus_handle(cx)),
            Pane::Notes => Some(self.notes.as_ref()?.read(cx).focus_handle(cx)),
        }
    }

    /// The workspace's notes, for a split to show them; they're selected
    /// there when they get the focus.
    pub fn set_notes(&mut self, notes: Entity<NotesPanel>, window: &mut Window, cx: &mut Context<Self>) {
        let handle = notes.read(cx).focus_handle(cx);
        let subscription = cx.on_focus_in(&handle, window, |this, _, cx| {
            if this.notes_split().is_some() {
                this.select(Pane::Notes, cx);
            }
        });
        self._notes_focus = Some(subscription);
        self.notes = Some(notes);
    }

    /// The tab the notes are split into, if they are.
    pub fn notes_split(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.contains(Pane::Notes))
    }

    /// Whether the notes show in a split, rather than behind another tab or
    /// a panel's.
    pub fn notes_in_sight(&self) -> bool {
        self.panel_showing().is_none() && self.notes_split() == Some(self.active)
    }

    /// The notes' split comes to the front, with them as its active pane.
    pub fn reveal_notes(&mut self, cx: &mut Context<Self>) {
        self.select(Pane::Notes, cx);
    }

    /// The notes leave their split: they're their tab again (or go to a
    /// tab of the code).
    pub fn unsplit_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remove(Pane::Notes, window, cx);
    }

    /// Closes the active pane: a terminal, killing its process, or the
    /// notes, back to their tab.
    pub fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.tabs.get(self.active).map(|tab| tab.active) else {
            return;
        };
        self.close_pane(pane, window, cx);
    }

    /// Closes a pane: a terminal, killing its process, or the notes, back to
    /// their tab.
    fn close_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        match pane {
            Pane::Term(term) => self.close_term(term, window, cx),
            Pane::Notes => self.remove(pane, window, cx),
        }
    }

    /// Closes a specific terminal and kills its process.
    fn close_term(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get(&term) {
            view.read(cx).terminal().read(cx).kill();
        }
        self.remove(Pane::Term(term), window, cx);
    }

    /// Closes a tab with all its terminals; the notes in it go back to their tab.
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        for pane in tab.tree.leaves() {
            self.close_pane(pane, window, cx);
        }
    }

    /// Closes every tab but the one at `ix`, with their terminals.
    fn close_other_tabs(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let had_focus = self.contains_focus(window, cx);
        let others: Vec<Pane> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != ix)
            .flat_map(|(_, tab)| tab.tree.leaves())
            .collect();
        for pane in others {
            self.close_pane(pane, window, cx);
        }
        // The one left is the active one, focused if a closed one was.
        if had_focus {
            self.activate_tab(0, window, cx);
        } else {
            self.active = 0;
            cx.notify();
        }
    }

    /// Types the tab's name in its place (Rename Tab, or a double click):
    /// of one terminal alone, the terminal's.
    fn start_rename(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
            return;
        };
        let named = match tab.tree {
            Tree::Leaf(Pane::Term(term)) => Named::Term(term),
            _ => Named::Tab(id),
        };
        let title = self.tab_title(tab, cx);
        self.type_name(named, title, window, cx);
    }

    /// Types a terminal's name: in its pane's title, or in its tab when that
    /// doesn't show.
    fn start_term_rename(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title(Pane::Term(term), cx);
        self.type_name(Named::Term(term), title, window, cx);
    }

    fn type_name(&mut self, named: Named, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
            InputEvent::Blur => this.cancel_rename(cx),
            _ => {}
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.renaming = Some(Renaming { named, input, _subscription: subscription });
        cx.notify();
    }

    /// Names terminal `term` (the Agents panel's Rename); false if it isn't
    /// here.
    pub fn rename_term(&mut self, term: TermId, name: Option<String>, cx: &mut Context<Self>) -> bool {
        if !self.views.contains_key(&term) {
            return false;
        }
        self.set_name(Named::Term(term), name);
        cx.notify();
        true
    }

    fn set_name(&mut self, named: Named, name: Option<String>) {
        match named {
            Named::Tab(id) => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.name = name;
                }
            }
            Named::Term(term) => match name {
                Some(name) => {
                    self.names.insert(term, name);
                }
                None => {
                    self.names.remove(&term);
                }
            },
        }
        self.save();
    }

    /// The typed name is given; none, and it's its title again.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        let name = renaming.input.read(cx).value().trim().to_string();
        self.set_name(renaming.named, Some(name).filter(|name| !name.is_empty()));
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The name being typed in `tab`'s place: its own, or one of its
    /// terminals' without a title of its own in sight.
    fn renaming_tab(&self, tab: &TerminalTab, cx: &App) -> Option<Entity<InputState>> {
        let renaming = self.renaming.as_ref()?;
        let here = match renaming.named {
            Named::Tab(id) => id == tab.id,
            Named::Term(term) => tab.contains(Pane::Term(term)) && !self.pane_titles(tab, cx),
        };
        here.then(|| renaming.input.clone())
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.renaming.take().is_some() {
            cx.notify();
        }
    }

    /// What a tab says: split, its name, or with its panes' titles in sight
    /// the workspace's folder, so it doesn't repeat one of them; else its
    /// pane's title.
    fn tab_title(&self, tab: &TerminalTab, cx: &App) -> String {
        if matches!(tab.tree, Tree::Split { .. })
            && let Some(name) = &tab.name
        {
            return name.clone();
        }
        match self.pane_titles(tab, cx) {
            true => self.cwd.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| self.group.clone()),
            false => self.title(tab.active, cx),
        }
    }

    /// Whether `tab`'s panes each show their title above them.
    fn pane_titles(&self, tab: &TerminalTab, cx: &App) -> bool {
        matches!(tab.tree, Tree::Split { .. }) && !Config::get(cx).hide_pane_titles
    }

    /// What a pane's title and its tab's say.
    fn title(&self, pane: Pane, cx: &App) -> String {
        match pane {
            Pane::Term(term) => match self.names.get(&term) {
                Some(name) => name.clone(),
                None => self.views.get(&term).map(|view| view.read(cx).title(cx)).unwrap_or_default(),
            },
            Pane::Notes => "Notes".to_string(),
        }
    }

    /// Splits next to `pane` (the one that was clicked).
    fn split_at(&mut self, pane: Pane, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.select(pane, cx);
        self.split(axis, window, cx);
    }

    /// The notes' pane menu: a terminal beside them, or them out of the split.
    fn notes_menu(&self, menu: PopupMenu) -> PopupMenu {
        let area = self.weak.clone();
        menu.item(
            menu::item("Split Right", &area, |this, window, cx| this.split_at(Pane::Notes, Axis::Row, window, cx))
                .action(Box::new(SplitRight)),
        )
        .item(
            menu::item("Split Down", &area, |this, window, cx| this.split_at(Pane::Notes, Axis::Column, window, cx))
                .action(Box::new(SplitDown)),
        )
        .separator()
        .item(menu::item("Open in Editor Tab", &area, |_, _, cx| cx.emit(TerminalAreaEvent::ToEditorTab(Panel::Notes))))
        .item(menu::item("Close Split", &area, |this, window, cx| this.unsplit_notes(window, cx)).action(Box::new(CloseTab)))
    }

    /// What the menu offers on a path or a URL that was right-clicked.
    fn link_items(&self, menu: PopupMenu, view: Entity<TerminalView>, link: Link, cx: &App) -> PopupMenu {
        let area = self.weak.clone();
        let (open, copy_label, copy) = match &link {
            Link::Url(url) => ("Open Link", "Copy Link", url.clone()),
            Link::Path { path, .. } => ("Open", "Copy Path", path.to_string_lossy().into_owned()),
        };
        let reveal = match &link {
            Link::Path { path, .. } => Some((path.clone(), view.read(cx).is_local())),
            Link::Url(_) => None,
        };
        menu.item(menu::item(open, &area, move |_, _, cx| {
            view.update(cx, |view, cx| view.open_link(link.clone(), cx))
        }))
        .when_some(reveal, |menu, (path, local)| {
            menu.item(menu::item("Reveal in Finder", &area, move |_, _, cx| cx.reveal_path(&path)).disabled(!local))
        })
        .item(menu::item(copy_label, &area, move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
        }))
        .separator()
    }

    fn pane_menu(&self, term: TermId, menu: PopupMenu, cx: &App) -> PopupMenu {
        let area = self.weak.clone();
        let view = self.views.get(&term).cloned();
        let has_selection = view.as_ref().is_some_and(|view| view.read(cx).has_selection(cx));
        let copy_view = view.clone();
        let unwrap_view = view.clone();
        let paste_view = view.clone();
        let select_view = view.clone();
        let clear_view = view.clone();
        // Copy and Paste have their shortcuts in the terminal's context.
        let menu = match &view {
            Some(view) => menu.action_context(view.focus_handle(cx)),
            None => menu,
        };
        let link = view.as_ref().and_then(|view| Some((view.clone(), view.read(cx).context_link()?.clone())));
        menu.when_some(link, |menu, (view, link)| self.link_items(menu, view, link, cx))
            .item(
            menu::item("Copy", &area, move |_, _, cx| {
                let text = copy_view
                    .as_ref()
                    .and_then(|view| view.read(cx).terminal().read(cx).selection_text());
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })
            .disabled(!has_selection)
            .action(Box::new(ui_term::Copy)),
        )
        // Without the breaks and the indentation Claude Code puts to fit the width.
        .item(
            menu::item("Copy Unwrapped", &area, move |_, _, cx| {
                let text = unwrap_view
                    .as_ref()
                    .and_then(|view| view.read(cx).terminal().read(cx).selection_text_unwrapped());
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })
            .disabled(!has_selection),
        )
        .item(
            menu::item("Paste", &area, move |_, _, cx| {
                if let Some(view) = &paste_view {
                    view.update(cx, |view, cx| view.paste_clipboard(cx));
                }
            })
            .action(Box::new(ui_term::Paste)),
        )
        .item(menu::item("Select All", &area, move |_, _, cx| {
            if let Some(view) = &select_view {
                view.read(cx).terminal().clone().update(cx, |terminal, cx| terminal.select_all(cx));
            }
        }))
        .separator()
        .item(menu::item("Clear", &area, move |_, _, cx| {
            if let Some(view) = &clear_view {
                view.read(cx).terminal().clone().update(cx, |terminal, cx| terminal.clear(cx));
            }
        }))
        .separator()
        .item(
            menu::item("Split Right", &area, move |this, window, cx| {
                this.split_at(Pane::Term(term), Axis::Row, window, cx)
            })
            .action(Box::new(SplitRight)),
        )
        .item(
            menu::item("Split Down", &area, move |this, window, cx| {
                this.split_at(Pane::Term(term), Axis::Column, window, cx)
            })
            .action(Box::new(SplitDown)),
        )
        .item(menu::item("New Tab", &area, |this, window, cx| this.new_terminal(window, cx)).action(Box::new(NewTerminal)))
        .separator()
        .item(menu::item("Rename", &area, move |this, window, cx| this.start_term_rename(term, window, cx)))
        .item(
            menu::item("Close Terminal", &area, move |this, window, cx| this.close_term(term, window, cx))
                .action(Box::new(CloseTab)),
        )
        .separator()
        .item(notes_item(&area))
    }

    fn select(&mut self, pane: Pane, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.contains(pane)) {
            self.active = ix;
            self.tabs[ix].active = pane;
            cx.notify();
        }
    }

    fn remove(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self.focus_handle(pane, cx).is_some_and(|handle| handle.is_focused(window));
        if let Pane::Term(term) = pane {
            self.views.remove(&term);
            self.view_subscriptions.remove(&term);
            self.names.remove(&term);
            if self.debug_term.take_if(|debug| *debug == term).is_some() {
                cx.emit(TerminalAreaEvent::DebugTerminal(None));
                cx.notify();
                return;
            }
        }
        let Some(tab_ix) = self.tabs.iter().position(|tab| tab.contains(pane)) else {
            return;
        };
        let tab = self.tabs.remove(tab_ix);
        match tab.tree.clone().remove(pane).map(|tree| TerminalTab { id: tab.id, tree, active: tab.active, name: tab.name.clone() }) {
            Some(mut left) if !left.notes_only() => {
                // Focus moves to a neighbor of the one being closed.
                if tab.active == pane {
                    left.active = [Direction::Left, Direction::Up, Direction::Right, Direction::Down]
                        .into_iter()
                        .find_map(|direction| tab.tree.neighbor(pane, direction))
                        .unwrap_or(left.tree.leaves()[0]);
                }
                self.tabs.insert(tab_ix, left);
            }
            _ => self.active = self.active.min(self.tabs.len().saturating_sub(1)),
        }
        if had_focus && !self.focus_active(window, cx) {
            cx.emit(TerminalAreaEvent::FocusLeft);
        }
        if pane == Pane::Notes || tab.contains(Pane::Notes) {
            cx.emit(TerminalAreaEvent::NotesMoved);
        }
        self.save();
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The active tab's active pane gets the focus, if there's one.
    fn focus_active(&self, window: &mut Window, cx: &mut App) -> bool {
        let handle = self.tabs.get(self.active).and_then(|tab| self.focus_handle(tab.active, cx));
        if let Some(handle) = &handle {
            handle.focus(window, cx);
        }
        handle.is_some()
    }

    /// Saves which terminal goes where.
    fn save(&self) {
        // Offscreen interaction tests must never overwrite the user's layout.
        if cfg!(test) {
            return;
        }
        let saved = SavedLayout {
            tabs: self.tabs.iter().map(SavedTab::of).collect(),
            names: self.names.clone(),
        };
        with_layouts(|layouts| {
            layouts.groups.insert(self.group.clone(), saved);
            layouts.store();
        });
    }

    fn render_tab_bar(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        // The ones that close (the debugger's) after the terminals'; the
        // ones always there (the notes') apart, at the far end.
        let (closable, fixed): (Vec<_>, Vec<_>) = self.panel_tabs.iter().partition(|tab| tab.closable);
        let closable: Vec<AnyElement> = closable.into_iter().map(|tab| self.render_panel_tab(tab, cx)).collect();
        let fixed: Vec<AnyElement> = fixed.into_iter().map(|tab| self.render_panel_tab(tab, cx)).collect();
        let theme = cx.theme();
        h_flex()
            .id("terminal-tabs")
            .h(px(34.))
            .flex_none()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .overflow_x_scroll()
            .on_drop(cx.listener(|this, drag: &TerminalDrag, window, cx| this.drop_on_bar(drag, None, window, cx)))
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let active = ix == self.active && self.panel_showing().is_none();
                let title = self.tab_title(tab, cx);
                let count = tab.tree.leaves().len();
                let id = tab.id;
                let input = self.renaming_tab(tab, cx);
                h_flex()
                    .id(("terminal-tab", tab.id))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("terminal-tab-{id}")))
                    // Its name's text is selected by dragging, not the tab.
                    .when(input.is_none(), |el| self.draggable(el, Source::Tab(id), title.clone().into()))
                    .drag_over::<TerminalDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TerminalDrag>, window, cx| {
                        this.hover_tab(id, event, window, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &TerminalDrag, window, cx| {
                        this.drop_on_bar(drag, Some(id), window, cx);
                    }))
                    .group("terminal-tab")
                    .h_full()
                    .flex_none()
                    .max_w(px(220.))
                    .pl_3()
                    .pr_1()
                    .gap_1()
                    .text_ui(cx)
                    .border_r_1()
                    .border_color(theme.border)
                    .when(active, |el| el.bg(theme.tab_active).text_color(theme.tab_active_foreground))
                    .when(!active, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
                    .child(match input {
                        Some(input) => div()
                            .w(px(140.))
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    cx.stop_propagation();
                                    this.cancel_rename(cx);
                                }
                            }))
                            .child(Input::new(&input).xsmall())
                            .into_any_element(),
                        None => div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title).into_any_element(),
                    })
                    .when(count > 1, |el| el.child(layout_glyph(&tab.tree, tab.active, active && focused, cx)))
                    // As the editor's tabs: on the active one, or on hover.
                    .child(
                        div()
                            .id(("terminal-tab-close", id))
                            .flex_none()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .hover(|style| style.bg(theme.muted))
                            .child(
                                svg()
                                    .path("icons/tab-close.svg")
                                    .size(px(14.))
                                    .text_color(theme.muted_foreground)
                                    .when(!active, |el| el.invisible().group_hover("terminal-tab", |s| s.visible())),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if this.tabs.get(ix).is_some_and(|tab| this.renaming_tab(tab, cx).is_some()) {
                            return;
                        }
                        if this.panel_showing().is_some() {
                            cx.emit(TerminalAreaEvent::ShowPanel(None));
                        }
                        this.activate_tab(ix, window, cx);
                        if event.click_count() >= 2 {
                            this.start_rename(id, window, cx);
                        }
                    }))
                    .context_menu({
                        let area = self.weak.clone();
                        let alone = self.tabs.len() == 1;
                        let notes = tab.contains(Pane::Notes);
                        move |menu, window, cx| {
                            menu.item(
                                menu::item("New Terminal", &area, |this, window, cx| this.new_terminal(window, cx))
                                    .action(Box::new(NewTerminal)),
                            )
                            .item(
                                menu::item("Split Right", &area, move |this, window, cx| {
                                    this.activate_tab(ix, window, cx);
                                    this.split(Axis::Row, window, cx);
                                })
                                .action(Box::new(SplitRight)),
                            )
                            .item(
                                menu::item("Split Down", &area, move |this, window, cx| {
                                    this.activate_tab(ix, window, cx);
                                    this.split(Axis::Column, window, cx);
                                })
                                .action(Box::new(SplitDown)),
                            )
                                .separator()
                                .item(menu::item("Rename Tab", &area, move |this, window, cx| {
                                    this.start_rename(id, window, cx)
                                }))
                                .separator()
                                .item(menu::item("Close Tab", &area, move |this, window, cx| {
                                    this.close_tab(ix, window, cx)
                                }))
                                .item(
                                    menu::item("Close Other Tabs", &area, move |this, window, cx| {
                                        this.close_other_tabs(ix, window, cx)
                                    })
                                    .disabled(alone),
                                )
                                .separator()
                                .map(|menu| match notes {
                                    true => menu
                                        .item(menu::item("Open Notes in Editor Tab", &area, |_, _, cx| {
                                            cx.emit(TerminalAreaEvent::ToEditorTab(Panel::Notes))
                                        }))
                                        .item(menu::item("Close Notes Split", &area, |this, window, cx| {
                                            this.unsplit_notes(window, cx)
                                        })),
                                    false => menu.item(notes_item(&area)),
                                })
                                .panel_items(hide_item(&area), window, cx)
                        }
                    })
            }))
            .children(closable)
            .child(
                div()
                    .id("new-terminal")
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.text_color(theme.foreground))
                    .child("+")
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.panel_showing().is_some() {
                            cx.emit(TerminalAreaEvent::ShowPanel(None));
                        }
                        this.new_terminal(window, cx)
                    })),
            )
            .child(
                div()
                    .id("terminal-tab-end")
                    .h_full()
                    .flex_1()
                    .min_w(px(24.))
                    .when(cfg!(test), |el| el.debug_selector(|| "terminal-tab-end".into()))
                    .drag_over::<TerminalDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .context_menu({
                        let area = self.weak.clone();
                        move |menu, window, cx| {
                            let bottom = Config::get(cx).layout.dock == crate::config::Dock::Bottom;
                            menu.item(
                                PopupMenuItem::new(if bottom { "Move to the Right" } else { "Move Under the Code" })
                                    .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::MoveTerminals), cx)),
                            )
                            .separator()
                            .item(notes_item(&area))
                            .panel_items(hide_item(&area), window, cx)
                        }
                    }),
            )
            .children(fixed)
    }

    /// A panel's tab, after the terminals'.
    fn render_panel_tab(&self, tab: &PanelTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (panel, showing) = (tab.panel, tab.showing);
        let name = match panel {
            Panel::Console => "console",
            Panel::Notes => "notes",
            _ => tab.title,
        };
        let group = SharedString::from(format!("{name}-tab"));
        h_flex()
            .id(SharedString::from(format!("{name}-tab")))
            .when(cfg!(test), |el| el.debug_selector(move || format!("{name}-tab")))
            .group(group.clone())
            // The notes go beside a terminal like one: dragged onto it.
            .when(panel == Panel::Notes, |el| self.draggable(el, Source::Pane(Pane::Notes), tab.title.into()))
            .h_full()
            .flex_none()
            .pl_3()
            .pr_1()
            .gap_1()
            .text_ui(cx)
            // At the far end, the line on its left.
            .when(tab.closable, |el| el.border_r_1())
            .when(!tab.closable, |el| el.border_l_1())
            .border_color(theme.border)
            .when(showing, |el| el.bg(theme.tab_active).text_color(theme.tab_active_foreground))
            .when(!showing, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
            .child(svg().path(tab.icon).size(px(14.)).flex_none().text_color(theme.muted_foreground))
            .child(tab.title)
            .children(tab.dot.map(|color| {
                div()
                    .when(cfg!(test), |el| el.debug_selector(move || format!("{name}-tab-dot")))
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(color)
            }))
            // The notes' tab, with no close button, takes the same room.
            .when(!tab.closable, |el| el.pr_3())
            .when(tab.closable, |el| el.child(
                div()
                    .id(SharedString::from(format!("{name}-tab-close")))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("{name}-tab-close")))
                    .flex_none()
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.muted))
                    .child(
                        svg()
                            .path("icons/tab-close.svg")
                            .size(px(14.))
                            .text_color(theme.muted_foreground)
                            .when(!showing, |el| el.invisible().group_hover(group, |s| s.visible())),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(TerminalAreaEvent::ClosePanel(panel));
                    })),
            ))
            // The notes' tab in front, clicked again, hides them: as their icon.
            .on_click(cx.listener(move |_, _, _, cx| match panel == Panel::Notes && showing {
                true => cx.emit(TerminalAreaEvent::ClosePanel(panel)),
                false => cx.emit(TerminalAreaEvent::ShowPanel(Some(panel))),
            }))
            .context_menu({
                let (area, closable) = (self.weak.clone(), tab.closable);
                // The notes' tab stays: Hide Panel hides the terminals' place,
                // and they can go to a tab of the code.
                move |menu, window, cx| {
                    let hide = match closable {
                        true => menu::item("Hide Panel", &area, move |_, _, cx| cx.emit(TerminalAreaEvent::ClosePanel(panel))),
                        false => hide_item(&area),
                    };
                    menu.when(panel == Panel::Notes, |menu| {
                        menu.item(menu::item("Open in Editor Tab", &area, move |_, _, cx| {
                            cx.emit(TerminalAreaEvent::ToEditorTab(panel))
                        }))
                        .separator()
                    })
                    .panel_items(hide, window, cx)
                }
            })
            .into_any_element()
    }

    /// Renders a branch of the tree; `path` makes each split's ids unique,
    /// `focused` is whether a pane here has the keyboard.
    fn render_tree(&self, tab: &TerminalTab, tree: &Tree<Pane>, path: String, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        match tree {
            Tree::Leaf(pane) => {
                let pane = *pane;
                let body: AnyElement = match pane {
                    Pane::Term(term) => match self.views.get(&term) {
                        Some(view) => view.clone().into_any_element(),
                        None => return div().into_any_element(),
                    },
                    Pane::Notes => match &self.notes {
                        Some(notes) => notes.clone().into_any_element(),
                        None => return div().into_any_element(),
                    },
                };
                let split = self.pane_titles(tab, cx);
                let theme = cx.theme();
                let key = match pane {
                    Pane::Term(term) => term.to_string(),
                    Pane::Notes => "notes".to_string(),
                };
                let this = self.weak.clone();
                v_flex()
                    .id(SharedString::from(format!("terminal-pane-{key}")))
                    .when(cfg!(test), |el| el.debug_selector(|| format!("terminal-pane-{key}")))
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TerminalDrag>, _, cx| {
                        this.track_drop(pane, event, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &TerminalDrag, window, cx| {
                        this.drop_on_pane(drag, pane, window, cx);
                    }))
                    .when(split, |el| {
                        // The tab's active pane: its title lit as the active
                        // tab, its hairline a shade darker while it has the
                        // keyboard. The rest muted, in the panels' grey.
                        let active = pane == tab.active;
                        let title = self.title(pane, cx);
                        let group = SharedString::from(format!("terminal-pane-title-{key}"));
                        let input = self
                            .renaming
                            .as_ref()
                            .filter(|renaming| pane.term().is_some_and(|term| renaming.named == Named::Term(term)))
                            .map(|renaming| renaming.input.clone());
                        let renaming = input.is_some();
                        let header = h_flex()
                                .id(SharedString::from(format!("terminal-pane-handle-{key}")))
                                .when(cfg!(test), |el| el.debug_selector(|| format!("terminal-pane-handle-{key}")))
                                .group(group.clone())
                                .h(px(24.))
                                .flex_none()
                                .pl_2()
                                .pr(px(2.))
                                .gap_1()
                                .text_ui_small(cx)
                                .border_b_1()
                                .border_color(if active && focused { theme.muted_foreground.opacity(0.35) } else { theme.border })
                                .when(active, |el| el.bg(theme.tab_active).text_color(theme.tab_active_foreground))
                                .when(!active, |el| el.bg(theme.sidebar).text_color(theme.muted_foreground))
                                .child(match input {
                                    Some(input) => div()
                                        .flex_1()
                                        .min_w_0()
                                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                            if event.keystroke.key == "escape" {
                                                cx.stop_propagation();
                                                this.cancel_rename(cx);
                                            }
                                        }))
                                        .child(Input::new(&input).xsmall())
                                        .into_any_element(),
                                    None => div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(title.clone())
                                        .into_any_element(),
                                })
                                // As the tabs': on the active one, or on hover.
                                .child(
                                    div()
                                        .id(SharedString::from(format!("terminal-pane-close-{key}")))
                                        .flex_none()
                                        .size(px(18.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(theme.radius)
                                        .hover(|style| style.bg(theme.muted))
                                        .child(
                                            svg()
                                                .path("icons/tab-close.svg")
                                                .size(px(12.))
                                                .text_color(theme.muted_foreground)
                                                .when(!active, |el| el.invisible().group_hover(group, |s| s.visible())),
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.close_pane(pane, window, cx);
                                        })),
                                )
                                .when_some(pane.term().filter(|_| !renaming), |el, term| {
                                    el.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                        if event.click_count() >= 2 {
                                            this.start_term_rename(term, window, cx);
                                        }
                                    }))
                                });
                        // Its name's text is selected by dragging, not the pane.
                        let handle = match renaming {
                            true => header,
                            false => self.draggable(header, Source::Pane(pane), title.into()),
                        };
                        // The notes' editor has its own right-click menu: theirs is on their title.
                        el.child(match pane {
                            Pane::Term(_) => handle.into_any_element(),
                            Pane::Notes => {
                                let this = this.clone();
                                handle
                                    .context_menu(move |menu, window, cx| match this.upgrade() {
                                        Some(area) => area.read(cx).notes_menu(menu).panel_items(hide_item(&this), window, cx),
                                        None => menu,
                                    })
                                    .into_any_element()
                            }
                        })
                    })
                    .child(div().flex_1().min_h_0().w_full().child(body))
                    .when_some(self.terminal_drop.filter(|(target, _)| *target == pane && cx.has_active_drag()), |el, (_, placement)| {
                        el.child(placement.indicator(cx))
                    })
                    .map(|el| match pane {
                        Pane::Term(term) => el
                            .context_menu(move |menu, window, cx| match this.upgrade() {
                                Some(area) => area.read(cx).pane_menu(term, menu, cx).panel_items(hide_item(&this), window, cx),
                                None => menu,
                            })
                            .into_any_element(),
                        Pane::Notes => el.into_any_element(),
                    })
            }
            Tree::Split { axis, children } => {
                let id = SharedString::from(format!("terminal-split-{}-{path}", tab.id));
                let group = match axis {
                    Axis::Row => h_resizable(id),
                    Axis::Column => v_resizable(id),
                };
                group
                    .children(children.iter().enumerate().map(|(ix, child)| {
                        resizable_panel().child(self.render_tree(tab, child, format!("{path}.{ix}"), focused, cx))
                    }))
                    .into_any_element()
            }
        }
    }
}

impl Render for TerminalArea {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.cancel_drag(window, cx);
        }
        let focused = self.contains_focus(window, cx);
        let body = match self.tabs.get(self.active) {
            _ if let Some(tab) = self.panel_showing() => tab.view.clone().into_any_element(),
            None => {
                let theme = cx.theme();
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_ui(cx)
                    .text_color(theme.muted_foreground)
                    .child(if self.client.is_some() {
                        "Cmd-T opens a terminal"
                    } else {
                        "No agent: no terminals"
                    })
                    .into_any_element()
            }
            Some(tab) => self.render_tree(tab, &tab.tree, "0".into(), focused, cx),
        };
        let theme = cx.theme();
        v_flex()
            .id("terminal-area")
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.stop_active_drag(window) {
                    this.cancel_drag(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .size_full()
            .when(cfg!(test), |el| el.debug_selector(|| "terminals".into()))
            .bg(theme.background)
            .child(self.render_tab_bar(focused, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .child({
                        let body_size = self.body_size.clone();
                        canvas(move |bounds, _, _| body_size.set(Some(bounds.size)), |_, _, _, _| {})
                            .absolute()
                            .size_full()
                    }),
            )
    }
}

/// Saved terminal layout, per group.
#[derive(Default, Serialize, Deserialize)]
struct SavedLayouts {
    groups: HashMap<String, SavedLayout>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct SavedLayout {
    tabs: Vec<SavedTab>,
    /// The terminals' names.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    names: HashMap<TermId, String>,
}

/// The saved layouts, read once: the Agents panel asks for the tabs' names
/// of every workspace while it draws, also of those not open.
static LAYOUTS: Mutex<Option<SavedLayouts>> = Mutex::new(None);

fn with_layouts<R>(act: impl FnOnce(&mut SavedLayouts) -> R) -> R {
    let mut layouts = LAYOUTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    act(layouts.get_or_insert_with(SavedLayouts::load))
}

/// The name of terminal `term` of `group`, as saved: of a workspace not
/// open in this window too. Its tab's, alone in one named before names were
/// the terminals'.
pub fn saved_term_name(group: &str, term: TermId) -> Option<String> {
    with_layouts(|layouts| {
        let saved = layouts.groups.get(group)?;
        saved.names.get(&term).cloned().or_else(|| {
            saved.tabs.iter().find_map(|tab| match tab {
                SavedTab::Named { tree, name } if *tree == Tree::Leaf(Pane::Term(term)) => Some(name.clone()),
                _ => None,
            })
        })
    })
}

/// Names terminal `term` of `group`, in a workspace not open
/// (`TerminalArea::rename_term` in one that is).
pub fn rename_saved_term(group: &str, term: TermId, name: Option<String>) {
    with_layouts(|layouts| {
        let saved = layouts.groups.entry(group.to_string()).or_default();
        for tab in &mut saved.tabs {
            if let SavedTab::Named { tree, .. } = tab
                && *tree == Tree::Leaf(Pane::Term(term))
            {
                *tab = SavedTab::Tree(tree.clone());
            }
        }
        match name {
            Some(name) => saved.names.insert(term, name),
            None => saved.names.remove(&term),
        };
        layouts.store();
    });
}

/// Forgets how `group`'s terminals were laid out: its workspace was removed.
pub fn forget_layout(group: &str) {
    with_layouts(|layouts| {
        if layouts.groups.remove(group).is_some() {
            layouts.store();
        }
    });
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum SavedTab {
    /// One renamed.
    Named { tree: Tree<Pane>, name: String },
    Tree(Tree<Pane>),
    /// Old format: a column of terminals.
    Column(Vec<TermId>),
}

impl SavedTab {
    fn of(tab: &TerminalTab) -> Self {
        match &tab.name {
            Some(name) => SavedTab::Named { tree: tab.tree.clone(), name: name.clone() },
            None => SavedTab::Tree(tab.tree.clone()),
        }
    }

    /// Its tree, and its name if it was renamed.
    fn into_tab(self) -> Option<(Tree<Pane>, Option<String>)> {
        match self {
            SavedTab::Named { tree, name } => Some((tree, Some(name))),
            SavedTab::Tree(tree) => Some((tree, None)),
            SavedTab::Column(terms) => match terms.as_slice() {
                [] => None,
                [term] => Some((Tree::Leaf(Pane::Term(*term)), None)),
                _ => Some((
                    Tree::Split {
                        axis: Axis::Column,
                        children: terms.into_iter().map(|term| Tree::Leaf(Pane::Term(term))).collect(),
                    },
                    None,
                )),
            },
        }
    }
}

static FILE: JsonFile = JsonFile::new("layout.json");

impl SavedLayouts {
    fn load() -> Self {
        FILE.load()
    }

    fn store(&self) {
        FILE.save_later(self);
    }
}

/// The tabs with only the terminals in `terms`: one left with none, or with
/// only the notes, goes.
fn keep_terms(tabs: Vec<(Tree<Pane>, Option<String>)>, terms: &HashSet<TermId>) -> Vec<(Tree<Pane>, Option<String>)> {
    tabs.into_iter()
        .filter_map(|(tree, name)| Some((tree.retain(&|pane| pane.term().is_none_or(|term| terms.contains(&term)))?, name)))
        .filter(|(tree, _)| *tree != Tree::Leaf(Pane::Notes))
        .collect()
}

/// Hide Panel: the terminals' place.
fn hide_item(area: &WeakEntity<TerminalArea>) -> menu::PopupMenuItem {
    menu::item("Hide Panel", area, |_, _, cx| cx.emit(TerminalAreaEvent::Hide))
}

/// The notes, a tab of this place (or of the code): in front, to write.
fn notes_item(area: &WeakEntity<TerminalArea>) -> menu::PopupMenuItem {
    menu::item("Show Notes", area, |_, _, cx| cx.emit(TerminalAreaEvent::ShowPanel(Some(Panel::Notes))))
        .action(Box::new(crate::ToggleNotes))
}

/// A split tab's shape in miniature, its active pane filled: where the
/// count of its panes was.
fn layout_glyph(tree: &Tree<Pane>, active: Pane, lit: bool, cx: &App) -> AnyElement {
    fn cell(tree: &Tree<Pane>, active: Pane, lit: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        match tree {
            Tree::Leaf(pane) => {
                let color = if lit { theme.foreground } else { theme.muted_foreground };
                div()
                    .flex_1()
                    .rounded(px(1.5))
                    .border_1()
                    .border_color(color.opacity(0.7))
                    .when(*pane == active, |el| el.bg(color.opacity(0.7)))
                    .into_any_element()
            }
            Tree::Split { axis, children } => div()
                .flex()
                .map(|el| match axis {
                    Axis::Row => el.flex_row(),
                    Axis::Column => el.flex_col(),
                })
                .flex_1()
            .gap(px(1.))
            .children(children.iter().map(|child| cell(child, active, lit, cx)))
            .into_any_element(),
        }
    }
    div().flex_none().w(px(15.)).h(px(11.)).flex().child(cell(tree, active, lit, cx)).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    /// A terminal that couldn't be attached leaves its tab, not the others:
    /// a split keeps the rest, and one alone with the notes goes.
    #[test]
    fn restoring_leaves_out_only_the_terminals_not_attached() {
        let leaf = |term| Tree::Leaf(Pane::Term(term));
        let split = |children| Tree::Split { axis: Axis::Row, children };
        let tabs = vec![
            (split(vec![leaf(1), leaf(2)]), Some("both".to_string())),
            (leaf(3), None),
            (split(vec![leaf(4), Tree::Leaf(Pane::Notes)]), None),
        ];
        let attached: HashSet<TermId> = [1, 3].into_iter().collect();
        assert_eq!(keep_terms(tabs, &attached), vec![(leaf(1), Some("both".to_string())), (leaf(3), None)]);
    }
}
