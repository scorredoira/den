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

/// The agent answering a read of `path` with `text`.
fn read(this: &mut Workspace, path: &str, reload: bool, text: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let generation = this.begin_read(Path::new(path));
    this.loaded(Path::new(path), reload, generation, Ok(Read::Text(text.into())), window, cx);
}

/// Two changes on disk, two reads: if the first one's answer comes last,
/// it's older than what the tab shows and is ignored.
#[gpui_kit::test]
fn a_stale_read_is_ignored(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        let path = Path::new("/files-test/a.txt");
        let (first, second) = (this.begin_read(path), this.begin_read(path));
        this.loaded(path, true, second, Ok(Read::Text("three".into())), window, cx);
        this.loaded(path, true, first, Ok(Read::Text("two".into())), window, cx);
        assert_eq!(editor.read(cx).value(), "three");
        assert_eq!(this.tabs[0].saved, "three");
    }));
}

fn type_in(this: &mut Workspace, editor: &Entity<EditorState>, text: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    editor.update(cx, |state, cx| state.set_value(text.to_string(), window, cx));
    this.on_edit(editor, window, cx);
}

/// A file without unsaved changes shows what another program wrote.
#[gpui_kit::test]
fn a_clean_tab_follows_the_file(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        read(this, "/files-test/a.txt", true, "two", window, cx);
        assert_eq!(editor.read(cx).value(), "two");
        let tab = &this.tabs[0];
        assert_eq!((tab.saved.as_str(), tab.dirty, tab.on_disk.is_none()), ("two", false, true));
    }));
}

/// What another program wrote while the tab had unsaved changes isn't
/// overwritten without asking: not by saving all, nor on losing focus.
#[gpui_kit::test]
fn changes_on_disk_are_not_overwritten_silently(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    let (editor, saving) = cx.update(|window, cx| workspace.update(cx, |this, cx| {
        cx.global_mut::<Config>().auto_save_on_focus_loss = true;
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        type_in(this, &editor, "mine", window, cx);
        read(this, "/files-test/a.txt", true, "theirs", window, cx);
        assert_eq!(editor.read(cx).value(), "mine", "the unsaved changes are kept");
        assert_eq!(this.tabs[0].on_disk.as_deref(), Some("theirs"));
        assert!(this.tabs[0].dirty);

        this.auto_save_editor(&editor, window, cx);
        assert!(this.message.as_deref().is_some_and(|message| message.contains("changed on disk")));
        let saving = this.save_all(cx);
        assert!(this.message.as_deref().is_some_and(|message| message.starts_with("Not saved")));
        (editor, saving)
    }));
    assert!(!cx.foreground_executor().block_test(saving));

    // Back as it was on disk: only the unsaved changes are left.
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        read(this, "/files-test/a.txt", true, "one", window, cx);
        assert!(this.tabs[0].on_disk.is_none());
        assert!(this.message.is_none());
        assert_eq!(editor.read(cx).value(), "mine");
    }));
}

/// Save asks: Overwrite saves (here, with no agent, tries to), Reload shows
/// what is on disk and drops what wasn't saved, Cancel does nothing.
#[gpui_kit::test]
fn save_asks_whether_to_overwrite_or_reload(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    let editor = cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        this.activate(0, window, cx);
        type_in(this, &editor, "mine", window, cx);
        read(this, "/files-test/a.txt", true, "theirs", window, cx);
        editor
    }));
    let save = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| workspace.update(cx, |this, cx| this.save(&Save, window, cx)));
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
    };

    save(cx);
    cx.simulate_prompt_answer("Cancel");
    cx.run_until_parked();
    cx.update(|_, cx| assert!(workspace.read(cx).tabs[0].on_disk.is_some()));

    save(cx);
    cx.simulate_prompt_answer("Overwrite");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let this = workspace.read(cx);
        assert!(this.tabs[0].on_disk.is_none());
        assert_eq!(this.message.as_deref(), Some("Couldn't save: no agent"));
    });

    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        read(this, "/files-test/a.txt", true, "theirs again", window, cx);
    }));
    save(cx);
    cx.simulate_prompt_answer("Reload, Losing My Changes");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let this = workspace.read(cx);
        assert!(this.tabs[0].on_disk.is_none());
        assert_eq!(editor.read(cx).value(), "theirs again");
        assert_eq!(this.tabs[0].saved, "theirs again");
        assert!(!this.tabs[0].dirty);
    });
}
