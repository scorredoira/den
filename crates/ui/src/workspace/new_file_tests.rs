use super::*;
use core::prelude::v1::test;

fn workspace<'a>(cx: &'a mut TestAppContext, open: Option<&str>) -> (Entity<Workspace>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let open = open.map(PathBuf::from);
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/new-file-test"), None, true, "new-file-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        if let Some(path) = open {
            let mut tab = workspace.new_tab(path, false, window, cx);
            tab.content = Content::Ready;
            workspace.tabs.push(tab);
            workspace.activate(0, window, cx);
        }
        workspace
    })
}

/// Cmd-N types the new file's name in the files panel, in the folder of the
/// file open.
#[gpui_kit::test]
fn a_new_file_goes_in_the_folder_of_the_file_open(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, Some("/new-file-test/src/app/main.ts"));
    workspace.update_in(cx, |workspace, window, cx| workspace.new_file(&NewFile, window, cx));
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.is_shown(Panel::Files, cx));
        let dir = workspace.file_tree.read(cx).new_file_dir().map(Path::to_path_buf);
        assert_eq!(dir, Some(PathBuf::from("/new-file-test/src/app")));
    });
}

/// With no file open, it goes in the workspace's folder.
#[gpui_kit::test]
fn a_new_file_with_nothing_open_goes_in_the_workspace(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, None);
    workspace.update_in(cx, |workspace, window, cx| workspace.new_file(&NewFile, window, cx));
    workspace.read_with(cx, |workspace, cx| {
        let dir = workspace.file_tree.read(cx).new_file_dir().map(Path::to_path_buf);
        assert_eq!(dir, Some(PathBuf::from("/new-file-test")));
    });
}
