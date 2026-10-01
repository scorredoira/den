//! Paths and URLs in terminal text, to open them with Cmd-click.

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq)]
pub enum Link {
    Url(String),
    Path {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// Finds a URL or a `path[:line[:column]]` path at position `col`
/// (in characters) of `line`, and also returns which characters it spans.
/// Relative paths are resolved against `cwd`. With `local`, they're only
/// returned if they exist; on a server (without its disk at hand), if they
/// look like a file.
pub fn link_at(line: &str, col: usize, cwd: Option<&Path>, local: bool) -> Option<(Link, Range<usize>)> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() || is_delimiter(chars[col]) {
        return None;
    }
    let start = (0..col).rev().find(|&i| is_delimiter(chars[i])).map_or(0, |i| i + 1);
    let end = (col..chars.len()).find(|&i| is_delimiter(chars[i])).unwrap_or(chars.len());
    let word: String = chars[start..end].iter().collect();
    let word = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let range = start..start + word.chars().count();
    if !range.contains(&col) {
        return None;
    }

    if word.starts_with("http://") || word.starts_with("https://") {
        return Some((Link::Url(word.to_string()), range));
    }

    let (path, line, column) = split_position(word);
    let path = path.strip_prefix("file://").unwrap_or(path);
    if path.is_empty() {
        return None;
    }
    let path = if let Some(rest) = path.strip_prefix("~/") {
        std::env::home_dir()?.join(rest)
    } else {
        let path = Path::new(path);
        if path.is_absolute() || word.starts_with('/') {
            path.to_path_buf()
        } else {
            cwd?.join(path)
        }
    };
    let plausible = if local {
        path.exists()
    } else {
        path.extension().is_some() || word.contains(['/', '\\'])
    };
    plausible.then_some((Link::Path { path, line, column }, range))
}

/// Splits `path:12:5` into path, line and column.
fn split_position(word: &str) -> (&str, Option<u32>, Option<u32>) {
    let mut parts = word.rsplitn(3, ':');
    let last = parts.next().unwrap_or_default();
    let middle = parts.next();
    let first = parts.next();
    match (first, middle.map(str::parse::<u32>), last.parse::<u32>()) {
        (Some(path), Some(Ok(line)), Ok(column)) => (path, Some(line), Some(column)),
        _ => match (middle, last.parse::<u32>()) {
            (Some(_), Ok(line)) => {
                let path = &word[..word.len() - last.len() - 1];
                (path, Some(line), None)
            }
            _ => (word, None, None),
        },
    }
}

fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | '│')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_positions() {
        assert_eq!(split_position("src/a.rs"), ("src/a.rs", None, None));
        assert_eq!(split_position("src/a.rs:12"), ("src/a.rs", Some(12), None));
        assert_eq!(split_position("src/a.rs:12:5"), ("src/a.rs", Some(12), Some(5)));
        assert_eq!(split_position(r"C:\src\a.rs:12:5"), (r"C:\src\a.rs", Some(12), Some(5)));
        assert_eq!(split_position(r"C:\src\a.rs:12"), (r"C:\src\a.rs", Some(12), None));
        assert_eq!(split_position(r"C:\src\a.rs"), (r"C:\src\a.rs", None, None));
    }

    #[test]
    fn remote_absolute_paths_keep_their_root() {
        let line = "/home/user/project/main.rs:12";
        let Some((Link::Path { path, line, .. }, _)) = link_at(line, 3, Some(Path::new("other")), false) else { panic!() };
        assert_eq!(path, Path::new("/home/user/project/main.rs"));
        assert_eq!(line, Some(12));
    }

    #[test]
    fn finds_urls_and_paths() {
        let cwd = std::env::current_dir().unwrap();
        let line = "error in src/links.rs:10:3, see https://example.com/x.";
        assert_eq!(
            link_at(line, 12, Some(&cwd), true),
            Some((
                Link::Path {
                    path: cwd.join("src/links.rs"),
                    line: Some(10),
                    column: Some(3),
                },
                9..26
            ))
        );
        assert_eq!(
            link_at(line, 40, Some(&cwd), true),
            Some((Link::Url("https://example.com/x".into()), 32..53))
        );
        assert_eq!(link_at(line, 2, Some(&cwd), true), None);
    }
}
