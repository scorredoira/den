//! Which tasks are working, which wait for an answer, and which terminals
//! run a coding agent: from their output, their screens and their
//! foreground processes.

use crate::blocked;
use super::*;
use super::{claude::{claude_title, claude_working}, terminals::bottom_lines};

/// A task is working while its terminals produced output less than this long ago.
const WORKING_WINDOW: Duration = Duration::from_secs(2);

/// After this long without output, check whether the screen is asking for an answer.
const QUIET: Duration = Duration::from_millis(800);

/// Output counts as work once it keeps coming (no pause of `QUIET`) for this
/// long: a redraw or a stray line from an idle terminal is not work, and
/// would mark the task as finished again after it was seen.
const WORK_BURST: Duration = Duration::from_secs(1);

/// Every so many ticks of the foreground check (2 s), the terminals whose
/// agent may have started or ended under a wrapper are looked at again.
pub(super) const AGENT_RECHECK_TICKS: u64 = 4;

impl AgentTerm {
    /// Whether it's doing something. Claude Code says so in its title (see
    /// `claude_working`): its redraws (on a resize, a reconnect, a status
    /// line) are not work. Anything else works while its output keeps
    /// coming, with no pause of `QUIET`, for `WORK_BURST`, past what it
    /// printed on starting up.
    pub(super) fn working(&self) -> bool {
        if let Some(working) = self.claude_title().and_then(claude_working) {
            return working;
        }
        self.settled
            && self.last_output.elapsed() < WORKING_WINDOW
            && self.last_output.duration_since(self.burst_start) >= WORK_BURST
    }

    /// Its title, if Claude Code's and Claude Code still runs: one left by
    /// a Claude Code that died (a spinner, say) is not its. Where the
    /// foreground process can't be read (Windows), the title is all there is.
    fn claude_title(&self) -> Option<&str> {
        self.title
            .as_deref()
            .filter(|title| claude_title(title))
            .filter(|_| cfg!(windows) || self.agent.as_deref() == Some("claude"))
    }

    /// Notes output, and whether that makes it work.
    pub(super) fn note_output(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_output) >= QUIET {
            self.burst_start = now;
        }
        self.last_output = now;
        self.blocked = false;
        self.working()
    }
}

impl State {
    /// Marks a task whose terminal produced output as working.
    pub(super) fn mark_working(&mut self, group: String) {
        if self.working.insert(group.clone()) {
            self.broadcast_all(|| Event::Activity {
                group: group.clone(),
                working: true,
            });
        }
    }

    /// Considers tasks without a working terminal as stopped.
    pub(super) fn expire_activity(&mut self) {
        let mut working = HashSet::new();
        for entry in self.terms.values() {
            let mut entry = entry.locked();
            if !entry.settled && entry.last_output.elapsed() >= WORKING_WINDOW {
                entry.settled = true;
            }
            if entry.working() {
                working.insert(entry.group.clone());
            }
        }
        let stopped: Vec<String> = self.working.difference(&working).cloned().collect();
        for group in stopped {
            self.working.remove(&group);
            self.broadcast_all(|| Event::Activity {
                group: group.clone(),
                working: false,
            });
        }
        self.check_blocked();
    }

    /// Checks the screen of terminals that just went quiet (and weren't checked
    /// since their last output) and notifies about tasks that start waiting
    /// for an answer, or stop doing so.
    fn check_blocked(&mut self) {
        let mut now = HashSet::new();
        for entry in self.terms.values() {
            let mut entry = entry.locked();
            if entry.checked != entry.last_output && entry.last_output.elapsed() >= QUIET {
                entry.checked = entry.last_output;
                entry.blocked = blocked::is_blocked(&bottom_lines(&entry.emulator));
            }
            if entry.blocked {
                now.insert(entry.group.clone());
            }
        }
        let changed: Vec<(String, bool)> = now
            .difference(&self.blocked)
            .map(|group| (group.clone(), true))
            .chain(self.blocked.difference(&now).map(|group| (group.clone(), false)))
            .collect();
        self.blocked = now;
        for (group, blocked) in changed {
            self.broadcast_all(|| Event::Blocked { group: group.clone(), blocked });
        }
    }

    /// The terminals whose foreground process changed since last looked at,
    /// with the new one: whether it's an agent is read off the lock. With
    /// `recheck`, also those running a program that isn't an agent, or whose
    /// agent under a wrapper ended: a wrapper that runs the agent as its
    /// child (`claude-soonest`) starts it, or starts it again in another
    /// account, while it stays in the foreground.
    pub(super) fn foregrounds_changed(&mut self, recheck: bool) -> Vec<(TermId, Option<u32>)> {
        let mut changed = Vec::new();
        for (term, entry) in &self.terms {
            let mut entry = entry.locked();
            let foreground = entry.pty.foreground_pid();
            // A wrapper that execs the agent (`python3 claude-soonest` →
            // `claude`) keeps its pid: only its executable says it changed.
            let executable = foreground.and_then(platform::process_executable);
            let unsure = match entry.agent_pid {
                None => foreground != entry.pty.pid(),
                Some(agent) => agent != foreground.unwrap_or(agent) && !platform::process_alive(agent),
            };
            if foreground != entry.foreground || executable != entry.foreground_executable || (recheck && unsure) {
                entry.foreground = foreground;
                entry.foreground_executable = executable;
                changed.push((*term, foreground));
            }
        }
        changed
    }

    /// The terminals running an agent: those whose foreground process is
    /// one or, where that can't be read (Windows), whose title is Claude
    /// Code's.
    pub(super) fn agent_list(&self) -> Vec<AgentInfo> {
        let mut agents: Vec<AgentInfo> = self
            .terms
            .iter()
            .filter_map(|(term, entry)| {
                let entry = entry.locked();
                let name = entry.agent.clone().or_else(|| entry.claude_title().map(|_| "claude".to_string()))?;
                Some(AgentInfo {
                    term: *term,
                    group: entry.group.clone(),
                    name,
                    title: entry.title.clone(),
                    working: entry.working(),
                    blocked: entry.blocked,
                })
            })
            .collect();
        agents.sort_by_key(|agent| agent.term);
        agents
    }

    /// Tells every connection the agents, if they changed.
    pub(super) fn send_agents(&mut self) {
        let agents = self.agent_list();
        if agents != self.agents {
            self.agents = agents.clone();
            self.broadcast_all(|| Event::Agents { agents: agents.clone() });
        }
    }
}
