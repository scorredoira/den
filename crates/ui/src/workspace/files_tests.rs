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
    tab.text = Some(text.into());
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

/// A change on disk to a tab with unsaved changes asks at once, and only
/// once for each version. Save asks again: Overwrite saves (here, with no
/// agent, tries to), Reload shows what is on disk and drops what wasn't
/// saved, Cancel does nothing.
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
    cx.run_until_parked();
    cx.simulate_prompt_answer("Keep My Changes");
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        read(this, "/files-test/a.txt", true, "theirs", window, cx);
    }));
    cx.run_until_parked();
    assert!(!cx.has_pending_prompt(), "the same version isn't asked about twice");
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
    cx.run_until_parked();
    cx.simulate_prompt_answer("Keep My Changes");
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

/// Closing a file's tab leaves its other view holding the file: what's
/// unsaved, what's on disk and the writes on their way.
#[gpui_kit::test]
fn the_other_view_takes_over_the_file(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        this.editor_split = Some(Axis::Row);
        this.new_view(0, 1, true, window, cx);
        type_in(this, &editor, "mine", window, cx);
        read(this, "/files-test/a.txt", true, "theirs", window, cx);
        let lock = this.tabs[0].save_lock.clone();
        this.close(0, window, cx);
        assert_eq!(this.tabs.len(), 1, "it had another view: closed at once");
        let tab = &this.tabs[0];
        assert!(tab.is_file() && tab.dirty);
        assert_eq!((tab.saved.as_str(), tab.on_disk.as_deref()), ("one", Some("theirs")));
        assert_eq!(tab.text.as_ref().map(|text| text.to_string()).as_deref(), Some("mine"));
        assert!(Arc::ptr_eq(&tab.save_lock, &lock));
    }));
}

/// An edit that keeps the text's length still makes it unsaved, undoing it
/// makes it saved again, and the file's other view follows both.
#[gpui_kit::test]
fn edits_of_the_same_length_count(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        let editor = add_tab(this, "/files-test/a.txt", "one", window, cx);
        this.editor_split = Some(Axis::Row);
        let view = this.new_view(0, 1, true, window, cx);
        let view = this.tabs[view].editor.clone();
        type_in(this, &editor, "two", window, cx);
        assert!(this.tabs[0].dirty);
        assert_eq!(view.read(cx).value(), "two");
        type_in(this, &view, "one", window, cx);
        assert!(!this.tabs[0].dirty);
        assert_eq!(editor.read(cx).value(), "one");
    }));
}

const PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63,
    0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

fn png(cx: &mut App) -> Arc<RenderImage> {
    Image::from_bytes(ImageFormat::Png, PNG.to_vec()).to_image_data(cx.svg_renderer()).unwrap()
}

/// An image tab's picture stays on the GPU while a tab shows it, its other
/// view too, and goes with the last one.
#[gpui_kit::test]
fn closing_an_image_frees_it_with_its_last_tab(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    let image = cx.update(|_, cx| png(cx));
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        add_tab(this, "/files-test/a.png", "", window, cx);
        this.tabs[0].image = Some(image.clone());
        this.activate(0, window, cx);
        this.split_editor(Axis::Row, window, cx);
    }));
    assert!(cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.has_image_atlas_entry(&image)
    }));
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.close(1, window, cx)));
    assert!(cx.update(|window, _| window.has_image_atlas_entry(&image)));
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.close(0, window, cx)));
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
}

struct Holder(Option<Entity<Workspace>>);

impl Render for Holder {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().children(self.0.clone())
    }
}

/// A workspace that goes takes its tabs' images off the GPU.
#[gpui_kit::test]
fn closing_a_workspace_frees_its_images(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    let (holder, cx) = cx.add_window_view(|window, cx| {
        let workspace = cx.new(|cx| Workspace::new(PathBuf::from("/files-test"), None, true, "files-test".into(), window, cx));
        Holder(Some(workspace))
    });
    let image = cx.update(|_, cx| png(cx));
    cx.update(|window, cx| {
        let workspace = holder.read(cx).0.clone().unwrap();
        workspace.update(cx, |this, cx| {
            add_tab(this, "/files-test/a.png", "", window, cx);
            this.tabs[0].image = Some(image.clone());
            this.activate(0, window, cx);
        });
        window.draw(cx).clear(cx);
        assert!(window.has_image_atlas_entry(&image));
    });
    cx.update(|_, cx| holder.update(cx, |holder, cx| {
        holder.0 = None;
        cx.notify();
    }));
    cx.run_until_parked();
    assert!(!cx.update(|window, _| window.has_image_atlas_entry(&image)));
}
