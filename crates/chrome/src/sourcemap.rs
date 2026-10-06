//! Source maps (version 3): which original line a generated position comes
//! from, and where an original line starts in the generated code. Sources are
//! resolved to files under the root; the rest are ignored.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug)]
struct Segment {
    column: u32,
    source: u32,
    line: u32,
}

/// A parsed map. Lines and columns are 0-based, as in the map and in CDP.
#[derive(Debug, Default)]
pub struct SourceMap {
    /// The file each source resolved to, relative to the root with `/`.
    files: Vec<Option<String>>,
    /// Segments of each generated line, sorted by column.
    lines: Vec<Vec<Segment>>,
    /// For each source: the first generated position of each original line.
    starts: Vec<BTreeMap<u32, (u32, u32)>>,
    by_file: HashMap<String, usize>,
}

/// An original position: a file of the root and a 0-based line.
#[derive(Clone, Debug, PartialEq)]
pub struct Original {
    pub file: String,
    pub line: u32,
}

impl SourceMap {
    pub fn parse(text: &str, root: &Path) -> Result<SourceMap> {
        let value: Value = serde_json::from_str(text).context("the source map is not JSON")?;
        if value.get("sections").is_some() {
            bail!("index source maps (with sections) are not supported");
        }
        let version = value.get("version").and_then(Value::as_u64);
        if version != Some(3) {
            bail!("source map version {version:?} is not 3");
        }
        let source_root = value.get("sourceRoot").and_then(Value::as_str).unwrap_or("");
        let sources = value.get("sources").and_then(Value::as_array).context("the source map has no sources")?;
        let files: Vec<Option<String>> = sources
            .iter()
            .map(|source| {
                let source = source.as_str()?;
                let joined = join_source_root(source_root, source);
                resolve_file(&joined, root)
            })
            .collect();
        let mappings = value.get("mappings").and_then(Value::as_str).context("the source map has no mappings")?;
        let lines = decode_mappings(mappings, files.len())?;

        let mut starts = vec![BTreeMap::new(); files.len()];
        for (gen_line, segments) in lines.iter().enumerate() {
            for segment in segments {
                if segment.source == NONE || files[segment.source as usize].is_none() {
                    continue;
                }
                let position = (gen_line as u32, segment.column);
                let first = starts[segment.source as usize].entry(segment.line).or_insert(position);
                if position < *first {
                    *first = position;
                }
            }
        }
        let mut by_file = HashMap::new();
        for (index, file) in files.iter().enumerate() {
            if let Some(file) = file {
                by_file.entry(file.clone()).or_insert(index);
            }
        }
        Ok(SourceMap { files, lines, starts, by_file })
    }

    /// The files of the root this map has code of.
    pub fn files(&self) -> impl Iterator<Item = &String> {
        self.by_file.keys()
    }

    pub fn has_file(&self, file: &str) -> bool {
        self.by_file.contains_key(file)
    }

    /// The original position of a generated one, when it maps to a file of
    /// the root.
    pub fn original(&self, line: u32, column: u32) -> Option<Original> {
        let segments = self.lines.get(line as usize)?;
        if segments.is_empty() {
            return None;
        }
        // the last segment at or before the column; before the first one
        // (indentation), the first one
        let index = segments.partition_point(|segment| segment.column <= column);
        let segment = segments[index.saturating_sub(1)];
        if segment.source == NONE {
            return None;
        }
        let file = self.files.get(segment.source as usize)?.as_ref()?;
        Some(Original { file: file.clone(), line: segment.line })
    }

    /// Where the code of an original line starts in the generated code: the
    /// first generated position of that line, or of the next line with code.
    /// Returns the original line used too.
    pub fn generated(&self, file: &str, line: u32) -> Option<(u32, (u32, u32))> {
        let source = *self.by_file.get(file)?;
        let (line, position) = self.starts[source].range(line..).next()?;
        Some((*line, *position))
    }
}

fn join_source_root(root: &str, source: &str) -> String {
    if root.is_empty() || source.contains("://") || source.starts_with('/') {
        return source.to_string();
    }
    format!("{}/{}", root.trim_end_matches('/'), source)
}

/// The file of the root a source names: the longest suffix of its path that
/// exists under the root, after dropping schemes and `.`/`..` parts.
pub fn resolve_file(source: &str, root: &Path) -> Option<String> {
    let mut path = source;
    for prefix in ["webpack://", "file://"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest;
        }
    }
    if let Some(at) = path.find("://") {
        path = &path[at + 3..];
    }
    let path = path.split(['?', '#']).next().unwrap_or(path);
    // `..` goes up a folder (`client/dist/../scl/a.ts` is `client/scl/a.ts`); one with
    // nothing left to go up from is dropped
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    for start in 0..parts.len() {
        let candidate = parts[start..].join("/");
        if root.join(&candidate).is_file() {
            return Some(candidate);
        }
    }
    None
}

fn decode_mappings(mappings: &str, sources: usize) -> Result<Vec<Vec<Segment>>> {
    let mut lines = Vec::new();
    let mut source: i64 = 0;
    let mut line: i64 = 0;
    for (index, text) in mappings.split(';').enumerate() {
        let mut segments = Vec::new();
        let mut gen_column: i64 = 0;
        for segment in text.split(',') {
            if segment.is_empty() {
                continue;
            }
            let fields = decode_vlq(segment).with_context(|| format!("mappings line {}", index + 1))?;
            match fields.len() {
                1 => {
                    gen_column += fields[0];
                    segments.push(Segment { column: to_u32(gen_column)?, source: NONE, line: 0 });
                }
                4 | 5 => {
                    gen_column += fields[0];
                    source += fields[1];
                    // the original column and the name are not used
                    line += fields[2];
                    if source < 0 || source as usize >= sources {
                        bail!("mappings line {}: source {source} is out of range", index + 1);
                    }
                    segments.push(Segment { column: to_u32(gen_column)?, source: source as u32, line: to_u32(line)? });
                }
                count => bail!("mappings line {}: a segment with {count} fields", index + 1),
            }
        }
        segments.sort_by_key(|segment| segment.column);
        lines.push(segments);
    }
    Ok(lines)
}

fn to_u32(value: i64) -> Result<u32> {
    u32::try_from(value).with_context(|| format!("{value} is out of range in the mappings"))
}

/// Base64 VLQ values of one segment.
fn decode_vlq(text: &str) -> Result<Vec<i64>> {
    let mut values = Vec::new();
    let mut value: i64 = 0;
    let mut shift = 0;
    for byte in text.bytes() {
        let digit = base64_digit(byte).with_context(|| format!("{:?} is not base64", byte as char))? as i64;
        if shift > 60 {
            bail!("a VLQ value is too long");
        }
        value |= (digit & 31) << shift;
        if digit & 32 != 0 {
            shift += 5;
            continue;
        }
        let negative = value & 1 == 1;
        let magnitude = value >> 1;
        values.push(if negative { -magnitude } else { magnitude });
        value = 0;
        shift = 0;
    }
    if shift != 0 {
        bail!("a VLQ value is cut short");
    }
    Ok(values)
}

fn base64_digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decodes standard or URL-safe base64; padding and whitespace are skipped.
pub fn decode_base64(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for byte in text.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let digit = base64_digit(byte).with_context(|| format!("{:?} is not base64", byte as char))?;
        buffer = (buffer << 6) | digit as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_decodes_signed_values() {
        assert_eq!(decode_vlq("AAAA").unwrap(), vec![0, 0, 0, 0]);
        assert_eq!(decode_vlq("gBADC").unwrap(), vec![16, 0, -1, 1]);
        assert!(decode_vlq("g").is_err());
        assert!(decode_vlq("A!").is_err());
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVsbG8gd29ybGQ").unwrap(), b"hello world");
        assert!(decode_base64("a$").is_err());
    }

    #[test]
    fn maps_both_ways() {
        let dir = std::env::temp_dir().join(format!("den-chrome-map-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.ts"), "x").unwrap();
        // line 0: nothing; line 1: col 2 -> a.ts line 0; line 2: col 4 -> line 2, col 0 -> line 1
        let map = r#"{"version":3,"sources":["webpack://app/./src/a.ts","missing.ts"],
            "mappings":";EAAA;AACA,IACA,CCAA"}"#;
        let map = SourceMap::parse(map, &dir).unwrap();
        assert_eq!(map.original(1, 5), Some(Original { file: "src/a.ts".into(), line: 0 }));
        assert_eq!(map.original(2, 0), Some(Original { file: "src/a.ts".into(), line: 1 }));
        assert_eq!(map.original(2, 4), Some(Original { file: "src/a.ts".into(), line: 2 }));
        assert_eq!(map.original(2, 5), None);
        assert_eq!(map.original(0, 0), None);
        assert_eq!(map.generated("src/a.ts", 0), Some((0, (1, 2))));
        assert_eq!(map.generated("src/a.ts", 2), Some((2, (2, 4))));
        assert_eq!(map.generated("src/a.ts", 3), None);
        assert!(!map.has_file("missing.ts"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_parent_folder_goes_up() {
        let dir = std::env::temp_dir().join(format!("den-chrome-up-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("client/scl")).unwrap();
        std::fs::write(dir.join("client/scl/a.ts"), "x").unwrap();
        // a sourceRoot naming the bundle's folder, and a source relative to it
        assert_eq!(resolve_file("client/dist/../scl/a.ts", &dir), Some("client/scl/a.ts".into()));
        assert_eq!(resolve_file("../../client/scl/a.ts", &dir), Some("client/scl/a.ts".into()));
        // without the bundle's folder, ../scl/a.ts is scl/a.ts, which is not a file here
        assert_eq!(resolve_file("../scl/a.ts", &dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
