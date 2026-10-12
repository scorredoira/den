use super::*;
use core::prelude::v1::test;

fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_global(Config::default());
    });
    cx.add_window_view(|window, cx| {
        window.activate_window();
        let mut workspace = Workspace::new(PathBuf::from("/edit-test"), None, true, "edit-test".into(), window, cx);
        workspace.hide_panel(Panel::Files, cx);
        workspace.hide_panel(Panel::Terminals, cx);
        workspace
    })
}

#[gpui_kit::test]
fn saving_hands_the_text_to_the_program_and_nothing_else(cx: &mut TestAppContext) {
    let (workspace, cx) = workspace(cx);
    let (id, events) = cx.update(|window, cx| {
        workspace.update(cx, |this, cx| {
            let id = this.open_edit("pwds.txt.cr", None, "secret".into(), window, cx);
            let editor = this.tabs[0].editor.clone();
            editor.update(cx, |state, cx| state.set_value("secret 2", window, cx));
            this.on_edit(&editor, window, cx);
            assert!(this.tabs[0].dirty);
            assert!(this.session(cx).tabs.is_empty(), "an edit isn't remembered with what's open");
            assert!(this.completion_target(&editor).is_none(), "nor sent to a language server");
            this.save_tab(0, cx).detach();
            (id, this.edit_events(id).unwrap())
        })
    });
    let Ok(EditEvent::Saved(text)) = events.try_recv() else { panic!("no save") };
    assert_eq!(text, "secret 2");

    cx.update(|window, cx| {
        workspace.update(cx, |this, cx| {
            this.edit_saved(id, Some("conflict".into()), cx).unwrap();
            assert!(this.tabs[0].dirty, "not kept: still unsaved");
            assert_eq!(this.message.as_deref(), Some("Couldn't save: conflict"));

            this.save_tab(0, cx).detach();
            this.edit_saved(id, None, cx).unwrap();
            assert!(!this.tabs[0].dirty);

            this.close_edit(id, window, cx);
            assert!(this.tabs.is_empty());
        })
    });
    assert!(matches!(events.try_recv(), Ok(EditEvent::Saved(_))));
    assert!(events.try_recv().is_err() && events.is_closed(), "the program hears the tab closed");
}
