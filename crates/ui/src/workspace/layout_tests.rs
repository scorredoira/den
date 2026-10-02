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
