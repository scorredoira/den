//! The terminals outlive the agent: handed over to the one that replaces
//! it in the same process, or saved for the next one to reopen them.

use serde::{Deserialize, Serialize};
use proto::ClientMessage;

use crate::pty::Pty;
use super::*;
use super::{
    claude::{claude_session, resume_command, resumed_session},
    terminals::{Screen, start},
};

/// A terminal as it was when the agent was restarted (to update it), so the
/// new agent opens it again under the same id and the UIs find it in place.
#[derive(Serialize, Deserialize)]
struct Restarted {
    term: TermId,
    group: String,
    cwd: PathBuf,
    cols: u16,
    rows: u16,
    /// Claude Code was running in it: the command that resumes its session
    /// (the same options and config folder, plus `--resume <id>`).
    claude: Option<String>,
}

/// Next to the socket (`agent-6.restart.json`), so an agent on another
/// socket (tests, another protocol) doesn't take it unless it shut that one
/// down. Agents of later protocols read it: fields are only added, with a default.
pub(super) fn restart_file() -> Result<PathBuf> {
    Ok(restart_file_of(&proto::socket_path()?))
}

fn restart_file_of(socket: &Path) -> PathBuf {
    socket.with_extension("restart.json")
}

/// Replaces this agent with `exe` in the same process, handing it the
/// terminals (see `handover.rs`). Saved first as for a restart, so that a
/// new agent that doesn't start leaves them for the next one.
#[cfg(unix)]
pub(super) fn replace_agent(state: &State, exe: &Path) -> Result<Response> {
    save_for_restart(state);
    let terms = state
        .terms
        .iter()
        .filter_map(|(term, entry)| {
            let entry = entry.locked();
            Some(crate::handover::Term {
                term: *term,
                group: entry.group.clone(),
                cwd: entry.cwd.path.clone(),
                fd: entry.pty.raw_fd()?,
                pid: entry.pty.pid()?,
                cols: entry.emulator.columns() as u16,
                rows: entry.emulator.screen_lines() as u16,
                title: entry.title.clone(),
                screen: snapshot(&entry.emulator, entry.title.as_deref()),
            })
        })
        .collect();
    let ide = crate::ide::handover().map(|(fd, token)| crate::handover::Ide { fd, token });
    let handover = Handover { listener: state.listener, next_term: state.next_term, terms, ide };
    eprintln!("replacing the agent with {}", exe.display());
    Err(crate::handover::replace(exe, &handover))
}

#[cfg(windows)]
pub(super) fn replace_agent(_: &State, _: &Path) -> Result<Response> {
    anyhow::bail!("on Windows the agent can't replace itself: it restarts")
}

/// The listener for Claude Code the agent before had.
pub(super) fn handed_ide(handover: &mut Handover) -> Option<(std::net::TcpListener, String)> {
    let ide = handover.ide.take()?;
    #[cfg(unix)]
    {
        Some((std::net::TcpListener::from(crate::handover::own(ide.fd)), ide.token))
    }
    #[cfg(windows)]
    {
        let _ = ide;
        None
    }
}

/// Goes on with the terminals the agent this process was before handed over,
/// under the same ids, with their screens.
pub(super) fn take_over(state: &Shared, handover: Handover) {
    {
        let mut state = state.locked();
        state.next_term = state.next_term.max(handover.next_term);
    }
    for handed in handover.terms {
        #[cfg(unix)]
        let pty = Pty::adopt(crate::handover::own(handed.fd), handed.pid);
        #[cfg(windows)]
        let pty: Result<(Pty, crate::pty::PtyIo)> = Err(anyhow::anyhow!("no handover on Windows"));
        match pty {
            Ok(pty) => {
                let screen = Screen { data: handed.screen, title: handed.title };
                start(state, handed.term, handed.group, handed.cwd, pty, handed.cols, handed.rows, Some(screen));
            }
            Err(err) => eprintln!("could not take over terminal {}: {err:#}", handed.term),
        }
    }
    eprintln!("took over the terminals of the agent before");
}

/// How long an agent of an earlier protocol may take to save its
/// terminals and exit (it reads each one's command line).
const OLDER_AGENT_SHUTDOWN: Duration = Duration::from_secs(10);

/// Agents of earlier protocols still running (Den was updated to a newer
/// one): their terminals are where the UI left them, so each is shut down,
/// which saves them as for a restart, and the files they leave are returned
/// to be restored here. Every protocol reads `Shutdown` alike.
pub(super) fn shut_down_older_agents() -> Vec<PathBuf> {
    let Ok(sockets) = proto::older_socket_paths() else {
        return Vec::new();
    };
    sockets
        .into_iter()
        .filter_map(|socket| {
            let mut stream = platform::connect(&socket).ok()?;
            proto::write_frame(&mut stream, &ClientMessage { id: None, request: Request::Shutdown }).ok()?;
            // It saves its terminals and exits, which closes the connection.
            // One that hangs instead is left alone, with them.
            let (closed, done) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut stream, &mut std::io::sink());
                let _ = closed.send(());
            });
            if done.recv_timeout(OLDER_AGENT_SHUTDOWN).is_err() {
                eprintln!("the agent at {} didn't shut down: its terminals stay with it", socket.display());
                return None;
            }
            eprintln!("shut down the agent at {} to take its terminals", socket.display());
            Some(restart_file_of(&socket))
        })
        .collect()
}

/// Every so many ticks of the foreground check (5 s), the terminals are saved
/// as for a restart, so that an agent that dies without one (the Mac is
/// restarted, it's killed) leaves them for the next to restore.
pub(super) const SAVE_TICKS: u64 = 10;

/// Saves the terminals for a restart if they changed since `saved`, with the
/// command lines last read: no `ps` every few seconds. Under the lock, so it
/// never overwrites what a shutdown saved.
pub(super) fn save_if_changed(state: &State, saved: &mut Vec<u8>) {
    let Ok(bytes) = serde_json::to_vec(&restart_terms(state, false)) else {
        return;
    };
    if bytes != *saved {
        write_restart_file(&bytes);
        *saved = bytes;
    }
}

/// Before shutting down to restart: notes each terminal's folder and whether
/// Claude Code is running in it. The processes die; their place doesn't.
pub(super) fn save_for_restart(state: &State) {
    if let Ok(bytes) = serde_json::to_vec(&restart_terms(state, true)) {
        write_restart_file(&bytes);
    }
}

/// Written beside it and renamed, so dying while writing leaves the last one.
fn write_restart_file(bytes: &[u8]) {
    let saved = restart_file().and_then(|path| {
        let partial = path.with_extension("partial");
        std::fs::write(&partial, bytes)?;
        Ok(std::fs::rename(partial, path)?)
    });
    if let Err(err) = saved {
        eprintln!("could not save the terminals for the restart: {err:#}");
    }
}

/// The terminals as a restart reopens them. `fresh` reads each one's command
/// line now; otherwise the one read when its foreground process last changed
/// (on Windows, where that's the shell, a Claude started later is missed).
fn restart_terms(state: &State, fresh: bool) -> Vec<Restarted> {
    state
        .terms
        .iter()
        .map(|(term, entry)| {
            let entry = entry.locked();
            let foreground = if fresh { entry.pty.foreground_pid() } else { entry.foreground };
            let args = if fresh { foreground.and_then(platform::process_args) } else { entry.agent_args.clone() };
            let args = args.filter(|args| agent_name(args).as_deref() == Some("claude"));
            let session = foreground.filter(|_| args.is_some()).and_then(claude_session);
            // `--resume <id>` finds the session only from the folder it began in.
            let cwd = session.as_ref().and_then(|session| session.cwd.clone());
            Restarted {
                term: *term,
                group: entry.group.clone(),
                cwd: cwd
                    .or_else(|| foreground.and_then(platform::process_cwd))
                    .unwrap_or_else(|| entry.cwd.path.clone()),
                cols: entry.emulator.columns() as u16,
                rows: entry.emulator.screen_lines() as u16,
                claude: args.and_then(|args| resume_command(&args, session.as_ref())),
            }
        })
        .collect()
}

/// On startup after a restart, or after an agent that died: opens the
/// terminals the previous agent had, with the same ids, and resumes Claude
/// Code where it was running. `resumed`: the Claude sessions already
/// resumed, by this file or an earlier one; a terminal that ran one of them
/// again is left out, as the agents of two protocols may both have it (each
/// one's UI opened it).
pub(super) fn restore_after_restart(state: &Shared, path: &Path, resumed: &mut HashSet<String>) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let _ = std::fs::remove_file(path);
    let terms: Vec<Restarted> = match serde_json::from_slice(&bytes) {
        Ok(terms) => terms,
        Err(err) => return eprintln!("invalid {}: {err:#}", path.display()),
    };
    for saved in terms {
        // The folder may be gone (a removed task): the terminal goes with it.
        // An id already taken is another agent's terminal, which the UI shows.
        if !saved.cwd.is_dir() || state.locked().terms.contains_key(&saved.term) {
            continue;
        }
        if let Some(session) = saved.claude.as_deref().and_then(resumed_session)
            && !resumed.insert(session.to_string())
        {
            continue;
        }
        let command = saved.claude.map(|command| format!("{command}\r").into_bytes());
        let created = create(state, Some(saved.term), saved.group, saved.cwd, None, saved.cols, saved.rows);
        match (created, command) {
            (Ok(_), Some(command)) => {
                // Typed into the shell, so it stays when Claude exits.
                if let Ok(entry) = state.locked().term(saved.term) {
                    entry.locked().pty.write(command);
                }
            }
            (Ok(_), None) => {}
            (Err(err), _) => eprintln!("could not reopen terminal {}: {err:#}", saved.term),
        }
    }
}
