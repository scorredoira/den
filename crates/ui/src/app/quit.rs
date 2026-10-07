//! Quitting or closing a window with unsaved files: the list of them, to
//! go to each, and Save All and Quit.

use super::*;

impl Den {
    /// Unsaved files across all tasks, relative to their own task.
    pub(super) fn unsaved(&self, cx: &App) -> Vec<(TaskKey, PathBuf)> {
        let mut unsaved: Vec<(TaskKey, PathBuf)> = self
            .workspaces
            .iter()
            .flat_map(|(key, workspace)| workspace.read(cx).unsaved().into_iter().map(move |file| (key.clone(), file)))
            .collect();
        unsaved.sort_by_key(|(key, file)| (self.label(key), file.clone()));
        unsaved
    }

    /// Before quitting (Cmd-Q) or closing the window: if any task has unsaved
    /// files, asks and only goes on if confirmed. Returns whether it can go
    /// on right away.
    pub fn confirm_quit(&mut self, closing: Closing, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.discarded || self.unsaved(cx).is_empty() {
            return true;
        }
        match &mut self.quit_confirm {
            Some((_, asked)) => *asked = closing,
            None => {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                self.quit_confirm = Some((focus, closing));
            }
        }
        cx.notify();
        false
    }

    /// The dialog's Quit (or Close) Without Saving. Quitting, the other
    /// windows with unsaved files still ask.
    pub(super) fn discard_and_close(&mut self, cx: &mut Context<Self>) {
        match self.quit_confirm.take() {
            Some((_, Closing::Window)) => self.close_window(cx),
            _ => {
                self.discarded = true;
                cx.defer(quit);
            }
        }
    }

    /// Closes its window, unsaved files and all.
    pub(super) fn close_window(&mut self, cx: &mut Context<Self>) {
        self.discarded = true;
        let handle = self.handle;
        cx.defer(move |cx| {
            handle.update(cx, |_, window, _| window.remove_window()).ok();
        });
    }

    pub(super) fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_confirm = None;
        crate::update::cancel_restart(cx);
        // Not quitting after all: a window that went on without saving asks
        // again.
        self.discarded = false;
        cx.defer(|cx| {
            for (_, den) in windows(cx) {
                den.update(cx, |den, _| den.discarded = false);
            }
        });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Saves everything and quits; if something couldn't be saved, the dialog
    /// stays with what's left (and the tab says why).
    pub(super) fn save_and_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_saving {
            return;
        }
        let saves: Vec<Task<bool>> = self
            .workspaces
            .values()
            .map(|workspace| workspace.update(cx, |workspace, cx| workspace.save_all(cx)))
            .collect();
        self.quit_saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut ok = true;
            for save in saves {
                ok &= save.await;
            }
            this.update(cx, |this, cx| {
                this.quit_saving = false;
                if ok && this.unsaved(cx).is_empty() {
                    match this.quit_confirm.take() {
                        Some((_, Closing::Window)) => this.close_window(cx),
                        // The other windows may have some too.
                        _ => cx.defer(quit),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Click on a file in the dialog: it closes and goes to its tab.
    pub(super) fn go_to_unsaved(&mut self, key: TaskKey, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // Not quitting after all, as with Cancel.
        self.cancel_quit(window, cx);
        let path = key.path.join(&file);
        self.activate(key, window, cx);
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.open(path, true, window, cx));
        }
    }

    pub(super) fn render_quit_confirm(&self, focus: &FocusHandle, closing: Closing, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let unsaved = self.unsaved(cx);
        let title = match unsaved.len() {
            1 => "There is 1 unsaved file".to_string(),
            n => format!("There are {n} unsaved files"),
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.25))
            .occlude()
            .child(
                v_flex()
                    .id("quit-confirm")
                    .track_focus(focus)
                    .w(px(480.))
                    .p_4()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .text_ui(cx)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.cancel_quit(window, cx),
                            "enter" => this.save_and_quit(cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(div().text_base().font_semibold().child(title))
                    .child(
                        v_flex()
                            .id("quit-confirm-files")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .children(unsaved.into_iter().enumerate().map(|(ix, (key, file))| {
                                let label = self.label(&key);
                                let name = file.display().to_string();
                                h_flex()
                                    .id(("unsaved", ix))
                                    .px_2()
                                    .py_1()
                                    .gap_2()
                                    .rounded(theme.radius)
                                    .hover(|style| style.bg(theme.accent))
                                    .child(div().flex_none().text_color(theme.muted_foreground).child(label))
                                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.go_to_unsaved(key.clone(), file.clone(), window, cx)
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child("Click a file to go to it."),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(dialog_button("quit-cancel", "Cancel", cx).on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                            .child(
                                dialog_button(
                                    "quit-discard",
                                    if closing == Closing::App { "Quit Without Saving" } else { "Close Without Saving" },
                                    cx,
                                )
                                .text_color(theme.danger)
                                .on_click(cx.listener(|this, _, _, cx| this.discard_and_close(cx))),
                            )
                            .child(
                                div()
                                    .id("quit-save")
                                    .px_3()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .hover(|style| style.bg(theme.primary_hover))
                                    .child(match (self.quit_saving, closing) {
                                        (true, _) => "Saving…",
                                        (false, Closing::App) => "Save All and Quit",
                                        (false, Closing::Window) => "Save All and Close",
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.save_and_quit(cx))),
                            ),
                    ),
            )
    }
}
