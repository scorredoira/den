use crate::input::InputModeKind;
use gpui::{Context, Window};
use regex::{Regex, RegexBuilder};
use ropey::Rope;
use std::{ops::Range, rc::Rc};

use super::{
    InputBaseState, Replace, RopeExt as _, Search, movement::MoveDirection, state::ScrollPadding,
};

/// How a query matches. (den) Whole words, regular expressions and the
/// case kept by a replacement, as VS Code's find widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchOptions {
    pub case_insensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Each replacement takes the case of what it replaces.
    pub preserve_case: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            case_insensitive: true,
            whole_word: false,
            regex: false,
            preserve_case: false,
        }
    }
}

/// Stateful, presentation-independent search engine used by text inputs.
#[derive(Debug, Clone)]
pub struct SearchMatcher {
    text: Rope,
    pub query: Option<Regex>,
    /// The query is not a valid regular expression.
    invalid: bool,
    options: SearchOptions,
    matched_ranges: Rc<Vec<Range<usize>>>,
    current_match_ix: usize,
    replacing: bool,
}

/// One search over an input: the query, how the built-in panel shows it, and
/// its matches. Read it through [`InputBaseState::search_session`]; it is
/// written only through the input state's search methods, and it grows, so
/// build it with `Default` and do not destructure it exhaustively.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchSession {
    /// The built-in search panel is showing.
    pub open: bool,
    pub replace_mode: bool,
    pub options: SearchOptions,
    pub query: String,
    pub replacement: String,
    pub anchor_offset: Option<usize>,
    pub matcher: SearchMatcher,
    /// A search is in progress and its matches are highlighted: the panel is
    /// open, or a query was set without it and not closed since.
    active: bool,
}

impl Default for SearchSession {
    fn default() -> Self {
        Self {
            open: false,
            active: false,
            replace_mode: false,
            options: SearchOptions::default(),
            query: String::new(),
            replacement: String::new(),
            anchor_offset: None,
            matcher: SearchMatcher::new(),
        }
    }
}

impl SearchSession {
    pub(crate) fn open(&mut self, replace_mode: bool, replaceable: bool) {
        self.open = true;
        self.active = true;
        self.replace_mode = replace_mode && replaceable;
    }

    /// Start a search without the built-in panel. A custom search UI drives
    /// the session through [`InputBaseState::set_search_query`], and the
    /// editor highlights the matches the same way it does for the panel.
    pub(crate) fn activate(&mut self) {
        self.active = true;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.active = false;
    }

    /// Whether a search is in progress: the built-in panel is open, or a
    /// query was set without it and [`InputBaseState::close_search`] has not
    /// run since. Matches are highlighted while this holds.
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn update_query(&mut self, query: impl Into<String>, options: SearchOptions) {
        let query = query.into();
        if self.query == query && self.options == options {
            return;
        }

        self.query = query;
        self.options = options;
        self.matcher.update_query(&self.query, options);
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Open the search session, or re-invoke it if it is already open.
    ///
    /// This is not idempotent: every call advances
    /// [`InputBaseState::search_activation_revision`], and the presentation
    /// layer answers that by re-focusing the search field and selecting its
    /// contents, the same as pressing the shortcut a second time. Call it from
    /// an action or another user gesture, never from a render pass or an
    /// observer that runs every frame — that would re-select the field under
    /// the user on every frame and make it impossible to type.
    pub fn open_search(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        if !self.searchable {
            return;
        }
        self.search_activation_revision = self.search_activation_revision.wrapping_add(1);
        self.search_session
            .open(replace_mode, self.is_replaceable());
        let selected = self.selected_text().to_string();
        let query = if selected.is_empty() {
            self.search_session.query.clone()
        } else {
            selected
        };
        let query_changed = query != self.search_session.query;
        // A retained query resumes its previous occurrence. Only a new query
        // is anchored to the current viewport.
        self.search_session.anchor_offset = if query_changed {
            self.last_layout
                .as_ref()
                .map(|layout| layout.visible_range_offset.start)
        } else {
            None
        };
        let options = self.search_session.options;
        self.search_session.update_query(query, options);
        self.search_session.matcher.update(&self.text);
        if query_changed && let Some(anchor) = self.search_session.anchor_offset {
            self.search_session.matcher.update_cursor_by_offset(anchor);
        }
        cx.notify();
    }

    pub fn search_session(&self) -> &SearchSession {
        &self.search_session
    }

    /// A counter that advances every time [`InputBaseState::open_search`] runs,
    /// including while the session is already open.
    ///
    /// Re-invoking search leaves the session itself identical, so a presentation
    /// layer that decides what to rebuild by comparing session state cannot see
    /// the second request. Fold this into that comparison to notice it.
    pub fn search_activation_revision(&self) -> u64 {
        self.search_activation_revision
    }

    #[doc(hidden)]
    pub fn set_search_replace_mode(&mut self, replace_mode: bool, cx: &mut Context<Self>) {
        self.search_session.replace_mode = replace_mode && self.is_replaceable();
        cx.notify();
    }

    /// Returns true if the search panel can replace the matches.
    ///
    /// This is false when the input is not `replaceable`, or when it is
    /// `disabled` or `readonly`.
    pub fn is_replaceable(&self) -> bool {
        self.replaceable && self.is_editable()
    }

    /// Set the search query and highlight its matches.
    ///
    /// This is the entry point for a custom search UI: it needs neither
    /// `searchable` nor the built-in panel. Navigate the matches with
    /// [`InputBaseState::next_search_match`] and
    /// [`InputBaseState::previous_search_match`], read the count and the
    /// current index from [`InputBaseState::search_session`], and end the
    /// search with [`InputBaseState::close_search`].
    pub fn set_search_query(
        &mut self,
        query: impl Into<String>,
        options: SearchOptions,
        cx: &mut Context<Self>,
    ) {
        self.search_session.activate();
        self.search_session.update_query(query, options);
        self.search_session.matcher.update(&self.text);
        cx.notify();
    }

    /// End the search: hide the built-in panel and the match highlights. The
    /// query is kept so the next [`InputBaseState::open_search`] resumes it.
    pub fn close_search(&mut self, cx: &mut Context<Self>) {
        self.search_session.close();
        cx.notify();
    }

    pub fn next_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.sync_search_matcher();
        let range = self.search_session.matcher.next()?;
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.end, None, ScrollPadding::SurroundingLines, cx);
        Some(range)
    }

    pub fn previous_search_match(&mut self, cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.sync_search_matcher();
        let range = self.search_session.matcher.next_back()?;
        // Match order does not describe viewport direction after a manual
        // scroll. Always allow search navigation to reveal the active match.
        self.scroll_to_with_padding(range.start, None, ScrollPadding::SurroundingLines, cx);
        Some(range)
    }

    /// Replace the current match and move on to the next one. Returns whether
    /// there was a match to replace.
    pub fn replace_current_search_match(
        &mut self,
        replacement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_replaceable() {
            return false;
        }
        self.sync_search_matcher();
        let matcher = &mut self.search_session.matcher;
        let Some(range) = matcher
            .matched_ranges()
            .get(matcher.current_match_index())
            .cloned()
        else {
            return false;
        };
        let replacement = matcher.replacement_at(&range, replacement);
        let next = matcher.peek().unwrap_or_else(|| range.clone());
        let direction = matcher
            .has_next_without_wrap()
            .then_some(MoveDirection::Down);
        if direction.is_none() {
            matcher.set_current_match_index(0);
        }
        matcher.begin_replacement();
        let range_utf16 = self.range_to_utf16(&range);
        self.scroll_to(next.end, direction, cx);
        self.replace_text_in_range_silent(Some(range_utf16), &replacement, window, cx);
        true
    }

    /// Replace every match. Returns how many were replaced.
    pub fn replace_all_search_matches(
        &mut self,
        replacement: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        if !self.is_replaceable() {
            return 0;
        }
        self.sync_search_matcher();
        let replacements = self.search_session.matcher.replacements(replacement);
        if replacements.is_empty() {
            return 0;
        }
        let mut text = self.text.clone();
        for (range, with) in replacements.iter().rev() {
            text.replace(range.clone(), with);
        }
        self.search_session.matcher.begin_replacement();
        let count = replacements.len();
        self.replace_text_in_range_silent(Some(0..self.text.len()), &text.to_string(), window, cx);
        self.scroll_to(0, Some(MoveDirection::Down), cx);
        count
    }

    /// Keep the matches in step with an edit. A closed search skips the scan:
    /// it copies and searches the whole document, and nothing reads the
    /// matches until the search is resumed or navigated, which sync first.
    pub(super) fn update_search(&mut self, _cx: &mut gpui::App) {
        if !self.search_session.is_active() {
            return;
        }
        self.sync_search_matcher();
    }

    /// Recompute the matches if the text changed since the last scan.
    fn sync_search_matcher(&mut self) {
        self.search_session.matcher.update(&self.text);
    }

    /// An input that is not `searchable` leaves the shortcut to its
    /// ancestors, so a custom search UI can take it.
    pub(super) fn on_action_search(&mut self, _: &Search, _: &mut Window, cx: &mut Context<Self>) {
        if !self.searchable {
            cx.propagate();
            return;
        }
        self.open_search(false, cx);
    }

    pub(super) fn on_action_replace(
        &mut self,
        _: &Replace,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.searchable {
            cx.propagate();
            return;
        }
        self.open_search(true, cx);
    }
}

impl Default for SearchMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchMatcher {
    pub fn new() -> Self {
        Self {
            text: "".into(),
            query: None,
            invalid: false,
            options: SearchOptions::default(),
            matched_ranges: Rc::new(Vec::new()),
            current_match_ix: 0,
            replacing: false,
        }
    }

    /// Update the source text and recompute matches.
    pub fn update(&mut self, text: &Rope) {
        if self.text.eq(text) {
            self.replacing = false;
            return;
        }
        self.text = text.clone();
        self.update_matches();
    }

    pub fn update_query(&mut self, query: &str, options: SearchOptions) {
        let built = (!query.is_empty()).then(|| build_query(query, options));
        self.invalid = matches!(built, Some(Err(_)));
        self.query = built.and_then(Result::ok);
        self.options = options;
        self.update_matches();
    }

    /// The query is not a valid regular expression, so nothing matches.
    pub fn is_invalid(&self) -> bool {
        self.invalid
    }

    /// What replaces the match at `range`: `$1`… expanded with a regular
    /// expression, in the case of the match with `preserve_case`.
    fn replacement_at(&self, range: &Range<usize>, replacement: &str) -> String {
        let Some(query) = self.query.as_ref().filter(|_| self.options.regex) else {
            return self.with_case(&self.text.slice(range.clone()).to_string(), replacement.into());
        };
        let text = self.text.to_string();
        match query.captures_at(&text, range.start) {
            Some(caps) if caps.get(0).is_some_and(|m| m.range() == *range) => {
                self.expand(&caps, replacement)
            }
            _ => self.with_case(&text[range.clone()], replacement.into()),
        }
    }

    /// Every match and what replaces it.
    fn replacements(&self, replacement: &str) -> Vec<(Range<usize>, String)> {
        let Some(query) = &self.query else {
            return Vec::new();
        };
        let text = self.text.to_string();
        query
            .captures_iter(&text)
            .filter_map(|caps| {
                let range = caps.get(0)?.range();
                (!range.is_empty()).then(|| (range, self.expand(&caps, replacement)))
            })
            .collect()
    }

    fn expand(&self, caps: &regex::Captures, replacement: &str) -> String {
        let mut with = String::new();
        if self.options.regex {
            caps.expand(replacement, &mut with);
        } else {
            with.push_str(replacement);
        }
        self.with_case(&caps[0], with)
    }

    fn with_case(&self, like: &str, with: String) -> String {
        if self.options.preserve_case {
            with_case_of(like, &with)
        } else {
            with
        }
    }

    pub fn matched_ranges(&self) -> Rc<Vec<Range<usize>>> {
        self.matched_ranges.clone()
    }

    pub fn current_match_index(&self) -> usize {
        self.current_match_ix
    }

    /// The index of the current match into [`SearchMatcher::matched_ranges`],
    /// `None` while there is no match.
    pub fn current(&self) -> Option<usize> {
        (!self.is_empty()).then_some(self.current_match_ix)
    }

    pub fn len(&self) -> usize {
        self.matched_ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matched_ranges.is_empty()
    }

    /// `2/5`: the current match and the total, `0/0` without matches.
    pub fn label(&self) -> String {
        match self.current() {
            Some(ix) => format!("{}/{}", ix + 1, self.len()),
            None => "0/0".into(),
        }
    }

    fn peek(&self) -> Option<Range<usize>> {
        self.next_index()
            .and_then(|ix| self.matched_ranges.get(ix).cloned())
    }

    fn has_next_without_wrap(&self) -> bool {
        self.current_match_ix < self.matched_ranges.len().saturating_sub(1)
    }

    pub fn update_cursor_by_offset(&mut self, offset: usize) {
        for (ix, range) in self.matched_ranges.iter().enumerate() {
            self.current_match_ix = ix;
            if range.contains(&offset) || range.end >= offset {
                return;
            }
        }
    }

    /// Preserve the current logical match while a replacement mutates text.
    fn begin_replacement(&mut self) {
        self.replacing = true;
    }

    fn set_current_match_index(&mut self, index: usize) {
        self.current_match_ix = index.min(self.matched_ranges.len().saturating_sub(1));
    }

    fn next_index(&self) -> Option<usize> {
        if self.is_empty() {
            None
        } else if self.has_next_without_wrap() {
            Some(self.current_match_ix + 1)
        } else {
            Some(0)
        }
    }

    fn update_matches(&mut self) {
        let mut ranges = Vec::new();
        if let Some(query) = &self.query {
            let text = self.text.to_string();
            ranges.extend(
                query
                    .find_iter(&text)
                    .map(|m| m.range())
                    .filter(|range| !range.is_empty()),
            );
        }
        self.matched_ranges = Rc::new(ranges);
        if !self.replacing || self.is_empty() {
            self.current_match_ix = 0;
        } else {
            self.current_match_ix = self.current_match_ix.min(self.len() - 1);
        }
        self.replacing = false;
    }
}

/// The query as a regular expression. A whole word is bounded only on the
/// sides that are word characters, so `.foo` still finds `x.foo`.
fn build_query(query: &str, options: SearchOptions) -> Result<Regex, regex::Error> {
    let mut pattern = if options.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    if options.whole_word {
        let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if options.regex {
            pattern = format!(r"\b(?:{pattern})\b");
        } else {
            if word(query.chars().next()) {
                pattern.insert_str(0, r"\b");
            }
            if word(query.chars().last()) {
                pattern.push_str(r"\b");
            }
        }
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(options.case_insensitive)
        .multi_line(true)
        .build()
}

/// `with`, in the case of `like`: all capitals, all lowercase, or with the
/// first letter capitalized or not (`payment` → `invoice`, `Payment` →
/// `Invoice`, `PAYMENT` → `INVOICE`).
fn with_case_of(like: &str, with: &str) -> String {
    let has_letters = like.chars().any(char::is_alphabetic);
    if has_letters && like.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase) && like.chars().count() > 1 {
        return with.to_uppercase();
    }
    if has_letters && like.chars().filter(|c| c.is_alphabetic()).all(char::is_lowercase) {
        return with.to_lowercase();
    }
    let mut chars = with.chars();
    match (like.chars().next(), chars.next()) {
        (Some(first), Some(head)) if first.is_uppercase() => head.to_uppercase().chain(chars).collect(),
        (Some(first), Some(head)) if first.is_lowercase() => head.to_lowercase().chain(chars).collect(),
        _ => with.to_string(),
    }
}

impl Iterator for SearchMatcher {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let ix = self.next_index()?;
        self.current_match_ix = ix;
        self.matched_ranges.get(ix).cloned()
    }
}

impl DoubleEndedIterator for SearchMatcher {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.is_empty() {
            return None;
        }
        if self.current_match_ix == 0 {
            self.current_match_ix = self.len();
        }
        self.current_match_ix -= 1;
        self.matched_ranges.get(self.current_match_ix).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(case_insensitive: bool) -> SearchOptions {
        SearchOptions {
            case_insensitive,
            ..SearchOptions::default()
        }
    }

    fn matches(text: &str, query: &str, options: SearchOptions) -> Vec<Range<usize>> {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from(text));
        matcher.update_query(query, options);
        matcher.matched_ranges().to_vec()
    }

    #[test]
    fn whole_words_are_bounded_only_on_their_word_sides() {
        let whole = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        assert_eq!(matches("save saved save_x save", "save", whole), [0..4, 18..22]);
        assert_eq!(matches("x.foo .foobar", ".foo", whole), [1..5]);
    }

    #[test]
    fn a_regex_matches_and_an_invalid_one_matches_nothing() {
        let regex = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };
        assert_eq!(matches("a1 b22 c", r"\d+", regex), [1..2, 4..6]);
        assert_eq!(matches("a\nb", "^b", regex), [2..3]);
        assert!(matches("aaa", "x*", regex).is_empty());

        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("a(b"));
        matcher.update_query("(", regex);
        assert!(matcher.is_invalid());
        assert!(matcher.is_empty());
        matcher.update_query("(", SearchOptions::default());
        assert!(!matcher.is_invalid());
        assert_eq!(matcher.len(), 1);
    }

    #[test]
    fn replacements_expand_groups_and_keep_the_case() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("let a = f(1); let b = f(22);"));
        matcher.update_query(r"f\((\d+)\)", SearchOptions {
            regex: true,
            ..SearchOptions::default()
        });
        let replaced: Vec<String> = matcher.replacements("g($1)").into_iter().map(|(_, with)| with).collect();
        assert_eq!(replaced, ["g(1)", "g(22)"]);
        assert_eq!(matcher.replacement_at(&(22..27), "[$1]"), "[22]");

        matcher.update(&Rope::from("payment Payment PAYMENT"));
        matcher.update_query("payment", SearchOptions {
            preserve_case: true,
            ..SearchOptions::default()
        });
        let replaced: Vec<String> = matcher.replacements("invoice").into_iter().map(|(_, with)| with).collect();
        assert_eq!(replaced, ["invoice", "Invoice", "INVOICE"]);
    }

    #[test]
    fn finds_navigates_and_preserves_replacement_position() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo FOO foo"));
        matcher.update_query("foo", case(true));
        assert_eq!(&*matcher.matched_ranges(), &[0..3, 4..7, 8..11]);
        assert_eq!(matcher.next(), Some(4..7));
        assert_eq!(matcher.next_back(), Some(0..3));

        matcher.set_current_match_index(2);
        matcher.begin_replacement();
        matcher.update(&Rope::from("foo FOO bar"));
        assert_eq!(matcher.current_match_index(), 1);
    }

    #[test]
    fn next_wraps_to_start() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from(".....aaaaa.....aaaaa.....aaaaa"));
        matcher.update_query("aaaaa", case(false));
        matcher.set_current_match_index(2);
        assert_eq!(matcher.next(), Some(5..10));
    }

    #[test]
    fn a_query_set_without_the_panel_keeps_the_session_active_until_closed() {
        let mut session = SearchSession::default();
        assert!(!session.is_active());

        session.open(false, true);
        assert!(session.is_active());
        session.close();
        assert!(!session.is_active());

        // A custom search UI never opens the panel; setting a query is what
        // turns the match highlights on, and closing turns them off again.
        session.activate();
        assert!(session.is_active());
        assert!(!session.open);
        session.close();
        assert!(!session.is_active());
    }

    #[test]
    fn identical_query_keeps_the_current_match() {
        let mut session = SearchSession::default();
        session.update_query("foo", case(true));
        session.matcher.update(&Rope::from("foo bar foo baz foo"));
        session.matcher.update_cursor_by_offset(12);
        assert_eq!(session.matcher.current_match_index(), 2);

        // Reopening Find and the styled search panel's initial query echo both
        // update the session with the same query. Neither should reset the
        // previously active occurrence.
        session.update_query("foo", case(true));

        assert_eq!(session.matcher.current_match_index(), 2);
        assert_eq!(session.matcher.label(), "3/3");
    }

    #[test]
    fn replacement_keeps_current_match_index_on_next_match() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo foo foo"));
        matcher.update_query("foo", case(true));
        assert_eq!(matcher.label(), "1/3");

        assert!(matcher.has_next_without_wrap());
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo foo"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/2");

        matcher.set_current_match_index(1);
        assert!(!matcher.has_next_without_wrap());
        matcher.set_current_match_index(0);
        matcher.begin_replacement();
        matcher.update(&Rope::from("bar foo bar"));
        assert_eq!(matcher.current_match_index(), 0);
        assert_eq!(matcher.matched_ranges()[0], 4..7);
        assert_eq!(matcher.label(), "1/1");
    }

    #[test]
    fn update_matches_clamps_current_match_index_while_replacing() {
        let mut matcher = SearchMatcher::new();
        matcher.update(&Rope::from("foo foo foo"));
        matcher.update_query("foo", case(true));
        matcher.set_current_match_index(2);
        matcher.begin_replacement();

        matcher.update(&Rope::from("foo xoo foo"));

        assert_eq!(matcher.len(), 2);
        assert_eq!(matcher.current_match_index(), 1);
        assert_eq!(matcher.label(), "2/2");
    }
}
