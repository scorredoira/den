use super::*;
use core::prelude::v1::test;

fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/auto-save-test"), None, true, "auto-save-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace
    })
}

fn add_tab(this: &mut Workspace, path: PathBuf, window: &mut Window, cx: &mut Context<Workspace>) {
    let mut tab = this.new_tab(path, false, window, cx);
    tab.content = Content::Ready;
    tab.saved = "original".into();
    tab.editor.update(cx, |state, cx| state.set_value("original", window, cx));
    this.tabs.push(tab);
}

#[test]
fn defaults_off_and_round_trips() {
    let mut config: Config = serde_json::from_str("{}").unwrap();
    assert!(!config.auto_save_on_focus_loss);
    config.auto_save_on_focus_loss = true;
    let saved = serde_json::to_string(&config).unwrap();
    assert!(serde_json::from_str::<Config>(&saved).unwrap().auto_save_on_focus_loss);
}

#[gpui_kit::test]
fn focus_loss_obeys_setting_and_keeps_unsaved_text_on_failure(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        add_tab(this, "/auto-save-test/first.txt".into(), window, cx);
        add_tab(this, "/auto-save-test/second.txt".into(), window, cx);
        let editor = this.tabs[0].editor.clone();
        editor.update(cx, |state, cx| state.set_value("unsaved", window, cx));
        this.on_edit(&editor, window, cx);
        this.activate(0, window, cx);
    }));
    cx.run_until_parked();
    cx.update(|window, _| assert!(window.is_window_active(), "test window must be active"));
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(1, window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        assert!(this.message.is_none(), "autosave is off by default");
        assert!(this.tabs[0].dirty);
        cx.global_mut::<Config>().auto_save_on_focus_loss = true;
        this.activate(0, window, cx);
    }));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(1, window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        assert_eq!(this.message.as_deref(), Some("Couldn't save: no agent"));
        assert!(this.tabs[0].dirty);
        assert_eq!(this.tabs[0].editor.read(cx).value(), "unsaved");
        assert!(!this.tabs[1].dirty);
        assert!(this.tabs[1].editor.read(cx).focus_handle(cx).is_focused(window));
    }));
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(0, window, cx)));
    cx.run_until_parked();
    cx.deactivate_window();
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert_eq!(workspace.read(cx).message.as_deref(), Some("Couldn't save: no agent"));
        assert!(workspace.read(cx).tabs[0].dirty);
    });
}

/// Run against an isolated test agent: SIK_TEST_AGENT is its executable and
/// SIK_AGENT_SOCKET / SIK_STATE_DIR must point to a temporary test directory.
#[gpui_kit::test]
#[ignore]
fn writes_and_formats_the_blurred_file_and_its_split_view(cx: &mut TestAppContext) {
    // The real agent's reader thread wakes UI futures outside the test scheduler.
    cx.executor().allow_parking();
    let executable = std::env::var_os("SIK_TEST_AGENT").expect("isolated test agent required");
    let socket = std::env::var_os("SIK_AGENT_SOCKET").expect("isolated test socket required");
    let root = PathBuf::from(std::env::var_os("SIK_STATE_DIR").expect("isolated test state required"));
    assert!(Path::new(&socket).starts_with(&root));
    let client = Client::connect_local(Path::new(&executable)).unwrap();
    let file = root.join("autosave.json");
    std::fs::write(&file, "original").unwrap();
    let (workspace, cx) = workspace(cx);
    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.root = root.clone();
        this.client = Some(client);
        add_tab(this, file.clone(), window, cx);
        add_tab(this, root.join("other.txt"), window, cx);
        let editor = this.tabs[0].editor.clone();
        editor.update(cx, |state, cx| state.set_value("{\"value\":1}", window, cx));
        this.on_edit(&editor, window, cx);
        this.activate(0, window, cx);
    }));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(1, window, cx)));
    cx.run_until_parked();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");

    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        cx.global_mut::<Config>().auto_save_on_focus_loss = true;
        cx.global_mut::<Config>().format_on_save = vec!["json".into()];
        this.activate(0, window, cx);
    }));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(1, window, cx)));
    wait_for_save(&workspace, cx);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains('\n'), "format on save: {saved}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&saved).unwrap()["value"], 1);

    cx.update(|window, cx| workspace.update(cx, |this, cx| {
        this.editor_split = Some(Axis::Row);
        let view = this.new_view(0, 1, true, window, cx);
        let editor = this.tabs[view].editor.clone();
        editor.update(cx, |state, cx| state.set_value("{\"value\":2}", window, cx));
        this.on_edit(&editor, window, cx);
        this.activate(view, window, cx);
    }));
    cx.run_until_parked();
    cx.update(|window, cx| workspace.update(cx, |this, cx| this.activate(1, window, cx)));
    wait_for_save(&workspace, cx);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&saved).unwrap()["value"], 2);

    // Queue two saves while another write holds the lock. Both must use the
    // latest buffer after acquiring it, never restore a stale snapshot.
    let lock = cx.update(|_, cx| workspace.read(cx).tabs[0].save_lock.clone());
    let guard = lock.try_lock().unwrap();
    let finished = Rc::new(std::cell::Cell::new(0));
    for value in [3, 4] {
        cx.update(|window, cx| workspace.update(cx, |this, cx| {
            cx.global_mut::<Config>().auto_save_on_focus_loss = false;
            let editor = this.tabs[0].editor.clone();
            editor.update(cx, |state, cx| state.set_value(format!("{{\"value\":{value}}}"), window, cx));
            this.on_edit(&editor, window, cx);
            let save = this.save_tab(0, cx);
            let finished = finished.clone();
            cx.spawn(async move |_, _| {
                assert!(save.await);
                finished.set(finished.get() + 1);
            }).detach();
        }));
        cx.run_until_parked();
    }
    drop(guard);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while finished.get() < 2 {
        cx.run_until_parked();
        assert!(std::time::Instant::now() < deadline, "queued saves did not finish");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let saved = std::fs::read_to_string(&file).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&saved).unwrap()["value"], 4);
}

fn wait_for_save(workspace: &Entity<Workspace>, cx: &mut VisualTestContext) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.update(|_, cx| !workspace.read(cx).tabs[0].dirty) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "autosave did not finish: {:?}", cx.update(|_, cx| {
            let workspace = workspace.read(cx);
            (workspace.message.clone(), workspace.tabs[0].editor.read(cx).value().to_string(), std::fs::read_to_string(&workspace.tabs[0].path))
        }));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    cx.update(|window, cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.tabs[1].editor.read(cx).focus_handle(cx).is_focused(window));
    });
}
