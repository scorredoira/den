use ropey::Rope;
use std::ops::Range;

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, DragMoveEvent, Empty, Entity, FocusHandle, Focusable, Global,
    Half, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, WeakEntity, Window,
    actions, div, prelude::FluentBuilder as _, px,
};

use crate::{
    ActiveTheme, Disableable, ElementExt, Icon, IconName, Selectable, Sizable,
    button::{Button, ButtonVariants},
    h_flex,
    input::{
        Enter, Escape, IndentInline, Input, InputBaseState, InputEvent, InputState, OutdentInline,
        Replace, SearchOptions,
    },
    label::Label,
    v_flex,
};

const CONTEXT: &'static str = "SearchPanel";

/// (den) The find bar floats at the editor's top right, as VS Code's.
const DEFAULT_WIDTH: Pixels = px(420.);
const MIN_WIDTH: Pixels = px(300.);
/// The queries remembered for ↑ and ↓.
const HISTORY: usize = 50;

actions!(input, [Tab]);

/// (den) What the find bar keeps across editors: its width as dragged, the
/// queries searched (the newest last) and its toggles. The app sets it on
/// start and observes it to save it.
#[derive(Clone, Default)]
pub struct FindBarMemory {
    pub width: Option<Pixels>,
    pub history: Vec<String>,
    pub options: SearchOptions,
}

impl Global for FindBarMemory {}

impl FindBarMemory {
    fn get(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }

    fn update(cx: &mut App, change: impl FnOnce(&mut Self)) {
        cx.update_default_global::<Self, _>(|memory, _| change(memory));
    }
}

/// What is dragged by the find bar's left edge.
#[derive(Clone)]
struct ResizeFindBar;

impl Render for ResizeFindBar {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}


#[cfg(test)]
use gpui_base::input::SearchMatcher;

#[derive(Clone, Copy)]
#[cfg(test)]
enum MoveDirection {
    Up,
    Down,
}

#[cfg(test)]
fn next_scroll_direction(
    previous_match_ix: usize,
    current_match_ix: usize,
) -> Option<MoveDirection> {
    if current_match_ix <= previous_match_ix {
        None
    } else {
        Some(MoveDirection::Down)
    }
}

#[cfg(test)]
fn prev_scroll_direction(
    previous_match_ix: usize,
    current_match_ix: usize,
) -> Option<MoveDirection> {
    if current_match_ix >= previous_match_ix {
        None
    } else {
        Some(MoveDirection::Up)
    }
}


pub(super) struct SearchPanel<M: crate::input::overlay::OverlayMode> {
    editor: WeakEntity<InputBaseState<M>>,
    search_input: Entity<InputState>,
    replace_input: Entity<InputState>,
    session: gpui_base::input::SearchSession,
    input_width: Pixels,

    _subscriptions: Vec<Subscription>,
}

impl<M: crate::input::overlay::OverlayMode> SearchPanel<M> {
    pub(super) fn replace_mode(&self) -> bool {
        self.session.replace_mode
    }

    pub(super) fn sync_session(&mut self, session: &gpui_base::input::SearchSession) {
        self.session = session.clone();
    }

    /// The query the panel's search input currently holds.
    pub(super) fn query(&self, cx: &App) -> gpui::SharedString {
        self.search_input.read(cx).value()
    }

    /// The panel's search input, so a test can assert what re-invoking search
    /// does to its value and selection.
    #[cfg(test)]
    pub(super) fn search_input(&self) -> &Entity<InputState> {
        &self.search_input
    }

    pub(crate) fn new(
        editor: Entity<InputBaseState<M>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find (↑↓ for history)"));
        let replace_input = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));

        cx.new(|cx| {
            let _subscriptions =
                vec![
                    cx.subscribe(&search_input, |this: &mut Self, _, ev: &InputEvent, cx| {
                        // Handle search input changes
                        match ev {
                            InputEvent::Change => {
                                this.update_search_query(None, cx);
                            }
                            _ => {}
                        }
                    }),
                ];

            Self {
                editor: editor.downgrade(),
                search_input,
                replace_input,
                session: gpui_base::input::SearchSession::default(),
                input_width: Pixels::ZERO,
                _subscriptions,
            }
        })
    }

    pub(super) fn show_with_focus(
        &mut self,
        selected_text: &Rope,
        replace_mode: bool,
        visible_range_offset: Option<Range<usize>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.open = true;
        self.session.replace_mode = replace_mode;
        if focus {
            self.search_input
                .update(cx, |input, cx| input.focus(window, cx));
        }

        self.search_input.update(cx, |this, cx| {
            if selected_text.len() > 0 {
                this.set_value(selected_text.to_string(), window, cx);
            }
            this.select_all(window, cx);
        });

        // The `set_value` does not emit `InputEvent::Change`, so update the query
        // here to match the value of the search input.
        self.update_search_query(visible_range_offset, cx);
    }

    /// Update the matcher by the value of the search input and the toggles.
    ///
    /// The `visible_range_offset` is to select the nearest match of the visible range,
    /// it is passed in, because the editor may be borrowed by the caller.
    fn update_search_query(
        &mut self,
        visible_range_offset: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        let query = self.search_input.read(cx).value();
        let options = FindBarMemory::get(cx).options;
        let editor = self.editor.clone();
        let _ = editor.update(cx, |state, cx| {
            state.set_search_query(query.clone(), options, cx);
        });
        if let Ok(session) = editor.read_with(cx, |state, _| state.search_session().clone()) {
            self.session = session;
        }
        if let Some(visible_range_offset) = visible_range_offset {
            self.session
                .matcher
                .update_cursor_by_offset(visible_range_offset.start);
        }
        cx.notify();
    }

    /// Flips one of the toggles, for every editor.
    fn toggle(&mut self, change: impl FnOnce(&mut SearchOptions), cx: &mut Context<Self>) {
        FindBarMemory::update(cx, |memory| change(&mut memory.options));
        self.update_search_query(None, cx);
    }

    /// The query searched goes last in the history.
    fn remember(&self, cx: &mut App) {
        let query = self.search_input.read(cx).value().to_string();
        if query.is_empty() {
            return;
        }
        FindBarMemory::update(cx, |memory| {
            memory.history.retain(|old| *old != query);
            memory.history.push(query);
            let extra = memory.history.len().saturating_sub(HISTORY);
            memory.history.drain(..extra);
        });
    }

    /// ↑ and ↓ in the search input: the query before or after in the
    /// history. What was typed and not searched yet is kept in it first.
    fn browse_history(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.search_input.read(cx).value().to_string();
        if !current.is_empty() && !FindBarMemory::get(cx).history.contains(&current) {
            self.remember(cx);
        }
        let history = FindBarMemory::get(cx).history;
        let at = history
            .iter()
            .rposition(|query| *query == current)
            .unwrap_or(history.len());
        let to = if back {
            at.checked_sub(1)
        } else {
            (at + 1 < history.len()).then_some(at + 1)
        };
        let Some(query) = to.and_then(|ix| history.get(ix)) else {
            return;
        };
        self.search_input.update(cx, |input, cx| {
            input.set_value(query.clone(), window, cx);
            input.select_all(window, cx);
        });
        self.update_search_query(None, cx);
    }

    fn replaceable(&self, cx: &App) -> bool {
        self.editor
            .read_with(cx, |editor, _| editor.is_replaceable())
            .unwrap_or(false)
    }

    pub(super) fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_with_focus(true, window, cx);
    }

    pub(super) fn hide_with_focus(
        &mut self,
        focus_editor: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember(cx);
        self.session.open = false;
        let _ = self.editor.update(cx, |state, cx| {
            state.close_search(cx);
            if focus_editor {
                state.focus(window, cx);
            }
        });
        cx.notify();
    }

    /// Enter goes to the next match (Shift-Enter, the previous one); in the
    /// replace input it replaces the current one.
    fn on_action_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.replace_mode && self.replace_input.read(cx).focus_handle(cx).is_focused(window) {
            self.replace_next(window, cx);
        } else if action.shift {
            self.prev(window, cx);
        } else {
            self.next(window, cx);
        }
    }

    fn on_action_escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.hide(window, cx);
    }

    fn on_action_tab(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        self.cycle_focus(window, cx);
    }

    fn on_action_tab_prev(
        &mut self,
        _: &OutdentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_focus(window, cx);
    }

    /// Cycle focus between the search and the replace input, to keep the Tab key
    /// staying in the panel.
    ///
    /// There are only 2 inputs, so the forward and the backward are the same.
    fn cycle_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.session.replace_mode || !self.replaceable(cx) {
            return;
        }

        let search_focus_handle = self.search_input.read(cx).focus_handle(cx);
        let focus_handle = if search_focus_handle.is_focused(window) {
            self.replace_input.read(cx).focus_handle(cx)
        } else {
            search_focus_handle
        };
        focus_handle.focus(window, cx);
    }

    fn on_action_replace(&mut self, _: &Replace, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_replace_mode(window, cx);
    }

    /// Toggle the replace field, and move focus to the field that is going to be used.
    fn toggle_replace_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            return;
        }

        self.session.replace_mode = !self.session.replace_mode;
        let replace_mode = self.session.replace_mode;
        let _ = self.editor.update(cx, |state, cx| {
            state.set_search_replace_mode(replace_mode, cx);
        });
        let focus_handle = if self.session.replace_mode {
            self.replace_input.read(cx).focus_handle(cx)
        } else {
            self.search_input.read(cx).focus_handle(cx)
        };
        focus_handle.focus(window, cx);
        cx.notify();
    }

    fn prev(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.remember(cx);
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.previous_search_match(cx);
        });
    }

    fn next(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.remember(cx);
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.next_search_match(cx);
        });
    }

    fn replace_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            self.session.replace_mode = false;
            cx.notify();
            return;
        }

        self.remember(cx);
        let replacement = self.replace_input.read(cx).value();
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.replace_current_search_match(&replacement, window, cx);
        });
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.replaceable(cx) {
            self.session.replace_mode = false;
            cx.notify();
            return;
        }

        self.remember(cx);
        let replacement = self.replace_input.read(cx).value();
        let _ = self.editor.update(cx, |state, cx| {
            _ = state.replace_all_search_matches(&replacement, window, cx);
        });
    }

    /// "3 of 12", "No results", or why the query matches nothing.
    fn status(&self) -> (SharedString, bool) {
        let matcher = &self.session.matcher;
        if matcher.is_invalid() {
            ("Invalid regex".into(), true)
        } else {
            match matcher.current() {
                Some(ix) => (format!("{} of {}", ix + 1, matcher.len()).into(), false),
                None => ("No results".into(), false),
            }
        }
    }
}

/// A toggle inside an input, VS Code's Aa, ab and .*.
fn toggle_button(
    id: &'static str,
    icon: impl Into<Icon>,
    tooltip: &'static str,
    on: bool,
) -> Button {
    Button::new(id)
        .xsmall()
        .compact()
        .ghost()
        .icon(icon)
        .tooltip(tooltip)
        .selected(on)
}

/// One of the den's own icons (`crates/ui/assets/icons`).
fn den_icon(name: &str) -> Icon {
    Icon::empty().path(format!("icons/{name}.svg"))
}

impl<M: crate::input::overlay::OverlayMode> Focusable for SearchPanel<M> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_input.read(cx).focus_handle(cx)
    }
}

impl<M: crate::input::overlay::OverlayMode> Render for SearchPanel<M> {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.session.open {
            return Empty.into_any_element();
        }

        let has_matches = !self.session.matcher.is_empty();
        let allow_replace = self.replaceable(cx);
        if !allow_replace {
            self.session.replace_mode = false;
        }
        let replace_mode = self.session.replace_mode;
        let options = FindBarMemory::get(cx).options;
        let width = FindBarMemory::get(cx).width.unwrap_or(DEFAULT_WIDTH);
        let (status, invalid) = self.status();

        h_flex()
            .id("search-panel")
            .occlude()
            .relative()
            .track_focus(&self.focus_handle(cx))
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::on_action_enter))
            .on_action(cx.listener(Self::on_action_escape))
            .on_action(cx.listener(Self::on_action_tab))
            .on_action(cx.listener(Self::on_action_tab_prev))
            .on_action(cx.listener(Self::on_action_replace))
            .on_drag_move(cx.listener(|_, event: &DragMoveEvent<ResizeFindBar>, _, cx| {
                let width = (event.bounds.right() - event.event.position.x).max(MIN_WIDTH);
                FindBarMemory::update(cx, |memory| memory.width = Some(width));
                cx.notify();
            }))
            .font_family(cx.theme().font_family.clone())
            .w(width)
            .min_w_0()
            .items_start()
            .gap_1()
            .py_1()
            .pl_1()
            .pr_1p5()
            .bg(cx.theme().tokens.popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius.half())
            .shadow_md()
            // The left edge sizes it.
            .child(
                div()
                    .id("search-panel-resize")
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(4.))
                    .cursor_col_resize()
                    .on_drag(ResizeFindBar, |drag, _, _, cx| cx.new(|_| drag.clone())),
            )
            .child(
                Button::new("replace-mode")
                    .xsmall()
                    .ghost()
                    .icon(if replace_mode {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .tooltip("Toggle Replace")
                    .disabled(!allow_replace)
                    .when(replace_mode, |this| this.h(px(52.)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_replace_mode(window, cx);
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .capture_key_down(cx.listener(
                                        |this, event: &KeyDownEvent, window, cx| {
                                            let keystroke = &event.keystroke;
                                            if keystroke.modifiers.modified() {
                                                return;
                                            }
                                            let back = match keystroke.key.as_str() {
                                                "up" => true,
                                                "down" => false,
                                                _ => return,
                                            };
                                            cx.stop_propagation();
                                            this.browse_history(back, window, cx);
                                        },
                                    ))
                                    .child(
                                        Input::new(&self.search_input)
                                            .focus_bordered(true)
                                            .when(invalid, |this| {
                                                this.border_color(cx.theme().danger)
                                            })
                                            .suffix(
                                                h_flex()
                                                    .gap_0p5()
                                                    .child(
                                                        toggle_button(
                                                            "case-sensitive",
                                                            IconName::CaseSensitive,
                                                            "Match Case",
                                                            !options.case_insensitive,
                                                        )
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.toggle(
                                                                |options| {
                                                                    options.case_insensitive =
                                                                        !options.case_insensitive
                                                                },
                                                                cx,
                                                            )
                                                        })),
                                                    )
                                                    .child(
                                                        toggle_button(
                                                            "whole-word",
                                                            den_icon("whole-word"),
                                                            "Match Whole Word",
                                                            options.whole_word,
                                                        )
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.toggle(
                                                                |options| {
                                                                    options.whole_word =
                                                                        !options.whole_word
                                                                },
                                                                cx,
                                                            )
                                                        })),
                                                    )
                                                    .child(
                                                        toggle_button(
                                                            "regex",
                                                            den_icon("regex"),
                                                            "Use Regular Expression",
                                                            options.regex,
                                                        )
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.toggle(
                                                                |options| options.regex = !options.regex,
                                                                cx,
                                                            )
                                                        })),
                                                    ),
                                            )
                                            .small()
                                            .w_full()
                                            .shadow_none(),
                                    )
                                    .on_prepaint({
                                        let view = cx.entity();
                                        move |bounds, _, cx| {
                                            view.update(cx, |r, _| {
                                                r.input_width = bounds.size.width
                                            })
                                        }
                                    }),
                            )
                            .child(
                                Label::new(status)
                                    .text_sm()
                                    .whitespace_nowrap()
                                    .when(invalid, |this| this.text_color(cx.theme().danger))
                                    .when(!has_matches && !invalid, |this| {
                                        this.text_color(cx.theme().muted_foreground)
                                    })
                                    .text_left()
                                    .min_w(px(72.)),
                            )
                            .child(
                                Button::new("prev")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::ArrowUp)
                                    .tooltip("Previous Match (⇧Enter)")
                                    .disabled(!has_matches)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.prev(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("next")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::ArrowDown)
                                    .tooltip("Next Match (Enter)")
                                    .disabled(!has_matches)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.next(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .tooltip("Close (Escape)")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_action_escape(&Escape, window, cx);
                                    })),
                            ),
                    )
                    .when(replace_mode, |this| {
                        this.child(
                            h_flex()
                                .w_full()
                                .gap_1()
                                .child(
                                    Input::new(&self.replace_input)
                                        .focus_bordered(true)
                                        .suffix(
                                            toggle_button(
                                                "preserve-case",
                                                den_icon("case-upper"),
                                                "Preserve Case",
                                                options.preserve_case,
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle(
                                                    |options| {
                                                        options.preserve_case =
                                                            !options.preserve_case
                                                    },
                                                    cx,
                                                )
                                            })),
                                        )
                                        .small()
                                        .w(self.input_width)
                                        .shadow_none(),
                                )
                                .child(
                                    Button::new("replace-one")
                                        .xsmall()
                                        .ghost()
                                        .icon(IconName::Replace)
                                        .tooltip("Replace (Enter)")
                                        .disabled(!has_matches)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.replace_next(window, cx);
                                        })),
                                )
                                .child(
                                    Button::new("replace-all")
                                        .xsmall()
                                        .ghost()
                                        .icon(den_icon("replace-all"))
                                        .tooltip("Replace All")
                                        .disabled(!has_matches)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.replace_all(window, cx);
                                        })),
                                ),
                        )
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn case(case_insensitive: bool) -> SearchOptions {
        SearchOptions {
            case_insensitive,
            ..SearchOptions::default()
        }
    }

    #[test]
    fn test_search() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));
        matcher.update_query("Is", case(true));

        assert_eq!(matcher.len(), 3);
        let mut matches = matcher.clone();
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next(), Some(18..20));
        assert_eq!(matches.next(), Some(23..25));
        assert_eq!(matches.current_match_index(), 2);
        assert_eq!(matches.next(), Some(15..17));
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next_back(), Some(23..25));
        assert_eq!(matches.current_match_index(), 2);
        assert_eq!(matches.next_back(), Some(18..20));
        assert_eq!(matches.current_match_index(), 1);
        assert_eq!(matches.next_back(), Some(15..17));
        assert_eq!(matches.current_match_index(), 0);
        assert_eq!(matches.next_back(), Some(23..25));

        matcher.update_query("IS", case(false));
        assert_eq!(matcher.len(), 0);
        assert_eq!(matcher.next(), None);
        assert_eq!(matcher.next_back(), None);
    }

    #[test]
    fn test_search_label() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("Hello 世界 this is a Is test string."));
        matcher.update_query("Is", case(true));
        assert_eq!(matcher.label(), "1/3");
        matcher.next();
        assert_eq!(matcher.label(), "2/3");
        matcher.next();
        assert_eq!(matcher.label(), "3/3");
        matcher.next();
        assert_eq!(matcher.label(), "1/3");

        matcher.update_query("IS", case(false));
        assert_eq!(matcher.label(), "0/0");
    }

    #[test]
    fn test_select_range_start() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from(".....aaaaa.....aaaaa.....aaaaa"));
        matcher.update_query("aaaaa", case(false));
        matcher.update_cursor_by_offset(0);
        assert_eq!(matcher.current_match_index(), 0);

        matcher.update_cursor_by_offset(5);
        assert_eq!(matcher.current_match_index(), 0);

        matcher.update_cursor_by_offset(12);
        assert_eq!(matcher.current_match_index(), 1);

        matcher.update_cursor_by_offset(16);
        assert_eq!(matcher.current_match_index(), 1);

        matcher.update_cursor_by_offset(30);
        assert_eq!(matcher.current_match_index(), 2);

        matcher.update_cursor_by_offset(31);
        assert_eq!(matcher.current_match_index(), 2);
    }

    #[test]
    fn test_next_scroll_direction_returns_down_without_wrap() {
        assert!(matches!(
            next_scroll_direction(0, 1),
            Some(MoveDirection::Down)
        ));
    }

    #[test]
    fn test_next_scroll_direction_returns_none_on_wrap() {
        assert!(next_scroll_direction(2, 0).is_none());
    }

    #[test]
    fn test_next_scroll_direction_returns_none_for_single_match() {
        assert!(next_scroll_direction(0, 0).is_none());
    }

    #[test]
    fn test_prev_scroll_direction_returns_up_without_wrap() {
        assert!(matches!(
            prev_scroll_direction(2, 1),
            Some(MoveDirection::Up)
        ));
    }

    #[test]
    fn test_prev_scroll_direction_returns_none_on_wrap() {
        assert!(prev_scroll_direction(0, 2).is_none());
    }

    #[test]
    fn test_prev_scroll_direction_returns_none_for_single_match() {
        assert!(prev_scroll_direction(0, 0).is_none());
    }
}
