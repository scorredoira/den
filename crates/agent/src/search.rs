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
                    // The searcher matches without the LF terminator.
                    let line = line.strip_suffix('\n').unwrap_or(line);
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

/// Replaces the matches of `query` in `files` (relative to `dir`) and
/// returns how many files changed and how many replacements were made.
/// Matches the way `search` does: line by line, so a match never spans lines.
pub fn replace(
    dir: &Path,
    files: &[String],
    query: &str,
    regex: bool,
    case_sensitive: bool,
    replacement: &str,
    preserve_case: bool,
) -> Result<(usize, usize)> {
    let pattern = if regex { query.to_string() } else { regex::escape(query) };
    let matcher = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()?;
    let (mut changed, mut total) = (0, 0);
    for file in files {
        let path = dir.join(file);
        let text = std::fs::read_to_string(&path)?;
        let mut count = 0;
        let mut new = String::with_capacity(text.len());
        // Match the searcher's LF-delimited records, excluding the terminator.
        // `multi_line` only changes anchors; it does not stop `\s` or `(?s)`
        // from consuming newlines when matching against the whole file.
        for record in text.split_inclusive('\n') {
            let line = record.strip_suffix('\n').unwrap_or(record);
            let replaced = matcher.replace_all(line, |caps: &regex::Captures| {
                count += 1;
                let mut with = String::new();
                if regex {
                    caps.expand(replacement, &mut with);
                } else {
                    with.push_str(replacement);
                }
                if preserve_case { with_case_of(&caps[0], &with) } else { with }
            });
            new.push_str(&replaced);
            if record.ends_with('\n') {
                new.push('\n');
            }
        }
        if count > 0 {
            crate::fs::write(&path, new.as_bytes())?;
            changed += 1;
            total += count;
        }
    }
    Ok((changed, total))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_takes_the_case_of_the_match() {
        assert_eq!(with_case_of("payment", "invoice"), "invoice");
        assert_eq!(with_case_of("Payment", "invoice"), "Invoice");
        assert_eq!(with_case_of("PAYMENT", "invoice"), "INVOICE");
        assert_eq!(with_case_of("paymentId", "invoiceNumber"), "invoiceNumber");
        assert_eq!(with_case_of("PaymentId", "invoiceNumber"), "InvoiceNumber");
        assert_eq!(with_case_of("payment", "Invoice"), "invoice");
        assert_eq!(with_case_of("P", "invoice"), "Invoice");
        assert_eq!(with_case_of("_x", "y"), "y");
    }

    #[test]
    fn replaces_in_the_given_files() {
        let dir = std::env::temp_dir().join(format!("sik-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ts"), "payment(Payment, PAYMENT)\n").unwrap();
        std::fs::write(dir.join("b.ts"), "payment\n").unwrap();
        let files = vec!["a.ts".to_string()];
        assert_eq!(replace(&dir, &files, "payment", false, false, "invoice", true).unwrap(), (1, 3));
        assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "invoice(Invoice, INVOICE)\n");
        assert_eq!(std::fs::read_to_string(dir.join("b.ts")).unwrap(), "payment\n");
        assert_eq!(replace(&dir, &files, r"(\w+)\(", true, true, "call_$1(", false).unwrap(), (1, 1));
        assert_eq!(std::fs::read_to_string(dir.join("a.ts")).unwrap(), "call_invoice(Invoice, INVOICE)\n");
        assert_eq!(replace(&dir, &files, "a.b", false, true, "x", false).unwrap(), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacement_only_changes_the_lines_search_matches() {
        let dir = std::env::temp_dir().join(format!("sik-replace-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        for (query, text, replacement, expected, matching_lines) in [
            (r"foo\s+bar", "foo\nbar\nfoo bar\n", "X", "foo\nbar\nX\n", vec![3]),
            (r"(?s)foo.*bar", "foo\nbar\nfoo bar", "X", "foo\nbar\nX", vec![3]),
            (r"^foo$", "foo\nfoo\r\nfoo", "X", "X\nfoo\r\nX", vec![1, 3]),
            (r"(foo) (bar)", "foo\nbar\nfoo bar foo bar\n", "$2 $1", "foo\nbar\nbar foo bar foo\n", vec![3]),
            (r"^$", "\nfoo\n", "X", "X\nfoo\n", vec![1]),
            (r"^$", "", "X", "", vec![]),
        ] {
            std::fs::write(&path, text).unwrap();
            let (hits, _) = search(&dir, query, true, true, 100).unwrap();
            assert_eq!(hits.iter().map(|hit| hit.line).collect::<Vec<_>>(), matching_lines, "{query}");
            replace(&dir, &["a.txt".into()], query, true, true, replacement, false).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "{query}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

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
