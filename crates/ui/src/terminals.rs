//! The task's terminal area: tabs, and in each tab terminals split into
//! resizable rows and columns. The processes live in the agent; here we only
//! store which terminal goes where and how big, to restore it when the app is reopened.

mod drag;

#[cfg(test)]
pub(crate) use drag::tests::area as test_area;

use drag::{Source, TerminalDrag};
use crate::drag_drop::DropPlacement;
use crate::menu::PanelItems as _;

use std::{
    cell::{Cell, RefCell},
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
    ResizableState, resizable_panel, v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::TermId;
use serde::{Deserialize, Serialize};
use ui_term::{Terminal, TerminalView, TerminalViewEvent, grid_for, links::Link};

use crate::{
    config::{Config, Panel, UiText, store::JsonFile},
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
    /// A dot of this color on it: the debugger's state.
    pub dot: Option<Hsla>,
}

impl PanelTab {
    fn key(&self) -> (Panel, EntityId, &'static str, bool, Option<Hsla>) {
        (self.panel, self.view.entity_id(), self.icon, self.showing, self.dot)
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
    tree: Tree,
    /// The tab's pane that last had focus.
    active: TermId,
    /// The name given to it when split (Rename Tab), rather than the
    /// workspace's folder; one pane alone is named itself.
    name: Option<String>,
}

impl TerminalTab {
    fn terms(&self) -> Vec<TermId> {
        self.tree.leaves()
    }

    fn contains(&self, term: TermId) -> bool {
        self.tree.leaves().contains(&term)
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
/// names, the splits' shares and the terminals attached.
type Restored = (
    Vec<(Tree, Option<String>)>,
    HashMap<TermId, String>,
    HashMap<Vec<TermId>, Vec<f32>>,
    Vec<(TermId, Entity<Terminal>)>,
);

pub struct TerminalArea {
    client: Option<Arc<Client>>,
    /// Terminal group in the agent (the task's folder).
    group: String,
    cwd: PathBuf,
    tabs: Vec<TerminalTab>,
    views: HashMap<TermId, Entity<TerminalView>>,
    active: usize,
    next_id: usize,
    terminal_drop: Option<(TermId, DropPlacement)>,
    drag_origin: Option<usize>,
    /// Size of the terminal area at the last paint, so each shell is created
    /// at its final size (otherwise it redraws the prompt when resized).
    body_size: Rc<Cell<Option<Size<Pixels>>>>,
    /// Terminals on this machine (not on a server).
    local: bool,
    /// `restore` is bringing back the agent's terminals: none shown yet
    /// isn't none there.
    restoring: bool,
    /// The focus was asked for while restoring: it goes to the terminals
    /// once they're back (to a new one if there are none).
    focus_restored: bool,
    /// The panels in the terminals' place, a tab each after theirs.
    panel_tabs: Vec<PanelTab>,
    /// The terminal the debugger runs its command in: in `views`, but in no
    /// tab, as the debugger's console draws it.
    debug_term: Option<TermId>,
    /// The names given to terminals, rather than their programs' titles.
    names: HashMap<TermId, String>,
    /// The name being typed, in its place.
    renaming: Option<Renaming>,
    /// For right-click menus.
    weak: WeakEntity<Self>,
    /// What each terminal's view says, heard while it's here.
    view_subscriptions: HashMap<TermId, Subscription>,
    /// The sizes of each split's panes, by its terminals (in order): kept
    /// here, as gpui drops an element's state the first frame it isn't
    /// painted (in another tab, or another workspace).
    split_sizes: RefCell<HashMap<Vec<TermId>, Entity<ResizableState>>>,
    /// The share of each split's panes, as last dragged: saved, for a split
    /// painted for the first time (when the app is reopened) to start so.
    split_shares: HashMap<Vec<TermId>, Vec<f32>>,
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
            restoring: false,
            focus_restored: false,
            panel_tabs: Vec::new(),
            debug_term: None,
            names: HashMap::new(),
            renaming: None,
            weak: cx.entity().downgrade(),
            view_subscriptions: HashMap::new(),
            split_sizes: RefCell::default(),
            split_shares: HashMap::new(),
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
        self.restoring = true;
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Restored> = async {
                let mut alive: HashSet<TermId> = agent::list(&client, group.clone()).await?.into_iter().collect();
                let debug = debug_term.filter(|term| alive.remove(term));
                let saved = with_layouts(|layouts| layouts.groups.get(&group).cloned()).unwrap_or_default();
                let mut names = saved.names.clone();
                let tabs: Vec<(Tree, Option<String>)> = saved
                    .tabs
                    .iter()
                    .cloned()
                    .filter_map(SavedTab::into_tab)
                    // A tab of one terminal named before names were theirs.
                    .map(|(tree, name)| match (&tree, name) {
                        (Tree::Leaf(term), Some(name)) => {
                            names.entry(*term).or_insert(name);
                            (tree, None)
                        }
                        (_, name) => (tree, name),
                    })
                    .collect();
                let mut tabs = keep_terms(tabs, &alive);
                let placed: HashSet<TermId> = tabs.iter().flat_map(|(tree, _)| tree.leaves()).collect();
                // Those still alive but not saved, each in its own tab.
                let mut orphans: Vec<TermId> = alive.difference(&placed).copied().collect();
                orphans.sort();
                tabs.extend(orphans.into_iter().map(|term| (Tree::Leaf(term), None)));

                let terms: Vec<TermId> =
                    debug.into_iter().chain(tabs.iter().flat_map(|(tree, _)| tree.leaves())).collect();
                // One that ended since it was listed is left out, not the rest.
                let terminals: Vec<(TermId, Entity<Terminal>)> = agent::attach_all(&client, &terms, cx)
                    .await
                    .into_iter()
                    .filter_map(|(term, terminal)| Some((term, terminal.ok()?)))
                    .collect();
                let attached: HashSet<TermId> = terminals.iter().map(|(term, _)| *term).collect();
                let tabs = keep_terms(tabs, &attached);
                names.retain(|term, _| attached.contains(term));
                Ok((tabs, names, saved.sizes.into_iter().collect(), terminals))
            }
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok((tabs, names, shares, terminals)) => this.restored(tabs, names, shares, terminals, debug_term, window, cx),
                Err(err) => {
                    this.restoring = false;
                    this.focus_restored = false;
                    this.error(format!("Couldn't restore terminals: {err:#}"), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The agent's terminals are back, with `tabs` their saved layout: placed
    /// before any opened meanwhile, which keep the focus; with none, one opens.
    #[allow(clippy::too_many_arguments)]
    fn restored(
        &mut self,
        tabs: Vec<(Tree, Option<String>)>,
        names: HashMap<TermId, String>,
        shares: HashMap<Vec<TermId>, Vec<f32>>,
        terminals: Vec<(TermId, Entity<Terminal>)>,
        debug_term: Option<TermId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.restoring = false;
        self.names.extend(names);
        self.split_shares = shares;
        for (term, terminal) in terminals {
            let view = self.add_view(term, terminal, window, cx);
            if Some(term) == self.debug_term {
                cx.emit(TerminalAreaEvent::DebugTerminal(Some(view)));
            }
        }
        // gone with the agent's restart
        if debug_term.is_some_and(|term| !self.views.contains_key(&term)) {
            self.debug_term = None;
        }
        let focus = std::mem::take(&mut self.focus_restored);
        if tabs.is_empty() && self.tabs.is_empty() {
            self.open(Place::NewTab, window, cx);
            return;
        }
        let opened = std::mem::take(&mut self.tabs);
        for (tree, name) in tabs {
            let id = self.next_tab_id();
            let active = tree.leaves()[0];
            self.tabs.push(TerminalTab { id, tree, active, name });
        }
        self.active = if opened.is_empty() { 0 } else { self.tabs.len() + self.active };
        self.tabs.extend(opened);
        if focus {
            self.focus_active(window, cx);
        }
        self.save();
        cx.notify();
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
                    this.remove(term, window, cx);
                }
                for (term, terminal) in added {
                    // Opened here while this attached it too: the duplicate is
                    // dropped, which ends only its own subscription.
                    if this.views.contains_key(&term) {
                        continue;
                    }
                    this.add_view(term, terminal, window, cx);
                    let id = this.next_tab_id();
                    this.tabs.push(TerminalTab { id, tree: Tree::Leaf(term), active: term, name: None });
                }
                this.save();
                // While restoring, `restore` opens one if there are none.
                if this.tabs.is_empty() && !this.restoring {
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
            TerminalViewEvent::Exited => this.remove(term, window, cx),
            TerminalViewEvent::Focused => this.select(term, cx),
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
            Place::Split(_) => self.tabs.get(self.active).map(|tab| tab.active),
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
                        tab.tree.split(tab.active, term, axis);
                        tab.active = term;
                    }
                    _ => {
                        let id = this.next_tab_id();
                        this.tabs.push(TerminalTab {
                            id,
                            tree: Tree::Leaf(term),
                            active: term,
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
            self.select(term, cx);
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
            self.select(term, cx);
        }
        let place = split.map_or(Place::NewTab, Place::Split);
        self.open_running(place, None, line, focus, window, cx)
    }

    /// The terminals by tab, with their titles and whether each is the active one.
    pub fn list(&self, cx: &App) -> Vec<(TermId, String, bool)> {
        let active = self.tabs.get(self.active).map(|tab| tab.active);
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
        self.select(term, cx);
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

    /// Whether any terminal has focus.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.views.values().any(|view| view.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Focuses the active pane; if there is none, opens a terminal. While
    /// restoring, once the terminals are back.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.get(self.active).and_then(|tab| self.focus_handle(tab.active, cx)) {
            Some(handle) => handle.focus(window, cx),
            None if self.restoring => self.focus_restored = true,
            None => self.new_terminal(window, cx),
        }
    }

    fn focus_handle(&self, term: TermId, cx: &App) -> Option<FocusHandle> {
        Some(self.views.get(&term)?.read(cx).focus_handle(cx))
    }

    /// Closes the active terminal, killing its process.
    pub fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.tabs.get(self.active).map(|tab| tab.active) else {
            return;
        };
        self.close_term(term, window, cx);
    }

    /// Closes a specific terminal and kills its process.
    fn close_term(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get(&term) {
            view.read(cx).terminal().read(cx).kill();
        }
        self.remove(term, window, cx);
    }

    /// Closes a tab with all its terminals.
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        for term in tab.tree.leaves() {
            self.close_term(term, window, cx);
        }
    }

    /// Closes every tab but the one at `ix`, with their terminals.
    fn close_other_tabs(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let had_focus = self.contains_focus(window, cx);
        let others: Vec<TermId> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(other, _)| *other != ix)
            .flat_map(|(_, tab)| tab.tree.leaves())
            .collect();
        for term in others {
            self.close_term(term, window, cx);
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
            Tree::Leaf(term) => Named::Term(term),
            _ => Named::Tab(id),
        };
        let title = self.tab_title(tab, cx);
        self.type_name(named, title, window, cx);
    }

    /// Types a terminal's name: in its pane's title, or in its tab when that
    /// doesn't show.
    fn start_term_rename(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.title(term, cx);
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
            Named::Term(term) => tab.contains(term) && !self.pane_titles(tab, cx),
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

    /// What a terminal's title and its tab's say.
    fn title(&self, term: TermId, cx: &App) -> String {
        match self.names.get(&term) {
            Some(name) => name.clone(),
            None => self.views.get(&term).map(|view| view.read(cx).title(cx)).unwrap_or_default(),
        }
    }

    /// Splits next to `term` (the one that was clicked).
    fn split_at(&mut self, term: TermId, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.select(term, cx);
        self.split(axis, window, cx);
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
                this.split_at(term, Axis::Row, window, cx)
            })
            .action(Box::new(SplitRight)),
        )
        .item(
            menu::item("Split Down", &area, move |this, window, cx| {
                this.split_at(term, Axis::Column, window, cx)
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
    }

    fn select(&mut self, term: TermId, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.contains(term)) {
            self.active = ix;
            self.tabs[ix].active = term;
            cx.notify();
        }
    }

    fn remove(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self.focus_handle(term, cx).is_some_and(|handle| handle.is_focused(window));
        self.views.remove(&term);
        self.view_subscriptions.remove(&term);
        self.names.remove(&term);
        if self.debug_term.take_if(|debug| *debug == term).is_some() {
            cx.emit(TerminalAreaEvent::DebugTerminal(None));
            cx.notify();
            return;
        }
        let Some(tab_ix) = self.tabs.iter().position(|tab| tab.contains(term)) else {
            return;
        };
        let tab = self.tabs.remove(tab_ix);
        match tab.tree.clone().remove(term).map(|tree| TerminalTab { id: tab.id, tree, active: tab.active, name: tab.name.clone() }) {
            Some(mut left) => {
                // Focus moves to a neighbor of the one being closed.
                if tab.active == term {
                    left.active = [Direction::Left, Direction::Up, Direction::Right, Direction::Down]
                        .into_iter()
                        .find_map(|direction| tab.tree.neighbor(term, direction))
                        .unwrap_or(left.tree.leaves()[0]);
                }
                self.tabs.insert(tab_ix, left);
            }
            None => self.active = self.active.min(self.tabs.len().saturating_sub(1)),
        }
        if had_focus && !self.focus_active(window, cx) {
            cx.emit(TerminalAreaEvent::FocusLeft);
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
        let splits: HashSet<Vec<TermId>> = self.tabs.iter().flat_map(|tab| splits(&tab.tree)).collect();
        let mut sizes: Vec<(Vec<TermId>, Vec<f32>)> =
            self.split_shares.iter().filter(|(terms, _)| splits.contains(*terms)).map(|(terms, shares)| (terms.clone(), shares.clone())).collect();
        sizes.sort_by(|a, b| a.0.cmp(&b.0));
        let saved = SavedLayout {
            tabs: self.tabs.iter().map(SavedTab::of).collect(),
            names: self.names.clone(),
            sizes,
        };
        with_layouts(|layouts| {
            layouts.groups.insert(self.group.clone(), saved);
            layouts.store();
        });
    }

    fn render_tab_bar(&self, focused: bool, cx: &mut Context<Self>) -> impl IntoElement {
        // The panels' (the debugger's) after the terminals'.
        let panels: Vec<AnyElement> = self.panel_tabs.iter().map(|tab| self.render_panel_tab(tab, cx)).collect();
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
                                .panel_items(hide_item(&area), window, cx)
                        }
                    })
            }))
            .children(panels)
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
                            .panel_items(hide_item(&area), window, cx)
                        }
                    }),
            )
    }

    /// A panel's tab, after the terminals'.
    fn render_panel_tab(&self, tab: &PanelTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (panel, showing) = (tab.panel, tab.showing);
        let name = match panel {
            Panel::Console => "console",
            _ => tab.title,
        };
        let group = SharedString::from(format!("{name}-tab"));
        h_flex()
            .id(SharedString::from(format!("{name}-tab")))
            .when(cfg!(test), |el| el.debug_selector(move || format!("{name}-tab")))
            .group(group.clone())
            .h_full()
            .flex_none()
            .pl_3()
            .pr_1()
            .gap_1()
            .text_ui(cx)
            .border_r_1()
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
            .child(
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
            )
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(TerminalAreaEvent::ShowPanel(Some(panel)))))
            .context_menu({
                let area = self.weak.clone();
                move |menu, window, cx| {
                    let hide = menu::item("Hide Panel", &area, move |_, _, cx| cx.emit(TerminalAreaEvent::ClosePanel(panel)));
                    menu.panel_items(hide, window, cx)
                }
            })
            .into_any_element()
    }

    /// Renders a branch of the tree; `path` makes each split's ids unique,
    /// `focused` is whether a pane here has the keyboard.
    fn render_tree(&self, tab: &TerminalTab, tree: &Tree, path: String, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        match tree {
            Tree::Leaf(term) => {
                let term = *term;
                let body: AnyElement = match self.views.get(&term) {
                    Some(view) => view.clone().into_any_element(),
                    None => return div().into_any_element(),
                };
                let split = self.pane_titles(tab, cx);
                let theme = cx.theme();
                let key = term.to_string();
                let this = self.weak.clone();
                v_flex()
                    .id(SharedString::from(format!("terminal-pane-{key}")))
                    .when(cfg!(test), |el| el.debug_selector(|| format!("terminal-pane-{key}")))
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TerminalDrag>, _, cx| {
                        this.track_drop(term, event, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &TerminalDrag, window, cx| {
                        this.drop_on_pane(drag, term, window, cx);
                    }))
                    .when(split, |el| {
                        // The tab's active pane: its title lit as the active
                        // tab, its hairline a shade darker while it has the
                        // keyboard. The rest muted, in the panels' grey.
                        let active = term == tab.active;
                        let title = self.title(term, cx);
                        let group = SharedString::from(format!("terminal-pane-title-{key}"));
                        let input = self
                            .renaming
                            .as_ref()
                            .filter(|renaming| renaming.named == Named::Term(term))
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
                                            this.close_term(term, window, cx);
                                        })),
                                )
                                .when(!renaming, |el| {
                                    el.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                        if event.click_count() >= 2 {
                                            this.start_term_rename(term, window, cx);
                                        }
                                    }))
                                });
                        // Its name's text is selected by dragging, not the pane.
                        el.child(match renaming {
                            true => header,
                            false => self.draggable(header, Source::Pane(term), title.into()),
                        })
                    })
                    .child(div().flex_1().min_h_0().w_full().child(body))
                    .when_some(self.terminal_drop.filter(|(target, _)| *target == term && cx.has_active_drag()), |el, (_, placement)| {
                        el.child(placement.indicator(cx))
                    })
                    .context_menu(move |menu, window, cx| match this.upgrade() {
                        Some(area) => area.read(cx).pane_menu(term, menu, cx).panel_items(hide_item(&this), window, cx),
                        None => menu,
                    })
                    .into_any_element()
            }
            Tree::Split { axis, children } => {
                let id = SharedString::from(format!("terminal-split-{}-{path}", tab.id));
                let terms = tree.leaves();
                let state = self
                    .split_sizes
                    .borrow_mut()
                    .entry(terms.clone())
                    .or_insert_with(|| cx.new(|_| ResizableState::default()))
                    .clone();
                // Until it measures its panes, a split shares the space as it
                // was saved: by flex, from nothing.
                let shares = self.split_shares.get(&terms).filter(|shares| shares.len() == children.len());
                let group = match axis {
                    Axis::Row => h_resizable(id),
                    Axis::Column => v_resizable(id),
                };
                let area = self.weak.clone();
                group
                    .with_state(&state)
                    .on_resize(move |state, _, cx| {
                        let sizes = state.read(cx).sizes().clone();
                        let total: f32 = sizes.iter().map(|size| size.as_f32()).sum();
                        if total <= 0. {
                            return;
                        }
                        let shares = sizes.iter().map(|size| size.as_f32() / total).collect();
                        area.update(cx, |this, _| {
                            this.split_shares.insert(terms.clone(), shares);
                            this.save();
                        })
                        .ok();
                    })
                    .children(children.iter().enumerate().map(|(ix, child)| {
                        resizable_panel()
                            .when_some(shares, |panel, shares| panel.flex_grow(shares[ix] * shares.len() as f32).flex_basis(px(0.)))
                            .child(self.render_tree(tab, child, format!("{path}.{ix}"), focused, cx))
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
        let splits: HashSet<Vec<TermId>> = self.tabs.iter().flat_map(|tab| splits(&tab.tree)).collect();
        self.split_sizes.get_mut().retain(|terms, _| splits.contains(terms));
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
    /// The share of each split's panes, by its terminals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sizes: Vec<(Vec<TermId>, Vec<f32>)>,
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
                SavedTab::Named { tree, name } if *tree == Tree::Leaf(Some(term)) => Some(name.clone()),
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
                && *tree == Tree::Leaf(Some(term))
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
    Named { tree: Tree<Leaf>, name: String },
    Tree(Tree<Leaf>),
    /// Old format: a column of terminals.
    Column(Vec<TermId>),
}

/// A saved split's leaf: a terminal, or (`null`) the notes, which could be
/// split beside the terminals and no longer are.
type Leaf = Option<TermId>;

impl SavedTab {
    fn of(tab: &TerminalTab) -> Self {
        let tree = tab.tree.clone().filter_map(&|term| Some(Some(term))).expect("every leaf kept");
        match &tab.name {
            Some(name) => SavedTab::Named { tree, name: name.clone() },
            None => SavedTab::Tree(tree),
        }
    }

    /// Its tree, and its name if it was renamed; none if it had only the notes.
    fn into_tab(self) -> Option<(Tree, Option<String>)> {
        match self {
            SavedTab::Named { tree, name } => Some((tree.filter_map(&|leaf| leaf)?, Some(name))),
            SavedTab::Tree(tree) => Some((tree.filter_map(&|leaf| leaf)?, None)),
            SavedTab::Column(terms) => match terms.as_slice() {
                [] => None,
                [term] => Some((Tree::Leaf(*term), None)),
                _ => Some((
                    Tree::Split {
                        axis: Axis::Column,
                        children: terms.into_iter().map(Tree::Leaf).collect(),
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

/// Each split in `tree`, by its terminals.
fn splits(tree: &Tree) -> Vec<Vec<TermId>> {
    match tree {
        Tree::Leaf(_) => Vec::new(),
        Tree::Split { children, .. } => std::iter::once(tree.leaves()).chain(children.iter().flat_map(splits)).collect(),
    }
}

/// The tabs with only the terminals in `terms`: one left with none goes.
fn keep_terms(tabs: Vec<(Tree, Option<String>)>, terms: &HashSet<TermId>) -> Vec<(Tree, Option<String>)> {
    tabs.into_iter().filter_map(|(tree, name)| Some((tree.retain(&|term| terms.contains(&term))?, name))).collect()
}

/// Hide Panel: the terminals' place.
fn hide_item(area: &WeakEntity<TerminalArea>) -> menu::PopupMenuItem {
    menu::item("Hide Panel", area, |_, _, cx| cx.emit(TerminalAreaEvent::Hide))
}

/// A split tab's shape in miniature, its active pane filled: where the
/// count of its panes was.
fn layout_glyph(tree: &Tree, active: TermId, lit: bool, cx: &App) -> AnyElement {
    fn cell(tree: &Tree, active: TermId, lit: bool, cx: &App) -> AnyElement {
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
    /// a split keeps the rest, and one left with none goes.
    #[test]
    fn restoring_leaves_out_only_the_terminals_not_attached() {
        let leaf = |term| Tree::Leaf(term);
        let split = |children| Tree::Split { axis: Axis::Row, children };
        let tabs = vec![(split(vec![leaf(1), leaf(2)]), Some("both".to_string())), (leaf(3), None), (leaf(4), None)];
        let attached: HashSet<TermId> = [1, 3].into_iter().collect();
        assert_eq!(keep_terms(tabs, &attached), vec![(leaf(1), Some("both".to_string())), (leaf(3), None)]);
    }

    /// Layouts saved with the notes split beside the terminals read without
    /// them: a tab of the notes alone goes.
    #[test]
    fn layouts_with_the_notes_split_read() {
        let saved: SavedLayout =
            serde_json::from_str(r#"{"tabs":[{"Split":{"axis":"Row","children":[{"Leaf":1},{"Leaf":null}]}},{"Leaf":null},{"Leaf":2}]}"#).unwrap();
        let tabs: Vec<_> = saved.tabs.into_iter().filter_map(SavedTab::into_tab).collect();
        assert_eq!(tabs, vec![(Tree::Leaf(1), None), (Tree::Leaf(2), None)]);
    }

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::config::Config::default());
        });
    }

    /// Asked for the focus with the agent's terminals still on their way
    /// (entering the workspace, showing the terminals), none is opened: the
    /// focus waits for them.
    #[gpui_kit::test]
    fn the_focus_while_restoring_opens_no_terminal(cx: &mut TestAppContext) {
        init(cx);
        let cx = cx.add_empty_window();
        let area = cx.new_window_entity(|window, cx| {
            let mut area = test_area(window, cx);
            area.tabs.clear();
            area.restoring = true;
            area
        });
        cx.update(|window, cx| {
            area.update(cx, |this, cx| {
                this.focus(window, cx);
                assert!(this.tabs.is_empty());
                assert!(this.focus_restored);
                this.restored(vec![(Tree::Leaf(1), None)], HashMap::new(), HashMap::new(), Vec::new(), None, window, cx);
                assert_eq!(this.tabs.iter().map(TerminalTab::terms).collect::<Vec<_>>(), vec![vec![1]]);
                assert!(this.contains_focus(window, cx));
            })
        });
    }

    /// One opened while restoring goes after the restored ones, still active.
    #[gpui_kit::test]
    fn restored_terminals_go_before_one_opened_meanwhile(cx: &mut TestAppContext) {
        init(cx);
        let cx = cx.add_empty_window();
        let area = cx.new_window_entity(|window, cx| {
            let mut area = test_area(window, cx);
            area.tabs.retain(|tab| tab.active == 3);
            area.active = 0;
            area.restoring = true;
            area
        });
        cx.update(|window, cx| {
            area.update(cx, |this, cx| {
                let tabs = vec![(Tree::Leaf(1), None), (Tree::Leaf(2), None)];
                this.restored(tabs, HashMap::new(), HashMap::new(), Vec::new(), None, window, cx);
                assert_eq!(this.tabs.iter().map(TerminalTab::terms).collect::<Vec<_>>(), vec![vec![1], vec![2], vec![3]]);
                assert_eq!(this.active, 2);
            })
        });
    }
}
