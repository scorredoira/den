//! Search in files and file listing, skipping what git ignores. Uses the
//! ripgrep crates.

use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use ignore::{WalkBuilder, WalkState};
use proto::SearchHit;

/// Cap on the files `files` returns, so millions aren't sent by mistake.
const MAX_FILES: usize = 200_000;

/// Walks the folder like git: without ignored files or `.git`.
fn walker(dir: &Path) -> WalkBuilder {
    let mut walker = WalkBuilder::new(dir);
    walker
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    walker
}

pub fn files(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = walker(dir)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(dir)
                .ok()
                .map(|path| path.to_string_lossy().into_owned())
        })
        .take(MAX_FILES)
        .collect();
    files.sort();
    files
}

/// Searches in parallel; stops upon reaching `max_hits`.
pub fn search(dir: &Path, query: &str, regex: bool, case_sensitive: bool, max_hits: usize) -> Result<(Vec<SearchHit>, bool)> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .fixed_strings(!regex)
        .build(query)?;
    let hits = Arc::new(Mutex::new(Vec::new()));
    let count = Arc::new(AtomicUsize::new(0));
    walker(dir).build_parallel().run(|| {
        let matcher = matcher.clone();
        let hits = hits.clone();
        let count = count.clone();
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .binary_detection(BinaryDetection::quit(0))
            .build();
        Box::new(move |entry| {
            if count.load(Ordering::Relaxed) >= max_hits {
                return WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                return WalkState::Continue;
            }
            let relative = entry
                .path()
                .strip_prefix(dir)
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut found = Vec::new();
            let _ = searcher.search_path(
                &matcher,
                entry.path(),
                UTF8(|line_number, line| {
                    use grep_matcher::Matcher as _;
                    let (column, length) = match matcher.find(line.as_bytes()) {
                        Ok(Some(m)) => (
                            line[..m.start()].chars().count() as u32,
                            line[m.start()..m.end()].chars().count() as u32,
                        ),
                        _ => (0, 0),
                    };
                    found.push(SearchHit {
                        path: relative.clone(),
                        line: line_number as u32,
                        column,
                        length,
                        text: line.trim_end_matches(['\n', '\r']).chars().take(400).collect(),
                    });
                    Ok(count.fetch_add(1, Ordering::Relaxed) + 1 < max_hits)
                }),
            );
            hits.lock().unwrap().extend(found);
            WalkState::Continue
        })
    });
    let mut hits = std::mem::take(&mut *hits.lock().unwrap());
    let truncated = hits.len() >= max_hits;
    hits.truncate(max_hits);
    hits.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    Ok((hits, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_and_lists_respecting_gitignore() {
        let dir = std::env::temp_dir().join(format!("sik-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    let Hello = 1;\n}\n").unwrap();
        std::fs::write(dir.join("notes.md"), "hello world\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/x.rs"), "hello\n").unwrap();

        assert_eq!(files(&dir), vec![".gitignore", "notes.md", "src/main.rs"]);

        let (hits, truncated) = search(&dir, "hello", false, false, 100).unwrap();
        assert!(!truncated);
        let found: Vec<(&str, u32, u32)> = hits.iter().map(|h| (h.path.as_str(), h.line, h.column)).collect();
        assert_eq!(found, vec![("notes.md", 1, 0), ("src/main.rs", 2, 8)]);

        let (hits, _) = search(&dir, "hello", false, true, 100).unwrap();
        assert_eq!(hits.len(), 1);
        let (hits, _) = search(&dir, r"let \w+", true, true, 100).unwrap();
        assert_eq!((hits[0].column, hits[0].length), (4, 9));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
