//! Editing as in VS Code, on top of the editor's selections: Cmd-D, moving
//! and duplicating lines, and the occurrences of the word under the cursor.
//! Selections are `(anchor, cursor)` byte offsets, the active one first; the
//! functions here only compute, the workspace applies the result.

use std::ops::Range;

use gpui_base::input;
use gpui_kit::{KeyBinding, actions};

actions!(
    editing,
    [SelectNextOccurrence, MoveLineUp, MoveLineDown, DuplicateLineUp, DuplicateLineDown]
);

/// They only apply in a code tab, so Cmd-D still splits the terminal when
/// the focus is there. GPUI ranks a binding without context as the deepest
/// one: to win over the app's shortcuts (Cmd-D, Cmd-Opt-↑/↓), these are
/// in the deepest context too (the editor's `Input`) and are registered
/// after them (see `shortcuts::apply`).
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("CodeEditor > Input");
    vec![
        KeyBinding::new("secondary-d", SelectNextOccurrence, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-up", input::AddCursorAbove, context),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-alt-down", input::AddCursorBelow, context),
        KeyBinding::new("alt-up", MoveLineUp, context),
        KeyBinding::new("alt-down", MoveLineDown, context),
        KeyBinding::new("shift-alt-up", DuplicateLineUp, context),
        KeyBinding::new("shift-alt-down", DuplicateLineDown, context),
    ]
}

pub type Selection = (usize, usize);

/// Text edits (ranges of the current text) and the selections afterwards.
pub struct Edit {
    pub edits: Vec<(Range<usize>, String)>,
    pub selections: Vec<Selection>,
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// The word (letters, digits and `_`) at `offset`, or ending right there.
pub fn word_at(text: &str, offset: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let offset = offset.min(bytes.len());
    let mut start = offset;
    while start > 0 && is_word(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = offset;
    while end < bytes.len() && is_word(bytes[end]) {
        end += 1;
    }
    (start < end).then_some(start..end)
}

/// Whether `range` is a whole word: word characters bounded by others.
fn is_whole_word(text: &str, range: &Range<usize>) -> bool {
    let bytes = text.as_bytes();
    !range.is_empty()
        && bytes[range.clone()].iter().all(|&byte| is_word(byte))
        && (range.start == 0 || !is_word(bytes[range.start - 1]))
        && (range.end == bytes.len() || !is_word(bytes[range.end]))
}

/// Where `needle` appears in `text`, in order.
fn find_all(text: &str, needle: &str, case_insensitive: bool, whole_word: bool) -> Vec<Range<usize>> {
    let (hay, needle) = (text.as_bytes(), needle.as_bytes());
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut at = 0;
    while at + needle.len() <= hay.len() {
        let candidate = &hay[at..at + needle.len()];
        let equal = if case_insensitive { candidate.eq_ignore_ascii_case(needle) } else { candidate == needle };
        if equal && text.is_char_boundary(at) && text.is_char_boundary(at + needle.len()) {
            let range = at..at + needle.len();
            if !whole_word || is_whole_word(text, &range) {
                found.push(range);
                at += needle.len();
                continue;
            }
        }
        at += 1;
    }
    found
}

fn range_of((anchor, cursor): Selection) -> Range<usize> {
    anchor.min(cursor)..anchor.max(cursor)
}

/// Cmd-D: with a cursor, selects its word; with a selection, adds the next
/// occurrence of the active one (after it, wrapping around) as the new active
/// selection. A whole word only matches whole words. `None` if nothing is
/// left to add.
pub fn select_next_occurrence(text: &str, selections: &[Selection], case_insensitive: bool) -> Option<Vec<Selection>> {
    let active = range_of(*selections.first()?);
    if active.is_empty() {
        let word = word_at(text, active.start)?;
        let mut selections = selections.to_vec();
        selections[0] = (word.start, word.end);
        return Some(selections);
    }
    let needle = &text[active.clone()];
    let matches = find_all(text, needle, case_insensitive, is_whole_word(text, &active));
    let taken = |range: &Range<usize>| {
        selections
            .iter()
            .any(|&selection| {
                let other = range_of(selection);
                other.start < range.end && range.start < other.end
            })
    };
    let next = matches
        .iter()
        .filter(|range| range.start >= active.end)
        .chain(matches.iter().filter(|range| range.start < active.end))
        .find(|range| !taken(range))?;
    let mut all = vec![(next.start, next.end)];
    all.extend_from_slice(selections);
    Some(all)
}

/// Byte offset where each line starts, plus the text's length at the end.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(ix, _)| ix + 1));
    starts
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&start| start <= offset) - 1
}

/// The lines each selection covers, merged when they touch: `(first, last)`.
/// A selection that ends at the start of a line doesn't take that line.
fn blocks(starts: &[usize], selections: &[Selection]) -> Vec<(usize, usize)> {
    let mut blocks: Vec<(usize, usize)> = selections
        .iter()
        .map(|&selection| {
            let range = range_of(selection);
            let first = line_of(starts, range.start);
            let mut last = line_of(starts, range.end);
            if last > first && starts[last] == range.end {
                last -= 1;
            }
            (first, last)
        })
        .collect();
    blocks.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (first, last) in blocks {
        match merged.last_mut() {
            Some(prev) if first <= prev.1 + 1 => prev.1 = prev.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

/// The end of `line`, before its `\n`.
fn line_end(text: &str, starts: &[usize], line: usize) -> usize {
    starts.get(line + 1).map_or(text.len(), |next| next - 1)
}

/// Opt-↑/↓: the selected lines swap with the one above or below. `None` at
/// the top or bottom of the file.
pub fn move_lines(text: &str, selections: &[Selection], up: bool) -> Option<Edit> {
    let starts = line_starts(text);
    let lines = starts.len();
    let blocks = blocks(&starts, selections);
    if up && blocks.first()?.0 == 0 || !up && blocks.last()?.1 + 1 >= lines {
        return None;
    }
    let mut edits = Vec::new();
    // Each selection moves by the length of the line it swaps with.
    let mut shifts: Vec<(usize, usize, isize)> = Vec::new();
    for &(first, last) in &blocks {
        let block = starts[first]..line_end(text, &starts, last);
        if up {
            let above = starts[first - 1]..line_end(text, &starts, first - 1);
            edits.push((above.start..block.end, format!("{}\n{}", &text[block.clone()], &text[above.clone()])));
            shifts.push((block.start, block.end, -((above.len() + 1) as isize)));
        } else {
            let below = starts[last + 1]..line_end(text, &starts, last + 1);
            edits.push((block.start..below.end, format!("{}\n{}", &text[below.clone()], &text[block.clone()])));
            shifts.push((block.start, block.end, (below.len() + 1) as isize));
        }
    }
    let moved = |offset: usize, near: usize| -> usize {
        let shift = shifts
            .iter()
            .find(|(start, end, _)| (*start..=*end).contains(&near))
            .map_or(0, |(_, _, shift)| *shift);
        (offset as isize + shift) as usize
    };
    let selections = selections
        .iter()
        .map(|&(anchor, cursor)| {
            let near = anchor.min(cursor);
            (moved(anchor, near), moved(cursor, near))
        })
        .collect();
    Some(Edit { edits, selections })
}

/// Shift-Opt-↑/↓: copies the selected lines below themselves. Down, the
/// selections go to the copy; up, they stay on the original, which is now
/// the upper one.
pub fn duplicate_lines(text: &str, selections: &[Selection], up: bool) -> Edit {
    let starts = line_starts(text);
    let blocks = blocks(&starts, selections);
    let mut edits = Vec::new();
    // Bytes inserted before each block, and the size of its copy.
    let mut inserted: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut before = 0;
    for &(first, last) in &blocks {
        let block = starts[first]..line_end(text, &starts, last);
        edits.push((block.end..block.end, format!("\n{}", &text[block.clone()])));
        inserted.push((block.start, block.end, before, block.len() + 1));
        before += block.len() + 1;
    }
    let moved = |offset: usize, near: usize| -> usize {
        let (_, _, before, copy) = inserted
            .iter()
            .copied()
            .find(|(start, end, _, _)| (*start..=*end).contains(&near))
            .unwrap_or_default();
        offset + before + if up { 0 } else { copy }
    };
    let selections = selections
        .iter()
        .map(|&(anchor, cursor)| {
            let near = anchor.min(cursor);
            (moved(anchor, near), moved(cursor, near))
        })
        .collect();
    Edit { edits, selections }
}

/// Occurrences of the word under a cursor (same word, same case), for
/// highlighting them; none if it appears only once or there are too many.
pub fn occurrences(text: &str, selections: &[Selection]) -> Vec<Range<usize>> {
    const MAX: usize = 2_000;
    let [(anchor, cursor)] = selections else {
        return Vec::new();
    };
    let word = if anchor == cursor {
        word_at(text, *cursor)
    } else {
        let range = range_of((*anchor, *cursor));
        is_whole_word(text, &range).then_some(range)
    };
    let Some(word) = word else {
        return Vec::new();
    };
    let found = find_all(text, &text[word], false, true);
    if found.len() < 2 || found.len() > MAX {
        return Vec::new();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies the edits (disjoint, in any order) as the editor would.
    fn apply(text: &str, edit: &Edit) -> String {
        let mut edits = edit.edits.clone();
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        let mut text = text.to_string();
        for (range, with) in edits {
            text.replace_range(range, &with);
        }
        text
    }

    #[test]
    fn cmd_d_selects_the_word_then_each_next_one() {
        let text = "pay payment pay Pay pay";
        let first = select_next_occurrence(text, &[(1, 1)], true).unwrap();
        assert_eq!(first, vec![(0, 3)]);
        // Whole word: `payment` doesn't count; case-insensitive: `Pay` does.
        let second = select_next_occurrence(text, &first, true).unwrap();
        assert_eq!(second, vec![(12, 15), (0, 3)]);
        let third = select_next_occurrence(text, &second, true).unwrap();
        assert_eq!(third[0], (16, 19));
        let sensitive = select_next_occurrence(text, &second, false).unwrap();
        assert_eq!(sensitive[0], (20, 23));
        // After the last one it wraps around, and when all are taken, nothing.
        let all = select_next_occurrence(text, &[(20, 23), (16, 19), (12, 15)], true).unwrap();
        assert_eq!(all[0], (0, 3));
        assert!(select_next_occurrence(text, &all, true).is_none());
        // Not a whole word: matches inside words too.
        let part = select_next_occurrence(text, &[(0, 2)], false).unwrap();
        assert_eq!(part[0], (4, 6));
    }

    #[test]
    fn moves_lines_up_and_down() {
        let text = "a\nbb\nccc\nd";
        let down = move_lines(text, &[(3, 3)], false).unwrap();
        assert_eq!(apply(text, &down), "a\nccc\nbb\nd");
        assert_eq!(down.selections, vec![(7, 7)]);
        let up = move_lines(text, &[(6, 8)], true).unwrap();
        assert_eq!(apply(text, &up), "a\nccc\nbb\nd");
        assert_eq!(up.selections, vec![(3, 5)]);
        // The last line has no `\n`; moving it up keeps the text's length.
        let last = move_lines(text, &[(10, 10)], true).unwrap();
        assert_eq!(apply(text, &last), "a\nbb\nd\nccc");
        assert!(move_lines(text, &[(0, 0)], true).is_none());
        assert!(move_lines(text, &[(10, 10)], false).is_none());
        // Two lines selected, ending at the start of a third that stays.
        let block = move_lines(text, &[(0, 5)], false).unwrap();
        assert_eq!(apply(text, &block), "ccc\na\nbb\nd");
        assert_eq!(block.selections, vec![(4, 9)]);
    }

    #[test]
    fn duplicates_lines() {
        let text = "a\nbb\nc";
        let down = duplicate_lines(text, &[(3, 3)], false);
        assert_eq!(apply(text, &down), "a\nbb\nbb\nc");
        assert_eq!(down.selections, vec![(6, 6)]);
        let up = duplicate_lines(text, &[(3, 3)], true);
        assert_eq!(apply(text, &up), "a\nbb\nbb\nc");
        assert_eq!(up.selections, vec![(3, 3)]);
        // Two cursors on different lines: each copy shifts the ones after it.
        let two = duplicate_lines(text, &[(0, 0), (5, 5)], false);
        assert_eq!(apply(text, &two), "a\na\nbb\nc\nc");
        assert_eq!(two.selections, vec![(2, 2), (9, 9)]);
    }

    #[test]
    fn occurrences_of_the_word() {
        let text = "let payment = payment + paymentId + Payment";
        assert_eq!(occurrences(text, &[(5, 5)]), vec![4..11, 14..21]);
        assert!(occurrences(text, &[(38, 38)]).is_empty());
        assert!(occurrences(text, &[(3, 3), (5, 5)]).is_empty());
        assert_eq!(occurrences(text, &[(4, 11)]), vec![4..11, 14..21]);
    }
}
