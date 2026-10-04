//! The coding agents (Claude Code, Codex…) running in the terminals of every
//! server, as the agent reports them (see `Event::Agents`). They're the one
//! source of what each workspace is doing: a workspace's dot in the column
//! is the most urgent of its agents'. The Agents panel lists them all, under
//! their workspaces; a click on one goes to its terminal.

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
    pub(super) fn agent_status(&self, host: &SharedString, agent: &AgentInfo, cx: &App) -> (&'static str, Hsla, &'static str) {
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

    /// The most urgent of the agents, for the panel's icon.
    pub(super) fn agents_badge(&self, cx: &App) -> Option<Hsla> {
        self.agents
            .iter()
            .flat_map(|(host, agents)| agents.iter().map(move |agent| (host, agent)))
            .map(|(host, agent)| self.agent_status(host, agent, cx))
            .filter(|(dot, color, _)| urgency(dot, *color, cx) > 0)
            .max_by_key(|(dot, color, _)| urgency(dot, *color, cx))
            .map(|(_, color, _)| color)
    }

    /// The Agents panel: every agent, on every server, under its workspace
    /// (in the column's order), with what it's on and its state's dot.
    pub(super) fn render_agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        let several = self.agents.values().filter(|agents| !agents.is_empty()).count() > 1;
        let ordered: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        for host in &self.hosts {
            let Some(agents) = self.agents.get(&host.name).filter(|agents| !agents.is_empty()).cloned() else {
                continue;
            };
            if several {
                rows.push(
                    div()
                        .px_3()
                        .pt_2()
                        .text_ui_small(cx)
                        .text_color(cx.theme().muted_foreground)
                        .child(host.name.clone())
                        .into_any_element(),
                );
            }
            // By workspace, in the column's order; those it doesn't list, last.
            let mut groups: Vec<&str> = agents.iter().map(|agent| agent.group.as_str()).collect();
            groups.sort();
            groups.dedup();
            groups.sort_by_key(|group| {
                ordered
                    .iter()
                    .position(|key| key.host == host.name && key.path == Path::new(group))
                    .unwrap_or(usize::MAX)
            });
            for group in groups {
                let key = TaskKey { host: host.name.clone(), path: PathBuf::from(group) };
                rows.push(self.render_agents_workspace(&key, cx));
                for agent in agents.iter().filter(|agent| agent.group == group) {
                    rows.push(self.render_agent(&host.name, agent, cx));
                }
            }
        }
        let theme = cx.theme();
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child("No agents running. Claude Code, Codex and the like show here while they run in a terminal.")
        });
        v_flex()
            .id("agents")
            .size_full()
            .py_1()
            .overflow_y_scroll()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .children(rows)
            .children(empty)
    }

    /// A workspace's name above its agents; a click goes to it.
    fn render_agents_workspace(&self, key: &TaskKey, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let task = self.task(key);
        let label = task.map(column_label).unwrap_or_else(|| folder_name(&key.path).into());
        let icon = task.map(kind_icon).unwrap_or("icons/folder.svg");
        let active = self.active.as_ref() == Some(key);
        let target = key.clone();
        h_flex()
            .id(SharedString::from(format!("agents-workspace-{}", key.config())))
            .h(px(26.))
            .mt_1()
            .px_3()
            .gap_2()
            .text_ui(cx)
            // Where you are, not a selection: bold rather than the selection's band.
            .when(active, |el| el.font_weight(FontWeight::SEMIBOLD))
            .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
            .child(svg().path(icon).size(px(14.)).flex_none().text_color(workspace_color(key)))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
            .tooltip({
                let path = key.path.display().to_string();
                move |window, cx| Tooltip::new(path.clone()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| this.activate(target.clone(), window, cx)))
            .into_any_element()
    }

    fn render_agent(&self, host: &SharedString, agent: &AgentInfo, cx: &mut Context<Self>) -> AnyElement {
        let (dot, color, _) = self.agent_status(host, agent, cx);
        let theme = cx.theme();
        let title = agent_title(agent);
        let (host, group, term) = (host.clone(), agent.group.clone(), agent.term);
        h_flex()
            .id(SharedString::from(format!("agent-{host}-{term}")))
            .h(px(24.))
            .pl(px(ROW_INDENT + 10.))
            .pr_3()
            .gap_2()
            .text_ui_small(cx)
            .text_color(theme.muted_foreground)
            .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
            .child(div().flex_none().w(px(12.)).text_color(color).child(dot))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title.clone()))
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.open_agent(host.clone(), group.clone(), term, window, cx)))
            .into_any_element()
    }
}

/// What the agent is on: its title without its spinner, or the
/// agent's name.
pub(super) fn agent_title(agent: &AgentInfo) -> SharedString {
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
