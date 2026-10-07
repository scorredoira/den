//! Finding text in a terminal, its history included: Cmd-F (Ctrl-Alt-F off
//! the Mac, where Ctrl-Shift-F is the Search panel). The matching is
//! alacritty's own regex search, which follows rows the terminal wrapped.

use alacritty_terminal::{
    Term,
    grid::Dimensions as _,
    index::{Column, Direction, Point as AlacPoint},
    term::search::{Match, RegexIter, RegexSearch},
};
use gpui_kit::component::input::SearchOptions;

/// Cap on matches: past it the count says so.
pub(crate) const MAX_MATCHES: usize = 10_000;

/// The pattern for what the find bar holds, with its toggles. Empty, or an
/// invalid regular expression, finds nothing (`Err` says it's invalid).
pub(crate) fn regex(query: &str, options: &SearchOptions) -> Result<Option<RegexSearch>, ()> {
    if query.is_empty() {
        return Ok(None);
    }
    let body = if options.regex { query.to_string() } else { escape(query) };
    // The lazy DFA has no Unicode word boundary, only the ASCII one.
    let body = if options.whole_word { format!(r"(?-u:\b)(?:{body})(?-u:\b)") } else { body };
    let flags = if options.case_insensitive { "(?i)" } else { "(?-i)" };
    RegexSearch::new(&format!("{flags}{body}")).map(Some).map_err(|_| ())
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Every match from the oldest line of the history to the last of the screen.
pub(crate) fn find_all<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let start = AlacPoint::new(term.topmost_line(), Column(0));
    let end = AlacPoint::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, regex).take(MAX_MATCHES).collect()
}

/// Where a match starts counted from the top of the history, which new
/// output doesn't move (lines are counted from the screen's top, which does).
pub(crate) fn anchor<T>(term: &Term<T>, point: AlacPoint) -> (i32, usize) {
    (point.line.0 + term.history_size() as i32, point.column.0)
}

/// The match that was active before the matches were found again: the one
/// starting where it did, else the first after it, else the last.
pub(crate) fn reanchor<T>(term: &Term<T>, matches: &[Match], anchor: (i32, usize)) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let after = matches.partition_point(|found| self::anchor(term, *found.start()) < anchor);
    Some(after.min(matches.len() - 1))
}

/// The match to start from: the last one that begins on a line in view or
/// above it, the closest to what was printed last; else the first.
pub(crate) fn nearest<T>(term: &Term<T>, matches: &[Match]) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let bottom = term.screen_lines() as i32 - 1 - term.grid().display_offset() as i32;
    let below = matches.partition_point(|found| found.start().line.0 <= bottom);
    Some(below.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::{
        event::VoidListener,
        index::Line,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::*;

    fn term(text: &str) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &TermSize::new(10, 3), VoidListener);
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, text.as_bytes());
        term
    }

    fn options(case_insensitive: bool, whole_word: bool, regex: bool) -> SearchOptions {
        SearchOptions { case_insensitive, whole_word, regex, ..SearchOptions::default() }
    }

    fn starts(term: &Term<VoidListener>, query: &str, options: &SearchOptions) -> Vec<(i32, usize)> {
        let mut regex = regex(query, options).unwrap().unwrap();
        find_all(term, &mut regex).iter().map(|found| (found.start().line.0, found.start().column.0)).collect()
    }

    #[test]
    fn finds_in_the_history_and_across_wrapped_rows() {
        // Ten columns: "error here" fills a row and the second "error" wraps.
        let term = term("error one\r\nok\r\nok\r\nmore errors\r\n$ ");
        let plain = options(true, false, false);
        assert_eq!(starts(&term, "error", &plain), vec![(-3, 0), (0, 5)]);
        assert_eq!(starts(&term, "errors", &plain), vec![(0, 5)]);
    }

    #[test]
    fn toggles_change_what_matches() {
        let term = term("Foo foo\r\nfood f.o\r\n");
        assert_eq!(starts(&term, "foo", &options(true, false, false)).len(), 3);
        assert_eq!(starts(&term, "foo", &options(false, false, false)).len(), 2);
        assert_eq!(starts(&term, "Foo", &options(true, false, false)).len(), 3);
        assert_eq!(starts(&term, "foo", &options(true, true, false)).len(), 2);
        assert_eq!(starts(&term, "f.o", &options(true, false, false)), vec![(Line(1).0, 5)]);
        assert_eq!(starts(&term, "f.o", &options(true, false, true)).len(), 4);
    }

    #[test]
    fn an_invalid_regex_is_an_error_and_empty_finds_nothing() {
        assert!(regex("(", &options(true, false, true)).is_err());
        assert!(regex("(", &options(true, false, false)).unwrap().is_some());
        assert!(regex("", &options(true, false, true)).unwrap().is_none());
    }

    #[test]
    fn the_active_match_stays_while_output_scrolls_it_up() {
        let mut term = term("a x\r\nb x\r\nc x\r\n");
        let plain = options(true, false, false);
        let mut regex = regex("x", &plain).unwrap().unwrap();
        let matches = find_all(&term, &mut regex);
        let active = anchor(&term, *matches[1].start());
        Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, b"d\r\ne\r\n".as_slice());
        let matches = find_all(&term, &mut regex);
        let again = reanchor(&term, &matches, active).unwrap();
        assert_eq!(anchor(&term, *matches[again].start()), active);
    }
}
