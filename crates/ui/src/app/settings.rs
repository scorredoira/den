//! Settings, in a modal (Cmd-, or the gear): they're app-wide, not per task.
//! At the top a search box that searches everything; on the left an index of
//! sections; on the right the chosen section, as its own page (while
//! searching, every section with a match).

use gpui_kit::component::{input::InputEvent, kbd::Kbd, switch::Switch};

use super::*;
use crate::shortcuts::{self, SHORTCUTS, Shortcut};

/// Sections, in index order.
const SECTIONS: [&str; 6] = ["Appearance", "Editor", "Servers", "Workspaces", "Updates", "Keyboard Shortcuts"];

pub(super) const SERVERS: usize = 2;

pub(super) struct Settings {
    focus: FocusHandle,
    search: Entity<InputState>,
    /// The extensions formatted on save, as typed.
    format_on_save: Entity<InputState>,
    /// The page, to start each one at its top.
    scroll: ScrollHandle,
    section: usize,
    /// Shortcut waiting for its new key combination, and what intercepts keys
    /// meanwhile (before they do what they already do).
    recording: Option<(&'static str, Subscription)>,
    /// The chosen combination already belongs to another shortcut: ask before
    /// taking it away (a shortcut is never removed silently).
    conflict: Option<Conflict>,
    _subscription: Subscription,
}

struct Conflict {
    id: &'static str,
    keys: String,
    other: &'static Shortcut,
}

fn shortcut(id: &str) -> &'static Shortcut {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id).expect("shortcut")
}

impl Sik {
    /// Opens settings (or focuses them if already open).
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.confirm_remove = None;
        self.error = None;
        if self.host_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("add: bill or user@host"));
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.add_host(window, cx);
                }
            });
            self._subscriptions.push(subscription);
            self.host_input = Some(input);
        }
        for ix in 0..self.hosts.len() {
            if self.hosts[ix].repo_input.is_none() {
                let name = self.hosts[ix].name.clone();
                let input = cx.new(|cx| InputState::new(window, cx).placeholder("add folder: ~/path"));
                let subscription = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.add_repo(name.clone(), window, cx);
                    }
                });
                self._subscriptions.push(subscription);
                self.hosts[ix].repo_input = Some(input);
            }
        }
        if self.settings.is_none() {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings"));
            let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    cx.notify();
                }
            });
            let typed = Config::get(cx).format_on_save.join(", ");
            let format_on_save =
                cx.new(|cx| InputState::new(window, cx).placeholder("json, ts, go").default_value(typed));
            let format_subscription = cx.subscribe(&format_on_save, |_, input, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    let extensions = extensions(&input.read(cx).value());
                    Config::update(cx, |config| config.format_on_save = extensions);
                }
            });
            self._subscriptions.push(format_subscription);
            self.settings = Some(Settings {
                focus: cx.focus_handle(),
                search,
                format_on_save,
                scroll: ScrollHandle::new(),
                section: 0,
                recording: None,
                conflict: None,
                _subscription: subscription,
            });
        }
        if let Some(settings) = &self.settings {
            settings.search.update(cx, |search, cx| search.focus(window, cx));
        }
        self.refresh_repos(window, cx);
        cx.notify();
    }

    /// Opens settings at one of its sections.
    pub(super) fn open_settings_at(&mut self, section: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        self.go_to_section(section, window, cx);
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        self.host_picker = None;
        self.folder_picker = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Shows a section's page; a search in progress ends.
    fn go_to_section(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(settings) = &mut self.settings {
            settings.section = ix;
            settings.scroll.set_offset(point(px(0.), px(0.)));
            settings.search.update(cx, |search, cx| search.set_value("", window, cx));
            cx.notify();
        }
    }

    /// "Change": the next combination pressed (without doing what it already
    /// does) becomes this shortcut's. Esc cancels.
    fn record_shortcut(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let sik = cx.entity().downgrade();
        let interceptor = cx.intercept_keystrokes(move |event, _, cx| {
            let keystroke = &event.keystroke;
            // Only modifiers: not a combination yet.
            if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "cmd" | "ctrl" | "fn") {
                return;
            }
            cx.stop_propagation();
            let keystroke = keystroke.clone();
            sik.update(cx, |this, cx| this.recorded(id, keystroke, cx)).ok();
        });
        if let Some(settings) = &mut self.settings {
            settings.recording = Some((id, interceptor));
            settings.conflict = None;
        }
        cx.notify();
    }

    fn recorded(&mut self, id: &'static str, keystroke: Keystroke, cx: &mut Context<Self>) {
        let Some(settings) = &mut self.settings else {
            return;
        };
        settings.recording = None;
        let plain_escape = keystroke.key == "escape" && !keystroke.modifiers.modified();
        if !plain_escape {
            self.set_shortcut(id, Some(keystroke.unparse()), cx);
        }
        cx.notify();
    }

    /// Changes a shortcut's combination (`None`: no shortcut). If it belongs to
    /// another, changes nothing and asks.
    fn set_shortcut(&mut self, id: &'static str, keys: Option<String>, cx: &mut Context<Self>) {
        if let Some(keys) = &keys
            && let Ok(keystroke) = Keystroke::parse(keys)
            && let Some(other) = shortcuts::owner(&keystroke, cx).filter(|other| other.id != id)
        {
            if let Some(settings) = &mut self.settings {
                settings.conflict = Some(Conflict { id, keys: keys.clone(), other });
            }
            return cx.notify();
        }
        let default = shortcut(id).default;
        Config::update(cx, |config| {
            let same_as_default = keys.as_deref().and_then(|keys| Keystroke::parse(keys).ok())
                == Keystroke::parse(default).ok();
            if same_as_default {
                config.keys.remove(id);
            } else {
                config.keys.insert(id.to_string(), keys.unwrap_or_default());
            }
        });
        shortcuts::apply(cx);
        if let Some(settings) = &mut self.settings {
            settings.conflict = None;
        }
        cx.notify();
    }

    /// "Reassign It Here": the other shortcut is left without a combination and this one goes to the chosen one.
    fn resolve_conflict(&mut self, cx: &mut Context<Self>) {
        let Some(conflict) = self.settings.as_mut().and_then(|settings| settings.conflict.take()) else {
            return;
        };
        self.set_shortcut(conflict.other.id, None, cx);
        self.set_shortcut(conflict.id, Some(conflict.keys), cx);
    }

    pub(super) fn render_settings(&self, settings: &Settings, cx: &mut Context<Self>) -> impl IntoElement {
        let query = settings.search.read(cx).value().trim().to_lowercase();
        let matches = |text: &str| query.is_empty() || text.to_lowercase().contains(&query);

        let searching = !query.is_empty();
        let sections: Vec<(AnyElement, bool)> = vec![
            self.render_appearance(&matches, cx),
            self.render_editor(settings, &matches, cx),
            self.render_hosts(&matches, cx),
            self.render_workspaces(&matches, cx),
            self.render_updates(&matches, cx),
            self.render_shortcuts(settings, &matches, cx),
        ];
        let visible: Vec<bool> = sections.iter().map(|(_, visible)| *visible).collect();
        let nothing = !visible.iter().any(|visible| *visible);
        // A page per section; searching, every section with a match.
        let shown: Vec<AnyElement> = sections
            .into_iter()
            .enumerate()
            .filter(|(ix, (_, visible))| if searching { *visible } else { *ix == settings.section })
            .map(|(_, (section, _))| section)
            .collect();

        let theme = cx.theme();
        let index = v_flex()
            .w(px(200.))
            .flex_none()
            .pt_2()
            .pr_2()
            .gap_0p5()
            .border_r_1()
            .border_color(theme.border)
            .children(SECTIONS.iter().enumerate().map(|(ix, title)| {
                let selected = !searching && settings.section == ix;
                div()
                    .id(("settings-index", ix))
                    .px_3()
                    .py_1()
                    .rounded(theme.radius)
                    .when(selected, |el| el.font_semibold().text_color(theme.foreground))
                    .when(!selected, |el| el.text_color(theme.muted_foreground))
                    .when(searching && !visible[ix], |el| el.opacity(0.4))
                    .hover(|style| style.bg(theme.accent))
                    .child(*title)
                    .on_click(cx.listener(move |this, _, window, cx| this.go_to_section(ix, window, cx)))
            }));

        let content = v_flex()
            .id("settings-sections")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&settings.scroll)
            .pl_6()
            .pr_4()
            .pb_8()
            .children(shown)
            .when(nothing, |el| {
                el.child(div().pt_4().text_color(theme.muted_foreground).child("No settings match your search."))
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
                    .id("settings")
                    .track_focus(&settings.focus)
                    .w(relative(0.9))
                    .max_w(px(1100.))
                    .h(relative(0.85))
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .text_ui(cx)
                    // A click outside closes it, except in the pickers it opens.
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        if this.host_picker.is_none() && this.folder_picker.is_none() {
                            this.close_settings(window, cx);
                        }
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "escape" {
                            this.close_settings(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        h_flex()
                            .h(px(40.))
                            .flex_none()
                            .px_4()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(div().flex_1().font_semibold().child("Settings"))
                            .child(
                                icon_button("settings-close", "icons/tab-close.svg", cx)
                                    .on_click(cx.listener(|this, _, window, cx| this.close_settings(window, cx))),
                            ),
                    )
                    .child(div().px_4().py_3().flex_none().child(Input::new(&settings.search)))
                    .child(h_flex().items_start().flex_1().min_h_0().px_4().child(index).child(content)),
            )
    }

    /// Section title and its rows, and whether anything in it matches the search.
    fn section(title: &'static str, rows: Vec<AnyElement>, visible: bool, cx: &App) -> (AnyElement, bool) {
        let theme = cx.theme();
        let element = v_flex()
            .when(visible, |el| {
                el.pt_5()
                    .gap_1()
                    .child(div().pb_1().text_lg().font_semibold().text_color(theme.foreground).child(title))
                    .children(rows)
            })
            .into_any_element();
        (element, visible)
    }

    fn render_appearance(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let sizes = [
            (TextArea::Interface, "Interface Font Size", "File tree, tabs, lists and panels."),
            (TextArea::Editor, "Editor Font Size", "Code and diffs."),
            (TextArea::Preview, "Markdown Preview Font Size", "Rendered Markdown files."),
            (TextArea::Terminal, "Terminal Font Size", "Every terminal."),
        ];
        let visible = ["Appearance", "Theme", "System", "Light", "Dark", "text"]
            .iter()
            .chain(sizes.iter().map(|(_, title, _)| title))
            .any(|text| matches(text));
        let current = Config::get(cx).theme;
        let theme = cx.theme();
        let choices = h_flex().gap_2().children(
            [(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")]
                .into_iter()
                .map(|(choice, label)| {
                    let selected = current == choice;
                    div()
                        .id(label)
                        .px_3()
                        .py_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .when(selected, |el| el.bg(theme.accent).font_semibold())
                        .hover(|style| style.bg(theme.accent))
                        .child(label)
                        .on_click(cx.listener(move |this, _, window, cx| this.set_theme(choice, window, cx)))
                }),
        );
        let step = |id: String, label: &'static str, area: TextArea, size: f32| {
            div()
                .id(SharedString::from(id))
                .w(px(28.))
                .py_1()
                .flex()
                .justify_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.bg(theme.accent))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.set_font_size(area, Some(size), cx)))
        };
        let mut rows = vec![setting("Theme", "Light, dark, or match the system.", choices, cx)];
        for (ix, (area, title, description)) in sizes.into_iter().enumerate() {
            let size = Config::get(cx).font_size(area);
            let control = h_flex()
                .gap_2()
                .child(step(format!("font-smaller-{ix}"), "−", area, size - 1.))
                .child(div().w(px(48.)).flex().justify_center().child(format!("{size} px")))
                .child(step(format!("font-larger-{ix}"), "+", area, size + 1.))
                .when(size != area.default_size(), |el| {
                    el.child(
                        link(format!("font-reset-{ix}"), "↺", cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_font_size(area, None, cx))),
                    )
                });
            rows.push(setting(title, description, control, cx));
        }
        Self::section(SECTIONS[0], rows, visible, cx)
    }

    fn render_editor(&self, settings: &Settings, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let visible = [SECTIONS[1], "Auto Save", "autosave", "focus", "Format on Save", "Format Document", "json"]
            .iter().any(|text| matches(text));
        let input = div().max_w(px(480.)).child(Input::new(&settings.format_on_save));
        let auto_save = Switch::new("auto-save-on-focus-loss")
            .accessibility_label("Auto Save on Focus Loss")
            .checked(Config::get(cx).auto_save_on_focus_loss)
            .on_click(cx.listener(|_, checked, _, cx| {
                Config::update(cx, |config| config.auto_save_on_focus_loss = *checked);
                cx.notify();
            }));
        let rows = vec![setting(
            "Auto Save on Focus Loss",
            "Save changed files when switching editor tabs, moving focus to another panel, or leaving the window. Uses Format on Save when enabled for the file type.",
            auto_save,
            cx,
        ), setting(
            "Format on Save",
            "File types formatted when saved, separated by commas. Formatting (also Format Document, Shift-Opt-F) uses the repo's .sik/format if it has one, else the language server; JSON works without either.",
            input,
            cx,
        )];
        Self::section(SECTIONS[1], rows, visible, cx)
    }

    fn render_hosts(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let title_matches = matches(SECTIONS[2]) || matches("ssh server");
        let theme = cx.theme();
        let mut rows = Vec::new();
        let mut any = false;
        for host in self.hosts.iter().skip(1) {
            if !title_matches && !matches(&host.name) {
                continue;
            }
            any = true;
            let name = host.name.clone();
            let status = match &host.status {
                HostStatus::Connected => "connected",
                HostStatus::Connecting(step) => step,
                HostStatus::Failed(_) => "offline",
            };
            rows.push(
                list_row(name.to_string(), status, cx)
                    .child(link(format!("host-remove-{name}"), "remove", cx).on_click(cx.listener(
                        move |this, _, window, cx| this.remove_host(name.clone(), window, cx),
                    )))
                    .into_any_element(),
            );
        }
        if title_matches {
            rows.extend(self.host_input.as_ref().map(|input| {
                h_flex()
                    .pt_1()
                    .gap_1()
                    .max_w(px(480.))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(icon_button("pick-host", "icons/server.svg", cx).on_click(
                        cx.listener(|this, _, window, cx| this.open_host_picker(window, cx)),
                    ))
                    .into_any_element()
            }));
            rows.extend(
                self.error
                    .as_ref()
                    .filter(|(target, _)| target.is_none())
                    .map(|(_, error)| error_text(error.clone(), cx).into_any_element()),
            );
            rows.push(
                div()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child("A name from ~/.ssh/config or user@host. The icon looks them up in ~/.ssh/config.")
                    .into_any_element(),
            );
        }
        Self::section(SECTIONS[2], rows, title_matches || any, cx)
    }

    /// Each server's folders, its repos' worktrees under them: shown in the
    /// column or hidden, removed from it, and the field to add another.
    fn render_workspaces(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let title_matches = matches(SECTIONS[3]) || matches("repos") || matches("hidden") || matches("folders");
        let hidden = Config::get(cx).hidden.clone();
        let muted = cx.theme().muted_foreground;
        let mut rows = Vec::new();
        let mut any = false;
        for host in self.hosts.iter().filter(|host| host.client.is_some()) {
            let host_matches = title_matches || matches(&host.name);
            let folders: Vec<&PathBuf> = host
                .repos
                .iter()
                .filter(|folder| host_matches || matches(&folder.to_string_lossy()))
                .collect();
            if !host_matches && folders.is_empty() {
                continue;
            }
            any = true;
            rows.push(
                div()
                    .pt_2()
                    .text_ui_small(cx)
                    .font_semibold()
                    .text_color(muted)
                    .child(host.name.to_uppercase())
                    .into_any_element(),
            );
            for folder in folders {
                let key = TaskKey { host: host.name.clone(), path: folder.clone() };
                let (name, path) = (host.name.clone(), folder.clone());
                rows.push(
                    visibility(list_row(folder_name(folder), &folder.display().to_string(), cx), &key, &hidden, cx)
                        .child(link(format!("folder-remove-{}", key.config()), "remove", cx).on_click(
                            cx.listener(move |this, _, window, cx| this.remove_repo(name.clone(), path.clone(), window, cx)),
                        ))
                        .into_any_element(),
                );
                for worktree in host.tasks.iter().filter(|task| task.repo == *folder && !task.main) {
                    let key = TaskKey { host: host.name.clone(), path: worktree.path.clone() };
                    let row = list_row(folder_name(&worktree.path), &worktree.path.display().to_string(), cx).pl(px(28.));
                    rows.push(visibility(row, &key, &hidden, cx).into_any_element());
                }
            }
            let name = host.name.clone();
            rows.extend(host.repo_input.as_ref().map(|input| {
                h_flex()
                    .pt_1()
                    .gap_1()
                    .max_w(px(480.))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(icon_button(format!("pick-folder-{name}"), "icons/tree-folder.svg", cx).on_click(
                        cx.listener(move |this, _, window, cx| this.open_folder_picker(name.clone(), FolderPurpose::AddFolder, window, cx)),
                    ))
                    .into_any_element()
            }));
        }
        Self::section(SECTIONS[3], rows, title_matches || any, cx)
    }

    fn render_updates(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let visible = [SECTIONS[4], "Check for Updates", "automatically", "release", "version"].iter().any(|text| matches(text));
        let check = Switch::new("check-for-updates")
            .accessibility_label("Check for Updates Automatically")
            .checked(Config::get(cx).checks_for_updates())
            .on_click(cx.listener(|_, checked, _, cx| {
                Config::update(cx, |config| config.check_for_updates = Some(*checked));
                if *checked {
                    crate::update::check_now(cx);
                }
                cx.notify();
            }));
        let rows = vec![setting(
            "Check for Updates Automatically",
            "Every few hours, look for a new release and install it in the background. The title bar shows when it's ready; it restarts only when you choose, asking first. Off, Check for Updates in the menu still works.",
            check,
            cx,
        )];
        Self::section(SECTIONS[4], rows, visible, cx)
    }

    fn render_shortcuts(&self, settings: &Settings, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let title_matches = matches(SECTIONS[5]) || matches("keybindings");
        let recording = settings.recording.as_ref().map(|(id, _)| *id);
        let mut rows = Vec::new();
        for shortcut in SHORTCUTS {
            let keys = shortcuts::keys(shortcut, cx);
            let keys_text = keys.as_ref().map(Kbd::format).unwrap_or_default();
            if !title_matches && !matches(shortcut.label) && !matches(&keys_text) {
                continue;
            }
            rows.push(self.shortcut_row(shortcut, keys, recording, settings, cx));
        }
        let visible = title_matches || !rows.is_empty();
        Self::section(SECTIONS[5], rows, visible, cx)
    }

    fn shortcut_row(
        &self,
        shortcut: &'static Shortcut,
        keys: Option<Keystroke>,
        recording: Option<&'static str>,
        settings: &Settings,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = shortcut.id;
        let changed = shortcuts::changed(shortcut, cx);
        let keys_element = if recording == Some(id) {
            div().text_color(theme.primary).child("Press a key combination… (Esc to cancel)").into_any_element()
        } else {
            match keys {
                Some(keys) => Kbd::new(keys).into_any_element(),
                None => div().text_color(theme.muted_foreground).child("no shortcut").into_any_element(),
            }
        };
        let conflict = settings.conflict.as_ref().filter(|conflict| conflict.id == id).map(|conflict| {
            let keys = Keystroke::parse(&conflict.keys).map(|keys| Kbd::format(&keys)).unwrap_or_default();
            h_flex()
                .pl_3()
                .pb_1()
                .gap_2()
                .text_ui_small(cx)
                .child(div().text_color(theme.warning).child(format!("{keys} is already “{}”.", conflict.other.label)))
                .child(link(format!("conflict-yes-{id}"), "Reassign It Here", cx).on_click(
                    cx.listener(|this, _, _, cx| this.resolve_conflict(cx)),
                ))
                .child(link(format!("conflict-no-{id}"), "Cancel", cx).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(settings) = &mut this.settings {
                        settings.conflict = None;
                    }
                    cx.notify();
                })))
        });
        v_flex()
            .child(
                h_flex()
                    .id(SharedString::from(format!("shortcut-{id}")))
                    .group("shortcut")
                    .h(px(30.))
                    .px_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.accent.opacity(0.5)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(shortcut.label)
                            .when(changed, |el| el.font_semibold()),
                    )
                    .child(keys_element)
                    .child(
                        link(format!("shortcut-change-{id}"), "Change", cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.record_shortcut(id, cx))),
                    )
                    .child(
                        link(format!("shortcut-remove-{id}"), "Remove", cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.set_shortcut(id, None, cx))),
                    )
                    .when(changed, |el| {
                        el.child(
                            link(format!("shortcut-reset-{id}"), "↺", cx)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.set_shortcut(id, Some(shortcut.default.to_string()), cx)
                                })),
                        )
                    }),
            )
            .children(conflict)
            .into_any_element()
    }
}

/// A setting: bold name, description and its control below, as in VS Code.
fn setting(title: &'static str, description: &'static str, control: impl IntoElement, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .py_2()
        .gap_1()
        .child(div().font_semibold().child(title))
        .child(div().text_color(theme.muted_foreground).child(description))
        .child(div().pt_1().child(control))
        .into_any_element()
}

/// A list row (servers, repos, hidden tasks).
/// A workspace row's "hide" or "show": whether the column lists it.
fn visibility(row: Div, key: &TaskKey, hidden: &[String], cx: &mut Context<Sik>) -> Div {
    let (key, config) = (key.clone(), key.config());
    let shown = !hidden.contains(&config);
    let muted = cx.theme().muted_foreground;
    row.when(!shown, |row| row.text_color(muted)).child(
        link(format!("workspace-visibility-{config}"), if shown { "hide" } else { "show" }, cx).on_click(cx.listener(
            move |this, _, _, cx| {
                if shown {
                    this.hide_task(&key, cx)
                } else {
                    this.show_task(&config, cx)
                }
            },
        )),
    )
}

fn list_row(name: String, detail: &str, cx: &App) -> Div {
    let theme = cx.theme();
    h_flex()
        .h(px(28.))
        .px_3()
        .gap_3()
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.accent.opacity(0.5)))
        .child(div().flex_none().child(name))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
                .child(detail.to_string()),
        )
}

fn link(id: impl Into<SharedString>, label: &'static str, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .text_ui_small(cx)
        .text_color(theme.link)
        .hover(|style| style.underline())
        .child(label)
}

/// `"JSON, .ts ,go"` → `["json", "ts", "go"]`.
fn extensions(text: &str) -> Vec<String> {
    text.split([',', ' '])
        .map(|ext| ext.trim().trim_start_matches('.').to_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect()
}
