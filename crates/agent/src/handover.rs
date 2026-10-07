//! An agent replaced by a newer one without closing its terminals: the
//! process runs the new binary (`exec`), so the programs in its terminals
//! are still its children, and the descriptors of their ptys stay open
//! across it. What the new one doesn't find in them (which terminal each is,
//! its screen) goes in a file it's told in `DEN_HANDOVER`.
//!
//! Agents of later versions read it: fields are only added, with a default.
//! Not on Windows, where the agent has no `exec`: there it restarts.

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use proto::TermId;
use serde::{Deserialize, Serialize};

const ENV: &str = "DEN_HANDOVER";

/// A descriptor, as `RawFd` is on Unix.
type RawFd = i32;

/// The file: this, then each of the `terms` in a frame of its own (a screen
/// with its history can be large).
#[derive(Serialize, Deserialize)]
struct Header {
    listener: RawFd,
    next_term: TermId,
    terms: usize,
    ide: Option<Ide>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ide {
    pub fd: RawFd,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
pub struct Term {
    pub term: TermId,
    pub group: String,
    pub cwd: PathBuf,
    pub fd: RawFd,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
    pub title: Option<String>,
    /// Its screen and history, as `snapshot` writes them.
    #[serde(with = "serde_bytes")]
    pub screen: Vec<u8>,
}

pub struct Handover {
    /// The socket it listened on: listening never stops, so a UI that
    /// reconnects meanwhile waits for the new agent instead of starting one.
    #[cfg_attr(windows, allow(dead_code))]
    pub listener: RawFd,
    pub next_term: TermId,
    pub terms: Vec<Term>,
    pub ide: Option<Ide>,
}

/// Runs `exe` in this process with `handover`; returns only if it couldn't,
/// with everything as it was.
#[cfg(unix)]
pub fn replace(exe: &Path, handover: &Handover) -> anyhow::Error {
    let file = match write(handover) {
        Ok(file) => file,
        Err(err) => return err,
    };
    let kept: Vec<RawFd> = handover
        .terms
        .iter()
        .map(|term| term.fd)
        .chain([handover.listener])
        .chain(handover.ide.as_ref().map(|ide| ide.fd))
        .collect();
    // Only what's handed over reaches the new agent: anything else open
    // (connections, watchers, language servers' pipes) closes.
    for fd in open_fds() {
        set_cloexec(fd, !kept.contains(&fd));
    }
    use std::os::unix::process::CommandExt as _;
    let err = std::process::Command::new(exe).arg("daemon").env(ENV, &file).exec();
    for fd in kept {
        set_cloexec(fd, true);
    }
    let _ = std::fs::remove_file(&file);
    anyhow::Error::new(err).context(format!("could not run {}", exe.display()))
}

/// What the agent this process was before handed over, if it was one. Taken
/// once, at the start: the terminals the new agent starts don't inherit it.
pub fn take() -> Option<Handover> {
    if cfg!(windows) {
        return None;
    }
    let path = std::env::var_os(ENV)?;
    // SAFETY: called first thing, before the agent starts any thread.
    unsafe { std::env::remove_var(ENV) };
    let read = read(Path::new(&path));
    let _ = std::fs::remove_file(&path);
    match read {
        Ok(handover) => Some(handover),
        Err(err) => {
            eprintln!("could not read the terminals handed over: {err:#}");
            None
        }
    }
}

/// The handed-over pty's descriptor, now this agent's.
#[cfg(unix)]
pub fn own(fd: RawFd) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    // SAFETY: the agent before left it open for this one, which takes it once.
    unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }
}

#[cfg(unix)]
fn write(handover: &Handover) -> Result<PathBuf> {
    let dir = proto::state_dir()?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("handover-{}", std::process::id()));
    let mut out = std::io::BufWriter::new(File::create(&path)?);
    let header = Header { listener: handover.listener, next_term: handover.next_term, terms: handover.terms.len(), ide: handover.ide.clone() };
    proto::write_frame(&mut out, &header)?;
    for term in &handover.terms {
        proto::write_frame(&mut out, term)?;
    }
    out.into_inner().map_err(|err| err.into_error())?.sync_all()?;
    Ok(path)
}

fn read(path: &Path) -> Result<Handover> {
    let mut file = std::io::BufReader::new(File::open(path)?);
    let header: Header = proto::read_frame(&mut file)?.context("empty")?;
    let terms = (0..header.terms)
        .map(|_| proto::read_frame(&mut file)?.context("cut short"))
        .collect::<Result<_>>()?;
    Ok(Handover { listener: header.listener, next_term: header.next_term, terms, ide: header.ide })
}

/// This process's open descriptors past stdin, stdout and stderr.
#[cfg(unix)]
fn open_fds() -> Vec<RawFd> {
    let Ok(entries) = std::fs::read_dir("/dev/fd") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > 2)
        .collect()
}

#[cfg(unix)]
fn set_cloexec(fd: RawFd, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            let flags = if on { flags | libc::FD_CLOEXEC } else { flags & !libc::FD_CLOEXEC };
            libc::fcntl(fd, libc::F_SETFD, flags);
        }
    }
}
