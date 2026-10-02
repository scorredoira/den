//! The Agents panel: every terminal running a coding agent (Claude Code,
//! Codex…), on every server, under its workspace, with its state. A click
//! goes to that workspace with that terminal in front.

use proto::{AgentInfo, TermId};

use super::*;

impl Sik {
    /// The agents running on `host` now (see `Event::Agents`). One that
    /// stops working while its workspace isn't in front is marked finished.
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

    /// The agents of workspace `key` have been looked at.
    pub(super) fn agents_seen(&mut self, key: &TaskKey) {
        let Some(agents) = self.agents.get(&key.host) else {
            return;
        };
        let terms: Vec<TermId> = agents.iter().filter(|agent| Path::new(&agent.group) == key.path).map(|agent| agent.term).collect();
        self.agents_attention.retain(|(host, term)| *host != key.host || !terms.contains(term));
    }

    /// Its dot as the workspaces': waiting for an answer, working, finished
    /// without being looked at, or idle; and the word for it.
    fn agent_status(&self, host: &SharedString, agent: &AgentInfo, cx: &App) -> (&'static str, Hsla, &'static str) {
        let theme = cx.theme();
        if agent.blocked {
            ("●", theme.danger, "waiting")
        } else if agent.working {
            ("◐", theme.warning, "working")
        } else if self.agents_attention.contains(&(host.clone(), agent.term)) {
            ("●", theme.success, "done")
        } else {
            ("○", theme.muted_foreground, "")
        }
    }

    /// The most urgent of the agents, for the panel's icon.
    pub(super) fn agents_badge(&self, cx: &App) -> Option<Hsla> {
        self.agents
            .iter()
            .flat_map(|(host, agents)| agents.iter().map(move |agent| (host, agent)))
            .map(|(host, agent)| self.agent_status(host, agent, cx))
            .map(|(dot, color, _)| (dot, color))
            .filter(|(dot, color)| urgency(dot, *color, cx) > 0)
            .max_by_key(|(dot, color)| urgency(dot, *color, cx))
            .map(|(_, color)| color)
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

    pub(super) fn render_agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut rows: Vec<AnyElement> = Vec::new();
        let several = self.agents.values().filter(|agents| !agents.is_empty()).count() > 1;
        let ordered: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        for host in &self.hosts {
            let Some(agents) = self.agents.get(&host.name).filter(|agents| !agents.is_empty()) else {
                continue;
            };
            if several {
                rows.push(
                    div()
                        .px_3()
                        .pt_2()
                        .text_ui_small(cx)
                        .text_color(theme.muted_foreground)
                        .child(host.name.clone())
                        .into_any_element(),
                );
            }
            // By workspace, in the column's order; those it doesn't list, last.
            let mut groups: Vec<&str> = agents.iter().map(|agent| agent.group.as_str()).collect();
            groups.dedup();
            groups.sort_by_key(|group| {
                ordered
                    .iter()
                    .position(|key| key.host == host.name && key.path == Path::new(group))
                    .unwrap_or(usize::MAX)
            });
            groups.dedup();
            for group in groups {
                let key = TaskKey { host: host.name.clone(), path: PathBuf::from(group) };
                let label = self.task(&key).map(task_label).unwrap_or_else(|| folder_name(Path::new(group)));
                let active = self.active.as_ref() == Some(&key);
                rows.push(
                    h_flex()
                        .h(px(24.))
                        .px_3()
                        .gap_2()
                        .text_ui_small(cx)
                        .text_color(if active { theme.sidebar_foreground } else { theme.muted_foreground })
                        .child(svg().path(if self.task(&key).is_some_and(|task| !task.main) { "icons/git-branch.svg" } else { "icons/folder.svg" }).size(px(12.)).flex_none().text_color(theme.muted_foreground))
                        .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label))
                        .into_any_element(),
                );
                for agent in agents.iter().filter(|agent| agent.group == group) {
                    rows.push(self.render_agent(&host.name, agent, cx));
                }
            }
        }
        let empty = rows.is_empty().then(|| {
            div()
                .px_3()
                .py_2()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
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

    fn render_agent(&self, host: &SharedString, agent: &AgentInfo, cx: &mut Context<Self>) -> AnyElement {
        let (dot, color, state) = self.agent_status(host, agent, cx);
        let theme = cx.theme();
        let title = agent_title(agent);
        let (host, group, term) = (host.clone(), agent.group.clone(), agent.term);
        h_flex()
            .id(SharedString::from(format!("agent-{host}-{term}")))
            .h(px(26.))
            .pl(px(ROW_INDENT))
            .pr_3()
            .gap_2()
            .text_ui(cx)
            .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
            .child(div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title.clone()))
            .child(div().flex_1())
            .child(div().flex_none().text_ui_small(cx).text_color(color).child(state))
            .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.open_agent(host.clone(), group.clone(), term, window, cx)))
            .into_any_element()
    }
}

/// What the agent is on: Claude Code's title without its spinner, or the
/// agent's name.
fn agent_title(agent: &AgentInfo) -> SharedString {
    let title = agent
        .title
        .as_deref()
        .map(|title| title.trim_start_matches(|ch: char| ch == '✳' || ('\u{2800}'..='\u{28FF}').contains(&ch)).trim())
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
