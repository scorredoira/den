//! The find bar of a view that isn't an editor (a Markdown preview, a
//! commit): Cmd-F (Ctrl-F off the Mac) opens it over the view, as the
//! terminal's, with the editor's toggles (they are the same ones, kept in
//! `FindBarMemory`). Enter and Shift-Enter go to the next and the previous
//! match, Escape closes it.

use std::ops::Range;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, h_flex,
    button::{Button, ButtonVariants as _},
    input::{self, FindBarMemory, Input, InputState, SearchOptions},
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use regex::Regex;

/// Cap on matches: past it the count says so.
pub const MAX_MATCHES: usize = 10_000;

actions!(find_bar, [NextMatch, PreviousMatch, CloseFind]);

/// The shortcut that opens it: the editor's own Find, anywhere in a
/// workspace (a preview leaves the focus on it); an editor or a terminal,
/// with the focus, keeps its own. And the bar's keys: its query lets Enter
/// and Escape go on, and they are the bar's before the view's under it
/// (the Graph's Enter opens a commit, its Escape goes back).
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("FindBar");
    vec![
        KeyBinding::new("secondary-f", input::Search, Some("Workspace")),
        KeyBinding::new("enter", NextMatch, context),
        KeyBinding::new("shift-enter", PreviousMatch, context),
        KeyBinding::new("escape", CloseFind, context),
    ]
}

/// The find's toggles, as the editor's find bar left them.
pub fn options(cx: &App) -> SearchOptions {
    cx.try_global::<FindBarMemory>().map(|memory| memory.options).unwrap_or_default()
}

/// The pattern for `query` with the toggles: `None` finds nothing (it's
/// empty), `Err` is an invalid regular expression.
pub fn pattern(query: &str, options: &SearchOptions) -> Result<Option<Regex>, ()> {
    if query.is_empty() {
        return Ok(None);
    }
    let body = if options.regex { query.to_string() } else { regex::escape(query) };
    let body = if options.whole_word { format!(r"\b(?:{body})\b") } else { body };
    let flags = if options.case_insensitive { "(?i)" } else { "" };
    Regex::new(&format!("{flags}{body}")).map(Some).map_err(|_| ())
}

/// Where `pattern` matches in `text`, at most `limit` times.
pub fn find_in(text: &str, pattern: &Regex, limit: usize) -> impl Iterator<Item = Range<usize>> {
    pattern.find_iter(text).filter(|found| !found.is_empty()).take(limit).map(|found| found.range())
}

/// The bar's query, empty.
pub fn new_input(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder("Find"))
}

/// Puts the selection of one line in the query, selects it all and focuses it.
pub fn focus_input(input: &Entity<InputState>, selected: &str, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| {
        if !selected.is_empty() && !selected.contains('\n') {
            input.set_value(selected.to_string(), window, cx);
        }
        input.select_all(window, cx);
        input.focus(window, cx);
    });
}

/// The next of `count` matches after `active` (`forward`) or the one
/// before, going round at the ends.
pub fn step(active: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match active {
        None => 0,
        Some(ix) if forward => (ix + 1) % count,
        Some(ix) => (ix + count - 1) % count,
    })
}

/// The colors of the matches, as the editor's: the active one stronger.
pub fn colors(cx: &App) -> (Hsla, Hsla) {
    let selection = cx.theme().selection;
    (Hsla { s: 0.1, ..selection }, selection)
}

/// What the bar tells of the matches.
pub struct Status {
    pub invalid: bool,
    pub count: usize,
    pub active: Option<usize>,
}

/// What the bar does to the view it's over.
pub struct Handlers<T> {
    /// Forward (Enter, the down arrow) or back.
    pub step: fn(&mut T, bool, &mut Context<T>),
    pub close: fn(&mut T, &mut Window, &mut Context<T>),
    /// A toggle changed: find again.
    pub toggled: fn(&mut T, &mut Context<T>),
}

/// A click on a toggle: it changes, and the view finds again.
fn toggling<T: 'static>(
    change: fn(&mut SearchOptions),
    toggled: fn(&mut T, &mut Context<T>),
    cx: &Context<T>,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |this: &mut T, _: &ClickEvent, _, cx| {
        cx.update_default_global::<FindBarMemory, _>(|memory, _| change(&mut memory.options));
        toggled(this, cx);
    })
}

/// The bar, at the top right of the view it's over (which is `relative`).
pub fn render<T: 'static>(input: &Entity<InputState>, status: Status, handlers: Handlers<T>, cx: &mut Context<T>) -> AnyElement {
    let options = options(cx);
    let Handlers { step, close, toggled } = handlers;
    let (case, word, regex) = (
        toggling(|options| options.case_insensitive = !options.case_insensitive, toggled, cx),
        toggling(|options| options.whole_word = !options.whole_word, toggled, cx),
        toggling(|options| options.regex = !options.regex, toggled, cx),
    );
    let theme = cx.theme();
    let has_matches = status.count > 0;
    let label: SharedString = if status.invalid {
        "Invalid regex".into()
    } else {
        match status.active {
            Some(ix) if status.count >= MAX_MATCHES => format!("{} of {}+", ix + 1, MAX_MATCHES).into(),
            Some(ix) => format!("{} of {}", ix + 1, status.count).into(),
            None => "No results".into(),
        }
    };
    let toggle = |id: &'static str, icon: Icon, tooltip: &'static str, on: bool| {
        Button::new(id).xsmall().compact().ghost().icon(icon).tooltip(tooltip).selected(on)
    };
    let own_icon = |name: &str| Icon::empty().path(format!("icons/{name}.svg"));
    h_flex()
        .id("find-bar")
        .debug_selector(|| "find-bar".into())
        .occlude()
        .absolute()
        .top_1()
        .right_3()
        .w(px(400.))
        // Wide enough for the input's focus ring, drawn outside it.
        .gap_2()
        .p_1()
        .bg(theme.tokens.popover)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .shadow_md()
        .font_family(theme.font_family.clone())
        .text_size(rems(0.875))
        .key_context("FindBar")
        .on_action(cx.listener(move |this, _: &NextMatch, _, cx| step(this, true, cx)))
        .on_action(cx.listener(move |this, _: &PreviousMatch, _, cx| step(this, false, cx)))
        .on_action(cx.listener(move |this, _: &CloseFind, window, cx| close(this, window, cx)))
        .child(
            div().flex_1().min_w_0().child(
                Input::new(input)
                    .small()
                    .w_full()
                    .shadow_none()
                    .focus_bordered(true)
                    .when(status.invalid, |input| input.border_color(theme.danger))
                    .suffix(
                        h_flex()
                            .gap_0p5()
                            .child(toggle("case-sensitive", IconName::CaseSensitive.into(), "Match Case", !options.case_insensitive).on_click(case))
                            .child(toggle("whole-word", own_icon("whole-word"), "Match Whole Word", options.whole_word).on_click(word))
                            .child(toggle("regex", own_icon("regex"), "Use Regular Expression", options.regex).on_click(regex)),
                    ),
            ),
        )
        .child(
            div()
                .text_sm()
                .whitespace_nowrap()
                .min_w(px(72.))
                .when(status.invalid, |el| el.text_color(theme.danger))
                .when(!has_matches && !status.invalid, |el| el.text_color(theme.muted_foreground))
                .child(label),
        )
        .child(
            Button::new("find-previous")
                .xsmall()
                .ghost()
                .icon(IconName::ArrowUp)
                .tooltip("Previous Match (⇧Enter)")
                .disabled(!has_matches)
                .on_click(cx.listener(move |this, _, _, cx| step(this, false, cx))),
        )
        .child(
            Button::new("find-next")
                .xsmall()
                .ghost()
                .icon(IconName::ArrowDown)
                .tooltip("Next Match (Enter)")
                .disabled(!has_matches)
                .on_click(cx.listener(move |this, _, _, cx| step(this, true, cx))),
        )
        .child(
            Button::new("find-close")
                .xsmall()
                .ghost()
                .icon(IconName::Close)
                .tooltip("Close (Escape)")
                .on_click(cx.listener(move |this, _, window, cx| close(this, window, cx))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{SearchOptions, find_in, pattern, step};

    fn found(text: &str, query: &str, case_insensitive: bool, whole_word: bool, regex: bool) -> Result<Vec<std::ops::Range<usize>>, ()> {
        let options = SearchOptions { case_insensitive, whole_word, regex, ..SearchOptions::default() };
        Ok(pattern(query, &options)?.map(|pattern| find_in(text, &pattern, 100).collect()).unwrap_or_default())
    }

    #[test]
    fn finds_with_the_toggles() {
        let text = "Cat cat category c.t";
        assert_eq!(found(text, "cat", true, false, false), Ok(vec![0..3, 4..7, 8..11]));
        assert_eq!(found(text, "cat", false, false, false), Ok(vec![4..7, 8..11]));
        assert_eq!(found(text, "cat", true, true, false), Ok(vec![0..3, 4..7]));
        assert_eq!(found(text, "c.t", true, false, false), Ok(std::iter::once(17..20).collect()));
        assert_eq!(found(text, "c.t", false, false, true), Ok(vec![4..7, 8..11, 17..20]));
        assert_eq!(found(text, "(", false, false, true), Err(()));
        assert_eq!(found(text, "", false, false, false), Ok(vec![]));
    }

    #[test]
    fn steps_go_round() {
        assert_eq!(step(None, 0, true), None);
        assert_eq!(step(None, 3, false), Some(0));
        assert_eq!(step(Some(2), 3, true), Some(0));
        assert_eq!(step(Some(0), 3, false), Some(2));
    }
}
