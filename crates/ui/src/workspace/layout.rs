//! Where the panels go: columns of stacks of tabs (see `config::Layout`),
//! changed by dragging a panel's tab, and which panel each stack shows.
use std::collections::{HashMap, HashSet};

use super::*;
use crate::config::{Layout, Panel, Side, Stack};
use crate::drag_drop::DropPlacement;

/// Drawn first in a panel's own bar (the terminals', the debugger's): the
/// tabs of the stack it is in.
pub(crate) type Leading = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

#[derive(Clone)]
pub(super) struct PanelDrag(pub Panel);

/// The app's workspaces column, drawn where its panel is placed.
pub(crate) struct WorkspacesPanel {
    /// The tabs of the place it is in, drawn first in its header.
    pub leading: Option<Leading>,
    render: Box<dyn Fn(Option<Leading>, &mut Window, &mut App) -> AnyElement>,
}

impl WorkspacesPanel {
    pub fn new(render: impl Fn(Option<Leading>, &mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { leading: None, render: Box::new(render) }
    }
}

impl Render for WorkspacesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.render)(self.leading.clone(), window, cx)
    }
}

pub(super) fn icon(panel: Panel) -> &'static str {
    match panel {
        Panel::Workspaces => "icons/layers.svg",
        Panel::Files => "icons/files.svg",
        Panel::Changes => "icons/git-branch.svg",
        Panel::Search => "icons/text-search.svg",
        Panel::References => "icons/references.svg",
        Panel::Code => "icons/code.svg",
        Panel::Terminals => "icons/terminal.svg",
        Panel::Debugger => "icons/bug.svg",
    }
}

pub(super) fn title(panel: Panel) -> &'static str {
    match panel {
        Panel::Workspaces => "Workspaces",
        Panel::Files => "Files",
        Panel::Changes => "Changes",
        Panel::Search => "Search",
        Panel::References => "References",
        Panel::Code => "Code",
        Panel::Terminals => "Terminals",
        Panel::Debugger => "Debugger",
    }
}

/// How tall the bar with a stack's tabs is, its own or its panel's.
const BAR_HEIGHT: f32 = 34.;

/// The panels with no bar of their own, which their stack's header gives them.
fn has_header(panel: Panel) -> bool {
    matches!(panel, Panel::Files | Panel::Changes | Panel::Search | Panel::References)
}

/// Which panel each stack shows and the stacks closed. It's per task,
/// while the places are the same for all, so it goes by panel, not by place.
/// The code is never closed: in its stack, a closed panel gives way to it.
pub(super) struct Panels {
    /// When each was last shown: a stack shows its most recent one.
    shown: HashMap<Panel, u64>,
    /// A stack is closed when the panel it shows is.
    closed: HashSet<Panel>,
    clock: u64,
}

impl Panels {
    pub fn new() -> Self {
        Self {
            shown: HashMap::from([(Panel::Files, 1), (Panel::Code, 2)]),
            // The workspaces as the app says (see `set_workspaces`).
            closed: HashSet::from([Panel::Debugger, Panel::Workspaces]),
            clock: 2,
        }
    }

    /// The panel `stack` shows: the last shown (the first, if none was).
    pub fn active(&self, stack: &Stack) -> Panel {
        let code = stack.panels.contains(&Panel::Code);
        let at = |panel: &&Panel| (!(code && self.closed.contains(*panel)), self.stamp(**panel));
        *stack.panels.iter().rev().max_by_key(at).expect("a stack has panels")
    }

    pub fn stamp(&self, panel: Panel) -> u64 {
        self.shown.get(&panel).copied().unwrap_or(0)
    }

    pub fn stack_open(&self, stack: &Stack) -> bool {
        !self.closed.contains(&self.active(stack))
    }

    pub fn is_shown(&self, layout: &Layout, panel: Panel) -> bool {
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
        if panel != Panel::Code && self.is_shown(layout, panel) {
            self.closed.insert(panel);
        }
    }
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

impl Workspace {
    pub(crate) fn is_shown(&self, panel: Panel, cx: &App) -> bool {
        self.panels.is_shown(&Config::get(cx).layout, panel)
    }

    /// Shows `panel` in its stack, opening the stack; focus doesn't move.
    pub(crate) fn show_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && self.workspaces_visible != Some(true) {
            self.workspaces_visible = Some(true);
            Config::update(cx, |config| config.tasks_column = Some(true));
        }
        self.panels.show(panel);
        match panel {
            Panel::Changes => self.changes.update(cx, |changes, cx| changes.shown(cx)),
            Panel::Debugger => self.debugger.update(cx, |debugger, cx| debugger.refresh_launches(cx)),
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn hide_panel(&mut self, panel: Panel, cx: &mut Context<Self>) {
        if panel == Panel::Workspaces && self.is_shown(panel, cx) {
            self.workspaces_visible = Some(false);
            Config::update(cx, |config| config.tasks_column = Some(false));
        }
        self.panels.hide(&Config::get(cx).layout, panel);
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
        if self.workspaces_visible == Some(visible) {
            return;
        }
        if !visible {
            self.panels.closed.insert(Panel::Workspaces);
        } else if self.workspaces_visible.is_none() {
            self.panels.closed.remove(&Panel::Workspaces);
        } else {
            self.panels.show(Panel::Workspaces);
        }
        self.workspaces_visible = Some(visible);
        cx.notify();
    }

    /// Cmd-B: the stack with the files, whichever panel it shows.
    pub(super) fn toggle_side_panel(&mut self, _: &ToggleSidePanel, _: &mut Window, cx: &mut Context<Self>) {
        let layout = &Config::get(cx).layout;
        if let Some((column, stack)) = layout.find(Panel::Files) {
            let active = self.panels.active(&layout.columns[column].stacks[stack]);
            self.toggle_panel(active, cx);
        }
    }

    fn track_panel_drop(&mut self, anchor: Panel, event: &DragMoveEvent<PanelDrag>, cx: &mut Context<Self>) {
        let dragged = event.drag(cx).0;
        let position = event.event.position;
        // Over a bar with tabs, it would be one more tab; only where it would
        // change something.
        let on_tabs = position.y < event.bounds.top() + px(BAR_HEIGHT);
        let next = DropPlacement::at(event.bounds, position, !on_tabs)
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
        let changes = self.is_shown(Panel::Changes, cx);
        let mut moved = false;
        Config::update(cx, |config| moved = config.layout.move_panel(drag.0, anchor, side));
        if moved {
            self.show_panel(drag.0, cx);
            // Left showing in the stack the dragged one left, it rereads too.
            if !changes && self.is_shown(Panel::Changes, cx) {
                self.changes.update(cx, |changes, cx| changes.shown(cx));
            }
            // The other windows' tasks share the places.
            cx.refresh_windows();
        }
        cx.notify();
    }

    /// The columns, with the code taking the width the others leave.
    pub(super) fn render_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let layout = Config::get(cx).layout.clone();
        self.place_panels(&layout, cx);
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
                let stacks = (0..column.stacks.len()).filter(|&stack| self.panels.stack_open(&column.stacks[stack])).collect();
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

    /// The panel a stack shows, under its tabs, and where a dragged panel
    /// would go if dropped on it.
    fn render_stack(&self, stack: &Stack, cx: &mut Context<Self>) -> AnyElement {
        let active = self.panels.active(stack);
        let content = match active {
            Panel::Workspaces => match &self.workspaces {
                Some(workspaces) => workspaces.clone().into_any_element(),
                None => div().into_any_element(),
            },
            Panel::Files => self.file_tree.clone().into_any_element(),
            Panel::Changes => self.changes.clone().into_any_element(),
            Panel::Search => self.search.clone().into_any_element(),
            Panel::References => self.references.clone().into_any_element(),
            Panel::Code => self.render_editor_area(cx).into_any_element(),
            Panel::Terminals => self.terminals.clone().into_any_element(),
            Panel::Debugger => self.debugger.clone().into_any_element(),
        };
        let drop = self.panel_drop.filter(|(target, _)| *target == active && cx.has_active_drag());
        let header = has_header(active).then(|| self.render_stack_header(stack, active, cx));
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

    fn render_stack_header(&self, stack: &Stack, active: Panel, cx: &mut Context<Self>) -> AnyElement {
        let workspace = cx.entity().downgrade();
        let tabs = has_tabs(&stack.panels, cx).then(|| stack_tabs(&workspace, &stack.panels, active, cx));
        // Alone, its title goes where the tabs would.
        let alone = tabs.is_none();
        let theme = cx.theme();
        h_flex()
            .h(px(BAR_HEIGHT))
            .flex_none()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .children(tabs)
            .child(
                div()
                    .id("panel-stack-title")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .when(alone, |el| el.pl_1())
                    .when(!alone, |el| el.justify_end())
                    .pr_1()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(title(active))
                    .context_menu(move |menu, _, _| {
                        menu.item(menu::item("Hide", &workspace, move |this, _, cx| this.hide_panel(active, cx)))
                            .item(menu::reset_layout())
                    }),
            )
            .into_any_element()
    }

    /// Drawn first in the code's tab bar: the tabs of the stack it is in.
    pub(super) fn code_tabs(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let layout = &Config::get(cx).layout;
        let (column, stack) = layout.find(Panel::Code)?;
        let panels = &layout.columns[column].stacks[stack].panels;
        let tabs = stack_tabs(&cx.entity().downgrade(), panels, Panel::Code, cx);
        Some(h_flex().flex_none().h_full().px_1().border_r_1().border_color(cx.theme().border).child(tabs).into_any_element())
    }

    /// Gives the panels that draw their own bar their stack's tabs, and the
    /// debugger its shape.
    fn place_panels(&mut self, layout: &Layout, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        let code = layout.find(Panel::Code).map(|(column, _)| column);
        for panel in [Panel::Workspaces, Panel::Terminals, Panel::Debugger] {
            let Some((column, stack)) = layout.find(panel) else {
                continue;
            };
            let panels = layout.columns[column].stacks[stack].panels.clone();
            let workspace = workspace.clone();
            let leading: Option<Leading> = has_tabs(&panels, cx).then(|| {
                Rc::new(move |_: &mut Window, cx: &mut App| div().px_1().child(stack_tabs(&workspace, &panels, panel, cx)).into_any_element())
                    as Leading
            });
            if panel == Panel::Workspaces {
                if let Some(workspaces) = &self.workspaces {
                    workspaces.update(cx, |workspaces, _| workspaces.leading = leading);
                }
            } else if panel == Panel::Terminals {
                self.terminals.update(cx, |terminals, _| terminals.leading = leading);
            } else {
                let tall = Some(column) != code;
                self.debugger.update(cx, |debugger, _| {
                    debugger.leading = leading;
                    debugger.tall = tall;
                });
            }
        }
    }
}

/// Whether a stack shows its tabs: alone, a panel's icon in the activity bar
/// already shows it and drags it, but for the code's, which has none there.
fn has_tabs(panels: &[Panel], cx: &App) -> bool {
    panels.len() > 1 || panels == [Panel::Code] || !Config::get(cx).shows_activity_bar()
}

/// A stack's tabs: a click shows the panel, dragging one moves it.
fn stack_tabs(workspace: &WeakEntity<Workspace>, panels: &[Panel], active: Panel, cx: &App) -> AnyElement {
    h_flex()
        .flex_none()
        .gap_1()
        .children(panels.iter().map(|&panel| {
            let click = workspace.clone();
            let drop = workspace.clone();
            mode_button(("panel-tab", panel as usize), icon(panel), panel == active, cx)
                .when(cfg!(test), |el| el.debug_selector(move || format!("panel-tab-{panel:?}")))
                .tooltip(move |window, cx| Tooltip::new(title(panel)).build(window, cx))
                .on_click(move |_, _, cx| {
                    click.update(cx, |this, cx| this.show_panel(panel, cx)).ok();
                })
                .on_drag(PanelDrag(panel), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into())))
                .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                .on_drop(move |drag: &PanelDrag, _, cx| {
                    cx.stop_propagation();
                    drop.update(cx, |this, cx| this.drop_panel(drag, panel, Side::Tab(Some(panel)), cx)).ok();
                })
        }))
        .into_any_element()
}
