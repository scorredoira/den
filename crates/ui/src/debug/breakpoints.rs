//! The breakpoints of a workspace: they exist with or without a debug
//! session, follow the lines they're on as the file is edited, and are saved
//! with the workspace.

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use crate::config::SavedBreakpoint;

#[derive(Clone, Debug, PartialEq)]
pub struct Breakpoint {
    /// 0-based.
    pub line: u32,
    pub condition: String,
    pub hit: String,
    /// A logpoint's message: it prints instead of stopping.
    pub log: String,
    pub enabled: bool,
    /// What the program said when it was set: the line it moved to is in
    /// `line` already; this is an error (a condition that doesn't parse).
    pub error: Option<String>,
}

impl Breakpoint {
    pub fn new(line: u32) -> Self {
        Self { line, condition: String::new(), hit: String::new(), log: String::new(), enabled: true, error: None }
    }

    /// It does more than stop: it has a condition, a hit count or a message.
    pub fn is_special(&self) -> bool {
        !self.condition.is_empty() || !self.hit.is_empty() || !self.log.is_empty()
    }
}

/// An edit of a file, by lines: it begins on line `start`, and `kept` is the
/// first line after it that it left whole, now `delta` lines further down
/// (up, when negative).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineEdit {
    pub start: u32,
    pub kept: u32,
    pub delta: i64,
}

impl LineEdit {
    /// The edit that replaced `range` of `old` with `with`. Text inserted at
    /// the start of a line goes before it: Enter there, or lines pasted
    /// there, move the line down.
    pub fn new(old: &str, range: Range<usize>, with: &str) -> Self {
        let line_of = |offset: usize| old[..offset].matches('\n').count() as u32;
        let start = line_of(range.start);
        let end = line_of(range.end);
        let ends_at_line_start = range.end == 0 || old.as_bytes()[range.end - 1] == b'\n';
        let kept = if ends_at_line_start { end } else { end + 1 };
        let delta = with.matches('\n').count() as i64 - (end - start) as i64;
        Self { start, kept, delta }
    }
}

#[derive(Default)]
pub struct Breakpoints {
    /// By absolute path, each list sorted by line.
    files: Vec<(PathBuf, Vec<Breakpoint>)>,
}

impl Breakpoints {
    pub fn load(root: &Path, saved: &[SavedBreakpoint]) -> Self {
        let mut breakpoints = Self::default();
        for saved in saved {
            let mut bp = Breakpoint::new(saved.line);
            bp.condition = saved.condition.clone();
            bp.hit = saved.hit.clone();
            bp.log = saved.log.clone();
            bp.enabled = saved.enabled;
            breakpoints.put(&root.join(&saved.path), bp);
        }
        breakpoints
    }

    pub fn save(&self, root: &Path) -> Vec<SavedBreakpoint> {
        let mut saved = Vec::new();
        for (path, list) in &self.files {
            let path = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            for bp in list {
                saved.push(SavedBreakpoint {
                    path: path.clone(),
                    line: bp.line,
                    condition: bp.condition.clone(),
                    hit: bp.hit.clone(),
                    log: bp.log.clone(),
                    enabled: bp.enabled,
                });
            }
        }
        saved
    }

    pub fn of(&self, path: &Path) -> &[Breakpoint] {
        self.files
            .iter()
            .find(|(file, _)| file == path)
            .map_or(&[], |(_, list)| list.as_slice())
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &[Breakpoint])> {
        self.files.iter().map(|(path, list)| (path.as_path(), list.as_slice()))
    }

    pub fn at(&self, path: &Path, line: u32) -> Option<&Breakpoint> {
        self.of(path).iter().find(|bp| bp.line == line)
    }

    /// Adds or replaces the breakpoint of its line.
    pub fn put(&mut self, path: &Path, bp: Breakpoint) {
        let list = match self.files.iter().position(|(file, _)| file == path) {
            Some(ix) => &mut self.files[ix].1,
            None => {
                self.files.push((path.to_path_buf(), Vec::new()));
                &mut self.files.last_mut().unwrap().1
            }
        };
        match list.binary_search_by_key(&bp.line, |other| other.line) {
            Ok(ix) => list[ix] = bp,
            Err(ix) => list.insert(ix, bp),
        }
    }

    pub fn remove(&mut self, path: &Path, line: u32) -> Option<Breakpoint> {
        let ix = self.files.iter().position(|(file, _)| file == path)?;
        let list = &mut self.files[ix].1;
        let at = list.iter().position(|bp| bp.line == line)?;
        let bp = list.remove(at);
        if list.is_empty() {
            self.files.remove(ix);
        }
        Some(bp)
    }

    /// Adds a breakpoint at the line, or removes the one there.
    pub fn toggle(&mut self, path: &Path, line: u32) {
        if self.remove(path, line).is_none() {
            self.put(path, Breakpoint::new(line));
        }
    }

    pub fn clear(&mut self) {
        self.files.clear();
    }

    /// After the program placed a file's breakpoints (`placed[i]` is where
    /// the i-th enabled one went, and its error), moves them there. Two that
    /// land on the same line become one, the enabled one if either is: it's
    /// the one the program has.
    pub fn placed(&mut self, path: &Path, placed: &[(u32, Option<String>)]) {
        let Some(ix) = self.files.iter().position(|(file, _)| file == path) else {
            return;
        };
        let list = std::mem::take(&mut self.files[ix].1);
        let mut moved = placed.iter();
        let (mut enabled, disabled): (Vec<Breakpoint>, Vec<Breakpoint>) = list.into_iter().partition(|bp| bp.enabled);
        for bp in &mut enabled {
            if let Some((line, error)) = moved.next() {
                bp.line = *line;
                bp.error = error.clone();
            }
        }
        for bp in disabled.into_iter().chain(enabled) {
            self.put(path, bp);
        }
    }

    /// Follows an edit of `path` (see `LineEdit`). Breakpoints from the
    /// first line it left whole move with their lines; those on lines it
    /// replaced stay within what replaced them, and those on removed lines
    /// go to its first.
    pub fn shift(&mut self, path: &Path, edit: LineEdit) -> bool {
        let Some(ix) = self.files.iter().position(|(file, _)| file == path) else {
            return false;
        };
        let LineEdit { start, kept, delta } = edit;
        let list = std::mem::take(&mut self.files[ix].1);
        let mut changed = false;
        for mut bp in list {
            let line = if bp.line >= kept {
                bp.line as i64 + delta
            } else if bp.line > start {
                (bp.line as i64).min(kept as i64 + delta - 1).max(start as i64)
            } else {
                bp.line as i64
            };
            let line = line.max(0) as u32;
            changed |= line != bp.line;
            bp.line = line;
            // two that met on a line become one
            if self.at(path, bp.line).is_none() {
                self.put(path, bp);
            }
        }
        if self.of(path).is_empty() {
            self.files.retain(|(file, _)| file != path);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(breakpoints: &Breakpoints, path: &Path) -> Vec<u32> {
        breakpoints.of(path).iter().map(|bp| bp.line).collect()
    }

    #[test]
    fn breakpoints_follow_inserted_and_removed_lines() {
        let a = Path::new("/w/a.ts");
        let b = Path::new("/w/b.ts");
        let mut bps = Breakpoints::default();
        for line in [2, 10, 20] {
            bps.toggle(a, line);
        }
        bps.toggle(b, 10);

        // three lines inserted after line 5: only those below move
        bps.shift(a, LineEdit { start: 5, kept: 6, delta: 3 });
        assert_eq!(lines(&bps, a), vec![2, 13, 23]);
        assert_eq!(lines(&bps, b), vec![10], "another file doesn't move");

        // lines 11..=14 removed from the end of line 10: the one on them goes to 10
        bps.shift(a, LineEdit { start: 10, kept: 15, delta: -4 });
        assert_eq!(lines(&bps, a), vec![2, 10, 19]);
    }

    #[test]
    fn breakpoints_that_meet_on_a_line_become_one() {
        let a = Path::new("/w/a.ts");
        let mut bps = Breakpoints::default();
        bps.toggle(a, 4);
        bps.toggle(a, 6);
        bps.shift(a, LineEdit { start: 3, kept: 9, delta: -5 });
        assert_eq!(lines(&bps, a), vec![3]);
    }

    /// The breakpoints of `a` after replacing `range` of `old` with `with`.
    fn after_edit(at: &[u32], old: &str, range: Range<usize>, with: &str) -> Vec<u32> {
        let a = Path::new("/w/a.ts");
        let mut bps = Breakpoints::default();
        for line in at {
            bps.toggle(a, *line);
        }
        bps.shift(a, LineEdit::new(old, range, with));
        lines(&bps, a)
    }

    #[test]
    fn an_edit_moves_breakpoints_by_where_it_is() {
        let old = "a\nb\nc\n";
        // Enter at the start of line 1: its code, and breakpoint, go down
        assert_eq!(after_edit(&[0, 1], old, 2..2, "\n"), vec![0, 2]);
        // lines pasted there too
        assert_eq!(after_edit(&[1], old, 2..2, "x\ny\n"), vec![3]);
        // Enter at its end: the breakpoint stays, the next lines go down
        assert_eq!(after_edit(&[1, 2], old, 3..3, "\n"), vec![1, 3]);
        // typing within a line moves nothing
        assert_eq!(after_edit(&[1, 2], old, 2..2, "x"), vec![1, 2]);
        // Backspace at the start of line 2 joins it to line 1
        assert_eq!(after_edit(&[0, 2], old, 3..4, ""), vec![0, 1]);
        // a line deleted whole: the next one takes its place
        assert_eq!(after_edit(&[1, 2], old, 2..4, ""), vec![1]);
    }

    #[test]
    fn breakpoints_in_lines_replaced_stay_in_what_replaced_them() {
        let old = "a\nb\nc\nd\ne\n";
        // lines 1..=3 replaced by one: those on them go to it
        assert_eq!(after_edit(&[0, 2, 3, 4], old, 2..8, "x\n"), vec![0, 1, 2]);
        // reformatted with as many lines: nothing moves
        assert_eq!(after_edit(&[1, 3], old, 2..8, "x\ny\nz\n"), vec![1, 3]);
    }

    #[test]
    fn the_program_moves_breakpoints_to_lines_with_code() {
        let a = Path::new("/w/a.ts");
        let mut bps = Breakpoints::default();
        bps.toggle(a, 3);
        bps.toggle(a, 7);
        let mut disabled = Breakpoint::new(5);
        disabled.enabled = false;
        bps.put(a, disabled);

        // only enabled ones are sent, in order
        bps.placed(a, &[(4, None), (8, Some("bad condition".into()))]);
        assert_eq!(lines(&bps, a), vec![4, 5, 8]);
        assert_eq!(bps.at(a, 8).unwrap().error.as_deref(), Some("bad condition"));
    }

    #[test]
    fn a_breakpoint_placed_on_a_disabled_one_stays_enabled() {
        let a = Path::new("/w/a.ts");
        let mut bps = Breakpoints::default();
        bps.toggle(a, 3);
        let mut disabled = Breakpoint::new(4);
        disabled.enabled = false;
        bps.put(a, disabled);
        bps.placed(a, &[(4, None)]);
        assert_eq!(lines(&bps, a), vec![4]);
        assert!(bps.at(a, 4).unwrap().enabled);
    }

    #[test]
    fn saved_paths_are_relative_to_the_workspace() {
        let root = Path::new("/w");
        let mut bps = Breakpoints::default();
        bps.toggle(Path::new("/w/lib/a.ts"), 1);
        let saved = bps.save(root);
        assert_eq!(saved[0].path, PathBuf::from("lib/a.ts"));
        let loaded = Breakpoints::load(root, &saved);
        assert_eq!(lines(&loaded, Path::new("/w/lib/a.ts")), vec![1]);
    }
}
