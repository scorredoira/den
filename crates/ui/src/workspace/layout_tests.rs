use super::*;
use crate::config::Side;
use core::prelude::v1::test;

/// Draws a workspace with a file, the terminals and the debugger open (in
/// front of its place, unless it's the terminals'), the panels where
/// `layout` says.
fn draw(cx: &mut TestAppContext, layout: impl FnOnce(&mut config::Layout)) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let mut config = Config::default();
        // Every icon on the bar.
        config.hidden_activity = Some(Vec::new());
        layout(&mut config.layout);
        cx.set_global(config);
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
        let mut tab = workspace.new_tab(PathBuf::from("main.ts"), false, window, cx);
        tab.content = Content::Ready;
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace.show_panel(Panel::Debugger, cx);
        workspace.show_panel(Panel::Terminals, cx);
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
fn files_code_and_terminals_with_the_debugger_as_their_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let files = bounds(cx, "stack-Files");
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    assert!(files.right() <= code.left(), "{files:?} {code:?}");
    assert!(terminals.left() >= code.right(), "{terminals:?} {code:?}");
    // Behind its tab; in front, under the terminals' bar.
    assert!(cx.debug_bounds("debugger").is_none());
    let tab = bounds(cx, "debug-tab");
    click(cx, "debug-tab");
    let debugger = bounds(cx, "debugger");
    assert!(debugger.left() >= code.right() && debugger.top() >= tab.bottom(), "{debugger:?} {tab:?}");
    // Closing its tab, the terminals show again and the tab goes; showing
    // it (debugging) brings it back in front.
    click(cx, "debug-tab-close");
    assert!(cx.debug_bounds("debugger").is_none());
    assert!(cx.debug_bounds("debug-tab").is_none());
    workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));
    workspace.update(cx, |workspace, cx| workspace.toggle_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    bounds(cx, "debugger");
    // The same key hides it, as its tab's close does.
    workspace.update(cx, |workspace, cx| workspace.toggle_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("debug-tab").is_none());
    bounds(cx, "terminals");
}

#[gpui_kit::test]
fn columns_in_any_order(cx: &mut TestAppContext) {
    // Files, debugger, code, terminals; the changes a column of their own.
    let (_, cx) = draw(cx, |layout| {
        assert!(layout.move_panel(Panel::Debugger, Panel::Code, Side::Left));
        assert!(layout.move_panel(Panel::Changes, Panel::Terminals, Side::Right));
    });
    let files = bounds(cx, "stack-Files");
    let debugger = bounds(cx, "debugger");
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    let changes = bounds(cx, "stack-Changes");
    assert!(files.right() <= debugger.left(), "{files:?} {debugger:?}");
    assert!(debugger.right() <= code.left(), "{debugger:?} {code:?}");
    assert!(code.right() <= terminals.left(), "{code:?} {terminals:?}");
    assert!(terminals.right() <= changes.left(), "{terminals:?} {changes:?}");
}

#[gpui_kit::test]
fn dragging_the_terminals_icon_under_the_code(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let start = bounds(cx, "activity-Terminals").center();
    let code = bounds(cx, "stack-Code");
    let end = point(code.center().x, code.bottom() - px(10.));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| {
        assert!(cx.has_active_drag());
        assert_eq!(workspace.read(cx).panel_drop, Some((Panel::Code, crate::drag_drop::DropPlacement::Bottom)));
    });
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let layout = cx.update(|_, cx| Config::get(cx).layout.clone());
    let (column, stack) = layout.find(Panel::Terminals).unwrap();
    assert_eq!(layout.find(Panel::Code), Some((column, stack - 1)));
    let code = bounds(cx, "editor-body-0");
    let terminals = bounds(cx, "terminals");
    assert!(terminals.top() >= code.bottom(), "{terminals:?} {code:?}");
    assert_eq!(cx.update(|_, cx| workspace.read(cx).panel_drop), None);
}

#[gpui_kit::test]
fn dropping_the_changes_on_the_terminals_bar_puts_them_together(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let start = bounds(cx, "activity-Changes").center();
    let terminals = bounds(cx, "stack-Terminals");
    let end = point(terminals.center().x, terminals.top() + px(10.));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    // Over their bar, the preview says it joins them, not that it goes above.
    let drop = cx.update(|_, cx| workspace.read(cx).panel_drop);
    assert_eq!(drop, Some((Panel::Terminals, crate::drag_drop::DropPlacement::Center)));
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let layout = cx.update(|_, cx| Config::get(cx).layout.clone());
    let (column, stack) = layout.find(Panel::Terminals).unwrap();
    assert_eq!(layout.columns[column].stacks[stack].panels, [Panel::Terminals, Panel::Debugger, Panel::Changes]);
    // The panel dropped shows; the terminals are behind it.
    cx.update(|_, cx| {
        assert!(workspace.read(cx).is_shown(Panel::Changes, cx));
        assert!(!workspace.read(cx).is_shown(Panel::Terminals, cx));
    });
    bounds(cx, "stack-Changes");
}

#[gpui_kit::test]
fn closing_a_stack_gives_its_width_to_the_code(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let before = bounds(cx, "editor-body-0");
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("terminals").is_none());
    let after = bounds(cx, "editor-body-0");
    assert!(after.size.width > before.size.width, "{after:?} {before:?}");
    // Cmd-B closes the stack with the files, whichever panel it shows.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Search, cx));
    cx.dispatch_action(ToggleSidePanel);
    cx.run_until_parked();
    assert!(cx.debug_bounds("stack-Search").is_none());
    cx.dispatch_action(ToggleSidePanel);
    cx.run_until_parked();
    bounds(cx, "stack-Search");
}

#[gpui_kit::test]
fn the_code_shares_a_place_like_any_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |layout| assert!(layout.move_panel(Panel::Code, Panel::Search, Side::Tab(Some(Panel::Search)))));
    // It has no icon: it stays where it is.
    assert!(cx.debug_bounds("activity-Code").is_none());
    let layout = cx.update(|_, cx| Config::get(cx).layout.clone());
    let (column, stack) = layout.find(Panel::Code).unwrap();
    assert_eq!(layout.columns[column].stacks[stack].panels, [Panel::Files, Panel::Changes, Panel::History, Panel::Commit, Panel::Code, Panel::Search, Panel::References, Panel::Outline]);
    bounds(cx, "editor-body-0");
    // Another panel there hides it; hiding that one brings it back, its place
    // never closes.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Files, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("editor-body-0").is_none());
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Files, cx));
    cx.run_until_parked();
    bounds(cx, "editor-body-0");
    // Opening a file shows it too.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Search, cx));
    workspace.update_in(cx, |workspace, window, cx| workspace.activate(0, window, cx));
    cx.run_until_parked();
    bounds(cx, "editor-body-0");
}

#[gpui_kit::test]
fn the_workspaces_are_a_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let panel = cx.update(|_, cx| {
        cx.new(|_| WorkspacesPanel::new(|_, _| div().size_full().debug_selector(|| "workspaces".into()).into_any_element()))
    });
    workspace.update(cx, |workspace, cx| workspace.set_workspaces(&panel, true, cx));
    cx.run_until_parked();
    let workspaces = bounds(cx, "workspaces");
    assert!(workspaces.right() <= bounds(cx, "stack-Files").left(), "on the left by default");
    assert!(bounds(cx, "activity-Workspaces").right() <= workspaces.left(), "the activity bar, left of everything");
    // Hidden from the workspace, the app's choice changes for every task.
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Workspaces, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("workspaces").is_none());
    assert_eq!(cx.update(|_, cx| Config::get(cx).tasks_column), Some(false));
    // Shown again by the app, it's in front of its stack.
    workspace.update(cx, |workspace, cx| {
        Config::update(cx, |config| assert!(config.layout.move_panel(Panel::Workspaces, Panel::Files, Side::Tab(None))));
        workspace.show_panel(Panel::Files, cx);
        workspace.set_workspaces(&panel, true, cx);
    });
    cx.run_until_parked();
    bounds(cx, "workspaces");
}

#[gpui_kit::test]
fn an_icon_shows_and_hides_its_panel_where_it_is(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    let shown = |panel, cx: &mut VisualTestContext| workspace.read_with(cx, |workspace, cx| workspace.is_shown(panel, cx));
    // The changes, a tab behind the files: to the front.
    click(cx, "activity-Changes");
    assert!(shown(Panel::Changes, cx));
    bounds(cx, "stack-Changes");
    // Again: their place closes.
    click(cx, "activity-Changes");
    assert!(!shown(Panel::Changes, cx));
    assert!(cx.debug_bounds("stack-Changes").is_none());
    assert!(cx.debug_bounds("stack-Files").is_none());
    // The terminals, a column of their own, close and open again.
    click(cx, "activity-Terminals");
    assert!(cx.debug_bounds("terminals").is_none());
    click(cx, "activity-Terminals");
    bounds(cx, "terminals");
}

#[gpui_kit::test]
fn dragging_an_icon_within_the_bar_reorders_it(cx: &mut TestAppContext) {
    let (_, cx) = draw(cx, |_| {});
    let start = bounds(cx, "activity-Debugger").center();
    let end = bounds(cx, "activity-Files").center();
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(start - point(px(0.), px(12.)), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let order = cx.update(|_, cx| Config::get(cx).activity());
    assert_eq!(order[..3], [Panel::Workspaces, Panel::Debugger, Panel::Files]);
    assert!(bounds(cx, "activity-Debugger").bottom() <= bounds(cx, "activity-Files").top());
    // The layout didn't change.
    let layout = cx.update(|_, cx| Config::get(cx).layout.clone());
    assert!(layout == config::Layout::default());
}

/// Going to another task leaves every place showing what it showed: picking
/// a workspace from its column, in the files' place, leaves the column there.
#[gpui_kit::test]
fn every_task_shows_the_same_panels(cx: &mut TestAppContext) {
    let (first, cx) = draw(cx, |layout| assert!(layout.move_panel(Panel::Workspaces, Panel::Files, Side::Tab(None))));
    let second = first.update_in(cx, |_, window, cx| cx.new(|cx| Workspace::new(PathBuf::from("/other"), None, true, "other".into(), window, cx)));
    first.update(cx, |workspace, cx| workspace.show_panel(Panel::Changes, cx));
    assert!(second.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Changes, cx)));
    first.update(cx, |workspace, cx| workspace.show_panel(Panel::Workspaces, cx));
    second.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Workspaces, cx));
        assert!(!workspace.is_shown(Panel::Changes, cx));
    });
    // Hiding the terminals in one hides them in the other.
    second.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    assert!(!first.read_with(cx, |workspace, cx| workspace.is_shown(Panel::Terminals, cx)));
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

/// Typing in a file's rendered Markdown switches to its source; a shortcut
/// doesn't.
#[gpui_kit::test]
fn typing_in_the_preview_edits_the_markdown(cx: &mut TestAppContext) {
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
    cx.simulate_keystrokes("cmd-a");
    assert!(!source(cx), "a shortcut stays in the preview");
    cx.simulate_keystrokes("x");
    assert!(source(cx), "typing switches to the source");
}
