//! Fuzzy-filtering picker (nucleo, the one Helix uses) over a list of
//! strings: Cmd-P (the task's files), Cmd-K (tasks) and Cmd-Shift-P (commands).
//! Without a list, it asks for a line of text (Ctrl-G, the line number).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

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
    /// No list: Enter picks what was typed.
    free_text: bool,
    /// What's typed is offered too, after the matches, unless it's one of them.
    typed: bool,
    /// The list's order is kept, also when filtering (the Command Palette:
    /// the recent ones, then the rest by name), and these, at its start,
    /// are labeled "recently used".
    ordered: Option<HashSet<String>>,
    matches: Vec<String>,
    /// The query `matches` are for: Enter right after typing may come before
    /// the filtering in the background is done.
    matched: Option<String>,
    selected: usize,
    scroll: ScrollHandle,
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
            free_text: false,
            typed: false,
            ordered: None,
            matches: Vec::new(),
            matched: None,
            selected: 0,
            scroll: ScrollHandle::new(),
            filter: None,
            _subscription: subscription,
        };
        finder.refilter(cx);
        finder
    }

    /// Asks for a line of text: Enter picks what was typed.
    pub fn free_text(placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self { free_text: true, ..Self::new(Arc::new(Vec::new()), placeholder, false, window, cx) }
    }

    /// The list, or anything typed (Add Server: a `user@host` not in the list).
    pub fn typed(mut self) -> Self {
        self.typed = true;
        self
    }

    /// Keeps the list's order, `recent` (at its start) labeled as recently used.
    pub fn ordered(mut self, recent: HashSet<String>) -> Self {
        self.ordered = Some(recent);
        self.refilter_now();
        self
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

    /// Before it's shown (`ordered` changes how): nothing is typed yet.
    fn refilter_now(&mut self) {
        self.filter = None;
        self.matches = matches(&self.files, "", self.typed, self.ordered.is_some());
        self.matched = Some(String::new());
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let (files, typed, ordered) = (self.files.clone(), self.typed, self.ordered.is_some());
        self.filter = Some(cx.spawn(async move |this, cx| {
            let matches = cx.background_spawn({
                let query = query.clone();
                async move { matches(&files, &query, typed, ordered) }
            });
            let matches = matches.await;
            this.update(cx, |this, cx| this.set_matches(matches, query, cx)).ok();
        }));
    }

    fn set_matches(&mut self, matches: Vec<String>, query: String, cx: &mut Context<Self>) {
        self.matches = matches;
        self.matched = Some(query);
        self.selected = 0;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        if self.free_text {
            cx.emit(PickerEvent::Pick(query));
            return;
        }
        // Typed faster than it filters: what's picked is for what's typed.
        if self.matched.as_ref() != Some(&query) {
            self.filter = None;
            let matches = matches(&self.files, &query, self.typed, self.ordered.is_some());
            self.set_matches(matches, query, cx);
        }
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
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }
}

/// What's offered for `query`: the best matches (or, `ordered`, all that
/// match in the list's order) and, if `typed`, what's typed after them
/// (unless it's one of them).
fn matches(files: &[String], query: &str, typed: bool, ordered: bool) -> Vec<String> {
    let mut matches = if ordered { filter_ordered(files, query) } else { filter(files, query) };
    let query = query.trim();
    if typed && !query.is_empty() && !matches.iter().any(|file| file == query) {
        matches.push(query.to_string());
    }
    matches
}

/// Those that match `query`, in the list's order.
fn filter_ordered(files: &[String], query: &str) -> Vec<String> {
    if query.trim().is_empty() {
        return files.to_vec();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    files.iter().filter(|file| pattern.score(Utf32Str::new(file, &mut buf), &mut matcher).is_some()).cloned().collect()
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
                    .track_scroll(&self.scroll)
                    .children(self.matches.iter().enumerate().map(|(ix, file)| {
                        // As in VS Code: the group on its first row, a line
                        // over the other commands when recent ones are above.
                        let recent = self.ordered.as_ref().map(|recent| recent.contains(file));
                        let previous = ix.checked_sub(1).and_then(|ix| self.matches.get(ix));
                        let previous_recent = previous.and_then(|file| Some(self.ordered.as_ref()?.contains(file)));
                        let separated = recent == Some(false) && previous_recent == Some(true);
                        let group = match (recent, previous_recent) {
                            (Some(true), None) => Some("recently used"),
                            _ if separated => Some("other commands"),
                            _ => None,
                        };
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
                            .when(separated, |el| el.border_t_1().border_color(theme.border))
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
                            .child(
                                h_flex()
                                    .ml_auto()
                                    .flex_none()
                                    .gap_3()
                                    .text_ui_small(cx)
                                    .text_color(theme.muted_foreground)
                                    .children(group)
                                    .children(hint),
                            )
                            .on_click(cx.listener(move |_, _, _, cx| cx.emit(PickerEvent::Pick(file.clone()))))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    use gpui_kit::*;

    use super::{Picker, PickerEvent, filter, matches};

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

    #[test]
    fn what_is_typed_is_offered_after_the_matches() {
        let hosts: Vec<String> = ["ws", "bill"].into_iter().map(String::from).collect();
        assert_eq!(matches(&hosts, "me@box ", true, false), ["me@box"]);
        assert_eq!(matches(&hosts, "ws", true, false), ["ws"]);
        assert_eq!(matches(&hosts, "me@box", false, false), Vec::<String>::new());
    }

    /// The Command Palette keeps its order (the recent ones, then by name)
    /// when filtering, as VS Code does.
    #[test]
    fn ordered_keeps_the_list_order() {
        let commands: Vec<String> = ["Toggle Terminal", "Go to File", "Format Document", "Go to Line"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(matches(&commands, "go", false, true), ["Go to File", "Go to Line"]);
        assert_eq!(matches(&commands, "", false, true), commands);
    }

    /// Enter right after typing, before the background filtering is done,
    /// picks from what's typed, not from the previous query's matches.
    #[gpui_kit::test]
    fn enter_right_after_typing_picks_what_was_typed(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::config::Config::default());
        });
        let hosts = Arc::new(vec!["ws".to_string(), "bill".to_string()]);
        let (picker, cx) = cx.add_window_view(|window, cx| Picker::new(hosts, "Server", false, window, cx).typed());
        cx.run_until_parked();
        let picked = Rc::new(RefCell::new(Vec::new()));
        let sink = picked.clone();
        cx.update(|window, cx| {
            cx.subscribe(&picker, move |_, event: &PickerEvent, _| {
                if let PickerEvent::Pick(choice) = event {
                    sink.borrow_mut().push(choice.clone());
                }
            })
            .detach();
            picker.update(cx, |picker, cx| {
                picker.input.update(cx, |input, cx| input.set_value("me@box", window, cx));
                picker.refilter(cx);
                picker.confirm(cx);
            });
        });
        assert_eq!(*picked.borrow(), ["me@box"]);
    }
}
