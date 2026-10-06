use super::*;
use crate::config::{Dock, Place};
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
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Watch, cx));
    cx.run_until_parked();
    bounds(cx, "stack-Watch");
    bounds(cx, "debugger");
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

/// The history is a tab of the code, not a panel: opened again, it's the
/// same tab, and it reopens with the workspace.
#[gpui_kit::test]
fn the_history_is_a_tab_that_reopens(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update_in(cx, |workspace, window, cx| {
        workspace.restore(window, cx);
        workspace.open_history(None, window, cx);
        workspace.open_history(Some(("main.ts".into(), false)), window, cx);
    });
    cx.run_until_parked();
    workspace.read_with(cx, |workspace, _| {
        let tabs = workspace.tabs.iter().filter(|tab| matches!(tab.page, Some(pages::Page::History))).count();
        assert_eq!(tabs, 1);
        assert_eq!(workspace.active, workspace.history_tab());
        assert!(workspace.history_visible());
    });
    bounds(cx, "history-commits");
    bounds(cx, "commit-files");
    let again = workspace.update_in(cx, |_, window, cx| {
        cx.new(|cx| {
            let mut workspace = Workspace::new(PathBuf::from("/layout-test"), None, true, "layout-test".into(), window, cx);
            workspace.restore(window, cx);
            workspace
        })
    });
    again.read_with(cx, |workspace, _| assert!(workspace.history_tab().is_some()));
    // Cmd-Shift-H closes it while it's in front.
    workspace.update_in(cx, |workspace, window, cx| workspace.toggle_history(window, cx));
    assert!(workspace.read_with(cx, |workspace, _| workspace.history_tab().is_none()));
}

/// Hiding the debugger leaves the panels it shares the column with.
#[gpui_kit::test]
fn the_debugger_hides_alone(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    // The debugger's parts in the explorer: hidden, the files stay.
    cx.update(|_, cx| {
        Config::update(cx, |config| {
            let explorer = config.layout.place_of(Panel::Files).unwrap();
            config.layout.move_panel(Panel::CallStack, explorer, None);
            config.layout.move_panel(Panel::Variables, explorer, None);
        })
    });
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    bounds(cx, "stack-CallStack");
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    bounds(cx, "stack-Files");
    assert!(cx.debug_bounds("stack-CallStack").is_none() && cx.debug_bounds("stack-Variables").is_none());
    // Shown again, they're all back.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    bounds(cx, "stack-CallStack");
    bounds(cx, "stack-Variables");
    // Alone in their place, they close the column.
    workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Watch, cx));
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Debugger, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("side-column").is_none());
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

#[gpui_kit::test]
fn the_debug_console_is_a_tab_of_the_terminals(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    assert!(cx.debug_bounds("console-tab").is_none());
    // A session starts: the Run and Debug group, and the console's tab.
    workspace.update(cx, |workspace, cx| workspace.reveal_debugger(cx));
    cx.run_until_parked();
    bounds(cx, "debugger");
    bounds(cx, "stack-CallStack");
    assert!(cx.debug_bounds("debug-console").is_none(), "behind its tab");
    click(cx, "console-tab");
    let console = bounds(cx, "debug-console");
    assert!(console.left() >= bounds(cx, "editor-body-0").right());
    // Closed, the terminals show again and the tab goes.
    click(cx, "console-tab-close");
    assert!(cx.debug_bounds("console-tab").is_none());
    assert!(cx.debug_bounds("debug-console").is_none());
    workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));
}

/// The notes are a tab at the far end of the terminals': the activity bar
/// brings them in front of the terminals and back. Open in Editor Tab takes
/// them to the code, and closing that tab brings them back to the terminals.
#[gpui_kit::test]
fn the_notes_are_a_tab_of_the_terminals(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    bounds(cx, "notes-tab");
    assert!(cx.debug_bounds("notes").is_none(), "behind their tab");
    click(cx, "activity-Notes");
    let notes = bounds(cx, "notes");
    assert!(notes.left() >= bounds(cx, "editor-body-0").right(), "{notes:?}");
    click(cx, "activity-Notes");
    assert!(cx.debug_bounds("notes").is_none());
    workspace.read_with(cx, |workspace, cx| assert!(workspace.is_shown(Panel::Terminals, cx)));

    // Show Notes, in the terminals' right-click menus.
    workspace.update(cx, |workspace, cx| {
        workspace.terminals.update(cx, |_, cx| cx.emit(TerminalAreaEvent::ShowPanel(Some(Panel::Notes))))
    });
    cx.run_until_parked();
    bounds(cx, "notes");
    click(cx, "activity-Notes");

    workspace.update_in(cx, |workspace, window, cx| workspace.notes_to_tab(window, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("notes-tab").is_none());
    let notes = bounds(cx, "notes");
    assert!(notes.right() <= bounds(cx, "editor-body-0").right() + px(1.), "{notes:?}");
    workspace.update_in(cx, |workspace, window, cx| {
        let ix = workspace.notes_tab().expect("their tab");
        workspace.close(ix, window, cx);
    });
    cx.run_until_parked();
    bounds(cx, "notes-tab");
}

/// With the terminals closed, the notes' icon opens them with the notes in
/// front, and closes them again on the next click.
#[gpui_kit::test]
fn the_notes_icon_toggles_them(cx: &mut TestAppContext) {
    let (workspace, cx) = draw(cx, |_| {});
    workspace.update(cx, |workspace, cx| workspace.hide_panel(Panel::Terminals, cx));
    cx.run_until_parked();
    click(cx, "activity-Notes");
    bounds(cx, "notes");
    click(cx, "activity-Notes");
    assert!(cx.debug_bounds("notes").is_none());
    workspace.read_with(cx, |workspace, cx| assert!(!workspace.is_shown(Panel::Terminals, cx)));
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
