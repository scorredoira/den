//! Confirming in a dialog what can't be undone: deleting a worktree,
//! restarting a server's agent and restarting into an update. Enter confirms, Esc or a click outside cancels.

use super::*;

/// What deleting a worktree would lose, as far as git knows.
pub(crate) struct AtRisk {
    /// Files changed and not committed, untracked ones included.
    changes: usize,
    /// Commits the main branch doesn't have, the newest first.
    commits: Vec<proto::CommitInfo>,
}

/// Commits listed by name in the dialog; the rest, counted.
const COMMITS_LISTED: usize = 3;

impl Sik {
    /// Delete Worktree…: asks git what it would lose while the dialog is up.
    pub(super) fn ask_remove(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.confirm_remove = Some((key.clone(), self.confirm_focus(window, cx), None));
        cx.notify();
        let Some(client) = self.client(&key.host) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let changes = client.request(Request::GitChanges { path: key.path.clone(), uncommitted: true }).await;
            let commits = client
                .request(Request::Git { path: key.path.clone(), op: GitOp::Unmerged { limit: 1000 } })
                .await;
            // What git couldn't answer (an older agent) isn't warned about.
            let at_risk = AtRisk {
                changes: match changes {
                    Ok(Response::Changes { files, .. }) => files.len(),
                    _ => 0,
                },
                commits: match commits {
                    Ok(Response::Commits(commits)) => commits,
                    _ => Vec::new(),
                },
            };
            this.update(cx, |this, cx| {
                if let Some((asked, _, pending)) = &mut this.confirm_remove
                    && *asked == key
                {
                    *pending = Some(at_risk);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Outdated agent · restart.
    pub(super) fn ask_restart(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_restart = Some((name, self.confirm_focus(window, cx)));
        cx.notify();
    }

    /// Restart to update, in the title bar or About.
    pub(super) fn ask_update(&mut self, version: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_update = Some((version, self.confirm_focus(window, cx)));
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

    pub(super) fn cancel_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_remove = None;
        self.confirm_restart = None;
        self.confirm_update = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn render_confirm_remove(
        &self,
        key: &TaskKey,
        focus: &FocusHandle,
        at_risk: Option<&AtRisk>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let detail = if key.host == LOCAL && self.task(key).is_some_and(|task| task.repo.join(".sik/remove").is_file()) {
            "The repo's .sik/remove deletes it; depending on the repo, along with its uncommitted changes."
        } else {
            "With the repo's .sik/remove if it has one; otherwise git worktree remove, which won't delete with uncommitted changes."
        };
        let (warning, action) = match at_risk {
            None => (Some((false, "Looking for uncommitted changes and unmerged commits…".to_string())), "Delete"),
            Some(at_risk) => match risk_text(at_risk) {
                Some(text) => (Some((true, text)), "Delete Anyway"),
                None => (None, "Delete"),
            },
        };
        let key = key.clone();
        let title = format!("Delete {}?", self.label(&key));
        // Not before git has said what it would lose.
        let confirm = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            if this.confirm_remove.as_ref().is_some_and(|(_, _, at_risk)| at_risk.is_some()) {
                this.remove_task(key.clone(), window, cx)
            }
        };
        self.render_confirm(focus, title, warning, detail, action, true, confirm, cx)
    }

    pub(super) fn render_confirm_restart(&self, name: &SharedString, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let name = name.clone();
        let title = format!("Restart the agent on {name}?");
        let detail = "A new version of the agent is available. Restarting it restarts its terminals: they reopen in place, without their scrollback, and Claude Code resumes its conversation.";
        self.render_confirm(focus, title, None, detail, "Restart", false, move |this, window, cx| this.restart_agent(name.clone(), window, cx), cx)
    }

    pub(super) fn render_confirm_update(&self, version: &SharedString, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let title = format!("Restart to update to Sik {version}?");
        let detail = "Workspaces, open files and terminals reopen as they are, and whatever runs in the terminals keeps running. Unsaved files are asked about first.";
        self.render_confirm(focus, title, None, detail, "Restart", false, |this, window, cx| this.restart_to_update(window, cx), cx)
    }

    fn restart_to_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_update = None;
        self.focus_active(window, cx);
        cx.notify();
        // Once this click is done: quitting may open the unsaved files dialog.
        cx.defer_in(window, |_, _, cx| crate::update::restart(cx));
    }

    /// The dialog: what's about to happen and its button, red if it
    /// destroys something. `warning`, above it: what would be lost (`true`,
    /// in the warning color) or a note.
    #[allow(clippy::too_many_arguments)]
    fn render_confirm(
        &self,
        focus: &FocusHandle,
        title: String,
        warning: Option<(bool, String)>,
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
                    .children(warning.map(|(lost, text)| {
                        div()
                            .text_color(if lost { theme.warning } else { theme.muted_foreground })
                            .whitespace_normal()
                            .child(text)
                    }))
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

/// What a worktree would lose, in words; none if nothing.
fn risk_text(at_risk: &AtRisk) -> Option<String> {
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let mut parts = Vec::new();
    if at_risk.changes > 0 {
        parts.push(plural(at_risk.changes, "file with uncommitted changes", "files with uncommitted changes"));
    }
    let commits = at_risk.commits.len();
    if commits > 0 {
        parts.push(plural(commits, "commit not merged into the main branch", "commits not merged into the main branch"));
    }
    if parts.is_empty() {
        return None;
    }
    let mut text = format!("It has {}.", parts.join(" and "));
    for commit in at_risk.commits.iter().take(COMMITS_LISTED) {
        text.push_str(&format!("\n• {} {}", commit.short, commit.subject));
    }
    if commits > COMMITS_LISTED {
        text.push_str(&format!("\n• …and {} more", commits - COMMITS_LISTED));
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::{AtRisk, risk_text};

    #[test]
    fn what_a_worktree_would_lose() {
        let commit = |subject: &str| proto::CommitInfo {
            hash: String::new(),
            short: "abc1234".into(),
            author: String::new(),
            time: 0,
            refs: String::new(),
            subject: subject.into(),
        };
        assert_eq!(risk_text(&AtRisk { changes: 0, commits: Vec::new() }), None);
        assert_eq!(
            risk_text(&AtRisk { changes: 1, commits: Vec::new() }).as_deref(),
            Some("It has 1 file with uncommitted changes.")
        );
        let commits = ["a", "b", "c", "d", "e"].map(commit).to_vec();
        assert_eq!(
            risk_text(&AtRisk { changes: 2, commits }).as_deref(),
            Some(
                "It has 2 files with uncommitted changes and 5 commits not merged into the main branch.\n• abc1234 a\n• abc1234 b\n• abc1234 c\n• …and 2 more"
            )
        );
    }
}
