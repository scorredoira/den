//! Where things go (see `config::Layout`): on the left the activity bar and
//! the side column, which shows a group of panels at a time, one above the
//! other, each folding to its header; the code in the middle; the terminals
//! on its right or under it. The notes open over it all.
use std::rc::Rc;

use super::*;
use crate::config::{Dock, Group, Layout, Panel, Place, SavedPanels};
use super::activity::IconDrag;
use crate::debug::DebugView;
use crate::menu::PanelItems as _;

/// Draws something of the app's in a workspace.
type Draw = dyn Fn(&mut Window, &mut App) -> AnyElement;

/// The app's workspaces or agents, drawn in their panel.
pub(crate) struct WorkspacesPanel {
    render: Box<Draw>,
    /// What goes on the right of its header.
    actions: Option<Rc<Draw>>,
}

impl WorkspacesPanel {
    pub fn new(render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { render: Box::new(render), actions: None }
    }

    pub fn with_actions(mut self, actions: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        self.actions = Some(Rc::new(actions));
        self
    }
}

impl Render for WorkspacesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.render)(window, cx)
    }
}

pub(super) fn icon(panel: Panel) -> &'static str {
    match panel {
        Panel::Workspaces => "icons/layers.svg",
        Panel::Agents => "icons/bot.svg",
        Panel::Files => "icons/files.svg",
        Panel::Changes => "icons/git-branch.svg",
        Panel::Search => "icons/search.svg",
        Panel::References => "icons/references.svg",
        Panel::Outline => "icons/list-tree.svg",
        Panel::Code => "icons/code.svg",
        Panel::Terminals => "icons/terminal.svg",
        Panel::Console => "icons/bug.svg",
        Panel::Notes => "icons/sticky-note.svg",
    }
}

/// The notes' icon: a blank sticky note, or one written on.
pub(crate) fn notes_icon(filled: bool) -> &'static str {
    if filled { "icons/sticky-note-text.svg" } else { "icons/sticky-note.svg" }
}

pub(super) fn group_icon(group: Group) -> &'static str {
    match group {
        Group::Explorer => "icons/files.svg",
        Group::Search => "icons/search.svg",
        Group::Git => "icons/git-branch.svg",
    }
}

pub(crate) fn title(panel: Panel) -> &'static str {
    match panel {
        Panel::Workspaces => "Workspaces",
        Panel::Agents => "Agents",
        Panel::Files => "Files",
        Panel::Changes => "Changes",
        Panel::Search => "Search",
        Panel::References => "References",
        Panel::Outline => "Outline",
        Panel::Code => "Code",
        Panel::Terminals => "Terminals",
        Panel::Console => "Debug",
        Panel::Notes => "Notes",
    }
}

/// How tall a side panel's header is.
const HEADER_HEIGHT: f32 = 26.;
/// The least a side panel shows under its header.
const MIN_BODY: f32 = 40.;

/// What of a workspace shows besides the code (see `SavedPanels`), and
/// what isn't saved: the debugger's console.
pub(super) struct Panels {
    terminals: bool,
    /// The debugger, a tab after the terminals' once shown.
    console: bool,
    /// The panel tab in front of the terminals, if one is: the debugger.
    front: Option<Panel>,
    /// Search or References, whichever showed last: F4 steps through it.
    pub results: Panel,
    /// The side panels open in the column this workspace last drew: one
    /// that wasn't (another place, brought there or unfolded) reads what it
    /// shows.
    seen: Vec<Panel>,
}

impl Panels {
    pub fn new() -> Self {
        Self::restored(SavedPanels::default())
    }

    fn restored(saved: SavedPanels) -> Self {
        Self {
            terminals: saved.terminals,
            console: false,
            front: None,
            results: Panel::Search,
            seen: Vec::new(),
        }
    }

    pub fn saved(&self) -> SavedPanels {
        SavedPanels { terminals: self.terminals }
    }

    /// What of the terminals' area shows: kept apart for editing and for
    /// debugging (`Workspace::debug_changed`).
    pub fn shown(&self) -> Shown {
        Shown { terminals: self.terminals, console: self.console, front: self.front }
    }

    pub fn show(&mut self, shown: Shown) {
        self.terminals = shown.terminals;
        self.console = shown.console;
        self.front = shown.front;
    }
}

/// The terminals and the debugger's tab, shown or not, and the tab in front.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Shown {
    pub terminals: bool,
    pub console: bool,
    pub front: Option<Panel>,
}

impl Shown {
    /// The first time a workspace debugs: the debugger's tab in front.
    pub fn debugging() -> Self {
        Self { terminals: true, console: true, front: Some(Panel::Console) }
    }
}

/// What a workspace's panels are shown for, each mode as it left them: the
/// terminals' area changes on going from one to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mode {
    Editing,
    /// A debug session runs, whatever is in front.
    Debugging,
}

/// What shows around the code while editing, the same in every workspace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SharedPanels {
    shown: Shown,
    maximized: bool,
}

/// A side panel dragged by its header: onto another's header it goes above
/// it, onto a place's icon into that place.
#[derive(Clone)]
pub(crate) struct PanelDrag(pub Panel);

/// A side panel's lower edge, dragged to size it.
#[derive(Clone)]
pub(crate) struct ResizeSide(Panel);

impl Render for ResizeSide {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The debugger's tab.
pub(super) fn debug_view(debugger: &Entity<Debugger>, cx: &mut App) -> Entity<DebugView> {
    cx.new(|cx| DebugView::new(debugger.clone(), cx))
}

impl Workspace {
    pub(crate) fn is_shown(&self, panel: Panel, cx: &App) -> bool {
        if self.plain {
            return panel == Panel::Code || self.plain_side == Some(panel);
        }
        let panels = &self.panels;
        let terminal_mode = self.in_terminal_mode();
        // Terminal Mode shows the terminals, with what goes in them, and
        // beside them the workspaces and the agents.
        let terminals = panels.terminals || terminal_mode;
        match panel {
            Panel::Code => !terminal_mode,
            Panel::Terminals => terminals,
            Panel::Console => terminals && panels.console && panels.front == Some(Panel::Console),
            Panel::Workspaces | Panel::Agents if terminal_mode => {
                Config::get(cx).terminal_mode.side && !Config::get(cx).layout.collapsed.contains(&panel)
            }
            _ if terminal_mode => false,
            Panel::Notes => self.notes_in_front(),
            _ => {
                let layout = &Config::get(cx).layout;
                self.in_side(panel, cx) && !layout.is_collapsed(panel) && layout.in_front(panel)
            }
        }
    }

    /// Whether the side column shows `panel`, folded or not.
    pub(super) fn in_side(&self, panel: Panel, cx: &App) -> bool {
        if self.plain {
            return self.plain_side == Some(panel);
        }
        let layout = &Config::get(cx).layout;
        layout.side && layout.place_of(panel).is_some() && layout.place_of(panel) == layout.current() && !layout.hidden.contains(&panel)
    }

    /// What the side column shows, if it shows.
    pub(crate) fn side_place(&self, cx: &App) -> Option<Place> {
        let layout = &Config::get(cx).layout;
        layout.side.then(|| layout.current()).flatten()
    }

    /// A plain editor (`den <file>` from a terminal outside den): the code,
    /// and beside it the files or the search, from its own activity bar.
    /// Nothing else shows, and nothing is reopened or saved.
    pub fn set_plain(&mut self, plain: bool, cx: &mut Context<Self>) {
        self.plain = plain;
        self.file_tree.update(cx, |tree, _| tree.set_plain(plain));
    }

    pub(crate) fn is_plain(&self) -> bool {
        self.plain
    }

    /// The window's Terminal Mode, the same for all its workspaces.
    pub fn set_terminal_mode(&mut self, mode: &Rc<Cell<bool>>) {
        if !Rc::ptr_eq(&self.terminal_mode, mode) {
            self.terminal_mode = mode.clone();
        }
    }

    pub(crate) fn in_terminal_mode(&self) -> bool {
        self.terminal_mode.get()
    }

    /// The window into Terminal Mode, with the keyboard in the terminals.
    pub(crate) fn enter_terminal_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_mode.set(true);
        self.terminals.update(cx, |terminals, cx| terminals.focus(window, cx));
        cx.notify();
    }

    /// The window out of Terminal Mode, for what needs the code or a side
    /// panel other than the workspaces and the agents: the layout as it was.
    /// Whether it was in it.
    pub(crate) fn leave_terminal_mode(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.terminal_mode.get() {
            return false;
        }
        self.terminal_mode.set(false);
        self.terminals_maximized = false;
        cx.notify();
        true
    }

    /// Terminal Mode's side column shows, or not.
    fn set_terminal_side(&mut self, side: bool, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.terminal_mode.side = side);
        self.layout_changed(cx);
    }

    /// Everything where it starts, and only the files, the code and the
    /// terminals shown (Reset Layout).
    pub(crate) fn reset_layout(&mut self, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.layout = Layout::default());
        self.panels = Panels::new();
        self.terminals_maximized = false;
        self.layout_changed(cx);
    }

    /// A debug session started or ended: the workspace's panels go to how
    /// they were the last time it debugged (the debugger's tab in front the
    /// first time) or back to how they were before. A restart is one
    /// session: it never ends in between.
    pub(super) fn debug_changed(&mut self, debugging: bool, cx: &mut Context<Self>) {
        if debugging == self.debugging {
            return;
        }
        self.debugging = debugging;
        if self.change_mode() {
            self.layout_changed(cx);
        }
    }

    /// Into the mode it's in now, if it wasn't: the panels of the one left
    /// are kept, and those of the next come back as it left them, or as it
    /// starts. Whether it changed.
    fn change_mode(&mut self) -> bool {
        if self.plain {
            return false;
        }
        let mode = if self.debugging { Mode::Debugging } else { Mode::Editing };
        if mode == self.mode {
            return false;
        }
        let now = self.panels.shown();
        self.kept_shown.insert(self.mode, now);
        self.mode = mode;
        let kept = self.kept_shown.remove(&mode);
        let next = match mode {
            Mode::Editing => kept.unwrap_or(now),
            Mode::Debugging => kept.unwrap_or_else(Shown::debugging),
        };
        self.panels.show(next);
        if !next.terminals {
            self.terminals_maximized = false;
        }
        true
    }

    /// What shows while editing, to be the same in the workspace gone to
    /// (`share_panels`): now, or as editing left it.
    pub(crate) fn editing_panels(&self) -> SharedPanels {
        if self.mode == Mode::Editing {
            return SharedPanels { shown: self.panels.shown(), maximized: self.terminals_maximized };
        }
        let shown = self.kept_shown.get(&Mode::Editing).copied().unwrap_or_else(|| self.panels.shown());
        SharedPanels { shown, maximized: false }
    }

    /// What showed editing in the workspace left shows here too: going from
    /// one to another doesn't change what's around the code. Debugging,
    /// it's what editing comes back to.
    pub(crate) fn share_panels(&mut self, shared: SharedPanels, cx: &mut Context<Self>) {
        if self.mode != Mode::Editing {
            self.kept_shown.insert(Mode::Editing, shared.shown);
            return;
        }
        if self.editing_panels() == shared {
            return;
        }
        self.panels.show(shared.shown);
        self.terminals_maximized = shared.maximized && shared.shown.terminals;
        self.layout_changed(cx);
    }

    /// What's saved with the workspace: editing's panels, as it opens
    /// editing.
    pub(super) fn saved_panels(&self) -> SavedPanels {
        match self.kept_shown.get(&Mode::Editing) {
            Some(editing) if self.mode != Mode::Editing => SavedPanels { terminals: editing.terminals },
            _ => self.panels.saved(),
        }
    }

    /// The layout in use follows the workspace drawn: the debugging one while
    /// it debugs (`Config::use_debug_layout`).
    pub(super) fn sync_debug_layout(&self, cx: &mut App) {
        if Config::get(cx).debugging() != self.debugging {
            let debugging = self.debugging;
            Config::update(cx, |config| config.use_debug_layout(debugging));
        }
    }

    /// What showed, as saved with the workspace.
    pub(crate) fn restore_panels(&mut self, saved: Option<SavedPanels>) {
        if let Some(saved) = saved {
            self.panels = Panels::restored(saved);
        }
    }

    /// What shows changed: saved with what's open, and drawn.
    pub(crate) fn layout_changed(&mut self, cx: &mut Context<Self>) {
        if self.plain {
            return cx.notify();
        }
        self.remember(cx);
        cx.notify();
    }

    /// Its panels read what they show when they come into sight: the side
    /// column changed, or this workspace came to the front with it.
    pub(super) fn place_shown(&mut self, cx: &mut Context<Self>) {
        // Only the workspaces and the agents, which aren't the workspace's.
        if self.in_terminal_mode() || self.plain {
            return;
        }
        let layout = &Config::get(cx).layout;
        let open: Vec<Panel> = self
            .side_place(cx)
            .map(|place| {
                let sections = layout.sections(place).into_iter().filter(|tabs| !layout.is_collapsed(tabs[0]));
                sections.map(|tabs| layout.front_of(&tabs)).collect()
            })
            .unwrap_or_default();
        if self.panels.seen == open {
            return;
        }
        let panels: Vec<Panel> = open.iter().copied().filter(|panel| !self.panels.seen.contains(panel)).collect();
        self.panels.seen = open;
        if panels.contains(&Panel::Changes) {
            self.changes.update(cx, |changes, cx| changes.shown(cx));
        }
    }

    /// Shows `panel`: where it is in the side column, unfolded, in front of
    /// its tabs and back if it was hidden; focus doesn't move.
    pub(crate) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.plain {
            if matches!(panel, Panel::Files | Panel::Search) {
                self.plain_side = Some(panel);
                cx.notify();
            }
            return;
        }
        // Terminal Mode has the workspaces and the agents; anything else of
        // the side column, or the code, takes the window out of it.
        if self.in_terminal_mode() {
            match panel {
                Panel::Workspaces | Panel::Agents => {
                    Config::update(cx, |config| config.layout.collapsed.retain(|other| *other != panel));
                    return self.set_terminal_side(true, cx);
                }
                Panel::Terminals | Panel::Console => {}
                _ => {
                    self.leave_terminal_mode(cx);
                }
            }
        }
        let panels = &mut self.panels;
        match panel {
            Panel::Code => {}
            Panel::Terminals => {
                panels.terminals = true;
                panels.front = None;
            }
            Panel::Console => {
                panels.terminals = true;
                panels.console = true;
                panels.front = Some(Panel::Console);
                // its targets and tests come from the launch file, which may have changed
                self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
            }
            // A tab of the code, which `show_notes` opens.
            Panel::Notes => return,
            _ => {
                if matches!(panel, Panel::Search | Panel::References) {
                    panels.results = panel;
                }
                let layout = &Config::get(cx).layout;
                let off = layout.place_of(panel).is_some_and(|place| layout.panels(place).is_empty());
                if layout.is_collapsed(panel) || layout.hidden.contains(&panel) || off || !layout.in_front(panel) {
                    Config::update(cx, |config| {
                        config.layout.set_collapsed(panel, false);
                        config.layout.hidden.retain(|other| *other != panel);
                        config.layout.set_on_bar(crate::config::Icon::Place(panel), true);
                        config.layout.bring_to_front(panel);
                    });
                }
                let Some(place) = Config::get(cx).layout.place_of(panel) else {
                    return;
                };
                set_side(true, place, cx);
            }
        }
        self.layout_changed(cx);
    }

    /// Hides `panel`: a side panel's place closes the side column.
    pub(crate) fn hide_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.plain {
            if self.plain_side == Some(panel) {
                self.plain_side = None;
                cx.notify();
            }
            return;
        }
        if self.in_terminal_mode() {
            match panel {
                Panel::Workspaces | Panel::Agents => return self.set_terminal_side(false, cx),
                // Without the terminals, there's the code.
                Panel::Terminals => {
                    self.leave_terminal_mode(cx);
                }
                _ => {}
            }
        }
        let panels = &mut self.panels;
        match panel {
            Panel::Code => return,
            Panel::Terminals => {
                panels.terminals = false;
                self.terminals_maximized = false;
            }
            Panel::Console => {
                panels.console = false;
                if panels.front == Some(Panel::Console) {
                    panels.front = None;
                }
            }
            // A tab of the code, which `hide_notes` hides.
            Panel::Notes => return,
            _ => {
                if self.is_shown(panel, cx) || self.in_side(panel, cx) {
                    close_side(cx);
                }
            }
        }
        self.layout_changed(cx);
    }

    /// The same key that shows a panel hides it; focus doesn't move.
    pub(super) fn toggle_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.is_shown(panel, cx) {
            self.hide_panel(panel, cx);
        } else {
            self.show_panel(panel, cx);
        }
    }

    /// Takes a side panel off the column (Hide Panel) until it's shown
    /// again; the column closes if that leaves it empty.
    pub(crate) fn remove_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        Config::update(cx, |config| {
            if !config.layout.hidden.contains(&panel) {
                config.layout.hidden.push(panel);
            }
        });
        // Nothing left where it was: the column closes rather than jump.
        let layout = &Config::get(cx).layout;
        if layout.place_of(layout.place.0).is_none_or(|place| layout.panels(place).is_empty()) {
            close_side(cx);
        }
        self.layout_changed(cx);
        cx.refresh_windows();
    }

    /// Gives a side panel an icon of its own, where it shows alone.
    pub(super) fn own_place(&mut self, panel: Panel, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.layout.own_place(panel));
        self.show_panel(panel, cx);
        cx.refresh_windows();
    }

    /// Brings a side panel to the place the column shows, from wherever it
    /// is (or hidden), a section of its own above `before`'s or last.
    pub(crate) fn bring_panel(&mut self, panel: Panel, before: Option<Panel>, cx: &mut Context<Self>) {
        // With every panel hidden there's no place showing: it shows in its own.
        let Some(place) = Config::get(cx).layout.current() else {
            return self.show_panel(panel, cx);
        };
        Config::update(cx, |config| config.layout.move_panel(panel, place, before));
        self.show_panel(panel, cx);
        cx.refresh_windows();
    }

    /// Puts a side panel among `with`'s tabs, before `before` or last, and
    /// shows it there.
    pub(crate) fn join_panel(&mut self, panel: Panel, with: Panel, before: Option<Panel>, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.layout.join(panel, with, before));
        self.show_panel(panel, cx);
        cx.refresh_windows();
    }

    /// Puts a place's panels in the one the column shows.
    fn merge_place(&mut self, from: Place, cx: &mut Context<Self>) {
        let Some(into) = Config::get(cx).layout.current() else {
            return;
        };
        Config::update(cx, |config| config.layout.merge(from, into));
        set_side(true, into, cx);
        self.layout_changed(cx);
        cx.refresh_windows();
    }

    /// A place's icon: shows it, or closes the side column if it's the one
    /// showing.
    pub(crate) fn click_place(&mut self, place: Place, cx: &mut Context<Self>) {
        if self.side_place(cx) == Some(place) {
            close_side(cx);
        } else {
            set_side(true, place, cx);
        }
        self.layout_changed(cx);
    }

    /// Folds a side panel's section to its header, or unfolds it; in
    /// Terminal Mode's column, the panel alone.
    fn toggle_collapsed(&mut self, panel: Panel, cx: &mut Context<Self>) {
        let terminal_mode = self.in_terminal_mode();
        Config::update(cx, |config| {
            let layout = &mut config.layout;
            if !terminal_mode {
                return layout.set_collapsed(panel, !layout.is_collapsed(panel));
            }
            if layout.collapsed.contains(&panel) {
                layout.collapsed.retain(|other| *other != panel);
            } else {
                layout.collapsed.push(panel);
            }
        });
        cx.notify();
    }

    /// A tab clicked: in front of its section, unfolded.
    fn front_tab(&mut self, panel: Panel, cx: &mut Context<Self>) {
        Config::update(cx, |config| {
            config.layout.bring_to_front(panel);
            config.layout.set_collapsed(panel, false);
        });
        self.layout_changed(cx);
    }

    /// The terminals on the right of the code, or under it: in every
    /// window, which View's check follows.
    pub(crate) fn set_dock(&mut self, dock: Dock, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.layout.dock = dock);
        self.show_panel(Panel::Terminals, cx);
        crate::app_menu::set(cx);
        cx.refresh_windows();
    }

    /// The terminals to the other place: under the code, or on its right.
    pub(super) fn move_terminals(&mut self, cx: &mut Context<Self>) {
        let dock = match Config::get(cx).layout.dock {
            Dock::Right => Dock::Bottom,
            Dock::Bottom => Dock::Right,
        };
        self.set_dock(dock, cx);
    }

    /// The app's agents panel, the same for every workspace.
    pub fn set_agents(&mut self, view: &Entity<WorkspacesPanel>) {
        if self.agents.is_none() {
            self.agents = Some(view.clone());
        }
    }

    /// The app's workspaces panel, the same for every workspace.
    pub fn set_workspaces(&mut self, view: &Entity<WorkspacesPanel>) {
        if self.workspaces.is_none() {
            self.workspaces = Some(view.clone());
        }
    }

    /// Cmd-B: the side column, with what it last showed (or the first
    /// place, if that's gone).
    pub(super) fn toggle_side_panel(&mut self, _: &ToggleSidePanel, _: &mut Window, cx: &mut Context<Self>) {
        if self.plain {
            self.plain_side = if self.plain_side.is_some() { None } else { Some(Panel::Files) };
            return cx.notify();
        }
        if self.in_terminal_mode() {
            let side = !Config::get(cx).terminal_mode.side;
            return self.set_terminal_side(side, cx);
        }
        if let Some(place) = Config::get(cx).layout.current() {
            self.click_place(place, cx);
        }
    }

    /// The columns: the side one, the code (with the terminals under it, if
    /// that's their place) and the terminals. Maximized, the terminals take
    /// the code's place and the side column stays.
    pub(super) fn render_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.in_terminal_mode() {
            return self.render_terminal_mode(window, cx);
        }
        if self.plain {
            return self.render_plain(cx);
        }
        let terminals = self.panels.terminals;
        let maximized = self.terminals_maximized && terminals;
        let layout = Config::get(cx).layout.clone();
        let side = self.side_place(cx).is_some();
        let right = terminals && layout.dock == Dock::Right && !maximized;
        let state = self.split.state(self.width, [side, right], cx).clone();
        // With no saved width, the terminals take half of what the others leave.
        let fixed = if side { layout.side_width } else { 0. };
        let half = ((f32::from(self.width) - fixed) / 2.).max(300.);
        let mut row = h_resizable("workspace-columns").with_state(&state);
        if side {
            row = row.child(
                resizable_panel()
                    .size(config::width(layout.side_width, 160., 800.))
                    .size_range(px(160.)..px(800.))
                    .child(self.render_side(window, cx)),
            );
        }
        let center = if maximized {
            self.terminals.clone().cached(StyleRefinement::default().size_full()).into_any_element()
        } else {
            self.render_center(&layout, window, cx)
        };
        row = row.child(resizable_panel().child(center));
        if right {
            row = row.child(
                resizable_panel()
                    .size(config::width(layout.dock_width.unwrap_or(half), 200., 4000.))
                    .size_range(px(200.)..px(4000.))
                    .child(self.terminals.clone().cached(StyleRefinement::default().size_full())),
            );
        }
        // The side column, the code, the terminals: in order.
        let kinds: Vec<&'static str> = side
            .then_some("side")
            .into_iter()
            .chain(["code"])
            .chain(right.then_some("terminals"))
            .collect();
        let workspace = cx.entity().downgrade();
        row.on_resize(move |state, _, cx| {
            let sizes = state.read(cx).sizes().clone();
            Config::update_quietly(cx, |config| {
                for (size, kind) in sizes.iter().zip(&kinds) {
                    let size = f32::from(*size);
                    match *kind {
                        "side" => config.layout.side_width = size,
                        "terminals" => config.layout.dock_width = Some(size),
                        _ => {}
                    }
                }
            });
            workspace.update(cx, |_, cx| cx.notify()).ok();
        })
        .into_any_element()
    }

    /// Terminal Mode: the workspaces and the agents, one above the other,
    /// and the terminals.
    fn render_terminal_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mode = Config::get(cx).terminal_mode.clone();
        let state = self.split.state(self.width, ("terminal-mode", mode.side), cx).clone();
        let mut row = h_resizable("terminal-mode-columns").with_state(&state);
        if mode.side {
            row = row.child(
                resizable_panel()
                    .size(config::width(mode.side_width, 160., 800.))
                    .size_range(px(160.)..px(800.))
                    .child(self.render_terminal_side(window, cx)),
            );
        }
        row = row.child(resizable_panel().child(self.terminals.clone().cached(StyleRefinement::default().size_full())));
        let workspace = cx.entity().downgrade();
        row.on_resize(move |state, _, cx| {
            if mode.side
                && let Some(size) = state.read(cx).sizes().first().copied()
            {
                Config::update_quietly(cx, |config| config.terminal_mode.side_width = f32::from(size));
            }
            workspace.update(cx, |_, cx| cx.notify()).ok();
        })
        .into_any_element()
    }

    /// Terminal Mode's side column: the workspaces, which take what the
    /// agents leave, and the agents under them, folded as in the other.
    fn render_terminal_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        let panels = [Panel::Workspaces, Panel::Agents];
        let open: Vec<Panel> = panels.into_iter().filter(|panel| !layout.collapsed.contains(panel)).collect();
        let filler = open.first().copied();
        let filler_at = filler.and_then(|filler| panels.iter().position(|panel| *panel == filler));
        let sections: Vec<AnyElement> = panels
            .iter()
            .enumerate()
            .map(|(ix, &panel)| {
                let height = (open.contains(&panel) && Some(panel) != filler).then(|| layout.heights.get(&panel).copied().unwrap_or(layout.height(panel)));
                let below = filler_at.is_some_and(|at| ix > at);
                self.render_section(&[panel], open.contains(&panel), height, below, ix > 0, window, cx)
            })
            .collect();
        let theme = cx.theme();
        v_flex()
            .id("terminal-mode-side")
            .when(cfg!(test), |el| el.debug_selector(|| "terminal-mode-side".into()))
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .children(sections)
            .when(filler.is_none(), |el| el.child(div().flex_1()))
            .into_any_element()
    }

    /// A plain editor: the files or the search, if one shows, and the code.
    fn render_plain(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        let code = self.render_editor_area(cx).into_any_element();
        let Some(panel) = self.plain_side else {
            return code;
        };
        let state = self.split.state(self.width, ("plain", true), cx).clone();
        let content = match panel {
            Panel::Search => self.search.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
            _ => self.file_tree.clone().cached(StyleRefinement::default().size_full()).into_any_element(),
        };
        let header = match panel {
            Panel::Files => folder_label(&self.root),
            _ => title(panel).to_uppercase(),
        };
        let theme = cx.theme();
        let side = v_flex()
            .id("plain-side")
            .when(cfg!(test), |el| el.debug_selector(|| "plain-side".into()))
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                div()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_2()
                    .text_ui_small(cx)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(header),
            )
            .child(div().flex_1().min_h_0().pt_1().child(content));
        h_resizable("plain-columns")
            .with_state(&state)
            .child(
                resizable_panel()
                    .size(config::width(layout.side_width, 160., 800.))
                    .size_range(px(160.)..px(800.))
                    .child(side),
            )
            .child(resizable_panel().child(code))
            .on_resize(|state, _, cx| {
                if let Some(size) = state.read(cx).sizes().first().copied() {
                    Config::update_quietly(cx, |config| config.layout.side_width = f32::from(size));
                }
            })
            .into_any_element()
    }

    /// The code, with the terminals under it while that's their place.
    fn render_center(&mut self, layout: &Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let code = self.render_editor_area(cx).into_any_element();
        if !(self.panels.terminals && layout.dock == Dock::Bottom) {
            return code;
        }
        let height = f32::from(window.viewport_size().height);
        let state = self.rows.state(px(height), [true], cx).clone();
        v_resizable("workspace-rows")
            .with_state(&state)
            .child(resizable_panel().child(code))
            .child(
                resizable_panel()
                    .size(px(layout.dock_height.unwrap_or(height / 3.).clamp(120., 4000.)))
                    .size_range(px(120.)..px(4000.))
                    .child(self.terminals.clone().cached(StyleRefinement::default().size_full())),
            )
            .on_resize(|state, _, cx| {
                if let Some(size) = state.read(cx).sizes().get(1).copied() {
                    Config::update_quietly(cx, |config| config.layout.dock_height = Some(f32::from(size)));
                }
            })
            .into_any_element()
    }

    /// The side column: its place's sections, one above the other, each a
    /// panel or several as tabs. One open section takes the height the
    /// others leave (see `Layout::filler`). A place's icon dropped on it
    /// brings its panels.
    fn render_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        let sections = layout.current().map(|place| layout.sections(place)).unwrap_or_default();
        let open: Vec<Vec<Panel>> = sections.iter().filter(|tabs| !layout.is_collapsed(tabs[0])).cloned().collect();
        let filler = Layout::filler(&open).map(|at| open[at][0]);
        let filler_at = filler.and_then(|filler| sections.iter().position(|tabs| tabs[0] == filler));
        // The first one's header needs no line above it.
        let sections: Vec<AnyElement> = sections
            .iter()
            .enumerate()
            .map(|(ix, tabs)| {
                let open = !layout.is_collapsed(tabs[0]);
                let height = (open && Some(tabs[0]) != filler).then(|| layout.height(tabs[0]));
                let below = filler_at.is_some_and(|at| ix > at);
                self.render_section(tabs, open, height, below, ix > 0, window, cx)
            })
            .collect();
        let theme = cx.theme();
        v_flex()
            .id("side-column")
            .when(cfg!(test), |el| el.debug_selector(|| "side-column".into()))
            .size_full()
            .overflow_hidden()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .drag_over::<IconDrag>(|style, drag, _, cx| match drag.0 {
                crate::config::Icon::Place(_) => style.bg(cx.theme().primary.opacity(0.1)),
                _ => style,
            })
            .on_drop(cx.listener(|this, drag: &IconDrag, _, cx| {
                if let crate::config::Icon::Place(panel) = drag.0 {
                    this.merge_place(Place(panel), cx);
                }
            }))
            .children(sections)
            // Every panel folded: the space below them.
            .when(filler.is_none(), |el| el.child(div().flex_1()))
            .into_any_element()
    }

    /// A section of the side column: a panel, or several as tabs. Its
    /// header folds it, and a tab clicked comes to the front; while open it
    /// shows the panel in front, with `height` unless it takes what the
    /// others leave. One `below` the section that takes it is sized by its
    /// upper edge, as in VS Code: the last one has no lower edge to drag. A
    /// panel dropped on the header joins its tabs (before the tab it's
    /// dropped on); on its upper edge, it goes above it.
    #[allow(clippy::too_many_arguments)]
    fn render_section(
        &mut self,
        tabs: &[Panel],
        open: bool,
        height: Option<f32>,
        below: bool,
        line: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let workspace = cx.entity().downgrade();
        // Terminal Mode's column is its own: its panels only fold.
        let terminal_mode = self.in_terminal_mode();
        let key = tabs[0];
        let panel = Config::get(cx).layout.front_of(tabs);
        let tabbed = tabs.len() > 1;
        let actions = match panel {
            Panel::Workspaces => {
                let actions = self.workspaces.as_ref().and_then(|view| view.read(cx).actions.clone());
                actions.map(|actions| actions(window, cx))
            }
            _ => None,
        };
        // Out of sight, the workspaces say the most urgent of their agents
        // on their header, or on their tab.
        let dot = |tab: Panel| match tab {
            Panel::Workspaces if !open || tab != panel => self.badges.workspaces,
            _ => None,
        };
        let name = if tabbed {
            h_flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .gap_2()
                .overflow_hidden()
                .children(tabs.iter().map(|&tab| {
                    let front = tab == panel;
                    h_flex()
                        .id(("side-tab", tab as usize))
                        .when(cfg!(test), |el| el.debug_selector(move || format!("tab-{tab:?}")))
                        .h_full()
                        .min_w(px(24.))
                        .gap_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .border_b_1()
                        .border_color(if front && open { theme.primary } else { gpui::transparent_black() })
                        .text_color(if front { theme.sidebar_foreground } else { theme.muted_foreground })
                        .hover(|style| style.text_color(theme.sidebar_foreground))
                        .child(div().overflow_hidden().text_ellipsis().child(title(tab).to_uppercase()))
                        .children(dot(tab).map(|color| div().flex_none().size(px(7.)).rounded_full().bg(color)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if front {
                                this.toggle_collapsed(tab, cx);
                            } else {
                                this.front_tab(tab, cx);
                            }
                        }))
                        .on_drag(PanelDrag(tab), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into())))
                        .drag_over::<PanelDrag>(|style, _, _, cx| style.border_color(cx.theme().primary).text_color(cx.theme().primary))
                        .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                            cx.stop_propagation();
                            this.join_panel(drag.0, tab, Some(tab), cx);
                        }))
                        // An icon that brings one panel joins as a tab too.
                        .drag_over::<IconDrag>(|style, drag, _, cx| match Config::get(cx).layout.icon_panel(drag.0) {
                            Some(_) => style.border_color(cx.theme().primary).text_color(cx.theme().primary),
                            None => style,
                        })
                        .on_drop(cx.listener(move |this, drag: &IconDrag, _, cx| {
                            if let Some(panel) = Config::get(cx).layout.icon_panel(drag.0) {
                                cx.stop_propagation();
                                this.join_panel(panel, tab, Some(tab), cx);
                            }
                        }))
                }))
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                // The files go by their folder, as in VS Code; as a tab, by
                // their name.
                .child(match panel {
                    Panel::Files => folder_label(&self.root),
                    _ => title(panel).to_uppercase(),
                })
                .into_any_element()
        };
        let header = h_flex()
            .id(("side-header", key as usize))
            .when(cfg!(test), |el| el.debug_selector(move || format!("title-{key:?}")))
            .relative()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .pl_1()
            .pr_2()
            .gap_1()
            .when(line, |el| el.border_t_1())
            .border_color(theme.sidebar_border)
            .text_ui_small(cx)
            .child(
                svg()
                    .path(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" })
                    .size(px(14.))
                    .flex_none()
                    .text_color(theme.muted_foreground),
            )
            .child(name)
            .when(!tabbed, |el| el.children(dot(panel).map(|color| div().flex_none().size(px(7.)).rounded_full().bg(color))))
            .children(actions.map(|actions| div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(actions)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_collapsed(panel, cx)))
            .when(!tabbed, |el| el.on_drag(PanelDrag(panel), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into()))))
            .when(!terminal_mode, |el| {
                el.drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.15)))
                    .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                        cx.stop_propagation();
                        this.join_panel(drag.0, panel, None, cx);
                    }))
                    // An icon that brings one panel: as a tab of it.
                    .drag_over::<IconDrag>(|style, drag, _, cx| match Config::get(cx).layout.icon_panel(drag.0) {
                        Some(_) => style.bg(cx.theme().primary.opacity(0.15)),
                        None => style,
                    })
                    .on_drop(cx.listener(move |this, drag: &IconDrag, _, cx| {
                        if let Some(dragged) = Config::get(cx).layout.icon_panel(drag.0) {
                            cx.stop_propagation();
                            this.join_panel(dragged, panel, None, cx);
                        }
                    }))
                    // Its upper edge: above it.
                    .child(
                        div()
                            .id(("side-above", key as usize))
                            .when(cfg!(test), |el| el.debug_selector(move || format!("above-{key:?}")))
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .h(px(5.))
                            .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary))
                            .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                                cx.stop_propagation();
                                this.bring_panel(drag.0, Some(key), cx);
                            })),
                    )
            })
            .context_menu({
                let workspace = workspace.clone();
                move |menu, window, cx| {
                    let fold = menu::item(if open { "Collapse" } else { "Expand" }, &workspace, move |this, _, cx| this.toggle_collapsed(panel, cx));
                    if terminal_mode {
                        return menu.item(fold);
                    }
                    // Its own icon, if it has company where it is.
                    let layout = &Config::get(cx).layout;
                    let company = layout.place_of(panel).is_some_and(|place| layout.panels(place).len() > 1);
                    let remove = menu::item("Hide Panel", &workspace, move |this, _, cx| this.remove_panel(panel, cx));
                    let side = menu::item("Hide Side Bar", &workspace, move |this, _, cx| this.hide_panel(panel, cx));
                    menu.item(fold)
                        .item(remove)
                        .when(company, |menu| {
                            menu.item(menu::item("Move to Its Own Icon", &workspace, move |this, _, cx| this.own_place(panel, cx)))
                        })
                        .separator()
                        .panel_items(side, window, cx)
                }
            });
        let section = v_flex()
            .id(("side-panel", key as usize))
            .when(cfg!(test), |el| el.debug_selector(move || format!("stack-{panel:?}")))
            .w_full()
            .child(header);
        if !open {
            return section.flex_none().into_any_element();
        }
        // The panels are drawn again only when they change, not with every
        // frame of a terminal or every blink of the cursor.
        let cached = || StyleRefinement::default().size_full();
        let content = match panel {
            Panel::Workspaces => self.workspaces.clone().map(|view| view.into_any_element()),
            Panel::Agents => self.agents.clone().map(|view| view.into_any_element()),
            Panel::Files => Some(self.file_tree.clone().cached(cached()).into_any_element()),
            Panel::Outline => Some(self.outline.clone().cached(cached()).into_any_element()),
            Panel::Search => Some(self.search.clone().cached(cached()).into_any_element()),
            Panel::References => Some(self.references.clone().cached(cached()).into_any_element()),
            Panel::Changes => Some(self.changes.clone().cached(cached()).into_any_element()),
            _ => None,
        };
        let hide = workspace.clone();
        let hide_workspace = workspace.clone();
        let content = div()
            .id(("side-content", key as usize))
            .flex_1()
            .min_h_0()
            .pt_1()
            .children(content)
            .capture_any_mouse_down(move |event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right && !terminal_mode {
                    let workspace = hide.clone();
                    let hide: menu::PanelAction = Rc::new(move |cx| {
                        workspace.update(cx, |this, cx| this.remove_panel(panel, cx)).ok();
                    });
                    // Its own icon, if it has company where it is.
                    let layout = &Config::get(cx).layout;
                    let company = layout.place_of(panel).is_some_and(|place| layout.panels(place).len() > 1);
                    let workspace = hide_workspace.clone();
                    let own_icon: Option<menu::PanelAction> = company.then(|| {
                        Rc::new(move |cx: &mut App| {
                            workspace.update(cx, |this, cx| this.own_place(panel, cx)).ok();
                        }) as menu::PanelAction
                    });
                    menu::set_panel_under(Some(hide), own_icon, cx);
                }
            });
        let section = section.child(content);
        match height {
            // It takes what the others leave.
            None => section.flex_1().min_h(px(HEADER_HEIGHT + MIN_BODY)).into_any_element(),
            // Its lower edge sizes it, or its upper one below the filler.
            Some(height) => section
                .flex_none()
                .relative()
                .h(px(height.max(HEADER_HEIGHT + MIN_BODY)))
                .child(
                    div()
                        .id(("side-resize", key as usize))
                        .h(px(4.))
                        .w_full()
                        .flex_none()
                        .when(below, |el| el.absolute().top_0().left_0())
                        .cursor_row_resize()
                        .on_drag(ResizeSide(key), |drag, _, _, cx| cx.new(|_| drag.clone())),
                )
                .on_drag_move(cx.listener(move |_, event: &DragMoveEvent<ResizeSide>, _, cx| {
                    if event.drag(cx).0 != key {
                        return;
                    }
                    let edge = if below {
                        event.bounds.bottom() - event.event.position.y
                    } else {
                        event.event.position.y - event.bounds.top()
                    };
                    let height = f32::from(edge).max(HEADER_HEIGHT + MIN_BODY);
                    Config::update_quietly(cx, |config| {
                        if terminal_mode {
                            config.layout.heights.insert(panel, height);
                        } else {
                            config.layout.set_height(panel, height);
                        }
                    });
                    cx.notify();
                }))
                .into_any_element(),
        }
    }

    /// The tabs after the terminals': the debugger once shown, with its
    /// state on it (yellow while stopped, green while running).
    pub(super) fn shape_terminals(&mut self, cx: &mut Context<Self>) {
        let mut tabs = Vec::new();
        if self.panels.console {
            let debugger = self.debugger.read(cx);
            let dot = if debugger.is_stopped() {
                Some(cx.theme().warning)
            } else {
                debugger.is_active().then(|| cx.theme().success)
            };
            tabs.push(PanelTab {
                panel: Panel::Console,
                view: self.debug_view.clone().into(),
                icon: icon(Panel::Console),
                title: title(Panel::Console),
                showing: self.panels.front == Some(Panel::Console),
                dot,
            });
        }
        self.terminals.update(cx, |terminals, cx| terminals.set_panel_tabs(tabs, cx));
    }
}

impl Workspace {
    /// A session started or stopped somewhere: the debugger's tab, in front.
    pub(super) fn reveal_debugger(&mut self, cx: &mut Context<Self>) {
        if !self.is_shown(Panel::Console, cx) {
            self.show_panel(Panel::Console, cx);
        } else {
            self.layout_changed(cx);
        }
    }
}

/// A folder's name as a header says it.
fn folder_label(root: &Path) -> String {
    root.file_name().map(|name| name.to_string_lossy().to_uppercase()).unwrap_or_else(|| root.display().to_string())
}

/// The side column shows `place`, in every workspace.
fn set_side(side: bool, place: Place, cx: &mut App) {
    Config::update(cx, |config| {
        config.layout.side = side;
        config.layout.place = place;
    });
}

/// The side column closes, in every workspace.
fn close_side(cx: &mut App) {
    Config::update(cx, |config| config.layout.side = false);
}
