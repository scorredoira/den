//! Fuzzy-filtering picker (nucleo, the one Helix uses) over a list of
//! strings: Cmd-P (the task's files), Cmd-K (tasks) and Cmd-Shift-P (commands).

use std::{collections::HashMap, sync::Arc};

use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::config::UiText;

/// Results shown.
const SHOWN: usize = 60;

pub enum PickerEvent {
    Pick(String),
    /// Esc: focus returns to where it was.
    Dismiss,
    /// Click outside: focus stays on what was clicked.
    Close,
}

pub struct Picker {
    input: Entity<InputState>,
    /// Whether the strings are paths: the name goes first and the folder in gray.
    paths: bool,
    files: Arc<Vec<String>>,
    /// Text on the right of a row (a command's shortcut), by string.
    hints: HashMap<String, String>,
    matches: Vec<String>,
    selected: usize,
    filter: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<PickerEvent> for Picker {}

impl Picker {
    pub fn new(
        files: Arc<Vec<String>>,
        placeholder: &'static str,
        paths: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.refilter(cx),
            InputEvent::PressEnter { .. } => this.confirm(cx),
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        let mut finder = Self {
            input,
            paths,
            files,
            hints: HashMap::new(),
            matches: Vec::new(),
            selected: 0,
            filter: None,
            _subscription: subscription,
        };
        finder.refilter(cx);
        finder
    }

    pub fn with_hints(mut self, hints: HashMap<String, String>) -> Self {
        self.hints = hints;
        self
    }

    /// The list arrived (or was refreshed) while the picker was open.
    pub fn set_files(&mut self, files: Arc<Vec<String>>, cx: &mut Context<Self>) {
        self.files = files;
        self.refilter(cx);
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let files = self.files.clone();
        let filter = cx.background_spawn(async move { filter(&files, &query) });
        self.filter = Some(cx.spawn(async move |this, cx| {
            let matches = filter.await;
            this.update(cx, |this, cx| {
                this.matches = matches;
                this.selected = 0;
                cx.notify();
            })
            .ok();
        }));
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(file) = self.matches.get(self.selected) {
            cx.emit(PickerEvent::Pick(file.clone()));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        cx.notify();
    }
}

/// The files that best match `query`, best first. With nothing typed, the
/// first ones in the list.
fn filter(files: &[String], query: &str) -> Vec<String> {
    if query.trim().is_empty() {
        return files.iter().take(SHOWN).cloned().collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, &String)> = files
        .iter()
        .filter_map(|file| {
            pattern
                .score(Utf32Str::new(file, &mut buf), &mut matcher)
                .map(|score| (score, file))
        })
        .collect();
    // On equal scores, shorter paths first.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    scored.into_iter().take(SHOWN).map(|(_, file)| file.clone()).collect()
}

impl Render for Picker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("picker")
            .w(px(560.))
            .max_h(px(420.))
            .p_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_ui(cx)
            // A click outside closes it, like Esc. Not on focus loss: a click
            // on a result also takes focus away from the field.
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Close)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                "up" => {
                    this.move_selection(-1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    this.move_selection(1, cx);
                    cx.stop_propagation();
                }
                "escape" => {
                    cx.emit(PickerEvent::Dismiss);
                    cx.stop_propagation();
                }
                _ => {}
            }))
            .child(Input::new(&self.input))
            .child(
                v_flex()
                    .id("picker-results")
                    .overflow_y_scroll()
                    .children(self.matches.iter().enumerate().map(|(ix, file)| {
                        let (name, dir) = match file.rsplit_once('/').filter(|_| self.paths) {
                            Some((dir, name)) => (name.to_string(), dir.to_string()),
                            None => (file.clone(), String::new()),
                        };
                        let hint = self.hints.get(file).cloned();
                        let file = file.clone();
                        h_flex()
                            .id(("picker-row", ix))
                            .h(px(26.))
                            .px_2()
                            .gap_2()
                            .rounded(theme.radius)
                            .when(ix == self.selected, |el| el.bg(theme.accent))
                            .hover(|style| style.bg(theme.accent.opacity(0.6)))
                            .child(div().flex_none().child(name))
                            .child(
                                div()
                                    .text_ui_small(cx)
                                    .text_color(theme.muted_foreground)
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(dir),
                            )
                            .children(hint.map(|hint| {
                                div().ml_auto().flex_none().text_ui_small(cx).text_color(theme.muted_foreground).child(hint)
                            }))
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PickerEvent::Pick(file.clone()))))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::filter;

    #[test]
    fn fuzzy_finds_by_path_fragments() {
        let files: Vec<String> = ["crates/ui/src/main.rs", "crates/agent/src/main.rs", "plan.md", "crates/ui/src/workspace.rs"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(filter(&files, "uimain")[0], "crates/ui/src/main.rs");
        assert_eq!(filter(&files, "wsp")[0], "crates/ui/src/workspace.rs");
        assert_eq!(filter(&files, "zzz"), Vec::<String>::new());
        assert_eq!(filter(&files, "").len(), 4);
    }
}
