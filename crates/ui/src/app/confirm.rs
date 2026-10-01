//! Confirming in a dialog what can't be undone: deleting a worktree and
//! restarting a server's agent. Enter confirms, Esc or a click outside cancels.

use super::*;

impl Sik {
    /// Delete Worktree…
    pub(super) fn ask_remove(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.confirm_remove = Some((key, self.confirm_focus(window, cx)));
        cx.notify();
    }

    /// Outdated agent · restart.
    pub(super) fn ask_restart(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_restart = Some((name, self.confirm_focus(window, cx)));
        cx.notify();
    }

    /// The dialog's focus, given once whatever opened it is done: a menu
    /// gives the focus back to where it was when it closes.
    fn confirm_focus(&self, window: &mut Window, cx: &mut Context<Self>) -> FocusHandle {
        let focus = cx.focus_handle();
        cx.defer_in(window, {
            let focus = focus.clone();
            move |_, window, cx| focus.focus(window, cx)
        });
        focus
    }

    fn cancel_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        self.confirm_restart = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn render_confirm_remove(&self, key: &TaskKey, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let detail = if key.host == LOCAL && self.task(key).is_some_and(|task| task.repo.join(".sik/remove").is_file()) {
            "The repo's .sik/remove deletes it; depending on the repo, along with its uncommitted changes."
        } else {
            "With the repo's .sik/remove if it has one; otherwise git worktree remove, which won't delete with uncommitted changes."
        };
        let key = key.clone();
        let title = format!("Delete {}?", self.label(&key));
        self.render_confirm(focus, title, detail, "Delete", true, move |this, window, cx| this.remove_task(key.clone(), window, cx), cx)
    }

    pub(super) fn render_confirm_restart(&self, name: &SharedString, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let name = name.clone();
        let title = format!("Restart the agent on {name}?");
        let detail = "A new version of the agent is available. Restarting it restarts its terminals: they reopen in place, without their scrollback, and Claude Code resumes its conversation.";
        self.render_confirm(focus, title, detail, "Restart", false, move |this, window, cx| this.restart_agent(name.clone(), window, cx), cx)
    }

    /// The dialog: what's about to happen and its button, red if it
    /// destroys something.
    #[allow(clippy::too_many_arguments)]
    fn render_confirm(
        &self,
        focus: &FocusHandle,
        title: String,
        detail: &'static str,
        action: &'static str,
        destructive: bool,
        confirm: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, hover) = if destructive {
            (theme.danger, theme.danger_foreground, theme.danger_hover)
        } else {
            (theme.primary, theme.primary_foreground, theme.primary_hover)
        };
        let on_key = confirm.clone();
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
                    .id("confirm")
                    .track_focus(focus)
                    .w(px(400.))
                    .max_w(relative(0.9))
                    .p_4()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .text_ui(cx)
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| this.cancel_confirm(window, cx)))
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.cancel_confirm(window, cx),
                            "enter" => on_key(this, window, cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(div().text_base().font_semibold().child(title))
                    .child(div().text_color(theme.muted_foreground).whitespace_normal().child(detail))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                dialog_button("confirm-cancel", "Cancel", cx)
                                    .on_click(cx.listener(|this, _, window, cx| this.cancel_confirm(window, cx))),
                            )
                            .child(
                                div()
                                    .id("confirm-action")
                                    .px_3()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .bg(bg)
                                    .text_color(fg)
                                    .hover(|style| style.bg(hover))
                                    .child(action)
                                    .on_click(cx.listener(move |this, _, window, cx| confirm(this, window, cx))),
                            ),
                    ),
            )
    }
}
