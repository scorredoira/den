//! The breakpoints of a workspace: they exist with or without a debug
//! session, follow the lines they're on as the file is edited, and are saved
//! with the workspace.

use std::path::{Path, PathBuf};

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
    /// land on the same line become one.
    pub fn placed(&mut self, path: &Path, placed: &[(u32, Option<String>)]) {
        let Some(ix) = self.files.iter().position(|(file, _)| file == path) else {
            return;
        };
        let list = std::mem::take(&mut self.files[ix].1);
        let mut moved = placed.iter();
        for mut bp in list {
            if bp.enabled
                && let Some((line, error)) = moved.next()
            {
                bp.line = *line;
                bp.error = error.clone();
            }
            self.put(path, bp);
        }
    }

    /// Follows an edit of `path`: `delta` lines were inserted (or removed,
    /// when negative) right after line `at`. Breakpoints after it move with
    /// their lines; those on removed lines go to `at`.
    pub fn shift(&mut self, path: &Path, at: u32, delta: i64) -> bool {
        let Some(ix) = self.files.iter().position(|(file, _)| file == path) else {
            return false;
        };
        if delta == 0 {
            return false;
        }
        let list = std::mem::take(&mut self.files[ix].1);
        let mut changed = false;
        for mut bp in list {
            if bp.line > at {
                let line = (bp.line as i64 + delta).max(at as i64) as u32;
                changed |= line != bp.line;
                bp.line = line;
            }
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
        bps.shift(a, 5, 3);
        assert_eq!(lines(&bps, a), vec![2, 13, 23]);
        assert_eq!(lines(&bps, b), vec![10], "another file doesn't move");

        // lines 11..=14 removed after line 10: the one on them goes to 10
        bps.shift(a, 10, -4);
        assert_eq!(lines(&bps, a), vec![2, 10, 19]);
    }

    #[test]
    fn breakpoints_that_meet_on_a_line_become_one() {
        let a = Path::new("/w/a.ts");
        let mut bps = Breakpoints::default();
        bps.toggle(a, 4);
        bps.toggle(a, 6);
        bps.shift(a, 3, -5);
        assert_eq!(lines(&bps, a), vec![3]);
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
