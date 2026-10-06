//! Where things go (see `config::Layout`): on the left the activity bar and
//! the side column, which shows a group of panels at a time, one above the
//! other, each folding to its header; the code in the middle; the terminals
//! on its right or under it. The notes open over it all.
use std::rc::Rc;

use super::*;
use crate::config::{Dock, Group, Layout, Panel, Place, SavedPanels};
use super::activity::PlaceDrag;
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
/// what isn't saved: the notes, the debugger's console.
pub(super) struct Panels {
    terminals: bool,
    /// The debugger, a tab after the terminals' once shown.
    console: bool,
    /// The panel tab in front of the terminals, if one is: the debugger or
    /// the notes (always a tab at the bar's far end).
    front: Option<Panel>,
    /// The terminals were opened for the notes: hiding these closes them.
    notes_alone: bool,
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
            notes_alone: false,
            results: Panel::Search,
            seen: Vec::new(),
        }
    }

    pub fn saved(&self) -> SavedPanels {
        SavedPanels { terminals: self.terminals }
    }
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
        let panels = &self.panels;
        match panel {
            Panel::Code => true,
            Panel::Terminals => panels.terminals,
            Panel::Console => panels.terminals && panels.console && panels.front == Some(Panel::Console),
            // In a tab of the code it's not among the terminals.
            Panel::Notes => panels.terminals && panels.front == Some(Panel::Notes) && self.notes_tab().is_none(),
            _ => self.in_side(panel, cx) && !Config::get(cx).layout.collapsed.contains(&panel),
        }
    }

    /// Whether the side column shows `panel`, folded or not.
    pub(super) fn in_side(&self, panel: Panel, cx: &App) -> bool {
        let layout = &Config::get(cx).layout;
        layout.side && layout.place_of(panel).is_some() && layout.place_of(panel) == layout.current() && !layout.hidden.contains(&panel)
    }

    /// What the side column shows, if it shows.
    pub(crate) fn side_place(&self, cx: &App) -> Option<Place> {
        let layout = &Config::get(cx).layout;
        layout.side.then(|| layout.current()).flatten()
    }

    /// Everything where it starts, and only the files, the code and the
    /// terminals shown (Reset Layout).
    pub(crate) fn reset_layout(&mut self, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.layout = Layout::default());
        self.panels = Panels::new();
        self.terminals_maximized = false;
        self.layout_changed(cx);
    }

    /// What showed, as saved with the workspace.
    pub(crate) fn restore_panels(&mut self, saved: Option<SavedPanels>) {
        if let Some(saved) = saved {
            self.panels = Panels::restored(saved);
        }
    }

    /// What shows changed: saved with what's open, and drawn.
    pub(crate) fn layout_changed(&mut self, cx: &mut Context<Self>) {
        self.remember(cx);
        cx.notify();
    }

    /// Its panels read what they show when they come into sight: the side
    /// column changed, or this workspace came to the front with it.
    pub(super) fn place_shown(&mut self, cx: &mut Context<Self>) {
        let layout = &Config::get(cx).layout;
        let open: Vec<Panel> = self
            .side_place(cx)
            .map(|place| layout.panels(place).into_iter().filter(|panel| !layout.collapsed.contains(panel)).collect())
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

    /// Shows `panel`: where it is in the side column, unfolded and back if
    /// it was hidden; focus doesn't move.
    pub(crate) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        let panels = &mut self.panels;
        match panel {
            Panel::Code => {}
            Panel::Terminals => {
                panels.terminals = true;
                panels.front = None;
                panels.notes_alone = false;
            }
            Panel::Console => {
                panels.terminals = true;
                panels.console = true;
                panels.front = Some(Panel::Console);
                panels.notes_alone = false;
                // its targets and tests come from the launch file, which may have changed
                self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx));
            }
            Panel::Notes => {
                panels.notes_alone = !panels.terminals || (panels.front == Some(Panel::Notes) && panels.notes_alone);
                panels.terminals = true;
                panels.front = Some(Panel::Notes);
            }
            _ => {
                if matches!(panel, Panel::Search | Panel::References) {
                    panels.results = panel;
                }
                let layout = &Config::get(cx).layout;
                if layout.collapsed.contains(&panel) || layout.hidden.contains(&panel) {
                    Config::update(cx, |config| {
                        config.layout.collapsed.retain(|other| *other != panel);
                        config.layout.hidden.retain(|other| *other != panel);
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
        let panels = &mut self.panels;
        match panel {
            Panel::Code => return,
            Panel::Terminals => {
                panels.terminals = false;
                panels.notes_alone = false;
                self.terminals_maximized = false;
            }
            Panel::Console => {
                panels.console = false;
                if panels.front == Some(Panel::Console) {
                    panels.front = None;
                }
            }
            // The terminals show again, or close if they opened for the notes.
            Panel::Notes => {
                if panels.front == Some(Panel::Notes) {
                    panels.front = None;
                    if panels.notes_alone {
                        panels.terminals = false;
                        self.terminals_maximized = false;
                    }
                }
                panels.notes_alone = false;
            }
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
    /// is (or hidden), above `before` or last.
    pub(crate) fn bring_panel(&mut self, panel: Panel, before: Option<Panel>, cx: &mut Context<Self>) {
        // With every panel hidden there's no place showing: it shows in its own.
        let Some(place) = Config::get(cx).layout.current() else {
            return self.show_panel(panel, cx);
        };
        Config::update(cx, |config| config.layout.move_panel(panel, place, before));
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

    /// Folds a side panel to its header, or unfolds it.
    fn toggle_collapsed(&mut self, panel: Panel, cx: &mut Context<Self>) {
        Config::update(cx, |config| {
            let collapsed = &mut config.layout.collapsed;
            if collapsed.contains(&panel) {
                collapsed.retain(|other| *other != panel);
            } else {
                collapsed.push(panel);
            }
        });
        cx.notify();
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
        if let Some(place) = Config::get(cx).layout.current() {
            self.click_place(place, cx);
        }
    }

    /// The columns: the side one, the code (with the terminals under it, if
    /// that's their place) and the terminals.
    pub(super) fn render_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let terminals = self.panels.terminals;
        if self.terminals_maximized && terminals {
            return self.terminals.clone().cached(StyleRefinement::default().size_full()).into_any_element();
        }
        let layout = Config::get(cx).layout.clone();
        let side = self.side_place(cx).is_some();
        let right = terminals && layout.dock == Dock::Right;
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
        row = row.child(resizable_panel().child(self.render_center(&layout, window, cx)));
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

    /// The side column: its place's panels, one above the other. One open
    /// panel takes the height the others leave (see `Layout::filler`). A
    /// place's icon dropped on it brings its panels.
    fn render_side(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        let panels = layout.current().map(|place| layout.panels(place)).unwrap_or_default();
        let open: Vec<Panel> = panels.iter().copied().filter(|panel| !layout.collapsed.contains(panel)).collect();
        let filler = Layout::filler(&open);
        // The first one's header needs no line above it.
        let sections: Vec<AnyElement> = panels
            .iter()
            .enumerate()
            .map(|(ix, &panel)| {
                let height = (!layout.collapsed.contains(&panel) && Some(panel) != filler).then(|| layout.height(panel));
                self.render_section(panel, open.contains(&panel), height, ix > 0, window, cx)
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
            .drag_over::<PlaceDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.1)))
            .on_drop(cx.listener(|this, drag: &PlaceDrag, _, cx| this.merge_place(drag.0, cx)))
            .children(sections)
            // Every panel folded: the space below them.
            .when(filler.is_none(), |el| el.child(div().flex_1()))
            .into_any_element()
    }

    /// A side panel: its header, which folds it, and while open its content;
    /// `height` it has unless it takes what the others leave.
    fn render_section(
        &mut self,
        panel: Panel,
        open: bool,
        height: Option<f32>,
        line: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let workspace = cx.entity().downgrade();
        let actions = match panel {
            Panel::Workspaces => {
                let actions = self.workspaces.as_ref().and_then(|view| view.read(cx).actions.clone());
                actions.map(|actions| actions(window, cx))
            }
            _ => None,
        };
        let header = h_flex()
            .id(("side-header", panel as usize))
            .when(cfg!(test), |el| el.debug_selector(move || format!("title-{panel:?}")))
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
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    // The files go by their folder, as in VS Code.
                    .child(match panel {
                        Panel::Files => folder_label(&self.root),
                        _ => title(panel).to_uppercase(),
                    }),
            )
            .children(actions.map(|actions| div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(actions)))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_collapsed(panel, cx)))
            .on_drag(PanelDrag(panel), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into())))
            .drag_over::<PanelDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
            .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                cx.stop_propagation();
                this.bring_panel(drag.0, Some(panel), cx);
            }))
            .context_menu({
                let workspace = workspace.clone();
                move |menu, window, cx| {
                    // Its own icon, if it has company where it is.
                    let layout = &Config::get(cx).layout;
                    let company = layout.place_of(panel).is_some_and(|place| layout.panels(place).len() > 1);
                    let fold = menu::item(if open { "Collapse" } else { "Expand" }, &workspace, move |this, _, cx| this.toggle_collapsed(panel, cx));
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
            .id(("side-panel", panel as usize))
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
            .id(("side-content", panel as usize))
            .flex_1()
            .min_h_0()
            .pt_1()
            .children(content)
            .capture_any_mouse_down(move |event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right {
                    let workspace = hide.clone();
                    let hide: Rc<dyn Fn(&mut App)> = Rc::new(move |cx| {
                        workspace.update(cx, |this, cx| this.remove_panel(panel, cx)).ok();
                    });
                    // Its own icon, if it has company where it is.
                    let layout = &Config::get(cx).layout;
                    let company = layout.place_of(panel).is_some_and(|place| layout.panels(place).len() > 1);
                    let workspace = hide_workspace.clone();
                    let own_icon: Option<Rc<dyn Fn(&mut App)>> = company.then(|| {
                        Rc::new(move |cx: &mut App| {
                            workspace.update(cx, |this, cx| this.own_place(panel, cx)).ok();
                        }) as Rc<dyn Fn(&mut App)>
                    });
                    menu::set_panel_under(Some(hide), own_icon, cx);
                }
            });
        let section = section.child(content);
        match height {
            // It takes what the others leave.
            None => section.flex_1().min_h(px(HEADER_HEIGHT + MIN_BODY)).into_any_element(),
            // Its lower edge sizes it.
            Some(height) => section
                .flex_none()
                .h(px(height.max(HEADER_HEIGHT + MIN_BODY)))
                .child(
                    div()
                        .id(("side-resize", panel as usize))
                        .h(px(4.))
                        .w_full()
                        .flex_none()
                        .cursor_row_resize()
                        .on_drag(ResizeSide(panel), |drag, _, _, cx| cx.new(|_| drag.clone())),
                )
                .on_drag_move(cx.listener(move |_, event: &DragMoveEvent<ResizeSide>, _, cx| {
                    if event.drag(cx).0 != panel {
                        return;
                    }
                    let height = f32::from(event.event.position.y - event.bounds.top()).max(HEADER_HEIGHT + MIN_BODY);
                    Config::update_quietly(cx, |config| {
                        config.layout.heights.insert(panel, height);
                    });
                    cx.notify();
                }))
                .into_any_element(),
        }
    }

    /// The tabs after the terminals': the debugger once shown, with its
    /// state on it (yellow while stopped, green while running), and at the
    /// far end the notes, always there unless in a tab of the code.
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
                closable: true,
                dot,
            });
        }
        if self.notes_tab().is_none() {
            tabs.push(PanelTab {
                panel: Panel::Notes,
                view: self.notes.clone().into(),
                icon: notes_icon(self.notes.read(cx).filled()),
                title: "Notes",
                showing: self.panels.front == Some(Panel::Notes),
                closable: false,
                dot: None,
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
