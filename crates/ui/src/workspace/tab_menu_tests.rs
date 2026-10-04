use super::*;
use core::prelude::v1::test;

/// A workspace with these files open in one group, in this order; the
/// dirty ones with unsaved changes.
fn workspace<'a>(
    cx: &'a mut TestAppContext,
    files: &[&str],
    dirty: &[&str],
) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let files: Vec<(PathBuf, bool)> =
        files.iter().map(|file| (PathBuf::from("/tab-menu-test").join(file), dirty.contains(file))).collect();
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/tab-menu-test"), None, true, "tab-menu-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        for (path, dirty) in files {
            let mut tab = workspace.new_tab(path, false, window, cx);
            tab.content = Content::Ready;
            tab.dirty = dirty;
            workspace.tabs.push(tab);
        }
        workspace.activate(0, window, cx);
        workspace
    })
}

fn names(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) -> Vec<String> {
    workspace.read_with(cx, |workspace, _| {
        workspace.tabs.iter().map(|tab| tab.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    })
}

/// Close to the Right leaves the tab and those before it, and the ones with
/// unsaved changes, with a warning.
#[gpui_kit::test]
fn close_to_the_right_keeps_unsaved_tabs(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, &["a.rs", "b.rs", "c.rs", "d.rs"], &["c.rs"]);
    workspace.update_in(cx, |workspace, window, cx| workspace.close_to_the_right(1, window, cx));
    assert_eq!(names(&workspace, cx), ["a.rs", "b.rs", "c.rs"]);
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.active, Some(1));
        assert!(workspace.message.is_some());
    });
}

/// Close Saved leaves only the tabs with unsaved changes.
#[gpui_kit::test]
fn close_saved_keeps_unsaved_tabs(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, &["a.rs", "b.rs", "c.rs"], &["b.rs"]);
    workspace.update_in(cx, |workspace, window, cx| workspace.close_saved(0, window, cx));
    assert_eq!(names(&workspace, cx), ["b.rs"]);
}

/// Keep Open pins a preview: the next preview gets a tab of its own.
#[gpui_kit::test]
fn keep_open_pins_a_preview(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, &["a.rs"], &[]);
    workspace.update_in(cx, |workspace, _, cx| {
        workspace.tabs[0].preview = true;
        workspace.keep_open(0, cx);
        assert!(!workspace.tabs[0].preview);
    });
}
