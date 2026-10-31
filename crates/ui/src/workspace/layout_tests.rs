use super::*;
use crate::config::{Dock, Place};
use crate::splits::{Axis, Tree};
use core::prelude::v1::test;

/// Draws a workspace with a file open, as a new one shows: the explorer,
/// the code and the terminals; `layout` first changes where things go.
fn draw(cx: &mut TestAppContext, layout: impl FnOnce(&mut config::Layout)) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let mut config = Config::default();
        layout(&mut config.layout);
        cx.set_global(config);
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
        let mut tab = workspace.new_tab(PathBuf::from("main.ts"), false, window, cx);
        tab.content = Content::Ready;
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    (workspace, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = bounds(cx, selector).center();
    cx.simulate_click(at, Modifiers::default());
    cx.run_until_parked();
}

fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Bounds<Pixels> {
    cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
}

#[gpui_kit::test]
fn terminal_shortcut_toggles_on_first_press_after_workspace_entry(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| {
        // On first entry, terminals restore asynchronously after focus has
        // already gone to the editor. Restoring them must not steal focus.
        workspace.focus(window, cx);
        workspace.terminals = cx.new(|cx| crate::terminals::test_area(window, cx));
        assert!(crate::shortcuts::set("FindReferences", "", cx).is_ok());
        assert!(crate::shortcuts::set("ToggleTerminals", "shift-f12", cx).is_ok());
        assert!(workspace.is_shown(Panel::Terminals, cx));
        assert!(!workspace.terminals.read(cx).contains_focus(window, cx));
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("shift-f12");
    workspace.read_with(cx, |workspace, cx| assert!(!workspace.is_shown(Panel::Terminals, cx)));
    cx.simulate_keystrokes("shift-f12");
    workspace.update_in(cx, |workspace, window, cx| {
        assert!(workspace.is_shown(Panel::Terminals, cx));
        assert!(workspace.terminals.read(cx).contains_focus(window, cx));
    });
}

#[gpui_kit::test]
fn terminal_shortcut_survives_redraws_without_mouse_movement(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.terminals = cx.new(|cx| crate::terminals::test_area(window, cx));
        assert!(crate::shortcuts::set("FindReferences", "", cx).is_ok());
        assert!(crate::shortcuts::set("ToggleTerminals", "shift-f12", cx).is_ok());
        workspace.set_terminals_visible(true, window, cx);
    });
    cx.run_until_parked();
    for _ in 0..8 {
        // Background updates redraw the workspace while the terminal is cached.
        workspace.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.simulate_keystrokes("shift-f12");
        workspace.read_with(cx, |workspace, cx| assert!(!workspace.is_shown(Panel::Terminals, cx)));
        cx.simulate_keystrokes("shift-f12");
        workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));
    }
}

#[gpui_kit::test]
fn the_side_column_the_code_and_the_terminals(cx: &mut TestAppContext) {
    let (_, cx) = draw(cx, |_| {});
    let side = bounds(cx, "side-column");
    let files = bounds(cx, "stack-Files");
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    assert!(files.left() >= side.left() && files.right() <= side.right(), "{files:?} {side:?}");
    assert!(side.right() <= code.left(), "{side:?} {code:?}");
    assert!(terminals.left() >= code.right(), "{terminals:?} {code:?}");
    // The explorer's panels one above the other, the outline folded.
    let workspaces = bounds(cx, "stack-Workspaces");
    assert!(workspaces.bottom() <= files.top(), "{workspaces:?} {files:?}");
    // The agents are off it until shown.
    assert!(cx.debug_bounds("stack-Agents").is_none());
    let outline = bounds(cx, "stack-Outline");
    assert!(outline.top() >= files.bottom() && outline.size.height <= px(27.), "{outline:?}");
}

#[gpui_kit::test]
fn the_terminals_go_under_the_code(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    assert!(terminals.top() >= code.bottom(), "{terminals:?} {code:?}");
    assert!(terminals.left() < code.right(), "{terminals:?} {code:?}");
    // And back on its right.
    workspace.update(cx, |workspace, cx| workspace.move_terminals(cx));
    cx.run_until_parked();
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    assert!(terminals.left() >= code.right(), "{terminals:?} {code:?}");
}

#[gpui_kit::test]
fn a_group_icon_shows_its_group_or_closes_the_column(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    click(cx, "activity-Place(Place(Changes))");
    assert!(cx.debug_bounds("stack-Files").is_none());
    bounds(cx, "stack-Changes");
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Changes, cx));
        assert!(!workspace.is_shown(Panel::Workspaces, cx));
    });
    // Its icon again: the column closes and the code takes its width.
    let code = bounds(cx, "editor-body-0");
    click(cx, "activity-Place(Place(Changes))");
    assert!(cx.debug_bounds("side-column").is_none());
    assert!(bounds(cx, "editor-body-0").left() < code.left());
    // Cmd-B brings it back with the same group.
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_side_panel(&ToggleSidePanel, window, cx));
    cx.run_until_parked();
    bounds(cx, "stack-Changes");
}

#[gpui_kit::test]
fn a_panel_folds_by_its_header(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let files = bounds(cx, "stack-Files");
    click(cx, "title-Workspaces");
    assert!(!workspace.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Workspaces, cx)));
    // Folded, the others take its room.
    assert!(bounds(cx, "stack-Files").top() < files.top());
    // Showing it unfolds it.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(workspace.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Workspaces, cx)));
    // Showing a panel of another group shows that group.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    cx.run_until_parked();
    bounds(cx, "stack-Changes");
    assert!(cx.debug_bounds("stack-Files").is_none());
}

#[gpui_kit::test]
fn a_panel_hides_or_gets_an_icon_of_its_own(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    // Hide Panel takes it off the column; the others stay.
    workspace.update(cx, |workspace, cx| workspace.remove_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("stack-Workspaces").is_none());
    bounds(cx, "stack-Files");
    // Shown again, it's back where it was.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(bounds(cx, "stack-Workspaces").bottom() <= bounds(cx, "stack-Files").top());
    // With an icon of its own, it has the column to itself.
    workspace.update(cx, |workspace, cx| workspace.own_place(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("stack-Files").is_none());
    let alone = bounds(cx, "stack-Workspaces");
    assert!(alone.size.height > bounds(cx, "side-column").size.height / 2., "{alone:?}");
    click(cx, "activity-Place(Place(Files))");
    bounds(cx, "stack-Files");
    assert!(cx.debug_bounds("stack-Workspaces").is_none());
    click(cx, "activity-Place(Place(Workspaces))");
    bounds(cx, "stack-Workspaces");
    // The menu checks what's in this place; the files, from the menu, come here.
    let panels = workspace.read_with(cx, |workspace, cx| workspace.menu_panels(cx));
    assert!(panels.contains(&(Panel::Workspaces, true)) && panels.contains(&(Panel::Files, false)), "{panels:?}");
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_from_menu(Panel::Files, window, cx));
    cx.run_until_parked();
    assert!(bounds(cx, "stack-Workspaces").bottom() <= bounds(cx, "stack-Files").top());
    // Clicking a workspace, going to another, moves nothing.
    let other = workspace.update_in(cx, |_, window, cx| cx.new(|cx| Workspace::new(PathBuf::from("/other"), None, true, "other".into(), window, cx)));
    other.read_with(cx, |other, cx| assert_eq!(other.side_place(cx), Some(Place(Panel::Workspaces))));
}

/// From the menu, a panel already in the place, or hidden there, comes
/// back in its spot; with the column closed, nothing is checked.
#[gpui_kit::test]
fn the_menu_brings_a_panel_back_in_its_spot(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let checked = |cx: &mut VisualTestContext| {
        let panels = workspace.read_with(cx, |workspace, cx| workspace.menu_panels(cx));
        panels.into_iter().filter(|(panel, checked)| *checked && config::Group::of(*panel).is_some()).count()
    };
    assert!(checked(cx) > 0);
    click(cx, "activity-Place(Place(Workspaces))");
    assert!(cx.debug_bounds("side-column").is_none());
    assert_eq!(checked(cx), 0);
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_from_menu(Panel::Files, window, cx));
    cx.run_until_parked();
    let files = bounds(cx, "stack-Files");
    assert!(bounds(cx, "stack-Workspaces").bottom() <= files.top() && files.bottom() <= bounds(cx, "stack-Outline").top());
    // The agents, hidden, come back between the workspaces and the files.
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_from_menu(Panel::Agents, window, cx));
    cx.run_until_parked();
    let agents = bounds(cx, "stack-Agents");
    assert!(bounds(cx, "stack-Workspaces").bottom() <= agents.top() && agents.bottom() <= bounds(cx, "stack-Files").top());
}

/// The graph is a tab of the code, and it reopens with the workspace.
#[gpui_kit::test]
fn the_graph_is_a_tab_that_reopens(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.restore(window, cx);
        workspace.toggle_graph(window, cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        let tabs = workspace.tabs.iter().filter(|tab| matches!(tab.page, Some(pages::Page::Graph))).count();
        assert_eq!(tabs, 1);
        assert_eq!(workspace.active, workspace.graph_tab());
        assert!(workspace.graph_visible());
    });
    bounds(cx, "history-commits");
    let again = workspace.update_in(cx, |_, window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
            workspace.restore(window, cx);
            workspace
        })
    });
    again.read_with(cx, |workspace, _| assert!(workspace.graph_tab().is_some()));
    // Show Graph closes it while it's in front.
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_graph(window, cx));
    assert!(workspace.read_with(cx, |workspace, _| workspace.graph_tab().is_none()));
}

/// The Graph tab has an icon on the activity bar, after the places': a
/// click opens it, another closes it.
#[gpui_kit::test]
fn the_graph_icon_toggles_its_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    assert!(bounds(cx, "activity-Graph").top() > bounds(cx, "activity-Place(Place(Changes))").top());
    click(cx, "activity-Graph");
    workspace.read_with(cx, |workspace, _| assert!(workspace.graph_visible()));
    bounds(cx, "history-commits");
    click(cx, "activity-Graph");
    workspace.read_with(cx, |workspace, _| assert!(workspace.graph_tab().is_none()));
}

#[gpui_kit::test]
fn the_debug_icon_toggles_its_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    assert!(bounds(cx, "activity-Debug").top() > bounds(cx, "activity-Graph").top());
    click(cx, "activity-Debug");
    workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Console, cx)));
    click(cx, "activity-Debug");
    workspace.read_with(cx, |workspace, cx| assert!(!workspace.is_shown(Panel::Console, cx)));
}

/// A file's history is the Graph tab's, with only the commits that changed
/// it; the Graph's icon shows every commit's again.
#[gpui_kit::test]
fn a_file_s_history_is_the_graph_s(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| workspace.open_history(Some(("main.ts".into(), false)), window, cx));
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| assert!(workspace.graph_visible()));
    bounds(cx, "history-file");
    click(cx, "activity-Graph");
    click(cx, "activity-Graph");
    workspace.read_with(cx, |workspace, _| assert!(workspace.graph_visible()));
    assert!(cx.debug_bounds("history-file").is_none());
}

/// The Graph tab in front leaves the panels as they were: the terminals
/// still show beside it.
#[gpui_kit::test]
fn the_graph_keeps_the_panels(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let editing = bounds(cx, "terminals");
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_graph(window, cx));
    cx.run_until_parked();
    assert_eq!(bounds(cx, "terminals"), editing);
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_graph(window, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminals").is_none(), "hidden in the graph, hidden after it");
}

#[gpui_kit::test]
fn the_workspaces_are_a_panel_of_the_explorer(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let panel = cx.update(|_, cx| {
        cx.new(|_| WorkspacesPanel::new(|_, _| div().size_full().debug_selector(|| "workspaces".into()).into_any_element()))
    });
    workspace.update(cx, |workspace, _| workspace.set_workspaces(&panel));
    workspace.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let workspaces = bounds(cx, "workspaces");
    assert!(workspaces.bottom() <= bounds(cx, "stack-Files").top());
    // Hiding them closes the explorer.
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("side-column").is_none());
}

/// The debugger is one tab of the terminals': its toolbar and its parts in
/// rows of two, the console beside the watches; the side column keeps the files.
#[gpui_kit::test]
fn the_debugger_is_a_tab_of_the_terminals(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    assert!(cx.debug_bounds("console-tab").is_none());
    // No icon of its own on the activity bar, nor panels in the side column.
    assert!(Config::default().layout.places.iter().flatten().flatten().all(|panel| config::Group::of(*panel).is_some()));
    // A session starts: the debugger's tab, in front.
    workspace.update(cx, |workspace, cx| workspace.reveal_debugger(cx));
    cx.run_until_parked();
    bounds(cx, "console-tab");
    bounds(cx, "stack-Files");
    let code = bounds(cx, "editor-body-0");
    let bar = bounds(cx, "debugger");
    let (tab, stack, watch) = (bounds(cx, "debug-tab"), bounds(cx, "debug-cell-Stack"), bounds(cx, "debug-cell-Watch"));
    let console = bounds(cx, "debug-console");
    assert!(bar.top() >= code.bottom(), "under the code: {bar:?} {code:?}");
    assert!(bar.bottom() <= stack.top() && watch.right() <= console.left() + px(1.), "{bar:?} {stack:?} {watch:?} {console:?}");
    // Wide too, rows of two, the variables wider than the call stack.
    rows_of_two(cx);
    let (stack, variables) = (bounds(cx, "debug-cell-Stack"), bounds(cx, "debug-cell-Variables"));
    assert!(variables.size.width > stack.size.width, "{stack:?} {variables:?}");
    let breakpoints = bounds(cx, "debug-cell-Breakpoints");
    assert!(breakpoints.size.width >= tab.size.width - px(1.), "with no terminal, the breakpoints at the tab's width");
    console_shows(cx);
    // Cmd-Shift-D hides it and shows it.
    workspace.update(cx, |workspace, cx| workspace.toggle_panel(Panel::Console, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("debug-tab").is_none());
    workspace.update(cx, |workspace, cx| workspace.toggle_panel(Panel::Console, cx));
    cx.run_until_parked();
    bounds(cx, "debug-tab");
    // Closed, the terminals show again and the tab goes.
    click(cx, "console-tab-close");
    assert!(cx.debug_bounds("console-tab").is_none());
    assert!(cx.debug_bounds("debug-console").is_none());
    workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));
}

/// The grid's cells in rows of two: the call stack and the variables above,
/// the watches and the console under them; the breakpoints (with no
/// terminal, alone) at the bottom.
fn rows_of_two(cx: &mut VisualTestContext) {
    let (stack, variables) = (bounds(cx, "debug-cell-Stack"), bounds(cx, "debug-cell-Variables"));
    let (watch, console) = (bounds(cx, "debug-cell-Watch"), bounds(cx, "debug-cell-Console"));
    assert!(stack.top() == variables.top() && stack.right() <= variables.left() + px(1.), "{stack:?} {variables:?}");
    assert!(watch.top() >= stack.bottom() - px(1.) && watch.top() == console.top(), "{stack:?} {watch:?} {console:?}");
    assert!(watch.right() <= console.left() + px(1.), "{watch:?} {console:?}");
    assert!(bounds(cx, "debug-cell-Breakpoints").top() >= watch.bottom() - px(1.));
}

/// The console is inside the tab, with a few lines at least.
fn console_shows(cx: &mut VisualTestContext) {
    let (tab, console) = (bounds(cx, "debug-tab"), bounds(cx, "debug-console"));
    assert!(console.bottom() <= tab.bottom() + px(1.) && console.size.height >= px(60.), "{tab:?} {console:?}");
}

/// In a narrow tab (the terminals on the code's right), the same rows of two.
#[gpui_kit::test]
fn a_narrow_debugger_has_rows_of_two(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| {
        layout.dock = Dock::Right;
        layout.dock_width = Some(500.);
    });
    workspace.update(cx, |workspace, cx| workspace.reveal_debugger(cx));
    cx.run_until_parked();
    rows_of_two(cx);
    console_shows(cx);
}

/// The splits of the tab with no terminal yet: two rows of parts over the
/// breakpoints.
const COLUMN: &str = "Stack+Variables|Watch+Console|Breakpoints";

/// Tall rows saved before (or a tab that got shorter) never cover the
/// console.
#[gpui_kit::test]
fn saved_rows_leave_the_console(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| {
        layout.dock = Dock::Right;
        layout.dock_width = Some(500.);
        layout.debug_grid.sizes.insert(COLUMN.into(), vec![5000., 5000.]);
    });
    workspace.update(cx, |workspace, cx| workspace.reveal_debugger(cx));
    cx.run_until_parked();
    rows_of_two(cx);
    console_shows(cx);
}

fn grid(cx: &mut VisualTestContext, change: impl FnOnce(&mut config::DebugGrid)) {
    cx.update(|_, cx| Config::update(cx, |config| change(&mut config.layout.debug_grid)));
    cx.run_until_parked();
}

fn reveal(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
    workspace.update(cx, |workspace, cx| workspace.reveal_debugger(cx));
    cx.run_until_parked();
}

/// Drags a part's header to `end`.
fn drag_part(cx: &mut VisualTestContext, header: &'static str, end: Point<Pixels>) {
    let start = bounds(cx, header).center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

fn leaf(part: config::DebugPart) -> Tree<config::DebugPart> {
    Tree::Leaf(part)
}

fn split(axis: Axis, children: Vec<Tree<config::DebugPart>>) -> Tree<config::DebugPart> {
    Tree::Split { axis, children }
}

/// A part's header dropped in the middle of another cell: the two swap,
/// and where they are is kept in the layout.
#[gpui_kit::test]
fn a_part_dropped_on_another_swaps_with_it(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    reveal(&workspace, cx);
    let end = bounds(cx, "debug-cell-Breakpoints").center();
    drag_part(cx, "debug-header-Stack", end);
    use config::DebugPart::*;
    let tree = cx.update(|_, cx| Config::get(cx).layout.debug_grid.tree());
    let expected = split(
        Axis::Column,
        vec![
            split(Axis::Row, vec![leaf(Breakpoints), leaf(Variables)]),
            split(Axis::Row, vec![leaf(Watch), leaf(Console)]),
            split(Axis::Row, vec![leaf(Terminal), leaf(Stack)]),
        ],
    );
    assert_eq!(tree, expected);
    let (breakpoints, stack) = (bounds(cx, "debug-cell-Breakpoints"), bounds(cx, "debug-cell-Stack"));
    let variables = bounds(cx, "debug-cell-Variables");
    assert!(breakpoints.top() == variables.top() && breakpoints.right() <= variables.left() + px(1.), "{breakpoints:?} {variables:?}");
    assert!(stack.top() >= breakpoints.bottom() - px(1.), "{stack:?} {breakpoints:?}");
    let saved = cx.update(|_, cx| serde_json::to_value(&Config::get(cx).layout).unwrap());
    assert_eq!(serde_json::from_value::<Tree<config::DebugPart>>(saved["debug_grid"]["tree"].clone()).unwrap(), expected);
}

/// A part's header dropped near another cell's edge goes beside it: the
/// console on the left of the call stack, at the height of its row; the
/// watches under the variables.
#[gpui_kit::test]
fn a_part_dropped_by_an_edge_goes_beside(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    reveal(&workspace, cx);
    let stack = bounds(cx, "debug-cell-Stack");
    drag_part(cx, "debug-header-Console", point(stack.left() + px(24.), stack.center().y));
    let (console, stack, variables) = (bounds(cx, "debug-cell-Console"), bounds(cx, "debug-cell-Stack"), bounds(cx, "debug-cell-Variables"));
    assert!(console.top() == stack.top() && console.right() <= stack.left() + px(1.), "{console:?} {stack:?}");
    assert!(stack.right() <= variables.left() + px(1.), "{stack:?} {variables:?}");
    let variables = bounds(cx, "debug-cell-Variables");
    drag_part(cx, "debug-header-Watch", point(variables.center().x, variables.bottom() - px(16.)));
    use config::DebugPart::*;
    let tree = cx.update(|_, cx| Config::get(cx).layout.debug_grid.tree());
    assert_eq!(
        tree,
        split(
            Axis::Column,
            vec![
                split(Axis::Row, vec![leaf(Console), leaf(Stack), split(Axis::Column, vec![leaf(Variables), leaf(Watch)])]),
                split(Axis::Row, vec![leaf(Terminal), leaf(Breakpoints)]),
            ],
        )
    );
    let (variables, watch) = (bounds(cx, "debug-cell-Variables"), bounds(cx, "debug-cell-Watch"));
    assert!(watch.top() >= variables.bottom() - px(1.) && watch.left() == variables.left(), "{variables:?} {watch:?}");
}

/// A hidden part's neighbour takes its space; shown again, it's back in its
/// place. Every part but the console hidden leaves it alone.
#[gpui_kit::test]
fn a_hidden_part_leaves_its_space(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    reveal(&workspace, cx);
    let tab = bounds(cx, "debug-tab");
    grid(cx, |grid| grid.set_hidden(config::DebugPart::Variables, true));
    assert!(cx.debug_bounds("debug-cell-Variables").is_none());
    let stack = bounds(cx, "debug-cell-Stack");
    assert!(stack.size.width >= tab.size.width - px(2.), "the call stack takes the row: {stack:?} {tab:?}");
    grid(cx, |grid| grid.set_hidden(config::DebugPart::Variables, false));
    rows_of_two(cx);
    use config::DebugPart::*;
    grid(cx, |grid| {
        for part in [Stack, Variables, Watch, Breakpoints, Terminal] {
            grid.set_hidden(part, true);
        }
    });
    let (bar, console) = (bounds(cx, "debugger"), bounds(cx, "debug-cell-Console"));
    assert!(console.top() <= bar.bottom() + px(2.) && console.bottom() >= tab.bottom() - px(2.), "{bar:?} {console:?} {tab:?}");
}

/// The sizes as dragged are kept, and go with the layout: the debugging one
/// has its own.
#[gpui_kit::test]
fn the_grid_sizes_are_kept_with_the_layout(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.dock = Dock::Bottom);
    debug(&workspace, cx, true);
    grid(cx, |grid| {
        grid.sizes.insert("Stack|Variables".into(), vec![300.]);
        grid.sizes.insert(COLUMN.into(), vec![100., 80.]);
    });
    let stack = bounds(cx, "debug-cell-Stack");
    assert!((stack.size.width - px(300.)).abs() <= px(2.), "{stack:?}");
    assert!((stack.size.height - px(100.)).abs() <= px(2.), "{stack:?}");
    debug(&workspace, cx, false);
    assert!(cx.update(|_, cx| Config::get(cx).layout.debug_grid.sizes.is_empty()), "the editing layout's own");
    debug(&workspace, cx, true);
    assert_eq!(cx.update(|_, cx| Config::get(cx).layout.debug_grid.sizes.get("Stack|Variables").cloned()), Some(vec![300.]));
}

/// A layout saved before the parts could move loads as the default; a part
/// saved twice is kept once, and one missing goes under the rest.
#[test]
fn a_layout_without_the_tree_loads() {
    let default = config::DebugGrid::default().tree();
    let layout: config::Layout = serde_json::from_str(r#"{ "side": true, "debug_grid": { "order": ["Watch"] } }"#).unwrap();
    assert_eq!(layout.debug_grid.tree(), default);
    let layout: config::Layout = serde_json::from_str(r#"{ "debug_grid": { "tree": { "Leaf": "Threads" }, "hidden": ["Threads", "Stack"] } }"#).unwrap();
    assert_eq!(layout.debug_grid.tree(), default);
    assert_eq!(layout.debug_grid.hidden, [config::DebugPart::Stack]);
    let json = r#"{ "debug_grid": { "tree": { "Split": { "axis": "Row", "children": [{ "Leaf": "Watch" }, { "Leaf": "Watch" }, { "Leaf": "Stack" }] } } } }"#;
    let layout: config::Layout = serde_json::from_str(json).unwrap();
    use config::DebugPart::*;
    assert_eq!(
        layout.debug_grid.tree(),
        split(
            Axis::Column,
            vec![split(Axis::Row, vec![leaf(Watch), leaf(Stack)]), leaf(Variables), leaf(Breakpoints), leaf(Terminal), leaf(Console)],
        )
    );
}

/// The notes are a tab of the code, never among the terminals: their icon
/// opens it in front and, on the next click, brings back the tab that was
/// in front before; the terminals stay as they were.
#[gpui_kit::test]
fn the_notes_icon_toggles_their_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    assert!(cx.debug_bounds("notes-tab").is_none(), "no tab among the terminals'");
    click(cx, "activity-Notes");
    let notes = bounds(cx, "notes");
    assert!(notes.right() <= bounds(cx, "editor-body-0").right() + px(1.), "{notes:?}");
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active, workspace.notes_tab());
        assert!(workspace.is_shown(Panel::Notes, cx));
        assert!(workspace.is_shown(Panel::Terminals, cx));
    });
    click(cx, "activity-Notes");
    workspace.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.active, Some(0), "the file again");
        assert!(!workspace.is_shown(Panel::Notes, cx));
    });
    assert!(cx.debug_bounds("notes").is_none());
    click(cx, "activity-Notes");
    bounds(cx, "notes");
}

/// With the terminals closed, the notes open without them.
#[gpui_kit::test]
fn the_notes_open_without_the_terminals(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    cx.run_until_parked();
    click(cx, "activity-Notes");
    bounds(cx, "notes");
    workspace.read_with(cx, |workspace, cx| assert!(!workspace.is_shown(Panel::Terminals, cx)));
}

/// `den panel show notes` opens their tab, and `hide` the one before.
#[gpui_kit::test]
fn den_panel_shows_and_hides_the_notes(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| workspace.set_panel(Panel::Notes, true, window, cx));
    cx.run_until_parked();
    bounds(cx, "notes");
    workspace.update_in(cx, |workspace, window, cx| workspace.set_panel(Panel::Notes, false, window, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("notes").is_none());
    workspace.read_with(cx, |workspace, _| assert_eq!(workspace.active, Some(0)));
}

/// The side column is the same in every workspace: going from one to
/// another doesn't move it. The terminals are each one's.
#[gpui_kit::test]
fn the_side_column_stays_from_workspace_to_workspace(cx: &mut TestAppContext) {
    let (first, cx) = draw(cx, |_| {});
    let second = first.update_in(cx, |_, window, cx| cx.new(|cx| Workspace::new(PathBuf::from("/other"), None, true, "other".into(), window, cx)));
    first.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    second.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Changes, cx));
        assert!(!workspace.is_shown(Panel::Files, cx));
    });
    first.update(cx, |workspace, cx| workspace.hide_panel(Panel::Changes, cx));
    assert_eq!(second.read_with(cx, |workspace, cx| workspace.side_place(cx)), None);
    // A new one shows the terminals, nothing else of its own.
    second.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));
    second.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    assert!(first.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Terminals, cx)));
    // Where the terminals go is every workspace's.
    first.update(cx, |workspace, cx| workspace.set_dock(Dock::Bottom, cx));
    assert_eq!(cx.update(|_, cx| Config::get(cx).layout.dock), Dock::Bottom);
}

/// What a workspace shows is saved with what it has open: opened again, it
/// shows as it did.
#[gpui_kit::test]
fn a_workspace_reopens_with_its_panels(cx: &mut TestAppContext) {
    let (first, cx) = draw(cx, |_| {});
    first.update_in(cx, |workspace, window, cx| {
        workspace.restore(window, cx);
        workspace.show_panel(Panel::Changes, cx);
        workspace.hide_panel(Panel::Terminals, cx);
    });
    let again = first.update_in(cx, |_, window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
            workspace.restore(window, cx);
            workspace
        })
    });
    again.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.side_place(cx), Some(Place(Panel::Changes)));
        assert!(!workspace.is_shown(Panel::Terminals, cx));
    });
    // Reset Layout puts it back as a new one's.
    again.update(cx, |workspace, cx| workspace.reset_layout(cx));
    again.read_with(cx, |workspace, cx| {
        assert_eq!(workspace.side_place(cx), Some(Place(Panel::Workspaces)));
        assert!(workspace.is_shown(Panel::Terminals, cx));
    });
}

/// A `den doc` opens again with the workspace, with its text; the
/// shortcuts guide does not.
#[gpui_kit::test]
fn a_den_doc_reopens_with_the_workspace(cx: &mut TestAppContext) {
    let (first, cx) = draw(cx, |_| {});
    first.update_in(cx, |workspace, window, cx| {
        workspace.restore(window, cx);
        workspace.open_doc(crate::guide::TITLE, "# Guide".into(), false, window, cx);
        workspace.open_doc("gm servers: keys", "# Keys\n\nWho holds them.".into(), true, window, cx);
    });
    let again = first.update_in(cx, |_, window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
            workspace.restore(window, cx);
            workspace
        })
    });
    again.read_with(cx, |workspace, cx| {
        let docs: Vec<(String, String)> = workspace
            .tabs
            .iter()
            .filter(|tab| tab.doc)
            .map(|tab| (tab.path.display().to_string(), tab.editor.read(cx).value().to_string()))
            .collect();
        assert_eq!(docs, [("gm servers: keys".to_string(), "# Keys\n\nWho holds them.".to_string())]);
        let tab = workspace.tabs.iter().find(|tab| tab.doc).unwrap();
        assert!(tab.rendered().is_some() && tab.kept, "rendered, and kept again");
    });
}

/// Run and Debug go on the lines that declare a test, and nowhere else.
#[gpui_kit::test]
fn the_tests_get_run_and_debug(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/lens-test"), None, true, "lens-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        let file = crate::debug::parse_launch_file(
            r#"{"tests":{"match":"^export function (test\\w+)\\(","run":"run ${test}","debug":"debug ${test}"}}"#,
        )
        .unwrap();
        workspace.debugger.update(cx, |debugger, _| debugger.tests = file.tests);
        let mut tab = workspace.new_tab(PathBuf::from("/lens-test/a_test.ts"), false, window, cx);
        tab.content = Content::Ready;
        let code = "function helper() {}\n\nexport function testOne() {\n}\n";
        tab.editor.update(cx, |state, cx| state.set_value(code, window, cx));
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("test-lens-2-false").is_some(), "Run on the test");
    assert!(cx.debug_bounds("test-lens-2-true").is_some(), "Debug on the test");
    assert!(cx.debug_bounds("test-lens-0-false").is_none(), "nothing on a helper");
}

/// Typing in a file's rendered Markdown does nothing: it stays rendered.
#[gpui_kit::test]
fn typing_in_the_preview_does_nothing(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/md-test"), None, true, "md-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        let mut tab = workspace.new_tab(PathBuf::from("/md-test/README.md"), false, window, cx);
        tab.content = Content::Ready;
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    let source = |cx: &mut gpui_kit::VisualTestContext| workspace.read_with(cx, |workspace, _| workspace.tabs[0].show_source);
    assert!(!source(cx), "Markdown opens rendered");
    cx.simulate_keystrokes("x");
    assert!(!source(cx), "typing stays in the preview");
}

/// Zooming a rendered Markdown draws its diagrams larger, as far as it
/// goes, and resetting it brings them back.
#[gpui_kit::test]
fn zooming_the_preview_grows_its_diagrams(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (_workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/zoom-test"), None, true, "zoom-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        let mut tab = workspace.new_tab(PathBuf::from("/zoom-test/README.md"), false, window, cx);
        tab.content = Content::Ready;
        let text = "```mermaid\nflowchart TD\n A --> B\n```\n";
        tab.editor.update(cx, |state, cx| state.set_value(text, window, cx));
        tab.preview(text, window, cx);
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    let width = |cx: &mut gpui_kit::VisualTestContext| bounds(cx, "image diagram").size.width;
    let normal = width(cx);
    cx.dispatch_action(crate::ZoomInPreview);
    cx.run_until_parked();
    assert!(width(cx) > normal * 1.2, "{:?} after {normal:?}", width(cx));
    for _ in 0..10 {
        cx.dispatch_action(crate::ZoomInPreview);
    }
    cx.run_until_parked();
    let most = width(cx);
    assert!(most > normal * 2.9 && most < normal * 3.1, "{most:?} for {normal:?}");
    cx.dispatch_action(crate::ResetPreviewZoom);
    cx.run_until_parked();
    assert_eq!(width(cx), normal);
}

/// A doc's preview zooms with Cmd-= and Cmd-+, and Cmd-0 brings it back.
#[gpui_kit::test]
fn a_doc_zooms_with_the_keys(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
        crate::shortcuts::apply(cx);
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/doc-zoom"), None, true, "doc-zoom".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace.open_doc("gm servers: keys", "```mermaid\nflowchart TD\n A --> B\n```\n".into(), true, window, cx);
        workspace
    });
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| workspace.focus_active(window, cx));
    let width = |cx: &mut gpui_kit::VisualTestContext| bounds(cx, "image diagram").size.width;
    let normal = width(cx);
    cx.simulate_keystrokes("cmd-=");
    cx.run_until_parked();
    let bigger = width(cx);
    assert!(bigger > normal, "cmd-=: {bigger:?} after {normal:?}");
    cx.simulate_keystrokes("cmd-+");
    cx.run_until_parked();
    assert!(width(cx) > bigger, "cmd-+");
    cx.simulate_keystrokes("cmd-0");
    cx.run_until_parked();
    assert_eq!(width(cx), normal);
}

/// Over the preview, Cmd and the wheel zoom it, a pinch too, and the wheel
/// alone does not; fitting draws a diagram wider than the preview to fit.
#[gpui_kit::test]
fn the_mouse_zooms_the_preview_and_its_diagrams_fit(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let fan: String = (0..12).map(|i| format!(" A --> B{i}[a long box number {i}]\n")).collect();
    let wide = format!("```mermaid\nflowchart TD\n{fan}```\n");
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/wheel-zoom"), None, true, "wheel-zoom".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace.open_doc("wide", wide, true, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(700.), px(600.)));
    workspace.update_in(cx, |workspace, window, cx| workspace.focus_active(window, cx));
    cx.run_until_parked();
    let width = |cx: &mut gpui_kit::VisualTestContext| bounds(cx, "image diagram").size.width;
    let preview = bounds(cx, "markdown-preview-area");
    let normal = width(cx);
    let tall = bounds(cx, "image diagram").size.height;
    assert!(normal > preview.size.width, "the case is wider than the preview: {normal:?} in {:?}", preview.size.width);
    let wheel = |cx: &mut gpui_kit::VisualTestContext, modifiers: Modifiers| {
        cx.simulate_event(ScrollWheelEvent { position: preview.center(), delta: ScrollDelta::Lines(point(0., 1.)), modifiers, ..Default::default() });
        cx.run_until_parked();
    };
    wheel(cx, Modifiers::none());
    assert_eq!(width(cx), normal, "the wheel alone scrolls");
    wheel(cx, Modifiers::command());
    let bigger = width(cx);
    assert!(bigger > normal, "Cmd and the wheel: {bigger:?} after {normal:?}");
    cx.simulate_event(PinchEvent { position: preview.center(), delta: -0.2, ..Default::default() });
    cx.run_until_parked();
    assert!(width(cx) < bigger, "a pinch");
    cx.dispatch_action(crate::ResetPreviewZoom);
    cx.dispatch_action(crate::FitPreviewDiagrams);
    cx.run_until_parked();
    let fitted = bounds(cx, "image diagram").size;
    assert!(fitted.width <= preview.size.width, "fit: {:?} in {:?}", fitted.width, preview.size.width);
    // As much shorter: the box is the diagram's, no room left under it.
    let shorter = tall * (fitted.width / normal);
    assert!((fitted.height - shorter).abs() <= px(2.), "{:?}, not {shorter:?}", fitted.height);
    cx.dispatch_action(crate::FitPreviewDiagrams);
    cx.run_until_parked();
    assert_eq!(width(cx), normal);
}

/// Cmd and the wheel zoom the preview under the pointer, in either group,
/// not the active tab.
#[gpui_kit::test]
fn the_wheel_zooms_the_preview_under_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/two-zoom"), None, true, "two-zoom".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace.open_doc("left", "# Left\n".into(), true, window, cx);
        workspace.open_doc("right", "# Right\n".into(), true, window, cx);
        workspace.tabs[1].group = 1;
        workspace.editor_split = Some(Axis::Row);
        workspace.activate(1, window, cx);
        workspace
    });
    cx.simulate_resize(size(px(1000.), px(600.)));
    cx.run_until_parked();
    // Both previews have the same selector: the left one is in the left half.
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(250.), px(300.)),
        delta: ScrollDelta::Lines(point(0., 1.)),
        modifiers: Modifiers::command(),
        ..Default::default()
    });
    cx.run_until_parked();
    let zooms = workspace.read_with(cx, |workspace, _| [workspace.tabs[0].zoom.get(), workspace.tabs[1].zoom.get()]);
    assert!(zooms[0] > 1.0 && zooms[1] == 1.0, "{zooms:?}");
}

/// The wheel over a value's card in the code stays in the card: the code
/// under it doesn't scroll, and the card stays.
#[gpui_kit::test]
fn the_wheel_over_a_values_card_leaves_the_code(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/hover-test"), None, true, "hover-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        let mut tab = workspace.new_tab(PathBuf::from("/hover-test/a.ts"), false, window, cx);
        tab.content = Content::Ready;
        let code = (0..400).map(|line| format!("let value{line} = {line}\n")).collect::<String>();
        tab.editor.update(cx, |state, cx| state.set_value(code, window, cx));
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    let code = bounds(cx, "editor-body-0");
    let anchor = Bounds::new(code.origin + point(px(80.), px(40.)), size(px(40.), px(16.)));
    workspace.update(cx, |workspace, cx| {
        workspace.debugger.update(cx, |debugger, cx| {
            let var = |name: &str| crate::debug::protocol::Var { name: name.into(), value: "1".into(), kind: "number".into(), reference: 0, count: 0 };
            debugger.hover = Some(crate::debug::HoverValue { var: var("value3"), anchor });
            cx.notify();
        });
    });
    cx.run_until_parked();
    let card = bounds(cx, "debug-hover-card");
    let offset = |cx: &mut VisualTestContext| workspace.read_with(cx, |workspace, cx| workspace.tabs[0].editor.read(cx).scroll_offset());
    let before = offset(cx);
    cx.simulate_mouse_move(card.center(), None, Modifiers::default());
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, cx| assert!(workspace.debugger.read(cx).hover.is_some(), "the card stays after the move {card:?}"));
    cx.simulate_event(ScrollWheelEvent {
        position: card.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-200.))),
        ..Default::default()
    });
    cx.run_until_parked();
    assert_eq!(offset(cx), before, "the code didn't scroll");
    workspace.read_with(cx, |workspace, cx| assert!(workspace.debugger.read(cx).hover.is_some(), "the card stays"));
}

#[gpui_kit::test]
fn the_outline_gets_an_icon_of_its_own(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update(cx, |workspace, cx| workspace.own_place(Panel::Outline, cx));
    cx.run_until_parked();
    // The column shows it alone; the files are behind the explorer's icon.
    assert!(cx.debug_bounds("stack-Files").is_none());
    assert!(bounds(cx, "stack-Outline").size.height > bounds(cx, "side-column").size.height / 2.);
    let places = cx.update(|_, cx| Config::get(cx).layout.places());
    assert!(places.contains(&Place(Panel::Outline)), "{places:?}");
}

/// With targets in the launch file, the debugger's toolbar shows the one
/// picked, first in the bar, before the buttons it decides; picking another
/// keeps it for the workspace and `den debug state` says it.
#[gpui_kit::test]
fn the_toolbar_picks_the_target(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Console, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("debug-target").is_none(), "no picker without targets");
    let debugger = workspace.read_with(cx, |workspace, _| workspace.debugger());
    debugger.update(cx, |debugger, cx| {
        debugger.targets = vec!["ios".into(), "android".into()];
        assert_eq!(debugger.state()["target"], "ios");
        assert!(debugger.set_target("web", cx).unwrap_err().contains("ios, android"));
        debugger.set_target("android", cx).unwrap();
        assert_eq!(debugger.state()["target"], "android");
        cx.notify();
    });
    cx.run_until_parked();
    let picker = bounds(cx, "debug-target");
    let bar = bounds(cx, "debugger");
    assert!(picker.origin.x - bar.origin.x < px(16.), "the target leads the bar: {picker:?} in {bar:?}");
    let saved = cx.update(|_, cx| Config::get(cx).debug.get("layout-test").and_then(|saved| saved.target.clone()));
    assert_eq!(saved.as_deref(), Some("android"));
}

/// A stop near the bottom of the view scrolls its line to the middle: on
/// the last lines a step looked as if it did nothing. Well inside the view,
/// it stays where it is.
#[gpui_kit::test]
fn a_stop_near_the_edge_goes_to_the_middle(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let path = PathBuf::from("/stop-test/a.ts");
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/stop-test"), None, true, "stop-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        let mut tab = workspace.new_tab(PathBuf::from("/stop-test/a.ts"), false, window, cx);
        tab.content = Content::Ready;
        let code = (0..400).map(|line| format!("let value{line} = {line}\n")).collect::<String>();
        tab.editor.update(cx, |state, cx| state.set_value(code, window, cx));
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace
    });
    cx.run_until_parked();
    let visible = |cx: &mut VisualTestContext| {
        workspace.read_with(cx, |workspace, cx| workspace.tabs[0].editor.read(cx).visible_row_range()).expect("laid out")
    };
    let stop = |cx: &mut VisualTestContext, line: u32| {
        let path = path.clone();
        workspace.update(cx, |workspace, cx| {
            workspace.debugger.update(cx, |_, cx| cx.emit(crate::debug::DebugEvent::Show { path, line, focus: false }));
        });
        cx.run_until_parked();
    };
    let start = visible(cx);
    assert!(start.len() > 20, "{start:?}");
    // well inside: nothing moves
    let middle = (start.start + start.len() / 2) as u32;
    stop(cx, middle);
    assert_eq!(visible(cx), start);
    // the line before the last: it goes to the middle
    let low = (start.end - 2) as u32;
    stop(cx, low);
    // in the view itself: the range drawn has rows past its bottom edge
    let at = workspace.read_with(cx, |workspace, cx| {
        let state = workspace.tabs[0].editor.read(cx);
        let (caret, _) = state.cursor_layout().expect("the cursor is drawn");
        let view = state.input_bounds();
        (caret.origin.y - view.origin.y + state.scroll_offset().y + caret.size.height / 2.) / view.size.height
    });
    assert!((0.45..0.55).contains(&at), "{low} at {at}");
}

fn debug(workspace: &Entity<Workspace>, cx: &mut VisualTestContext, on: bool) {
    workspace.update(cx, |workspace, cx| {
        workspace.debugger.update(cx, |debugger, cx| if on { debugger.pretend_connected(cx) } else { debugger.stop(cx) })
    });
    cx.run_until_parked();
}

/// A debug session puts the debugging layout in use: the debugger's tab in
/// front, the side column where it was. Its end puts back the editing
/// layout exactly as it was.
#[gpui_kit::test]
fn a_session_uses_the_debugging_layout(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| {
        layout.dock = Dock::Bottom;
        layout.side_width = 300.;
    });
    let editing = cx.update(|_, cx| Config::get(cx).layout.clone());
    let shown = workspace.read_with(cx, |workspace, _| workspace.panels.shown());
    debug(&workspace, cx, true);
    bounds(cx, "side-column");
    bounds(cx, "debug-tab");
    assert!(cx.update(|_, cx| Config::get(cx).debugging()));
    let whereabouts = workspace.read_with(cx, |workspace, cx| workspace.whereabouts(cx));
    assert_eq!(whereabouts["layout"], "debugging");
    debug(&workspace, cx, false);
    assert!(cx.update(|_, cx| Config::get(cx).layout == editing), "the editing layout back as it was");
    assert_eq!(workspace.read_with(cx, |workspace, _| workspace.panels.shown()), shown);
    bounds(cx, "side-column");
    assert!(cx.debug_bounds("debug-tab").is_none());
}

/// What changes while debugging stays for the next session, and not in the
/// editing layout; but the side column and the activity bar are one in
/// both, so going between a workspace that debugs and one that doesn't
/// moves neither.
#[gpui_kit::test]
fn a_change_while_debugging_stays_for_the_next_session(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    debug(&workspace, cx, true);
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    cx.update(|_, cx| Config::update(cx, |config| config.layout.move_panel(Panel::Files, Place(Panel::Workspaces), None)));
    let column = cx.update(|_, cx| Config::get(cx).layout.places.clone());
    cx.run_until_parked();
    debug(&workspace, cx, false);
    // editing: the terminals as before the session, the column as it was left
    bounds(cx, "terminals");
    assert_eq!(cx.update(|_, cx| Config::get(cx).layout.places.clone()), column, "the side column's places as moved while debugging");
    debug(&workspace, cx, true);
    assert!(cx.debug_bounds("terminals").is_none(), "the terminals hidden, as the last session left them");
    assert_eq!(cx.update(|_, cx| Config::get(cx).layout.places.clone()), column);
}

/// A restart is one session: the layout doesn't go back to editing in
/// between. A session that never connects ends, and the debugger stays in
/// sight with what it said until Stop puts the editing layout back.
#[gpui_kit::test]
fn a_restart_does_not_flip_and_an_ended_session_stays_until_stop(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    debug(&workspace, cx, true);
    let flips = Rc::new(Cell::new(0));
    let _watch = cx.update(|_, cx| {
        let flips = flips.clone();
        let mut last = Config::get(cx).debugging();
        cx.observe_global::<Config>(move |cx| {
            let now = Config::get(cx).debugging();
            if now != last {
                flips.set(flips.get() + 1);
                last = now;
            }
        })
    });
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.debugger.update(cx, |debugger, cx| debugger.restart(window, cx));
    });
    assert!(workspace.read_with(cx, |workspace, _| workspace.debugging), "still the same session");
    assert_eq!(flips.get(), 0);
    cx.run_until_parked();
    assert!(workspace.read_with(cx, |workspace, _| workspace.debugging), "restarting");
    assert_eq!(flips.get(), 0);
    // the program restarted never listens: the session fails and ends
    workspace.update(cx, |workspace, cx| workspace.debugger.update(cx, |debugger, cx| debugger.pretend_failed(cx)));
    cx.run_until_parked();
    assert_eq!(flips.get(), 0);
    assert!(workspace.read_with(cx, |workspace, _| workspace.debugging), "ended, still in sight");
    let state = workspace.read_with(cx, |workspace, cx| workspace.debugger.read(cx).state());
    assert_eq!(state["status"], "idle");
    assert_eq!(state["console"].as_array().and_then(|lines| lines.last()), Some(&serde_json::json!("error: the program ended before it listened")));
    bounds(cx, "debug-tab");
    debug(&workspace, cx, false);
    assert_eq!(flips.get(), 1);
    assert!(!workspace.read_with(cx, |workspace, _| workspace.debugging));
    assert!(!cx.update(|_, cx| Config::get(cx).debugging()));
    bounds(cx, "side-column");
}

/// No debug session outlives the app: a config saved while debugging loads
/// with the editing layout in use and the debugging one kept.
#[test]
fn the_app_starts_editing() {
    let mut config = Config::default();
    config.layout.side_width = 300.;
    config.use_debug_layout(true);
    assert!(config.layout.side, "the side column stays");
    config.layout.dock = Dock::Bottom;
    let saved = serde_json::to_vec(&config).unwrap();
    let mut loaded: Config = serde_json::from_slice(&saved).unwrap();
    assert!(loaded.debugging());
    loaded.use_debug_layout(false);
    assert!(loaded.layout.side && loaded.layout.side_width == 300. && loaded.layout.dock == Dock::Right);
    assert_eq!(loaded.debug_layout.as_ref().map(|layout| layout.dock), Some(Dock::Bottom));
}

/// The workspace in a window in Terminal Mode, drawn: the window's switch.
fn in_terminal_mode(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Rc<Cell<bool>> {
    let mode = Rc::new(Cell::new(true));
    workspace.update(cx, |workspace, cx| {
        workspace.set_terminal_mode(&mode);
        cx.notify();
    });
    cx.run_until_parked();
    mode
}

#[gpui_kit::test]
fn terminal_mode_shows_the_terminals_the_workspaces_and_the_agents(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    in_terminal_mode(&workspace, cx);
    // No activity bar, no code, no files: the agents too, hidden in the other.
    assert!(cx.debug_bounds("side-column").is_none());
    assert!(cx.debug_bounds("editor-body-0").is_none());
    assert!(cx.debug_bounds("stack-Files").is_none());
    assert!(cx.debug_bounds("activity-Place(Place(Files))").is_none());
    let side = bounds(cx, "terminal-mode-side");
    let workspaces = bounds(cx, "stack-Workspaces");
    let agents = bounds(cx, "stack-Agents");
    let terminals = bounds(cx, "terminals");
    assert!(workspaces.bottom() <= agents.top(), "{workspaces:?} {agents:?}");
    assert!(side.right() <= terminals.left(), "{side:?} {terminals:?}");
    workspace.read_with(cx, |workspace, cx| {
        assert!(!workspace.is_shown(Panel::Code, cx));
        assert!(workspace.is_shown(Panel::Terminals, cx));
        assert!(workspace.is_shown(Panel::Agents, cx));
        assert!(!workspace.is_shown(Panel::Files, cx));
    });
}

#[gpui_kit::test]
fn terminal_mode_hides_its_column_apart_from_the_other(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let mode = in_terminal_mode(&workspace, cx);
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_side_panel(&ToggleSidePanel, window, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminal-mode-side").is_none());
    cx.update(|_, cx| {
        assert!(!Config::get(cx).terminal_mode.side);
        assert!(Config::get(cx).layout.side);
    });
    // The workspaces bring it back.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    bounds(cx, "terminal-mode-side");
    assert!(mode.get());
}

#[gpui_kit::test]
fn what_needs_the_code_leaves_terminal_mode(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let mode = in_terminal_mode(&workspace, cx);
    // A file opened: the layout as it was, with it.
    workspace.update_in(cx, |workspace, window, cx| workspace.activate(0, window, cx));
    cx.run_until_parked();
    assert!(!mode.get());
    bounds(cx, "editor-body-0");
    bounds(cx, "side-column");
    // A side panel other than the workspaces and the agents.
    mode.set(true);
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    assert!(!mode.get());
    // Cmd-J: back to the code.
    mode.set(true);
    cx.dispatch_action(ToggleTerminals);
    cx.run_until_parked();
    assert!(!mode.get());
    bounds(cx, "editor-body-0");
}

/// A panel's header dropped on another's joins its tabs, each with an icon
/// that shows the column with it in front; dropped on a header's upper
/// edge, it's a section of its own again.
#[gpui_kit::test]
fn panels_join_as_tabs(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| layout.join(Panel::Changes, Panel::Files, None));
    // The Changes' place went with it: its icon goes to the explorer, and
    // it joined in front.
    assert!(cx.debug_bounds("activity-Place(Place(Changes))").is_none());
    bounds(cx, "stack-Changes");
    assert!(cx.debug_bounds("stack-Files").is_none());
    bounds(cx, "stack-Workspaces");
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Changes, cx) && !workspace.is_shown(Panel::Files, cx));
        assert!(workspace.shows(Item::Tab(Panel::Changes), cx) && !workspace.shows(Item::Tab(Panel::Files), cx));
    });
    // The files' icon brings them to the front; again, the column closes.
    click(cx, "activity-Tab(Files)");
    bounds(cx, "stack-Files");
    click(cx, "activity-Tab(Files)");
    assert!(cx.debug_bounds("side-column").is_none());
    // The changes' opens it with them in front.
    click(cx, "activity-Tab(Changes)");
    bounds(cx, "stack-Changes");
    // A tab clicked comes to the front.
    click(cx, "tab-Files");
    bounds(cx, "stack-Files");
    // The outline's header on the workspaces': it joins them, in front.
    let end = bounds(cx, "title-Workspaces").center();
    drag_part(cx, "title-Outline", end);
    bounds(cx, "tab-Outline");
    bounds(cx, "stack-Outline");
    assert!(cx.debug_bounds("stack-Workspaces").is_none());
    // A tab on a header's upper edge: above it, alone.
    let end = bounds(cx, "above-Files").center();
    drag_part(cx, "tab-Outline", end);
    assert!(cx.debug_bounds("tab-Outline").is_none());
    assert!(bounds(cx, "stack-Workspaces").bottom() <= bounds(cx, "stack-Outline").top());
    assert!(bounds(cx, "stack-Outline").bottom() <= bounds(cx, "title-Files").top());
}

/// Every icon drags along the bar, its right-click menu takes any off and
/// puts it back where it was, and a panel's key brings back its place's.
#[gpui_kit::test]
fn the_icons_drag_and_come_off_the_bar(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let end = bounds(cx, "activity-Place(Place(Workspaces))").center();
    drag_part(cx, "activity-Debug", end);
    assert!(bounds(cx, "activity-Debug").top() < bounds(cx, "activity-Place(Place(Workspaces))").top());
    let end = bounds(cx, "activity-Debug").center();
    drag_part(cx, "activity-Notes", end);
    assert!(bounds(cx, "activity-Notes").top() < bounds(cx, "activity-Debug").top());

    use config::Icon;
    let off = |cx: &mut VisualTestContext, icon: Icon, on: bool| {
        cx.update(|_, cx| Config::update(cx, |config| config.layout.set_on_bar(icon, on)));
        cx.run_until_parked();
    };
    off(cx, Icon::Debug, false);
    assert!(cx.debug_bounds("activity-Debug").is_none());
    off(cx, Icon::Debug, true);
    assert!(bounds(cx, "activity-Debug").top() < bounds(cx, "activity-Place(Place(Workspaces))").top());

    off(cx, Icon::Place(Panel::Changes), false);
    assert!(cx.debug_bounds("activity-Place(Place(Changes))").is_none());
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    cx.run_until_parked();
    bounds(cx, "activity-Place(Place(Changes))");
    bounds(cx, "stack-Changes");
}

/// A jump puts its line a third of the way down, counted in rows as drawn:
/// long lines above it that wrap don't leave it at the bottom edge or out
/// of sight. One well inside the view stays where it is.
#[gpui_kit::test]
fn a_jump_lands_a_third_of_the_way_down(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let long = "word ".repeat(200);
    let text = (0..800).map(|row| if row % 5 == 0 { format!("{row} {long}") } else { format!("line {row}") }).collect::<Vec<_>>().join("\n");
    let editor = workspace.update_in(cx, |workspace, window, cx| {
        let editor = workspace.tabs[0].editor.clone();
        editor.update(cx, |state, cx| {
            state.set_soft_wrap(true, window, cx);
            state.set_value(text, window, cx);
        });
        editor
    });
    cx.run_until_parked();
    // Where the cursor's row is, as a share of the view's height.
    let jump = |cx: &mut VisualTestContext, line: u32| {
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| state.set_cursor_position(Position::new(line, 0), window, cx));
            super::reveal_centered(&editor, line, false, cx);
        });
        cx.run_until_parked();
        editor.read_with(cx, |state, _| {
            let (caret, _) = state.cursor_layout().expect("the cursor is drawn");
            let view = state.input_bounds();
            (caret.origin.y - view.origin.y + state.scroll_offset().y) / view.size.height
        })
    };
    for line in [662, 300, 790] {
        let at = jump(cx, line);
        assert!((0.15..0.5).contains(&at) || line == 790 && (0.15..=1.).contains(&at), "{line} at {at}");
    }
    let at = jump(cx, 300);
    let near = jump(cx, 303);
    assert!(near > at, "{at} {near}: it stayed");
}

/// From the bar's menu: a panel checked is seen, in its place; unchecked
/// it isn't, and a place left with none loses its icon.
#[gpui_kit::test]
fn the_bar_s_menu_shows_and_hides_panels(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    assert!(cx.debug_bounds("stack-Agents").is_none());
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Agents, cx));
    cx.run_until_parked();
    bounds(cx, "stack-Agents");
    workspace.update(cx, |workspace, cx| workspace.remove_panel(Panel::Agents, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("stack-Agents").is_none());
    workspace.update(cx, |workspace, cx| workspace.remove_panel(Panel::Changes, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("activity-Place(Place(Changes))").is_none());
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    cx.run_until_parked();
    bounds(cx, "activity-Place(Place(Changes))");
    bounds(cx, "stack-Changes");
}

#[gpui_kit::test]
fn a_plain_editor_shows_the_code_and_only_the_files_or_the_search(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update(cx, |workspace, cx| {
        workspace.set_plain(true, cx);
        cx.notify();
    });
    cx.run_until_parked();
    bounds(cx, "editor-body-0");
    assert!(cx.debug_bounds("side-column").is_none());
    assert!(cx.debug_bounds("plain-side").is_none());
    assert!(cx.debug_bounds("terminals").is_none());
    assert!(cx.debug_bounds("activity-bar").is_none());
    // Its bar shows the files, and hides them again.
    click(cx, "plain-Files");
    let side = bounds(cx, "plain-side");
    assert!(side.right() <= bounds(cx, "editor-body-0").left());
    assert!(workspace.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Files, cx)));
    click(cx, "plain-Files");
    assert!(cx.debug_bounds("plain-side").is_none());
    // The search takes the files' place.
    click(cx, "plain-Search");
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Search, cx));
        assert!(!workspace.is_shown(Panel::Files, cx));
    });
    // Nothing else shows.
    workspace.update(cx, |workspace, cx| {
        workspace.show_panel(Panel::Changes, cx);
        workspace.show_panel(Panel::Terminals, cx);
        workspace.show_panel(Panel::Workspaces, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminals").is_none());
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Search, cx));
        assert!(!workspace.is_shown(Panel::Changes, cx) && !workspace.is_shown(Panel::Terminals, cx));
    });
}

/// Cmd-J from the terminals with a diff in one column in front: the keys go
/// to the editor it shows, so the shortcuts still work.
#[gpui_kit::test]
fn hiding_the_terminals_focuses_a_diff(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.terminals = cx.new(|cx| crate::terminals::test_area(window, cx));
        let mut tab = workspace.new_tab_with(PathBuf::from("/layout-test/main.ts"), false, "diff", window, cx);
        tab.diff = Some(DiffOf { file: "main.ts".into(), commit: None, source: false });
        tab.content = Content::Ready;
        let ix = workspace.place_tab(tab, true);
        let sides = crate::diff::split("@@ -1 +1 @@\n-a\n+b\n").unwrap();
        workspace.show_side_by_side(ix, sides, window, cx);
        workspace.activate_with(ix, false, window, cx);
        workspace.set_terminals_visible(true, window, cx);
    });
    cx.run_until_parked();
    cx.dispatch_action(ToggleTerminals);
    cx.run_until_parked();
    workspace.update_in(cx, |workspace, window, cx| {
        assert!(!workspace.is_shown(Panel::Terminals, cx));
        assert!(workspace.focus_handle.contains_focused(window, cx));
    });
}
