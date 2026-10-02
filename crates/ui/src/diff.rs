//! Side-by-side diffs: a unified diff with the whole file as context, split
//! into its two sides and aligned line by line, with a gap on one side where
//! only the other has lines. Too narrow for two sides, one: `inline`.

use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Same,
    /// Removed on the old side, added on the new one.
    Changed,
    /// Only the other side has this line.
    Gap,
}

#[derive(Debug, PartialEq)]
pub struct Line {
    /// In the file; `None` for a gap.
    pub number: Option<u32>,
    pub kind: Kind,
    /// Bytes of `Side::text` that changed within the line, when it pairs
    /// with a line on the other side.
    pub changed: Option<Range<usize>>,
}

#[derive(Debug, Default)]
pub struct Side {
    pub text: String,
    pub lines: Vec<Line>,
}

impl Side {
    fn push(&mut self, number: Option<u32>, kind: Kind, text: &str) -> usize {
        if !self.lines.is_empty() {
            self.text.push('\n');
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.lines.push(Line { number, kind, changed: None });
        start
    }
}

#[derive(Debug, Default)]
pub struct SideBySide {
    pub old: Side,
    pub new: Side,
    /// Row where each block of changes starts.
    pub changes: Vec<usize>,
}

/// The two sides of `diff` (one file's), or `None` without hunks: no
/// changes, or a binary file.
pub fn split(diff: &str) -> Option<SideBySide> {
    let mut result = SideBySide::default();
    let mut lines = diff.lines();
    let mut hunks = false;
    while let Some(line) = lines.next() {
        let Some((mut old_number, mut old_left, mut new_number, mut new_left)) = hunk_header(line) else {
            continue;
        };
        hunks = true;
        let (mut removed, mut added): (Vec<&str>, Vec<&str>) = (Vec::new(), Vec::new());
        while old_left + new_left > 0 {
            let Some(line) = lines.next() else {
                break;
            };
            let line = line.strip_suffix('\r').unwrap_or(line);
            match line.as_bytes().first() {
                Some(b'-') if old_left > 0 => {
                    removed.push(&line[1..]);
                    old_left -= 1;
                }
                Some(b'+') if new_left > 0 => {
                    added.push(&line[1..]);
                    new_left -= 1;
                }
                // "\ No newline at end of file"
                Some(b'\\') => {}
                _ => {
                    flush(&mut result, &mut removed, &mut added, &mut old_number, &mut new_number);
                    let text = line.get(1..).unwrap_or("");
                    result.old.push(Some(old_number), Kind::Same, text);
                    result.new.push(Some(new_number), Kind::Same, text);
                    old_number += 1;
                    new_number += 1;
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
        }
        flush(&mut result, &mut removed, &mut added, &mut old_number, &mut new_number);
    }
    hunks.then_some(result)
}

/// A diff in one column, as when there's no room for two: each block of
/// changes, its removed lines and then its added ones.
#[derive(Debug, Default)]
pub struct Inline {
    pub text: String,
    pub lines: Vec<InlineLine>,
    /// Row where each block of changes starts.
    pub changes: Vec<usize>,
}

#[derive(Debug, PartialEq)]
pub struct InlineLine {
    /// Its number on each side: only the old for a removed line, only the
    /// new for an added one.
    pub old: Option<u32>,
    pub new: Option<u32>,
    /// Bytes of `Inline::text` that changed within the line.
    pub changed: Option<Range<usize>>,
}

impl Inline {
    fn push(&mut self, old: Option<u32>, new: Option<u32>, text: &str, changed: Option<Range<usize>>) {
        if !self.lines.is_empty() {
            self.text.push('\n');
        }
        let start = self.text.len();
        self.text.push_str(text);
        let changed = changed.map(|range| start + range.start..start + range.end);
        self.lines.push(InlineLine { old, new, changed });
    }
}

/// `sides` in one column.
pub fn inline(sides: &SideBySide) -> Inline {
    // Each line's text, and what changed in it from its start.
    let rows = |side: &Side| -> Vec<(String, Option<Range<usize>>)> {
        let mut start = 0;
        side.text
            .split('\n')
            .zip(&side.lines)
            .map(|(text, line)| {
                let changed = line.changed.clone().map(|range| range.start - start..range.end - start);
                start += text.len() + 1;
                (text.to_string(), changed)
            })
            .collect()
    };
    let (old_rows, new_rows) = (rows(&sides.old), rows(&sides.new));
    let mut result = Inline::default();
    let mut row = 0;
    while row < sides.old.lines.len() {
        let (old, new) = (&sides.old.lines[row], &sides.new.lines[row]);
        if old.kind == Kind::Same {
            result.push(old.number, new.number, &new_rows[row].0, None);
            row += 1;
            continue;
        }
        let end = (row..sides.old.lines.len()).find(|&end| sides.old.lines[end].kind == Kind::Same).unwrap_or(sides.old.lines.len());
        result.changes.push(result.lines.len());
        for ix in row..end {
            let line = &sides.old.lines[ix];
            if line.kind == Kind::Changed {
                result.push(line.number, None, &old_rows[ix].0, old_rows[ix].1.clone());
            }
        }
        for ix in row..end {
            let line = &sides.new.lines[ix];
            if line.kind == Kind::Changed {
                result.push(None, line.number, &new_rows[ix].0, new_rows[ix].1.clone());
            }
        }
        row = end;
    }
    result
}

/// `@@ -a,b +c,d @@`: the first line and the line count of each side.
fn hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = line.strip_prefix("@@ ")?.split(' ');
    let range = |part: Option<&str>, sign: char| -> Option<(u32, u32)> {
        let part = part?.strip_prefix(sign)?;
        match part.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((part.parse().ok()?, 1)),
        }
    };
    let (old_start, old_count) = range(parts.next(), '-')?;
    let (new_start, new_count) = range(parts.next(), '+')?;
    // An empty side starts at 0: its next line would be 1.
    Some((old_start.max(1), old_count, new_start.max(1), new_count))
}

/// Writes a block of removed and added lines side by side, pairing them in
/// order, the longer one against gaps.
fn flush(result: &mut SideBySide, removed: &mut Vec<&str>, added: &mut Vec<&str>, old_number: &mut u32, new_number: &mut u32) {
    if removed.is_empty() && added.is_empty() {
        return;
    }
    result.changes.push(result.old.lines.len());
    for ix in 0..removed.len().max(added.len()) {
        let (old, new) = (removed.get(ix).copied(), added.get(ix).copied());
        let old_start = match old {
            Some(text) => {
                *old_number += 1;
                result.old.push(Some(*old_number - 1), Kind::Changed, text)
            }
            None => result.old.push(None, Kind::Gap, ""),
        };
        let new_start = match new {
            Some(text) => {
                *new_number += 1;
                result.new.push(Some(*new_number - 1), Kind::Changed, text)
            }
            None => result.new.push(None, Kind::Gap, ""),
        };
        if let (Some(old), Some(new)) = (old, new) {
            let (old_changed, new_changed) = changed(old, new);
            result.old.lines.last_mut().unwrap().changed = Some(old_start + old_changed.start..old_start + old_changed.end);
            result.new.lines.last_mut().unwrap().changed = Some(new_start + new_changed.start..new_start + new_changed.end);
        }
    }
    removed.clear();
    added.clear();
}

/// What differs between two versions of a line: all but their common
/// beginning and end.
fn changed(old: &str, new: &str) -> (Range<usize>, Range<usize>) {
    let prefix: usize = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    let (old_rest, new_rest) = (&old[prefix..], &new[prefix..]);
    let suffix: usize = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    (prefix..old.len() - suffix, prefix..new.len() - suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,4 +1,5 @@\n a\n-b\n+B\n+c2\n d\n-e\n+E\n\\ No newline at end of file\n";

    #[test]
    fn aligns_the_sides() {
        let sides = split(DIFF).unwrap();
        assert_eq!(sides.old.text, "a\nb\n\nd\ne");
        assert_eq!(sides.new.text, "a\nB\nc2\nd\nE");
        let numbers = |side: &Side| side.lines.iter().map(|line| line.number).collect::<Vec<_>>();
        assert_eq!(numbers(&sides.old), [Some(1), Some(2), None, Some(3), Some(4)]);
        assert_eq!(numbers(&sides.new), [Some(1), Some(2), Some(3), Some(4), Some(5)]);
        assert_eq!(sides.old.lines[2].kind, Kind::Gap);
        assert_eq!(sides.changes, [1, 4]);
        assert_eq!(sides.old.lines[1].changed, Some(2..3));
        assert_eq!(sides.new.lines[2].changed, None);
    }

    #[test]
    fn in_one_column() {
        let inline = inline(&split(DIFF).unwrap());
        assert_eq!(inline.text, "a\nb\nB\nc2\nd\ne\nE");
        let numbers = inline.lines.iter().map(|line| (line.old, line.new)).collect::<Vec<_>>();
        assert_eq!(
            numbers,
            [(Some(1), Some(1)), (Some(2), None), (None, Some(2)), (None, Some(3)), (Some(3), Some(4)), (Some(4), None), (None, Some(5))]
        );
        assert_eq!(inline.changes, [1, 5]);
        // "b" → "B": the letter, in each.
        assert_eq!(inline.lines[1].changed, Some(2..3));
        assert_eq!(inline.lines[2].changed, Some(4..5));
        assert_eq!(inline.lines[3].changed, None);
    }

    #[test]
    fn new_file_and_no_hunks() {
        let sides = split("--- /dev/null\n+++ b/f\n@@ -0,0 +1,2 @@\n+x\n+y\n").unwrap();
        assert_eq!(sides.old.text, "\n");
        assert_eq!(sides.new.text, "x\ny");
        assert_eq!(sides.new.lines[0].number, Some(1));
        assert!(split("Binary files a/x and b/x differ\n").is_none());
        assert!(split("").is_none());
    }

    #[test]
    fn changed_within_a_line() {
        assert_eq!(changed("let a = 1;", "let a = 22;"), (8..9, 8..10));
        assert_eq!(changed("same", "same"), (4..4, 4..4));
    }
}
