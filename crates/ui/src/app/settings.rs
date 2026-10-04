//! Settings, in a modal (Cmd-, or the gear): they're app-wide, not per task.
//! At the top a search box that searches everything; on the left an index of
//! sections; on the right the chosen section, as its own page (while
//! searching, every section with a match).

use gpui_kit::component::{input::InputEvent, kbd::Kbd, switch::Switch};

use super::*;
use crate::shortcuts::{self, SHORTCUTS, Shortcut};

/// Sections, in index order.
const SECTIONS: [&str; 5] = ["Appearance", "Editor", "Workspaces", "Updates", "Keyboard Shortcuts"];

/// What − and + change the width under which diffs go to one column.
const DIFF_WIDTH_STEP: f32 = 100.;

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
    /// The search's and the extensions' (they go when Settings closes).
    _subscriptions: [Subscription; 2],
}

struct Conflict {
    id: &'static str,
    keys: String,
    other: &'static Shortcut,
}

fn shortcut(id: &str) -> &'static Shortcut {
    SHORTCUTS.iter().find(|shortcut| shortcut.id == id).expect("shortcut")
}

impl Den {
    /// Opens settings (or focuses them if already open).
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.confirm_remove = None;
        self.confirm_force_remove = None;
        self.error = None;
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
            self.settings = Some(Settings {
                focus: cx.focus_handle(),
                search,
                format_on_save,
                scroll: ScrollHandle::new(),
                section: 0,
                recording: None,
                conflict: None,
                _subscriptions: [subscription, format_subscription],
            });
        }
        if let Some(settings) = &self.settings {
            settings.search.update(cx, |search, cx| search.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
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
        let den = cx.entity().downgrade();
        let interceptor = cx.intercept_keystrokes(move |event, _, cx| {
            let keystroke = &event.keystroke;
            // Only modifiers: not a combination yet.
            if matches!(keystroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function" | "cmd" | "ctrl" | "fn") {
                return;
            }
            cx.stop_propagation();
            let keystroke = keystroke.clone();
            den.update(cx, |this, cx| this.recorded(id, keystroke, cx)).ok();
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
                    // A click outside closes it.
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_settings(window, cx)))
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
        let choices = h_flex().gap_2().children(
            [(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")]
                .into_iter()
                .map(|(choice, label)| {
                    chip(label, label, current == choice, cx)
                        .on_click(cx.listener(move |this, _, window, cx| this.set_theme(choice, window, cx)))
                }),
        );
        let step = |id: String, label: &'static str, area: TextArea, size: f32| {
            step(id, label, cx).on_click(cx.listener(move |this, _, _, cx| this.set_font_size(area, Some(size), cx)))
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
        let visible = [SECTIONS[1], "Auto Save", "autosave", "focus", "Format on Save", "Format Document", "json", "Diff Layout", "Side by Side", "One Column", "One-Column Width", "Automatic"]
            .iter().any(|text| matches(text));
        let layout = Config::get(cx).diff_layout;
        let layouts = h_flex().gap_2().children(
            [(DiffLayout::Automatic, "Automatic"), (DiffLayout::SideBySide, "Side by Side"), (DiffLayout::OneColumn, "One Column")]
                .into_iter()
                .map(|(choice, label)| {
                    chip(label, label, layout == choice, cx)
                        .on_click(cx.listener(move |_, _, _, cx| crate::workspace::set_diff_layout(choice, cx)))
                }),
        );
        let width = Config::get(cx).side_by_side_width();
        let set_width = |width: Option<f32>| {
            cx.listener(move |_, _, _, cx| {
                Config::update(cx, |config| config.set_side_by_side_width(width));
                cx.refresh_windows();
            })
        };
        let one_column_width = h_flex()
            .gap_2()
            .child(step("diff-width-smaller".into(), "−", cx).on_click(set_width(Some(width - DIFF_WIDTH_STEP))))
            .child(div().w(px(64.)).flex().justify_center().child(format!("{width} px")))
            .child(step("diff-width-larger".into(), "+", cx).on_click(set_width(Some(width + DIFF_WIDTH_STEP))))
            .when(width != config::DEFAULT_SIDE_BY_SIDE_WIDTH, |el| {
                el.child(link("diff-width-reset", "↺", cx).on_click(set_width(None)))
            });
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
            "File types formatted when saved, separated by commas. Formatting (also Format Document, Shift-Opt-F) uses the repo's .den/format if it has one, else the language server; JSON works without either.",
            input,
            cx,
        ), setting(
            "Diff Layout",
            "Diffs side by side, in one column, or Automatic: side by side when there's room for two. Also in a diff's right-click menu.",
            layouts,
            cx,
        ), setting(
            "One-Column Width",
            "Narrower than this, an Automatic diff shows in one column.",
            one_column_width,
            cx,
        )];
        Self::section(SECTIONS[1], rows, visible, cx)
    }

    fn render_workspaces(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let visible = [SECTIONS[2], "Only My Worktrees", "worktree", "agent"].iter().any(|text| matches(text));
        let only = Switch::new("only-own-worktrees")
            .accessibility_label("Only My Worktrees")
            .checked(Config::get(cx).only_own_worktrees)
            .on_click(cx.listener(|_, checked, _, cx| {
                Config::update(cx, |config| config.only_own_worktrees = *checked);
                cx.notify();
            }));
        let rows = vec![
            setting(
                "Only My Worktrees",
                "The workspaces column shows only the worktrees made with New Worktree, not those the agents make on their own. One in front, or with an agent running in it, always shows.",
                only,
                cx,
            ),
        ];
        Self::section(SECTIONS[2], rows, visible, cx)
    }

    fn render_updates(&self, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let visible = [SECTIONS[3], "Check for Updates", "automatically", "release", "version"].iter().any(|text| matches(text));
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
        Self::section(SECTIONS[3], rows, visible, cx)
    }

    fn render_shortcuts(&self, settings: &Settings, matches: &dyn Fn(&str) -> bool, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let title_matches = matches(SECTIONS[4]) || matches("keybindings");
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
        Self::section(SECTIONS[4], rows, visible, cx)
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

/// One of a row of choices, highlighted if it's the current one.
fn chip(id: &'static str, label: &'static str, selected: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .when(selected, |el| el.bg(theme.accent).font_semibold())
        .hover(|style| style.bg(theme.accent))
        .child(label)
}

/// The − or + beside a number.
fn step(id: String, label: &'static str, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
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
