use super::*;
use crate::config::PanelAt;
use core::prelude::v1::test;

/// Draws a workspace with a file, the terminals and the debugger, their
/// places as `layout` says, and returns where the code, the terminals and
/// the debugger are.
fn draw(cx: &mut TestAppContext, debug_at: PanelAt, terminals_at: PanelAt) -> [Bounds<Pixels>; 3] {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let mut config = Config::default();
        config.layout.debug_at = debug_at;
        config.layout.terminals_at = terminals_at;
        config.layout.terminals = Some(400.);
        cx.set_global(config);
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
        workspace.side_panel_visible = false;
        let mut tab = workspace.new_tab(PathBuf::from("main.ts"), false, window, cx);
        tab.content = Content::Ready;
        workspace.tabs.push(tab);
        workspace.activate(0, window, cx);
        workspace.debugger.update(cx, |debugger, _| debugger.visible = true);
        workspace
    });
    cx.run_until_parked();
    let code = cx.debug_bounds("editor-body-0").expect("the code is drawn");
    let terminals = cx.debug_bounds("terminals").expect("the terminals are drawn");
    let debugger = cx.debug_bounds("debugger").expect("the debugger is drawn");
    [code, terminals, debugger]
}

#[gpui_kit::test]
fn debugger_under_everything_and_terminals_on_the_right(cx: &mut TestAppContext) {
    let [code, terminals, debugger] = draw(cx, PanelAt::Bottom, PanelAt::Right);
    assert!(terminals.left() >= code.right(), "{terminals:?} {code:?}");
    assert!(debugger.top() >= code.bottom() && debugger.top() >= terminals.bottom());
    assert!(debugger.left() <= code.left() && debugger.right() >= terminals.right());
}

#[gpui_kit::test]
fn debugger_under_everything_and_terminals_under_the_code(cx: &mut TestAppContext) {
    let [code, terminals, debugger] = draw(cx, PanelAt::Bottom, PanelAt::Bottom);
    assert!(terminals.top() >= code.bottom(), "{terminals:?} {code:?}");
    assert_eq!((terminals.left(), terminals.right()), (code.left(), code.right()));
    assert!(debugger.top() >= terminals.bottom());
}

#[gpui_kit::test]
fn debugger_on_the_right_and_terminals_on_the_right(cx: &mut TestAppContext) {
    let [code, terminals, debugger] = draw(cx, PanelAt::Right, PanelAt::Right);
    assert!(terminals.left() >= code.right(), "{terminals:?} {code:?}");
    assert!(debugger.left() >= terminals.right(), "{debugger:?} {terminals:?}");
    assert!(debugger.top() <= code.top() && debugger.bottom() >= code.bottom());
}

#[gpui_kit::test]
fn debugger_on_the_right_and_terminals_under_the_code(cx: &mut TestAppContext) {
    let [code, terminals, debugger] = draw(cx, PanelAt::Right, PanelAt::Bottom);
    assert!(terminals.top() >= code.bottom(), "{terminals:?} {code:?}");
    assert!(debugger.left() >= code.right() && debugger.left() >= terminals.right());
    assert!(debugger.top() <= code.top() && debugger.bottom() >= terminals.bottom());
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
        workspace.side_panel_visible = false;
        workspace.terminals_visible = false;
        let file = crate::debug::parse_launch_file(
            r#"{"configurations":[],"tests":{"match":"^export function (test\\w+)\\(","run":"run ${test}","debug":"debug ${test}"}}"#,
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
        workspace.side_panel_visible = false;
        workspace.terminals_visible = false;
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
