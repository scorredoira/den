use super::*;
use core::prelude::v1::test;

fn workspace(cx: &mut TestAppContext, local: bool) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/new-file-test"), None, local, "new-file-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace
    })
}

fn type_in(this: &mut Workspace, ix: usize, text: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let editor = this.tabs[ix].editor.clone();
    editor.update(cx, |state, cx| state.set_value(text.to_string(), window, cx));
    this.on_edit(&editor, window, cx);
}

/// Cmd-N opens an empty tab to write in, with no file: nothing is made on
/// disk, and the next one is numbered after it.
#[gpui_kit::test]
fn a_new_file_is_an_untitled_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, true);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.new_file(&NewFile, window, cx);
        this.new_file(&NewFile, window, cx);
        let names: Vec<String> = this.tabs.iter().map(|tab| file_name(&tab.path)).collect();
        assert_eq!(names, ["Untitled-1", "Untitled-2"]);
        assert_eq!(this.active, Some(1));
        assert!(this.tabs[1].editor.read(cx).focus_handle(cx).is_focused(window), "ready to write in");
        assert!(!this.is_shown(Panel::Files, cx));
        assert_eq!(this.tabs[1].editor.read(cx).value(), "");
        assert!(!this.tabs[1].dirty, "empty, it closes without asking");
        assert!(this.session(cx).tabs.is_empty(), "not remembered with what's open");
        assert!(this.completion_target(&this.tabs[1].editor.clone()).is_none(), "nor sent to a language server");

        // Closing the first, its number is free again.
        this.close(0, window, cx);
        this.new_file(&NewFile, window, cx);
        let names: Vec<String> = this.tabs.iter().map(|tab| file_name(&tab.path)).collect();
        assert_eq!(names, ["Untitled-2", "Untitled-1"]);
    }));
}

/// What's written in it is unsaved, but only Save, which asks for a name,
/// saves it: not losing focus, nor Save All.
#[gpui_kit::test]
fn an_untitled_tab_is_saved_only_with_a_name(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, true);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        Config::update(cx, |config| config.auto_save_on_focus_loss = true);
        this.new_file(&NewFile, window, cx);
        type_in(this, 0, "notes", window, cx);
        assert!(this.tabs[0].dirty);
        assert_eq!(this.unsaved(), [PathBuf::from("Untitled-1")]);

        let editor = this.tabs[0].editor.clone();
        this.auto_save_editor(&editor, window, cx);
        assert!(this.message.is_none(), "losing focus leaves it alone");

        this.save_all(cx).detach();
        assert_eq!(this.message.as_deref(), Some("Not saved: Untitled-1 has no name yet; Save asks for one"));
        assert!(this.tabs[0].dirty);
    }));
}

/// On a server there's no system dialog: its name is typed in the files
/// panel, in the task's folder.
#[gpui_kit::test]
fn on_a_server_its_name_is_typed_in_the_files_panel(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, false);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.new_file(&NewFile, window, cx);
        this.save(&Save, window, cx);
        assert!(this.is_shown(Panel::Files, cx));
        let dir = this.file_tree.read(cx).save_as_dir().map(Path::to_path_buf);
        assert_eq!(dir, Some(PathBuf::from("/new-file-test")));
    }));
}

/// Saved, it's the tab of its file: with the language of its name, the
/// text and cursor it had, and remembered with what's open.
#[gpui_kit::test]
fn saved_it_becomes_the_tab_of_its_file(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, true);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.new_file(&NewFile, window, cx);
        type_in(this, 0, "# Notes\n\nmore", window, cx);
        this.tabs[0].editor.update(cx, |state, cx| state.set_cursor_position(Position::new(2, 2), window, cx));
        this.became_file(0, PathBuf::from("/new-file-test/notes.md"), "# Notes\n\nmore".into(), window, cx);

        let tab = &this.tabs[0];
        assert_eq!(tab.path, Path::new("/new-file-test/notes.md"));
        assert!(!tab.has_no_file() && tab.is_file());
        assert!(!tab.dirty);
        assert!(tab.markdown.is_some(), "a Markdown file now");
        assert_eq!(tab.editor.read(cx).value(), "# Notes\n\nmore");
        assert_eq!(tab.editor.read(cx).cursor_position(), Position::new(2, 2));
        assert_eq!(this.session(cx).tabs.len(), 1);
        assert!(this.unsaved().is_empty());
    }));
}

/// The quit dialog lists it by its name, and clicking it goes to it.
#[gpui_kit::test]
fn the_quit_dialog_goes_to_an_untitled_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, true);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.new_file(&NewFile, window, cx);
        type_in(this, 0, "draft", window, cx);
        this.new_file(&NewFile, window, cx);
        this.show_unsaved(Path::new("Untitled-1"), window, cx);
        assert_eq!(this.active, Some(0));
        assert_eq!(this.tabs.len(), 2);
    }));
}

/// Save As on a server names the file again in the files panel, in its
/// file's folder; the tab keeps its changes and becomes the new file's.
#[gpui_kit::test]
fn save_as_names_a_file_again(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, false);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.new_file(&NewFile, window, cx);
        type_in(this, 0, "fn main() {}", window, cx);
        this.became_file(0, PathBuf::from("/new-file-test/src/main.rs"), "fn main() {}".into(), window, cx);
        type_in(this, 0, "fn main() { run() }", window, cx);
        this.save_as(&SaveAs, window, cx);
        let dir = this.file_tree.read(cx).save_as_dir().map(Path::to_path_buf);
        assert_eq!(dir, Some(PathBuf::from("/new-file-test/src")));

        this.became_file(0, PathBuf::from("/new-file-test/src/run.rs"), "fn main() { run() }".into(), window, cx);
        assert_eq!(this.tabs.len(), 1);
        assert_eq!(this.tabs[0].path, Path::new("/new-file-test/src/run.rs"));
        assert!(!this.tabs[0].dirty);
    }));
}

/// Files from anywhere on this machine (dropped from Finder, Open File)
/// open in tabs, outside the workspace's folder too; folders don't.
#[gpui_kit::test]
fn files_from_outside_the_folder_open(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "elsewhere").unwrap();
    let (workspace, cx) = workspace(cx, true);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.open_files(vec![dir.path().to_path_buf(), file.clone()], window, cx);
        assert_eq!(this.tabs.len(), 1);
        assert_eq!(this.tabs[0].path, normalize(&file));
        assert!(!this.tabs[0].preview);
    }));
}

/// A server can't read this machine's files: dropped on its workspace, they
/// don't open.
#[gpui_kit::test]
fn files_from_this_machine_dont_open_on_a_server(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx, false);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.open_files(vec![PathBuf::from("/tmp/notes.txt")], window, cx);
        assert!(this.tabs.is_empty());
        assert!(this.message.is_some());
    }));
}
