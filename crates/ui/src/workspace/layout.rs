//! Where the panels go: columns of stacks of panels, one showing at a time
//! (see `config::Layout`), changed by dragging a panel's icon in the activity
//! bar, and which panel each stack shows.
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::*;
use crate::config::{Layout, Panel, Side, Stack};
use crate::drag_drop::DropPlacement;

/// A panel being moved: by its icon, its title or its tab among the
/// terminals'.
#[derive(Clone)]
pub(crate) struct PanelDrag(pub Panel);

/// The app's workspaces column, drawn where its panel is placed.
pub(crate) struct WorkspacesPanel {
    render: Box<dyn Fn(&mut Window, &mut App) -> AnyElement>,
}

impl WorkspacesPanel {
    pub fn new(render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { render: Box::new(render) }
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
        Panel::History => "icons/history.svg",
        Panel::Commit => "icons/git-commit.svg",
        Panel::Search => "icons/search.svg",
        Panel::References => "icons/references.svg",
        Panel::Outline => "icons/list-tree.svg",
        Panel::Code => "icons/code.svg",
        Panel::Terminals => "icons/terminal.svg",
        Panel::Debugger => "icons/bug.svg",
        Panel::Device => "icons/smartphone.svg",
        Panel::Notes => "icons/notebook-pen.svg",
    }
}

pub(crate) fn title(panel: Panel) -> &'static str {
    match panel {
        Panel::Workspaces => "Workspaces",
        Panel::Agents => "Agents",
        Panel::Files => "Files",
        Panel::Changes => "Changes",
        Panel::History => "History",
        Panel::Commit => "Commit Files",
        Panel::Search => "Search",
        Panel::References => "References",
        Panel::Outline => "Outline",
        Panel::Code => "Code",
        Panel::Terminals => "Terminals",
        Panel::Debugger => "Debugger",
        Panel::Device => "Device",
        Panel::Notes => "Notes",
    }
}

/// How tall a panel's bar is: its title, or its own (the terminals' tabs).
const BAR_HEIGHT: f32 = 34.;

/// The panels with no bar of their own, which their stack's header gives
/// them: their title.
fn has_header(panel: Panel) -> bool {
    matches!(panel, Panel::Files | Panel::Changes | Panel::History | Panel::Commit | Panel::Search | Panel::References | Panel::Outline | Panel::Notes)
}

/// Which panel each stack shows and the stacks closed: the same for every
/// task of a window, as the places are, so going to another one only changes
/// what the panels have in them (and picking a workspace from its column
/// leaves the column there). Each window has its own. It goes by panel, not
/// by place, so it outlives moves. The code is never closed: in its stack, a
/// closed panel gives way to it.
pub(super) struct Panels {
    /// When each was last shown: a stack shows its most recent one.
    shown: HashMap<Panel, u64>,
    /// A stack is closed when the panel it shows is.
    closed: HashSet<Panel>,
    clock: u64,
    /// Whether the app shows the workspaces column, as last told; unset
    /// until it says.
    workspaces: Option<bool>,
    /// Showing or hiding the workspaces column is remembered (in
    /// `config.tasks_column`); not in a window opened with `den -s`.
    remember: bool,
}

/// The panels of each window.
#[derive(Default)]
struct WindowPanels(HashMap<WindowId, Panels>);

impl Global for WindowPanels {}

impl Panels {
    pub fn of(window: WindowId, cx: &App) -> &Self {
        cx.global::<WindowPanels>().0.get(&window).expect("made with the window's first task")
    }

    pub fn of_mut(window: WindowId, cx: &mut App) -> &mut Self {
        cx.default_global::<WindowPanels>().0.entry(window).or_insert_with(|| Self::new(true))
    }

    /// The window's, made by its first task unless the app made them first
    /// (see `init_panels`).
    pub fn init(window: WindowId, cx: &mut App) {
        Self::of_mut(window, cx);
    }

    fn new(remember: bool) -> Self {
        Self {
            shown: HashMap::from([(Panel::Files, 1), (Panel::Code, 2)]),
            // The workspaces as the app says (see `set_workspaces`).
            closed: HashSet::from([Panel::Debugger, Panel::Device, Panel::Workspaces]),
            clock: 2,
            workspaces: None,
            remember,
        }
    }

    /// The panel `stack` shows: the last shown (the first, if none was).
    /// The commit's files, with the history, are part of it.
    pub fn active(&self, stack: &Stack) -> Panel {
        let code = stack.panels.contains(&Panel::Code);
        let history = stack.panels.contains(&Panel::History);
        let at = |panel: &&Panel| (!(code && self.closed.contains(*panel)), self.stamp(**panel));
        *stack
            .panels
            .iter()
            .rev()
            .filter(|panel| !(history && **panel == Panel::Commit))
            .max_by_key(at)
            .expect("a stack has panels")
    }

    pub fn stamp(&self, panel: Panel) -> u64 {
        self.shown.get(&panel).copied().unwrap_or(0)
    }

    pub fn stack_open(&self, stack: &Stack) -> bool {
        !self.closed.contains(&self.active(stack))
    }

    pub fn is_shown(&self, layout: &Layout, panel: Panel) -> bool {
        let panel = part_of(layout, panel);
        layout
            .find(panel)
            .is_some_and(|(column, stack)| self.active(&layout.columns[column].stacks[stack]) == panel)
            && !self.closed.contains(&panel)
    }

    pub fn show(&mut self, panel: Panel) {
        self.clock += 1;
        self.shown.insert(panel, self.clock);
        self.closed.remove(&panel);
    }

    /// The workspaces column as the app shows it: shown again, it's the
    /// panel its stack shows. Returns whether it changed.
    fn set_column(&mut self, visible: bool) -> bool {
        if self.workspaces == Some(visible) {
            return false;
        }
        if !visible {
            self.closed.insert(Panel::Workspaces);
        } else if self.workspaces.is_none() {
            self.closed.remove(&Panel::Workspaces);
        } else {
            self.show(Panel::Workspaces);
        }
        self.workspaces = Some(visible);
        true
    }

    /// Closes its stack, if it's the panel the stack shows.
    pub fn hide(&mut self, layout: &Layout, panel: Panel) {
        let panel = part_of(layout, panel);
        if panel != Panel::Code && self.is_shown(layout, panel) {
            self.closed.insert(panel);
        }
    }
}

/// The panel that shows `panel`: the history its commit's files are part of
/// while they share its place, or the panel itself.
fn part_of(layout: &Layout, panel: Panel) -> Panel {
    if panel == Panel::Commit && layout.commit_in_history() { Panel::History } else { panel }
}

fn side(placement: DropPlacement) -> Side {
    match placement {
        DropPlacement::Center => Side::Tab(None),
        DropPlacement::Left => Side::Left,
        DropPlacement::Right => Side::Right,
        DropPlacement::Top => Side::Top,
        DropPlacement::Bottom => Side::Bottom,
    }
}

/// What shows goes back to how it starts, in every window: the files, the
/// code and the terminals (Reset Layout).
pub(crate) fn reset_panels(cx: &mut App) {
    for panels in cx.default_global::<WindowPanels>().0.values_mut() {
        *panels = Panels::new(panels.remember);
    }
}

/// The panels of a window the app opens: with `remember`, showing or hiding
/// the workspaces column is saved.
pub(crate) fn init_panels(window: WindowId, remember: bool, cx: &mut App) {
    cx.default_global::<WindowPanels>().0.insert(window, Panels::new(remember));
}

/// The window's workspaces column, as last shown or hidden: unset if it
/// never was.
pub(crate) fn column_shown(window: WindowId, cx: &App) -> Option<bool> {
    cx.try_global::<WindowPanels>()?.0.get(&window)?.workspaces
}

/// Shows or hides the window's workspaces column while it has no task.
pub(crate) fn set_column(window: WindowId, visible: bool, cx: &mut App) {
    Panels::of_mut(window, cx).set_column(visible);
}

/// Forgets a closed window's panels.
pub(crate) fn drop_panels(window: WindowId, cx: &mut App) {
    cx.default_global::<WindowPanels>().0.remove(&window);
}

impl Workspace {
    pub(crate) fn is_shown(&self, panel: Panel, cx: &App) -> bool {
        Panels::of(self.window_id, cx).is_shown(&Config::get(cx).layout, panel)
    }

    /// Shows `panel` in its stack, opening the stack; focus doesn't move.
    pub(crate) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && Panels::of(self.window_id, cx).workspaces != Some(true) {
            self.column_shown(true, cx);
        }
        let panel = part_of(&Config::get(cx).layout, panel);
        Panels::of_mut(self.window_id, cx).show(panel);
        match panel {
            Panel::Changes => self.changes.update(cx, |changes, cx| changes.shown(cx)),
            Panel::History => self.history.update(cx, |history, cx| history.shown(cx)),
            Panel::Debugger => self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx)),
            Panel::Device => self.device.update(cx, |device, cx| device.shown(cx)),
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn hide_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && self.is_shown(panel, cx) {
            self.column_shown(false, cx);
        }
        let layout = Config::get(cx).layout.clone();
        // The notes' tab stays: the terminals show instead.
        if panel == Panel::Notes && with_terminals(&layout, panel) {
            let panels = Panels::of_mut(self.window_id, cx);
            if panels.is_shown(&layout, panel) {
                panels.show(Panel::Terminals);
            }
            cx.notify();
            return;
        }
        if panel == Panel::Debugger && with_terminals(&layout, panel) {
            // Its tab closes, and the terminals show if it was in front.
            let panels = Panels::of_mut(self.window_id, cx);
            if panels.is_shown(&layout, panel) {
                panels.show(Panel::Terminals);
            }
            panels.closed.insert(panel);
            cx.notify();
            return;
        }
        Panels::of_mut(self.window_id, cx).hide(&layout, panel);
        if panel == Panel::Terminals {
            self.terminals_maximized = false;
        }
        cx.notify();
    }

    /// The workspaces column shown or hidden by hand.
    fn column_shown(&mut self, visible: bool, cx: &mut Context<Self>) {
        let panels = Panels::of_mut(self.window_id, cx);
        panels.workspaces = Some(visible);
        if panels.remember {
            Config::update(cx, |config| config.tasks_column = Some(visible));
        }
    }

    /// The same key that shows a panel hides it; focus doesn't move.
    pub(super) fn toggle_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if self.is_shown(panel, cx) {
            self.hide_panel(panel, cx);
        } else {
            self.show_panel(panel, cx);
        }
    }

    /// The app's workspaces column and whether it shows, which is the same
    /// for every task: shown again, it's the panel its stack shows.
    pub fn set_workspaces(&mut self, view: &Entity<WorkspacesPanel>, visible: bool, cx: &mut Context<Self>) {
        if self.workspaces.is_none() {
            self.workspaces = Some(view.clone());
        }
        if Panels::of_mut(self.window_id, cx).set_column(visible) {
            cx.notify();
        }
    }

    /// The selected commit's files: under the commits while they share the
    /// history's place, else their own panel. With the history hidden, it
    /// shows with them.
    pub(super) fn toggle_commit_files(&mut self, _: &ToggleCommitFiles, _: &mut Window, cx: &mut Context<Self>) {
        if !Config::get(cx).layout.commit_in_history() {
            self.toggle_panel(Panel::Commit, cx);
        } else if !self.is_shown(Panel::History, cx) {
            self.history.update(cx, |history, cx| history.show_files(true, cx));
            self.show_panel(Panel::History, cx);
        } else {
            self.history.update(cx, |history, cx| history.show_files(!history.files_open(cx), cx));
        }
    }

    /// Cmd-B: the stack with the files, whichever panel it shows.
    pub(super) fn toggle_side_panel(&mut self, _: &ToggleSidePanel, _: &mut Window, cx: &mut Context<Self>) {
        let layout = &Config::get(cx).layout;
        if let Some((column, stack)) = layout.find(Panel::Files) {
            let active = Panels::of(self.window_id, cx).active(&layout.columns[column].stacks[stack]);
            self.toggle_panel(active, cx);
        }
    }

    fn track_panel_drop(&mut self, anchor: Panel, event: &DragMoveEvent<PanelDrag>, cx: &mut Context<Self>) {
        let dragged = event.drag(cx).0;
        let position = event.event.position;
        // Over a panel's bar, it would join that place; only where it would
        // change something.
        let on_bar = position.y < event.bounds.top() + px(BAR_HEIGHT);
        let next = DropPlacement::at(event.bounds, position, !on_bar)
            .filter(|placement| Config::get(cx).layout.clone().move_panel(dragged, anchor, side(*placement)))
            .map(|placement| (anchor, placement));
        // Every stack's listener sees the move. Only clear this stack's own indicator.
        if (next.is_some() || self.panel_drop.is_some_and(|(target, _)| target == anchor)) && self.panel_drop != next {
            self.panel_drop = next;
            cx.notify();
        }
    }

    pub(super) fn drop_panel(&mut self, drag: &PanelDrag, anchor: Panel, side: Side, cx: &mut Context<Self>) {
        self.panel_drop = None;
        let before = self.git_panels().map(|(panel, _)| self.is_shown(panel, cx));
        let mut moved = false;
        Config::update(cx, |config| moved = config.layout.move_panel(drag.0, anchor, side));
        if moved {
            self.show_panel(drag.0, cx);
            // Left showing in the stack the dragged one left, they reread too.
            for ((panel, entity), shown) in self.git_panels().into_iter().zip(before) {
                if !shown && self.is_shown(panel, cx) {
                    entity.clone().update(cx, |entity, cx| entity.shown(cx));
                }
            }
            // The other windows' tasks share the places.
            cx.refresh_windows();
        }
        cx.notify();
    }

    /// The columns, with the code taking the width the others leave.
    pub(super) fn render_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        self.shape_debugger(&layout, cx);
        if self.terminals_maximized
            && let Some((column, stack)) = layout.find(Panel::Terminals)
        {
            return self.render_stack(&layout.columns[column].stacks[stack], cx);
        }
        // The open stacks of each column; a column with none is hidden.
        let visible: Vec<(usize, Vec<usize>)> = layout
            .columns
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                let stacks = (0..column.stacks.len()).filter(|&stack| Panels::of(self.window_id, cx).stack_open(&column.stacks[stack])).collect();
                (ix, stacks)
            })
            .filter(|(_, stacks): &(usize, Vec<usize>)| !stacks.is_empty())
            .collect();
        let code = layout.find(Panel::Code).map(|(column, _)| column);
        let key: Vec<Vec<&[Panel]>> = visible
            .iter()
            .map(|(column, stacks)| stacks.iter().map(|&stack| layout.columns[*column].stacks[stack].panels.as_slice()).collect())
            .collect();
        let state = self.split.state(self.width, &key, cx).clone();
        // With no saved width, a column takes half of what the others leave.
        let fixed: f32 = visible
            .iter()
            .filter(|(column, _)| Some(*column) != code)
            .filter_map(|(column, _)| layout.columns[*column].width)
            .sum();
        let half = ((f32::from(self.width) - fixed) / 2.).max(400.);
        // A panel in each sized column finds it again on resizing.
        let ids: Vec<Option<Panel>> = visible
            .iter()
            .map(|(column, _)| (Some(*column) != code).then(|| layout.columns[*column].stacks[0].panels[0]))
            .collect();
        let mut row = h_resizable("workspace-columns").with_state(&state);
        for (column, stacks) in &visible {
            let panel = resizable_panel();
            let panel = if Some(*column) == code {
                panel
            } else {
                let width = layout.columns[*column].width.unwrap_or(half);
                panel.size(config::width(width, 160., 4000.)).size_range(px(160.)..px(4000.))
            };
            row = row.child(panel.child(self.render_column(&layout, *column, stacks, window, cx)));
        }
        row.on_resize(move |state, _, cx| {
            let sizes = state.read(cx).sizes().clone();
            Config::update_quietly(cx, |config| {
                for (size, id) in sizes.iter().zip(&ids) {
                    if let Some(id) = id
                        && let Some((column, _)) = config.layout.find(*id)
                    {
                        config.layout.columns[column].width = Some(f32::from(*size));
                    }
                }
            });
        })
        .into_any_element()
    }

    /// The open stacks of a column, one above the other; the code's (or
    /// else the first) takes the height the others leave.
    fn render_column(&mut self, layout: &Layout, column: usize, stacks: &[usize], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let col = &layout.columns[column];
        if let [stack] = stacks {
            return self.render_stack(&col.stacks[*stack], cx);
        }
        let flexible = stacks.iter().copied().find(|&stack| col.stacks[stack].panels.contains(&Panel::Code)).unwrap_or(stacks[0]);
        let height = f32::from(window.viewport_size().height);
        while self.column_splits.len() <= column {
            self.column_splits.push(config::Split::new(cx));
        }
        let key: Vec<&[Panel]> = stacks.iter().map(|&stack| col.stacks[stack].panels.as_slice()).collect();
        let state = self.column_splits[column].state(px(height), &key, cx).clone();
        let ids: Vec<Option<Panel>> = stacks.iter().map(|&stack| (stack != flexible).then(|| col.stacks[stack].panels[0])).collect();
        let mut group = v_resizable(("workspace-column", column)).with_state(&state);
        for &stack in stacks {
            let panel = resizable_panel();
            let panel = if stack == flexible {
                panel
            } else {
                let size = col.stacks[stack].height.unwrap_or(height / 2.);
                panel.size(px(size.clamp(120., 2000.))).size_range(px(120.)..px(2000.))
            };
            group = group.child(panel.child(self.render_stack(&col.stacks[stack], cx)));
        }
        group
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes().clone();
                Config::update_quietly(cx, |config| {
                    for (size, id) in sizes.iter().zip(&ids) {
                        if let Some(id) = id
                            && let Some((column, stack)) = config.layout.find(*id)
                        {
                            config.layout.columns[column].stacks[stack].height = Some(f32::from(*size));
                        }
                    }
                });
            })
            .into_any_element()
    }

    /// The panel a stack shows, and where a dragged panel would go if dropped
    /// on it.
    fn render_stack(&self, stack: &Stack, cx: &mut Context<Self>) -> AnyElement {
        let active = Panels::of(self.window_id, cx).active(stack);
        let content = match active {
            Panel::Workspaces => match &self.workspaces {
                Some(workspaces) => workspaces.clone().into_any_element(),
                None => div().into_any_element(),
            },
            // Never placed: dropped from the config on reading.
            Panel::Agents => div().into_any_element(),
            Panel::Files => self.file_tree.clone().into_any_element(),
            Panel::Changes => self.changes.clone().into_any_element(),
            Panel::History => self.history.clone().into_any_element(),
            Panel::Commit => self.commit.clone().into_any_element(),
            Panel::Search => self.search.clone().into_any_element(),
            Panel::References => self.references.clone().into_any_element(),
            Panel::Outline => self.outline.clone().into_any_element(),
            Panel::Code => self.render_editor_area(cx).into_any_element(),
            Panel::Terminals => self.terminals.clone().into_any_element(),
            // A tab of the terminals', when it's in their place.
            Panel::Debugger if stack.panels.contains(&Panel::Terminals) => self.terminals.clone().into_any_element(),
            Panel::Debugger => self.debugger.clone().into_any_element(),
            Panel::Device => self.device.clone().into_any_element(),
            Panel::Notes if stack.panels.contains(&Panel::Terminals) => self.terminals.clone().into_any_element(),
            Panel::Notes => self.notes.clone().into_any_element(),
        };
        let drop = self.panel_drop.filter(|(target, _)| *target == active && cx.has_active_drag());
        // The terminals' tabs are the bar of what's a tab of theirs.
        let header = (has_header(active) && !stack.panels.contains(&Panel::Terminals)).then(|| self.render_stack_header(active, cx));
        let content = div().id("panel-stack-content").flex_1().min_h_0().child(content);
        // Its own menus end with Hide Panel; elsewhere in it, that alone.
        let content = if has_header(active) {
            content.context_menu(|menu, _, _| menu.item(menu::hide_panel())).into_any_element()
        } else {
            content.into_any_element()
        };
        let workspace = cx.entity().downgrade();
        let sidebar = cx.theme().sidebar;
        v_flex()
            .id(("panel-stack", active as usize))
            .when(cfg!(test), |el| el.debug_selector(move || format!("stack-{active:?}")))
            .relative()
            .size_full()
            .when_some(header, |el, header| el.bg(sidebar).child(header))
            .child(content)
            .capture_any_mouse_down(move |event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right {
                    let workspace = workspace.clone();
                    let hide = (active != Panel::Code).then(|| -> Rc<dyn Fn(&mut App)> {
                        Rc::new(move |cx| {
                            workspace.update(cx, |this, cx| this.hide_panel(active, cx)).ok();
                        })
                    });
                    menu::set_panel_under(hide, cx);
                }
            })
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<PanelDrag>, _, cx| {
                this.track_panel_drop(active, event, cx);
            }))
            .on_drop(cx.listener(move |this, drag: &PanelDrag, _, cx| {
                match this.panel_drop.filter(|(target, _)| *target == active) {
                    Some((_, placement)) => this.drop_panel(drag, active, side(placement), cx),
                    None => {
                        this.panel_drop = None;
                        cx.notify();
                    }
                }
            }))
            .when_some(drop, |el, (_, placement)| el.child(placement.indicator(cx)))
            .into_any_element()
    }

    fn render_stack_header(&self, active: Panel, cx: &mut Context<Self>) -> AnyElement {
        let workspace = cx.entity().downgrade();
        let theme = cx.theme();
        h_flex()
            .h(px(BAR_HEIGHT))
            .flex_none()
            .px_2()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(
                div()
                    .id("panel-stack-title")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .px_1()
                    .text_ui_small(cx)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.muted_foreground)
                    // As the workspaces column's.
                    .child(title(active).to_uppercase())
                    // Dragged, the panel moves, as by its icon.
                    .on_drag(PanelDrag(active), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into())))
                    .context_menu(move |menu, _, _| {
                        menu.item(menu::item("Hide Panel", &workspace, move |this, _, cx| this.hide_panel(active, cx)))
                    }),
            )
            .into_any_element()
    }

    /// The debugger is tall in a column of its own, wide in the code's; in
    /// the terminals' place, a tab after theirs while it's open. The notes
    /// there, a tab always.
    fn shape_debugger(&mut self, layout: &Layout, cx: &mut Context<Self>) {
        let column = |panel| layout.find(panel).map(|(column, _)| column);
        let tall = column(Panel::Debugger) != column(Panel::Code);
        let tab = with_terminals(layout, Panel::Debugger);
        self.debugger.update(cx, |debugger, _| {
            debugger.tall = tall;
            debugger.tab = tab;
        });
        let panels = Panels::of(self.window_id, cx);
        let showing = |panel| {
            layout.find(panel).is_some_and(|(column, stack)| panels.active(&layout.columns[column].stacks[stack]) == panel)
        };
        let tab = |panel, view: AnyView, dot, closable| PanelTab {
            panel,
            view,
            icon: icon(panel),
            title: if panel == Panel::Debugger { "Debug" } else { title(panel) },
            showing: showing(panel),
            dot,
            closable,
        };
        let mut tabs = Vec::new();
        if with_terminals(layout, Panel::Debugger) && !panels.closed.contains(&Panel::Debugger) {
            tabs.push(tab(Panel::Debugger, self.debugger.clone().into(), false, true));
        }
        // The notes' tab is always there.
        if with_terminals(layout, Panel::Notes) {
            tabs.push(tab(Panel::Notes, self.notes.clone().into(), self.notes.read(cx).filled(), false));
        }
        self.terminals.update(cx, |terminals, cx| terminals.set_panel_tabs(tabs, cx));
    }

    /// Whether the debugger is a tab of the terminals'.
    pub(super) fn debugger_with_terminals(&self, cx: &App) -> bool {
        with_terminals(&Config::get(cx).layout, Panel::Debugger)
    }
}

/// Whether `panel` is a tab of the terminals'.
fn with_terminals(layout: &Layout, panel: Panel) -> bool {
    layout.find(panel).is_some() && layout.find(panel) == layout.find(Panel::Terminals)
}
