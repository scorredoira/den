use super::*;
use core::prelude::v1::test;

fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/files-test"), None, true, "files-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace
    })
}

/// A file's tab, read with `text`.
fn add_tab(this: &mut Workspace, path: &str, text: &str, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<EditorState> {
    let mut tab = this.new_tab(PathBuf::from(path), false, window, cx);
    tab.content = Content::Ready;
    tab.saved = text.into();
    tab.text = Some(text.to_string().into());
    tab.editor.update(cx, |state, cx| state.set_value(text.to_string(), window, cx));
    let editor = tab.editor.clone();
    this.tabs.push(tab);
    editor
}

/// The editor's cursor blinking redraws the editor, not the workspace;
/// moving the cursor redraws both.
#[gpui_kit::test]
fn a_blinking_cursor_leaves_the_workspace_alone(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    let editor = cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one\ntwo\n", window, cx);
        this.activate(0, window, cx);
        editor
    }));
    cx.run_until_parked();
    let (workspace_notified, editor_notified) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
    let _counting = cx.update(|_, cx| {
        let (workspace_notified, editor_notified) = (workspace_notified.clone(), editor_notified.clone());
        [
            cx.observe(&workspace, move |_, _| workspace_notified.set(workspace_notified.get() + 1)),
            cx.observe(&editor, move |_, _| editor_notified.set(editor_notified.get() + 1)),
        ]
    });
    cx.update(|window, cx| assert!(editor.read(cx).focus_handle(cx).is_focused(window)));
    for _ in 0..4 {
        cx.executor().advance_clock(std::time::Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert!(editor_notified.get() > 0, "the cursor blinked");
    assert_eq!(workspace_notified.get(), 0);

    cx.update(|window, cx| editor.update(cx, |state, cx| state.set_cursor_position(Position::new(1, 2), window, cx)));
    cx.run_until_parked();
    assert!(workspace_notified.get() > 0, "the status bar shows where the cursor is");
}
