//! The agent's terminals: each one's pty, read on a thread of its own into
//! an emulator that keeps its screen, under a lock of its own.

use std::io::Read as _;

use alacritty_terminal::{
    Term,
    event::{Event as AlacEvent, EventListener},
    index::{Column, Line},
    term::Config,
    vte::ansi::Processor,
};

use crate::pty::Pty;
use super::*;

/// After a terminal's process ends, how long its output may take to be read.
const DRAIN: Duration = Duration::from_secs(1);

/// Collects the events of the agent's emulator.
#[derive(Clone, Default)]
pub(super) struct Listener_(Arc<Mutex<Vec<AlacEvent>>>);

impl EventListener for Listener_ {
    fn send_event(&self, event: AlacEvent) {
        self.0.locked().push(event);
    }
}

pub(super) struct AgentTerm {
    pub(super) group: String,
    pub(super) pty: Pty,
    pub(super) cwd: crate::shell_cwd::ShellCwd,
    pub(super) emulator: Term<Listener_>,
    pub(super) parser: Processor,
    events: Listener_,
    pub(super) title: Option<String>,
    /// The connections attached to it, each with its outbox: its output goes
    /// to them without the agent's lock.
    pub(super) subscribers: HashMap<ConnId, Outbox>,
    pub(super) last_output: Instant,
    /// When the output that keeps coming up to `last_output` began.
    pub(super) burst_start: Instant,
    /// Its first quiet moment has passed: what a shell or a resumed session
    /// prints on starting up is not work, so until then output doesn't mark
    /// the task as working (on an agent restart every task would end up
    /// "finished").
    pub(super) settled: bool,
    /// Whether the screen (no output since `last_output`) asks for an answer,
    /// and up to which output it was checked.
    pub(super) blocked: bool,
    pub(super) checked: Instant,
    /// Its foreground process when last looked at, with its executable (an
    /// `exec` keeps the pid), and the coding agent it runs (`claude`,
    /// `codex`…), if one, with its pid and command line: the foreground
    /// process's, or that of its child under a wrapper.
    pub(super) foreground: Option<u32>,
    pub(super) foreground_executable: Option<PathBuf>,
    pub(super) agent: Option<String>,
    pub(super) agent_pid: Option<u32>,
    pub(super) agent_args: Option<String>,
}

impl AgentTerm {
    pub(super) fn broadcast(&self, make: impl Fn() -> Event) {
        for outbox in self.subscribers.values() {
            let _ = outbox.send(ServerMessage::Event(make()));
        }
    }
}

/// A terminal has a lock of its own, so one that prints a lot holds up
/// only itself. Whoever takes both takes the agent's first: never the
/// agent's while holding a terminal's.
pub(super) type SharedTerm = Arc<Mutex<AgentTerm>>;

/// The last `count` lines of a terminal's history and screen as text, with
/// the rows a long line wrapped into joined again. Blank lines at the end
/// (the empty screen below the prompt) don't count. Read from the bottom,
/// only as far as needed: the agent's lock is held meanwhile.
pub(super) fn last_lines<T: EventListener>(term: &Term<T>, count: usize) -> Vec<String> {
    use alacritty_terminal::term::cell::Flags;
    let grid = term.grid();
    let last = Column(grid.columns() - 1);
    let top = -(grid.history_size() as i32);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut line = grid.screen_lines() as i32 - 1;
    while line >= top && lines.len() < count {
        let row = &grid[Line(line)];
        let text: String = (0..grid.columns())
            .map(|col| &row[Column(col)])
            .filter(|cell| !cell.flags.contains(Flags::WIDE_CHAR_SPACER))
            .map(|cell| cell.c)
            .collect();
        current.insert_str(0, &text);
        // A row starts its line unless the one above wrapped into it.
        let continues = line > top && grid[Line(line - 1)][last].flags.contains(Flags::WRAPLINE);
        if !continues {
            let text = std::mem::take(&mut current).trim_end().to_string();
            if !(lines.is_empty() && text.is_empty()) {
                lines.push(text);
            }
        }
        line -= 1;
    }
    lines.reverse();
    lines
}

/// The last non-empty lines on screen (without the history).
pub(super) fn bottom_lines<T: EventListener>(term: &Term<T>) -> Vec<String> {
    let grid = term.grid();
    let mut lines: Vec<String> = (0..grid.screen_lines() as i32)
        .map(|line| {
            let row = &grid[Line(line)];
            let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
            text.trim_end().to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(25);
    lines.drain(..skip);
    lines
}

/// The largest screen a terminal has: its emulator keeps a grid of that
/// size, and history as wide.
const MAX_TERM_SIZE: u16 = 1000;

/// A terminal's size as asked for, within what one can be.
pub(super) fn term_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(2, MAX_TERM_SIZE), rows.clamp(1, MAX_TERM_SIZE))
}

pub(super) fn gone(term: TermId) -> anyhow::Error {
    anyhow::anyhow!("terminal {term} no longer exists")
}

pub(super) fn create(
    state: &Shared,
    id: Option<TermId>,
    group: String,
    cwd: PathBuf,
    command: Option<Vec<String>>,
    cols: u16,
    rows: u16,
) -> Result<Response> {
    let (cols, rows) = term_size(cols, rows);
    // The id goes in the terminal's environment, so it's taken before starting it.
    let term = {
        let mut state = state.locked();
        let term = id.unwrap_or(state.next_term);
        state.next_term = state.next_term.max(term + 1);
        term
    };
    let (pty, io) = Pty::spawn(term, &cwd, command.as_deref(), cols, rows)?;
    start(state, term, group, cwd, (pty, io), cols, rows, None);
    Ok(Response::TermCreated { term })
}

/// What a terminal the previous agent left running had on screen.
pub(super) struct Screen {
    pub(super) data: Vec<u8>,
    pub(super) title: Option<String>,
}

/// Puts a started terminal (or one taken over, with its `screen`) in the
/// state, and starts reading its output and waiting for its process.
#[allow(clippy::too_many_arguments)]
pub(super) fn start(state: &Shared, term: TermId, group: String, cwd: PathBuf, (pty, io): (Pty, crate::pty::PtyIo), cols: u16, rows: u16, screen: Option<Screen>) {
    let events = Listener_::default();
    let mut emulator = Term::new(
        Config::default(),
        &TermSize::new(cols as usize, rows as usize),
        events.clone(),
    );
    let mut parser = Processor::new();
    let mut title = None;
    let settled = screen.is_some();
    if let Some(screen) = screen {
        parser.advance(&mut emulator, &screen.data);
        events.0.locked().clear();
        title = screen.title;
    }
    let entry = Arc::new(Mutex::new(AgentTerm {
        group,
        pty,
        cwd: crate::shell_cwd::ShellCwd::new(cwd),
        emulator,
        parser,
        events,
        title,
        subscribers: HashMap::new(),
        last_output: Instant::now(),
        burst_start: Instant::now(),
        settled,
        blocked: false,
        checked: Instant::now(),
        foreground: None,
        foreground_executable: None,
        agent: None,
        agent_pid: None,
        agent_args: None,
    }));
    // The reader doesn't keep it: closing it drops it, which kills its process.
    let weak = Arc::downgrade(&entry);
    {
        let mut state = state.locked();
        state.terms.insert(term, entry);
        state.update_idle();
    }

    let mut output = io.output;
    // Dropped when the reader is done, which the end of its process waits for.
    let (drained_tx, drained) = mpsc::channel::<()>();
    std::thread::spawn({
        let state = state.clone();
        move || {
            let _drained = drained_tx;
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = match output.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let Some(entry) = weak.upgrade() else {
                    // ConPTY may emit final output while closing. Keep draining
                    // until EOF; its close runs on a separate thread.
                    continue;
                };
                // Parsed under the terminal's own lock, not the agent's: a
                // terminal printing a lot doesn't hold up the others.
                let working = {
                    let mut entry = entry.locked();
                    let entry = &mut *entry;
                    entry.cwd.advance(&buf[..n]);
                    entry.parser.advance(&mut entry.emulator, &buf[..n]);
                    let events = std::mem::take(&mut *entry.events.0.locked());
                    let mut title_changed = false;
                    for event in events {
                        match event {
                            // Replies to queries (cursor position, attributes…)
                            // come from the agent, which is always present.
                            AlacEvent::PtyWrite(text) => entry.pty.write(text.into_bytes()),
                            AlacEvent::Title(title) => {
                                entry.title = Some(title);
                                title_changed = true;
                            }
                            AlacEvent::ResetTitle => {
                                entry.title = None;
                                title_changed = true;
                            }
                            _ => {}
                        }
                    }
                    let data = buf[..n].to_vec();
                    let working = entry.note_output().then(|| entry.group.clone());
                    entry.broadcast(|| Event::TermOutput {
                        term,
                        data: data.clone(),
                    });
                    if title_changed {
                        let title = entry.title.clone();
                        entry.broadcast(|| Event::TermTitle {
                            term,
                            title: title.clone(),
                        });
                    }
                    working
                };
                drop(entry);
                if let Some(group) = working {
                    state.locked().mark_working(group);
                }
            }
        }
    });

    let mut child = io.child;
    std::thread::spawn({
        let state = state.clone();
        move || {
            let _ = child.wait();
            // What it printed last is still to be read: it goes out before
            // the exit. Not for long, as a program it left running may keep
            // the pty open; nor on Windows, where ConPTY ends its output only
            // once closed.
            if cfg!(unix) {
                let _ = drained.recv_timeout(DRAIN);
            }
            let mut state = state.locked();
            state.broadcast(term, || Event::TermExit { term });
            state.terms.remove(&term);
            state.update_idle();
        }
    });
}

#[cfg(test)]
mod blocked_tests {
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::bottom_lines;
    use crate::blocked::is_blocked;

    #[test]
    fn permission_prompt_on_a_real_screen() {
        let mut term = Term::new(Config::default(), &TermSize::new(60, 20), VoidListener);
        let mut parser: Processor = Processor::new();
        let screen = "● Bash(ls)\r\n\x1b[2m──────────────────────────────\x1b[0m\r\n Bash command\r\n   ls -la\r\n Do you want to proceed?\r\n \x1b[36m❯ 1. Yes\x1b[0m\r\n   2. No\r\n";
        parser.advance(&mut term, screen.as_bytes());
        let lines = bottom_lines(&term);
        assert!(lines.iter().any(|line| line.contains("Do you want to proceed?")), "{lines:?}");
        assert!(is_blocked(&lines));

        let mut idle = Term::new(Config::default(), &TermSize::new(60, 20), VoidListener);
        parser.advance(&mut idle, "● Done.\r\n──────────────────────────────\r\n❯ \r\n".as_bytes());
        assert!(!is_blocked(&bottom_lines(&idle)));
    }
}

#[cfg(test)]
mod read_tests {
    use alacritty_terminal::{
        Term,
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::last_lines;

    #[test]
    fn joins_wrapped_lines_and_keeps_the_last_ones() {
        let mut term = Term::new(Config::default(), &TermSize::new(10, 5), VoidListener);
        let mut parser: Processor = Processor::new();
        let text = "one\r\ntwo\r\nthree\r\nabcdefghijklmnop\r\n$ ";
        parser.advance(&mut term, text.as_bytes());
        assert_eq!(last_lines(&term, 100), ["one", "two", "three", "abcdefghijklmnop", "$"]);
        assert_eq!(last_lines(&term, 2), ["abcdefghijklmnop", "$"]);
    }
}

#[cfg(all(test, unix))]
mod lock_tests {
    use super::*;

    #[test]
    fn a_busy_terminal_holds_up_only_itself() {
        let state: Shared = Arc::default();
        let dir = std::env::temp_dir();
        let cat = || Some(vec!["/bin/cat".to_string()]);
        let (Ok(Response::TermCreated { term: busy }), Ok(Response::TermCreated { term: other })) = (
            create(&state, None, "a".into(), dir.clone(), cat(), 80, 24),
            create(&state, None, "b".into(), dir, cat(), 80, 24),
        ) else {
            panic!("no terminals");
        };
        // As while its reader parses a large output.
        let busy = state.locked().term(busy).unwrap();
        let parsing = busy.locked();
        let (done, finished) = mpsc::channel();
        std::thread::spawn({
            let state = state.clone();
            move || {
                handle(&state, 0, Request::TermInput { term: other, data: b"x".to_vec() }).unwrap();
                handle(&state, 0, Request::TermResize { term: other, cols: 100, rows: 30 }).unwrap();
                handle(&state, 0, Request::TermRead { term: other, lines: 1 }).unwrap();
                let _ = done.send(());
            }
        });
        finished.recv_timeout(Duration::from_secs(2)).expect("the other terminal waited");
        drop(parsing);
        drop(busy);
        state.locked().terms.clear();
    }

    #[test]
    fn a_terminal_is_never_too_large() {
        let state: Shared = Arc::default();
        let cat = Some(vec!["/bin/cat".to_string()]);
        let Ok(Response::TermCreated { term }) = create(&state, None, "a".into(), std::env::temp_dir(), cat, u16::MAX, u16::MAX) else {
            panic!("no terminal");
        };
        let size = || {
            let entry = state.locked().term(term).unwrap();
            let entry = entry.locked();
            (entry.emulator.columns(), entry.emulator.screen_lines())
        };
        assert_eq!(size(), (1000, 1000));
        handle(&state, 0, Request::TermResize { term, cols: 0, rows: u16::MAX }).unwrap();
        assert_eq!(size(), (2, 1000));
        state.locked().terms.clear();
    }

    #[test]
    fn a_terminal_exits_after_its_last_output() {
        let state: Shared = Arc::default();
        let (outbox, events) = Outbox::new(Arc::new(|| {}));
        state.locked().clients.insert(1, outbox);
        let script = "sleep 0.3; printf A; sleep 0.3; printf THE-END";
        let command = Some(vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()]);
        let Ok(Response::TermCreated { term }) = create(&state, None, "a".into(), std::env::temp_dir(), command, 80, 24) else {
            panic!("no terminal");
        };
        handle(&state, 1, Request::TermAttach { term }).unwrap();
        // As while its reader parses a large output: what it printed last is
        // still to be read when the process is gone.
        let entry = state.locked().term(term).unwrap();
        let parsing = entry.lock().unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        drop(parsing);
        drop(entry);
        let mut output = Vec::new();
        loop {
            let event = events.recv_timeout(Duration::from_secs(10)).expect("no exit");
            match &event.message {
                ServerMessage::Event(Event::TermOutput { data, .. }) => output.extend_from_slice(data),
                ServerMessage::Event(Event::TermExit { .. }) => break,
                _ => {}
            }
        }
        assert!(output.ends_with(b"THE-END"), "lost the end");
    }
}
