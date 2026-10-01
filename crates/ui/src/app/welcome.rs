//! What the window shows with no folder open: the logo and, below it, how to
//! open one (here or on a server), the recent ones and, without repos, how to
//! add one for tasks.

use super::*;

/// Recent folders listed; the rest are in Open Recent.
const SHOWN: usize = 8;

impl Sik {
    pub(super) fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let keys = |id: &str| {
            SHORTCUTS
                .iter()
                .find(|shortcut| shortcut.id == id)
                .and_then(|shortcut| shortcuts::keys(shortcut, cx))
                .map(|keys| Kbd::format(&keys))
                .unwrap_or_default()
        };
        let heading = |label: &'static str| {
            div()
                .px_2()
                .pb_1()
                .text_ui_small(cx)
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(label)
        };
        let row = |id: ElementId| {
            h_flex()
                .id(id)
                .px_2()
                .py_1()
                .gap_4()
                .rounded(theme.radius)
                .hover(|style| style.bg(theme.accent))
        };
        let action = |id: &'static str, label: &'static str, detail: String| {
            row(id.into())
                .child(div().flex_1().text_color(theme.primary).child(label))
                .child(div().text_color(theme.muted_foreground).child(detail))
        };
        // Tasks need a repo: without any, the way to add one.
        let no_repos = self.hosts.iter().all(|host| host.tasks.is_empty());

        let start = v_flex()
            .child(heading("START"))
            .child(
                action("welcome-open", "Open Folder…", keys("OpenFolder"))
                    .on_click(cx.listener(|this, _, window, cx| this.open_folder(&OpenFolder, window, cx))),
            )
            .child(
                action("welcome-open-remote", "Open Folder on Server…", keys("OpenRemoteFolder")).on_click(
                    cx.listener(|this, _, window, cx| this.open_remote_folder(&OpenRemoteFolder, window, cx)),
                ),
            )
            .when(no_repos, |el| {
                el.child(
                    action("welcome-add-repo", "Add Repo…", "for tasks: branches side by side".into())
                        .on_click(cx.listener(|this, _, window, cx| this.add_local_repo(window, cx))),
                )
            });

        let recents = self.recents(cx);
        let more = recents.len() > SHOWN;
        let recent = (!recents.is_empty()).then(|| {
            v_flex()
                .child(heading("RECENT"))
                .children(recents.into_iter().take(SHOWN).enumerate().map(|(ix, (key, label))| {
                    row(("welcome-recent", ix).into())
                        .child(div().flex_none().text_color(theme.primary).child(folder_name(&key.path)))
                        .child(
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_color(theme.muted_foreground)
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_path(key.host.clone(), key.path.clone(), window, cx)
                        }))
                }))
                .when(more, |el| {
                    el.child(
                        action("welcome-more", "More…", keys("OpenRecent"))
                            .on_click(cx.listener(|this, _, window, cx| this.open_recent(&OpenRecent, window, cx))),
                    )
                })
        });

        div()
            .id("welcome")
            .size_full()
            .overflow_y_scroll()
            .text_ui(cx)
            .child(
                v_flex()
                    .min_h_full()
                    .items_center()
                    .justify_center()
                    .px_4()
                    .py(px(48.))
                    .gap_10()
                    // The same logo as an empty editor, as subtle.
                    .child(
                        svg()
                            .path("icons/sik-empty.svg")
                            .size(px(280.))
                            .max_w_full()
                            .flex_none()
                            .text_color(theme.muted_foreground.opacity(0.22)),
                    )
                    .child(v_flex().w(px(400.)).max_w_full().gap_6().child(start).children(recent)),
            )
            .into_any_element()
    }
}
