//! About, in a modal: the version and its updates, each server's agent, and
//! where the project lives. Check for Updates opens it and checks right away.

use super::*;
use crate::update::{self, Status};

const REPO: &str = "https://github.com/scorredoira/sik";

impl Sik {
    pub(super) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.about.get_or_insert_with(|| cx.focus_handle()).clone();
        focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_about(window, cx);
        update::check_now(cx);
    }

    fn close_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.about = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(super) fn render_about(&self, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let version = env!("CARGO_PKG_VERSION");
        let link = |id: SharedString, label: &'static str| {
            div()
                .id(id)
                .text_color(theme.link)
                .hover(|style| style.underline())
                .child(label)
        };
        let heading = |label: &'static str| {
            div()
                .pt_2()
                .text_ui_small(cx)
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child(label)
        };

        let status = update::status(cx);
        let (text, color): (SharedString, Hsla) = match &status {
            Status::Idle => ("Not checked yet".into(), theme.muted_foreground),
            Status::Checking => ("Checking for updates…".into(), theme.muted_foreground),
            Status::UpToDate(latest) if latest == version => ("Sik is up to date".into(), theme.success),
            Status::UpToDate(latest) => (format!("Up to date (the latest release is {latest})").into(), theme.success),
            Status::Failed(err) => (format!("Couldn't check: {err}").into(), theme.danger),
            Status::NotInstalled => ("A development build: only an installed Sik updates".into(), theme.muted_foreground),
            Status::Ready(latest) => (format!("Sik {latest} is installed").into(), theme.primary),
        };
        let updates = h_flex()
            .gap_3()
            .child(div().flex_1().min_w_0().whitespace_normal().text_color(color).child(text))
            .child(match &status {
                Status::Ready(_) => link("about-restart".into(), "Restart to Update")
                    .on_click(|_, _, cx| update::restart(cx))
                    .into_any_element(),
                Status::Checking | Status::NotInstalled => div().into_any_element(),
                _ => link("about-check".into(), "Check for Updates")
                    .on_click(|_, _, cx| update::check_now(cx))
                    .into_any_element(),
            });

        let agents = self.hosts.iter().map(|host| {
            let (state, color): (&str, Hsla) = match (&host.status, host.client.as_ref()) {
                (HostStatus::Connected, Some(client)) if client.outdated() => ("outdated agent", theme.warning),
                (HostStatus::Connected, _) => ("up to date", theme.muted_foreground),
                (HostStatus::Connecting(step), _) => (step, theme.muted_foreground),
                (HostStatus::Failed(_), _) => ("offline", theme.danger),
            };
            let outdated = host.client.as_ref().is_some_and(|client| client.outdated());
            let name = host.name.clone();
            h_flex()
                .gap_3()
                .child(div().flex_1().child(host.name.clone()))
                .child(div().text_color(color).child(state))
                // The column asks first: restarting restarts its terminals.
                .when(outdated, |row| {
                    row.child(
                        link(format!("about-restart-agent-{name}").into(), "Restart…").on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_restart = Some(name.clone());
                            this.show_tasks_column(true, cx);
                            this.close_about(window, cx);
                        })),
                    )
                })
        });

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
                    .id("about")
                    .track_focus(focus)
                    .w(px(440.))
                    .max_w(relative(0.9))
                    .p_6()
                    .gap_2()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .text_ui(cx)
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_about(window, cx)))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "escape" | "enter") {
                            this.close_about(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        h_flex()
                            .gap_4()
                            .child(svg().path("icons/sik-empty.svg").size(px(56.)).flex_none().text_color(theme.foreground))
                            .child(
                                v_flex()
                                    .child(div().text_xl().font_semibold().child("Sik"))
                                    .child(div().text_color(theme.muted_foreground).child(format!("Version {version}"))),
                            ),
                    )
                    .child(heading("UPDATES"))
                    .child(updates)
                    .child(heading("AGENTS"))
                    .children(agents)
                    .child(
                        h_flex()
                            .pt_4()
                            .gap_4()
                            .text_ui_small(cx)
                            .child(link("about-github".into(), "GitHub").on_click(|_, _, cx| cx.open_url(REPO)))
                            .child(link("about-notes".into(), "Release Notes").on_click(move |_, _, cx| {
                                cx.open_url(&format!("{REPO}/releases/tag/v{version}"))
                            }))
                            .child(link("about-license".into(), "License (GPL-3.0)").on_click(|_, _, cx| {
                                cx.open_url(&format!("{REPO}/blob/master/LICENSE"))
                            }))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .id("about-close")
                                    .text_color(theme.muted_foreground)
                                    .hover(|style| style.text_color(theme.foreground))
                                    .child("Close")
                                    .on_click(cx.listener(|this, _, window, cx| this.close_about(window, cx))),
                            ),
                    ),
            )
    }

    /// Help → Keyboard Shortcuts: the guide, as a tab of the workspace, or
    /// in the window while none is open.
    pub(super) fn open_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = crate::guide::markdown(cx);
        match self.active_workspace() {
            Some(workspace) => {
                workspace.update(cx, |workspace, cx| workspace.open_doc(crate::guide::TITLE, text, window, cx))
            }
            None => {
                let guide = cx.new(|cx| gpui_kit::component::text::TextViewState::markdown(&text, cx));
                self.guide = Some(guide);
                cx.notify();
            }
        }
    }

    /// The guide while no workspace is open, with the way back.
    pub(super) fn render_guide(
        &self,
        guide: &Entity<gpui_kit::component::text::TextViewState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_ui_small(cx)
                    .child(
                        div()
                            .id("guide-back")
                            .text_color(theme.link)
                            .hover(|style| style.underline())
                            .child("← Welcome")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.guide = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .text_size(px(Config::get(cx).font_size(crate::config::TextArea::Preview)))
                    .child(
                        gpui_kit::component::text::TextView::new(guide)
                            .selectable(true)
                            .scrollable(true)
                            .size_full()
                            .px_8()
                            .py_6(),
                    ),
            )
            .into_any_element()
    }

    /// Help → Welcome: the welcome screen, with the open workspace kept;
    /// entering any brings it back.
    pub(super) fn show_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.guide = None;
        if let Some(active) = self.active.take() {
            self.previous = Some(active);
        }
        window.set_window_title("sik");
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
}
