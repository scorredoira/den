//! The coding agents (Claude Code, Codex…) running in the terminals of every
//! server, as the agent reports them (see `Event::Agents`). They're the one
//! source of what each workspace is doing: in the workspaces column, each
//! has a row under its workspace with its state, and a workspace's dot is
//! the most urgent of its agents'. A click on one goes to its terminal.

use proto::{AgentInfo, TermId};

use super::*;

impl Den {
    /// The agents running on `host` now. One that stops working while its
    /// workspace isn't in front is marked done.
    pub(super) fn set_agents(&mut self, host: SharedString, agents: Vec<AgentInfo>, cx: &mut Context<Self>) {
        let before = self.agents.remove(&host).unwrap_or_default();
        for old in before.iter().filter(|old| old.working) {
            let key = TaskKey { host: host.clone(), path: PathBuf::from(&old.group) };
            let stopped = agents.iter().any(|new| new.term == old.term && !new.working);
            if stopped && self.active.as_ref() != Some(&key) {
                self.agents_attention.insert((host.clone(), old.term));
            }
        }
        self.agents_attention
            .retain(|(other, term)| *other != host || agents.iter().any(|agent| agent.term == *term));
        self.agents.insert(host, agents);
        cx.notify();
    }

    /// The agents running in workspace `key`.
    pub(super) fn workspace_agents(&self, key: &TaskKey) -> Vec<&AgentInfo> {
        self.agents
            .get(&key.host)
            .map(|agents| agents.iter().filter(|agent| Path::new(&agent.group) == key.path).collect())
            .unwrap_or_default()
    }

    /// The agents of workspace `key` have been looked at.
    pub(super) fn agents_seen(&mut self, key: &TaskKey) {
        let terms: Vec<TermId> = self.workspace_agents(key).iter().map(|agent| agent.term).collect();
        self.agents_attention.retain(|(host, term)| *host != key.host || !terms.contains(term));
    }

    /// An agent's dot: waiting for an answer, working, done without being
    /// looked at, or idle; and the word for it.
    fn agent_status(&self, host: &SharedString, agent: &AgentInfo, cx: &App) -> (&'static str, Hsla, &'static str) {
        let theme = cx.theme();
        if agent.blocked {
            ("●", theme.danger, "waiting")
        } else if agent.working {
            ("◐", theme.warning, "working")
        } else if self.agents_attention.contains(&(host.clone(), agent.term)) {
            ("●", theme.success, "done")
        } else {
            ("○", theme.muted_foreground, "idle")
        }
    }

    /// The most urgent state of workspace `key`'s agents, idle without any.
    pub(super) fn workspace_state(&self, key: &TaskKey, cx: &App) -> (&'static str, Hsla, &'static str) {
        self.workspace_agents(key)
            .into_iter()
            .map(|agent| self.agent_status(&key.host, agent, cx))
            .max_by_key(|(dot, color, _)| urgency(dot, *color, cx))
            .unwrap_or(("○", cx.theme().muted_foreground, "idle"))
    }

    /// Goes to the agent's workspace, its terminal in front and with the keyboard.
    fn open_agent(&mut self, host: SharedString, group: String, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let key = TaskKey { host, path: PathBuf::from(group) };
        self.activate(key.clone(), window, cx);
        let Some(workspace) = self.workspaces.get(&key).cloned() else {
            return;
        };
        if workspace.update(cx, |workspace, cx| workspace.focus_terminal(term, window, cx)) {
            return;
        }
        // A workspace just opened is still attaching its terminals.
        cx.spawn_in(window, async move |_, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            workspace.update_in(cx, |workspace, window, cx| workspace.focus_terminal(term, window, cx)).ok();
        })
        .detach();
    }

    /// The rows of workspace `key`'s agents, `indent` from the left: what
    /// each is on and its state.
    pub(super) fn render_agents_of(&self, key: &TaskKey, indent: f32, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let agents: Vec<AgentInfo> = self.workspace_agents(key).into_iter().cloned().collect();
        agents.iter().map(|agent| self.render_agent(&key.host, agent, indent, cx)).collect()
    }

    fn render_agent(&self, host: &SharedString, agent: &AgentInfo, indent: f32, cx: &mut Context<Self>) -> AnyElement {
        // The dot's color says it all; the word is for `den workspaces`.
        let (dot, color, _) = self.agent_status(host, agent, cx);
        let theme = cx.theme();
        let title = agent_title(agent);
        let (host, group, term) = (host.clone(), agent.group.clone(), agent.term);
        h_flex()
            .id(SharedString::from(format!("agent-{host}-{term}")))
            .h(px(24.))
            .pl(px(indent))
            .pr_3()
            .gap_2()
            .text_ui_small(cx)
            .text_color(theme.muted_foreground)
            .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
            .child(div().flex_none().w(px(12.)).text_color(color).child(dot))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.open_agent(host.clone(), group.clone(), term, window, cx)
            }))
            .into_any_element()
    }
}

/// What the agent is on: its title without its spinner, or the
/// agent's name.
fn agent_title(agent: &AgentInfo) -> SharedString {
    let title = agent
        .title
        .as_deref()
        // Whatever spinner it starts with (✳, braille, ◐…).
        .map(|title| title.trim_start_matches(|ch: char| !ch.is_alphanumeric()).trim())
        .filter(|title| !title.is_empty());
    match title {
        Some(title) => title.to_string().into(),
        None => {
            let mut name = agent.name.clone();
            if let Some(first) = name.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            name.into()
        }
    }
}
