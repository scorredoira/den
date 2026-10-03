//! Format Document: the repo's `.sik/format` if it has one, else the file's language server, else JSON on its own.
//!
//! `.sik/format <file>` gets the text on stdin and writes it formatted to
//! stdout; exiting with 2 means it doesn't format that kind of file, and the
//! next way is tried.

use std::{
    io::{Read, Write},
    path::Path,
    process::Stdio,
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use proto::Response;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
/// What `.sik/format` exits with for a file it doesn't format.
const NOT_MINE: i32 = 2;

pub fn format(task: &Path, path: &Path, text: &str) -> Result<Response> {
    let formatted = |text: String, by: &str| Ok(Response::Formatted { text: Some(text), by: Some(by.to_string()) });
    if let Some((text, hook)) = script(task, path, text)? {
        return formatted(text, &hook);
    }
    if let Some((text, server)) = crate::lsp::format(task, path, text, indentation(text))? {
        return formatted(text, &server);
    }
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("json")) {
        return formatted(json(text, indentation(text).unwrap_or(Indent::Spaces(4)))?, "json");
    }
    Ok(Response::Formatted { text: None, by: None })
}

/// Runs the repo's `.sik/format`: the text, and the hook's name. `None` if there's none or the file isn't its.
fn script(task: &Path, path: &Path, text: &str) -> Result<Option<(String, String)>> {
    let Some(script) = crate::platform::repo_hook(task, "format").filter(|path| crate::platform::is_executable_script(path)) else {
        return Ok(None);
    };
    let hook = script.strip_prefix(task).unwrap_or(&script).to_string_lossy().replace('\\', "/");
    let mut child = crate::platform::script_command(&script)
        .arg(dunce::simplified(path))
        .current_dir(task)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("{hook} did not start"))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let input = text.to_string();
    std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let (stdout, stderr) = (child.stdout.take().context("no stdout")?, child.stderr.take().context("no stderr")?);
    let (tx, rx) = mpsc::channel();
    let out_tx = tx.clone();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let result = stdout.take(proto::MAX_FILE_BYTES as u64 + 1).read_to_end(&mut out).map(|_| out);
        let _ = out_tx.send((true, result));
    });
    std::thread::spawn(move || {
        let _ = tx.send((false, diagnostic_tail(stderr)));
    });
    let deadline = Instant::now() + SCRIPT_TIMEOUT;
    let result = (|| -> Result<_> {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        for _ in 0..2 {
            let (stdout, bytes) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .with_context(|| format!("{hook} did not finish in {} s", SCRIPT_TIMEOUT.as_secs()))?;
            let bytes = bytes.with_context(|| format!("could not read {hook}'s output"))?;
            if stdout {
                if bytes.len() > proto::MAX_FILE_BYTES {
                    bail!("{hook} produced too much output");
                }
                out = bytes;
            } else {
                err = bytes;
            }
        }
        // Closing stdout/stderr does not necessarily mean the child exited.
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok((out, err, status));
            }
            if Instant::now() >= deadline {
                bail!("{hook} did not finish in {} s", SCRIPT_TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let (out, err, status) = result?;
    let err = String::from_utf8_lossy(&err);
    match status.code() {
        Some(0) => Ok(Some((String::from_utf8(out).map_err(|_| anyhow!("{hook} did not write UTF-8"))?, hook))),
        Some(NOT_MINE) => Ok(None),
        _ => match err.trim() {
            "" => bail!("{hook} failed ({status})"),
            err => bail!("{hook}: {}", err.lines().last().unwrap_or(err)),
        },
    }
}

/// Drain diagnostics concurrently with stdout, retaining only the tail.
fn diagnostic_tail(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    const LIMIT: usize = 8192;
    let mut tail = Vec::new();
    let mut buffer = [0; LIMIT];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            return Ok(tail);
        }
        let discard = (tail.len() + n).saturating_sub(LIMIT);
        tail.drain(..discard);
        tail.extend_from_slice(&buffer[..n]);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Indent {
    Tabs,
    Spaces(usize),
}

impl Indent {
    fn unit(self) -> String {
        match self {
            Indent::Tabs => "\t".to_string(),
            Indent::Spaces(n) => " ".repeat(n),
        }
    }
}

/// How the text indents: tabs, or the smallest indentation in spaces.
/// `None` if nothing is indented.
pub fn indentation(text: &str) -> Option<Indent> {
    let mut spaces = None;
    for line in text.lines() {
        let rest = line.trim_start_matches([' ', '\t']);
        if rest.is_empty() || rest.len() == line.len() {
            continue;
        }
        if line.starts_with('\t') {
            return Some(Indent::Tabs);
        }
        let n = line.len() - line.trim_start_matches(' ').len();
        spaces = Some(spaces.map_or(n, |m: usize| m.min(n)));
    }
    spaces.map(Indent::Spaces)
}

enum Token<'a> {
    Punct(u8),
    /// A string, number, `true`, `false` or `null`, as written.
    Value(&'a str),
    /// `trailing`: on the same line as what came before.
    Comment { text: &'a str, line: bool, trailing: bool },
}

/// JSON (comments allowed, as in `tsconfig.json`) with one item per line,
/// indented with `indent`. Keys keep their order and values their spelling.
pub fn json(text: &str, indent: Indent) -> Result<String> {
    let tokens = json_tokens(text)?;
    let unit = indent.unit();
    let mut out = String::new();
    let mut depth = 0usize;
    let mut newline = false;
    let line = |out: &mut String, depth: usize| {
        let trimmed = out.trim_end_matches([' ', '\t']).len();
        out.truncate(trimmed);
        out.push('\n');
        out.push_str(&unit.repeat(depth));
    };
    let mut ix = 0;
    while ix < tokens.len() {
        match &tokens[ix] {
            Token::Punct(open @ (b'{' | b'[')) => {
                if newline {
                    line(&mut out, depth);
                    newline = false;
                }
                let close = if *open == b'{' { b'}' } else { b']' };
                if matches!(tokens.get(ix + 1), Some(Token::Punct(next)) if *next == close) {
                    out.push(*open as char);
                    out.push(close as char);
                    ix += 1;
                } else {
                    out.push(*open as char);
                    depth += 1;
                    newline = true;
                }
            }
            Token::Punct(close @ (b'}' | b']')) => {
                depth = depth.checked_sub(1).context("not valid JSON: unbalanced brackets")?;
                line(&mut out, depth);
                out.push(*close as char);
                newline = false;
            }
            Token::Punct(b',') => {
                out.push(',');
                newline = true;
            }
            Token::Punct(_) => out.push_str(": "),
            Token::Value(value) => {
                if newline {
                    line(&mut out, depth);
                    newline = false;
                }
                out.push_str(value);
            }
            Token::Comment { text, line: is_line, trailing } => {
                if *trailing && !out.is_empty() {
                    out.push(' ');
                } else if !out.is_empty() {
                    line(&mut out, depth);
                }
                out.push_str(text);
                newline = *is_line || newline || !*trailing;
            }
        }
        ix += 1;
    }
    if depth != 0 {
        bail!("not valid JSON: unbalanced brackets");
    }
    if text.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn json_tokens(text: &str) -> Result<Vec<Token<'_>>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut ix = 0;
    let mut same_line = false;
    let line_of = |at: usize| text[..at].matches('\n').count() + 1;
    while ix < bytes.len() {
        let start = ix;
        match bytes[ix] {
            b'\n' => {
                same_line = false;
                ix += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' => {
                ix += 1;
                continue;
            }
            b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                tokens.push(Token::Punct(bytes[ix]));
                ix += 1;
            }
            b'"' => {
                ix += 1;
                loop {
                    match bytes.get(ix) {
                        None | Some(b'\n') => bail!("not valid JSON: unterminated string on line {}", line_of(start)),
                        Some(b'\\') => ix += 2,
                        Some(b'"') => break,
                        Some(_) => ix += 1,
                    }
                }
                ix += 1;
                tokens.push(Token::Value(&text[start..ix]));
            }
            b'/' if bytes.get(ix + 1) == Some(&b'/') => {
                ix = text[ix..].find('\n').map_or(bytes.len(), |end| ix + end);
                tokens.push(Token::Comment { text: text[start..ix].trim_end(), line: true, trailing: same_line });
            }
            b'/' if bytes.get(ix + 1) == Some(&b'*') => {
                let end = text[ix + 2..].find("*/").with_context(|| format!("not valid JSON: unterminated comment on line {}", line_of(start)))?;
                ix += end + 4;
                tokens.push(Token::Comment { text: &text[start..ix], line: false, trailing: same_line });
            }
            ch if ch == b'-' || ch.is_ascii_alphanumeric() => {
                while bytes.get(ix).is_some_and(|ch| *ch == b'-' || *ch == b'+' || *ch == b'.' || ch.is_ascii_alphanumeric()) {
                    ix += 1;
                }
                let value = &text[start..ix];
                if !matches!(value, "true" | "false" | "null") && value.parse::<f64>().is_err() {
                    bail!("not valid JSON: “{value}” on line {}", line_of(start));
                }
                tokens.push(Token::Value(value));
            }
            _ => bail!("not valid JSON: unexpected “{}” on line {}", text[ix..].chars().next().unwrap_or(' '), line_of(start)),
        }
        same_line = true;
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn drains_verbose_diagnostics_while_formatting() {
        use std::os::unix::fs::PermissionsExt;
        let task = tempfile::tempdir().unwrap();
        std::fs::create_dir(task.path().join(".sik")).unwrap();
        let hook = task.path().join(".sik/format");
        std::fs::write(&hook, "#!/bin/sh\ndd if=/dev/zero bs=65536 count=32 >&2 2>/dev/null\ntr a-z A-Z\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input = "hello\n".repeat(20_000);
        let result = script(task.path(), &task.path().join("a.txt"), &input).unwrap().unwrap();
        assert_eq!(result.0, input.to_uppercase());
        let diagnostics = vec![b'x'; 100_000];
        assert_eq!(diagnostic_tail(diagnostics.as_slice()).unwrap().len(), 8192);
    }

    #[test]
    fn json_one_item_per_line_keeping_order_and_spelling() {
        let text = "{\"b\":1.50,\"a\":[1,2,{}],\"e\":[],\"s\":\"x, \\\"y\\\"\"}\n";
        let want = "{\n\t\"b\": 1.50,\n\t\"a\": [\n\t\t1,\n\t\t2,\n\t\t{}\n\t],\n\t\"e\": [],\n\t\"s\": \"x, \\\"y\\\"\"\n}\n";
        assert_eq!(json(text, Indent::Tabs).unwrap(), want);
    }

    #[test]
    fn json_keeps_comments() {
        let text = "{\n  // options\n  \"strict\": true, // yes\n  /* end */\n}";
        let want = "{\n  // options\n  \"strict\": true, // yes\n  /* end */\n}";
        assert_eq!(json(text, Indent::Spaces(2)).unwrap(), want);
    }

    #[test]
    fn json_errors_and_indentation() {
        assert!(json("{\"a\": 1", Indent::Tabs).is_err());
        assert!(json("{\"a\": nope}", Indent::Tabs).is_err());
        assert_eq!(indentation("{\n    \"a\": {\n        \"b\": 1\n    }\n}"), Some(Indent::Spaces(4)));
        assert_eq!(indentation("{\n\t\"a\": 1\n}"), Some(Indent::Tabs));
        assert_eq!(indentation("{}"), None);
    }

    #[test]
    #[cfg(windows)]
    fn formats_with_powershell_hook() {
        let task = std::env::temp_dir().join(format!("sik-format-ps-{}", std::process::id()));
        std::fs::create_dir_all(task.join(".sik")).unwrap();
        std::fs::write(task.join(".sik/format.ps1"),
            "[Console]::Out.Write([Console]::In.ReadToEnd().ToUpperInvariant())").unwrap();
        let Response::Formatted { text, .. } = format(&task, &task.join("a.txt"), "hello").unwrap() else { panic!() };
        assert_eq!(text.as_deref(), Some("HELLO"));
        let _ = std::fs::remove_dir_all(task);
    }

    #[test]
    #[cfg(unix)]
    fn the_repo_script_first_and_then_the_rest() {
        use std::os::unix::fs::PermissionsExt as _;
        let task = std::env::temp_dir().join(format!("sik-format-{}", std::process::id()));
        std::fs::create_dir_all(task.join(".sik")).unwrap();
        let script = task.join(".sik/format");
        std::fs::write(&script, "#!/bin/sh\ncase \"$1\" in *.xml) tr a-z A-Z ;; *.bad) echo broken >&2; exit 1 ;; *) exit 2 ;; esac\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let Response::Formatted { text, by } = format(&task, &task.join("a.xml"), "<a/>").unwrap() else { panic!() };
        assert_eq!((text.as_deref(), by.as_deref()), (Some("<A/>"), Some(".sik/format")));
        let Response::Formatted { text, by } = format(&task, &task.join("a.json"), "{\"a\":1}").unwrap() else { panic!() };
        assert_eq!((text.as_deref(), by.as_deref()), (Some("{\n    \"a\": 1\n}"), Some("json")));
        let Response::Formatted { text, .. } = format(&task, &task.join("a.css"), "a{}").unwrap() else { panic!() };
        assert_eq!(text, None);
        let err = format(&task, &task.join("a.bad"), "x").unwrap_err();
        assert_eq!(err.to_string(), ".sik/format: broken");
        let _ = std::fs::remove_dir_all(&task);
    }
}
