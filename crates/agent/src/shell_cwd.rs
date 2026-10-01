//! Tracks OSC 7 directory reports across arbitrary PTY output boundaries.
//! The initial directory remains useful for shells without integration.

use std::path::PathBuf;

pub struct ShellCwd {
    pub path: PathBuf,
    state: u8,
    osc: Vec<u8>,
}

impl ShellCwd {
    pub fn new(path: PathBuf) -> Self { Self { path, state: 0, osc: Vec::new() } }

    pub fn advance(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match (self.state, byte) {
                (0, 0x1b) => self.state = 1,
                (1, b']') => { self.osc.clear(); self.state = 2; }
                (1, _) => self.state = 0,
                (2, 7) | (3, b'\\') => { self.finish(); self.state = 0; }
                (2, 0x1b) => self.state = 3,
                (2, _) if self.osc.len() < 8192 => self.osc.push(byte),
                (2 | 3, _) => { self.osc.clear(); self.state = 0; }
                _ => {}
            }
        }
    }

    fn finish(&mut self) {
        let Some(uri) = self.osc.strip_prefix(b"7;").and_then(|s| std::str::from_utf8(s).ok()) else { return };
        let Ok(mut url) = url::Url::parse(uri) else { return };
        if url.scheme() != "file" { return }
        // OSC 7 includes the local hostname; it is not a UNC network share.
        let _ = url.set_host(None);
        if let Ok(path) = url.to_file_path() {
            self.path = path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_reports_survive_split_reads_and_ignore_other_osc() {
        let path = if cfg!(windows) { PathBuf::from(r"C:\repo\with space") } else { PathBuf::from("/repo/with space") };
        let uri = url::Url::from_file_path(&path).unwrap();
        let report = format!("hello\x1b]7;{uri}\x1b\\");
        for split in 0..report.len() {
            let mut cwd = ShellCwd::new(PathBuf::from("initial"));
            cwd.advance(&report.as_bytes()[..split]);
            cwd.advance(&report.as_bytes()[split..]);
            cwd.advance(b"\x1b]0;window title\x07");
            assert_eq!(cwd.path, path);
        }
    }
}
