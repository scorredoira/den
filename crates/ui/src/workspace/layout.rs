//! Where the panels go: columns of stacks of panels, one showing at a time
//! (see `config::Layout`), changed by dragging a panel's icon in the activity
//! bar, and which panel each stack shows.
use std::collections::{HashMap, HashSet};

use super::*;
use crate::config::{Layout, Panel, Side, Stack};
use crate::drag_drop::DropPlacement;

#[derive(Clone)]
pub(super) struct PanelDrag(pub Panel);

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
        Panel::Code => "icons/code.svg",
        Panel::Terminals => "icons/terminal.svg",
        Panel::Debugger => "icons/bug.svg",
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
        Panel::Code => "Code",
        Panel::Terminals => "Terminals",
        Panel::Debugger => "Debugger",
    }
}

/// How tall a panel's bar is: its title, or its own (the terminals' tabs).
const BAR_HEIGHT: f32 = 34.;

/// The panels with no bar of their own, which their stack's header gives
/// them: their title.
fn has_header(panel: Panel) -> bool {
    matches!(panel, Panel::Files | Panel::Changes | Panel::History | Panel::Commit | Panel::Search | Panel::References)
}

/// Which panel each stack shows and the stacks closed: the same for every
/// task, as the places are, so going to another one only changes what the
/// panels have in them (and picking a workspace from its column leaves the
/// column there). It goes by panel, not by place, so it outlives moves. The
/// code is never closed: in its stack, a closed panel gives way to it.
pub(super) struct Panels {
    /// When each was last shown: a stack shows its most recent one.
    shown: HashMap<Panel, u64>,
    /// A stack is closed when the panel it shows is.
    closed: HashSet<Panel>,
    clock: u64,
    /// Whether the app shows the workspaces column, as last told; unset
    /// until it says.
    workspaces: Option<bool>,
}

impl Panels {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// The app's, made by the first task.
    pub fn init(cx: &mut App) {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::new());
        }
    }

    fn new() -> Self {
        Self {
            shown: HashMap::from([(Panel::Files, 1), (Panel::Code, 2)]),
            // The workspaces as the app says (see `set_workspaces`).
            closed: HashSet::from([Panel::Debugger, Panel::Workspaces]),
            clock: 2,
            workspaces: None,
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

impl Global for Panels {}

/// What shows goes back to how it starts: the files, the code and the
/// terminals (Reset Layout).
pub(crate) fn reset_panels(cx: &mut App) {
    cx.set_global(Panels::new());
}

impl Workspace {
    pub(crate) fn is_shown(&self, panel: Panel, cx: &App) -> bool {
        Panels::get(cx).is_shown(&Config::get(cx).layout, panel)
    }

    /// Shows `panel` in its stack, opening the stack; focus doesn't move.
    pub(crate) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && Panels::get(cx).workspaces != Some(true) {
            cx.global_mut::<Panels>().workspaces = Some(true);
            Config::update(cx, |config| config.tasks_column = Some(true));
        }
        let panel = part_of(&Config::get(cx).layout, panel);
        cx.global_mut::<Panels>().show(panel);
        match panel {
            Panel::Changes => self.changes.update(cx, |changes, cx| changes.shown(cx)),
            Panel::History => self.history.update(cx, |history, cx| history.shown(cx)),
            Panel::Debugger => self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx)),
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn hide_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && self.is_shown(panel, cx) {
            cx.global_mut::<Panels>().workspaces = Some(false);
            Config::update(cx, |config| config.tasks_column = Some(false));
        }
        let layout = Config::get(cx).layout.clone();
        if panel == Panel::Debugger && with_terminals(&layout) {
            // Its tab closes, and the terminals show if it was in front.
            let panels = cx.global_mut::<Panels>();
            if panels.is_shown(&layout, panel) {
                panels.show(Panel::Terminals);
            }
            panels.closed.insert(panel);
            cx.notify();
            return;
        }
        cx.global_mut::<Panels>().hide(&layout, panel);
        if panel == Panel::Terminals {
            self.terminals_maximized = false;
        }
        cx.notify();
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
        let panels = cx.global_mut::<Panels>();
        if panels.workspaces == Some(visible) {
            return;
        }
        if !visible {
            panels.closed.insert(Panel::Workspaces);
        } else if panels.workspaces.is_none() {
            panels.closed.remove(&Panel::Workspaces);
        } else {
            panels.show(Panel::Workspaces);
        }
        panels.workspaces = Some(visible);
        cx.notify();
    }

    /// Cmd-B: the stack with the files, whichever panel it shows.
    pub(super) fn toggle_side_panel(&mut self, _: &ToggleSidePanel, _: &mut Window, cx: &mut Context<Self>) {
        let layout = &Config::get(cx).layout;
        if let Some((column, stack)) = layout.find(Panel::Files) {
            let active = Panels::get(cx).active(&layout.columns[column].stacks[stack]);
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
                let stacks = (0..column.stacks.len()).filter(|&stack| Panels::get(cx).stack_open(&column.stacks[stack])).collect();
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
        let active = Panels::get(cx).active(stack);
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
            Panel::Code => self.render_editor_area(cx).into_any_element(),
            Panel::Terminals => self.terminals.clone().into_any_element(),
            // A tab of the terminals', when it's in their place.
            Panel::Debugger if stack.panels.contains(&Panel::Terminals) => self.terminals.clone().into_any_element(),
            Panel::Debugger => self.debugger.clone().into_any_element(),
        };
        let drop = self.panel_drop.filter(|(target, _)| *target == active && cx.has_active_drag());
        let header = has_header(active).then(|| self.render_stack_header(active, cx));
        let sidebar = cx.theme().sidebar;
        v_flex()
            .id(("panel-stack", active as usize))
            .when(cfg!(test), |el| el.debug_selector(move || format!("stack-{active:?}")))
            .relative()
            .size_full()
            .when_some(header, |el, header| el.bg(sidebar).child(header))
            .child(div().flex_1().min_h_0().child(content))
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
                    .context_menu(move |menu, _, _| {
                        menu.item(menu::item("Hide Panel", &workspace, move |this, _, cx| this.hide_panel(active, cx)))
                    }),
            )
            .into_any_element()
    }

    /// The debugger is tall in a column of its own, wide in the code's; in
    /// the terminals' place, a tab after theirs while it's open.
    fn shape_debugger(&mut self, layout: &Layout, cx: &mut Context<Self>) {
        let column = |panel| layout.find(panel).map(|(column, _)| column);
        let tall = column(Panel::Debugger) != column(Panel::Code);
        let tab = with_terminals(layout);
        self.debugger.update(cx, |debugger, _| {
            debugger.tall = tall;
            debugger.tab = tab;
        });
        let panels = Panels::get(cx);
        let tab = (tab && !panels.closed.contains(&Panel::Debugger)).then(|| {
            let showing = layout.find(Panel::Debugger).is_some_and(|(column, stack)| {
                panels.active(&layout.columns[column].stacks[stack]) == Panel::Debugger
            });
            (AnyView::from(self.debugger.clone()), showing)
        });
        self.terminals.update(cx, |terminals, cx| terminals.set_debug_tab(tab, cx));
    }

    /// Whether the debugger is a tab of the terminals'.
    pub(super) fn debugger_with_terminals(&self, cx: &App) -> bool {
        with_terminals(&Config::get(cx).layout)
    }
}

fn with_terminals(layout: &Layout) -> bool {
    layout.find(Panel::Debugger).is_some() && layout.find(Panel::Debugger) == layout.find(Panel::Terminals)
}
