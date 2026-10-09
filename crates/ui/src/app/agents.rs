//! The coding agents (Claude Code, Codex…) running in the terminals of every
//! server, as the agent reports them (see `Event::Agents`). They're the one
//! source of what each workspace is doing: a workspace's dot in the column
//! is the most urgent of its agents'. The Agents panel lists them all, a row
//! each with its workspace and state; a click on one goes to its terminal.
//! An agent's name is its terminal tab's (Rename, also from its row).

use proto::{AgentInfo, TermId};

use super::*;
use crate::terminals;

/// An agent's name being typed in its row: Enter gives it, Escape or a
/// click elsewhere leaves it as it was.
pub(super) struct AgentRename {
    host: SharedString,
    group: String,
    term: TermId,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl Den {
    /// The agents running on `host` now. One that stops working while its
    /// workspace isn't in front is marked done.
    pub(super) fn set_agents(&mut self, host: SharedString, agents: Vec<AgentInfo>, cx: &mut Context<Self>) {
        let before = self.agents.remove(&host).unwrap_or_default();
        for old in before.iter().filter(|old| old.working) {
            let key = TaskKey { host: host.clone(), path: PathBuf::from(&old.group) };
            let stopped = agents.iter().any(|new| new.term == old.term && new.group == old.group && !new.working);
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
        // A workspace just opened is still attaching its terminals: the
        // keyboard goes to it as soon as it's there.
        cx.spawn_in(window, async move |_, cx| {
            for _ in 0..100 {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                let focused = workspace.update_in(cx, |workspace, window, cx| workspace.focus_terminal(term, window, cx));
                if !matches!(focused, Ok(false)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Types the agent's name in its row: its terminal tab's.
    fn start_agent_rename(&mut self, host: SharedString, group: String, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let name = terminals::saved_term_name(&group, term).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.commit_agent_rename(window, cx),
            InputEvent::Blur => this.cancel_agent_rename(cx),
            _ => {}
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.agent_rename = Some(AgentRename { host, group, term, input, _subscription: subscription });
        cx.notify();
    }

    /// The typed name is the agent's tab's, in its workspace if it's open
    /// here, or saved for when it opens; none, and the tab goes back to its
    /// terminal's title.
    fn commit_agent_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(AgentRename { host, group, term, input, .. }) = self.agent_rename.take() else {
            return;
        };
        let name = Some(input.read(cx).value().trim().to_string()).filter(|name| !name.is_empty());
        let key = TaskKey { host, path: PathBuf::from(&group) };
        let renamed = self
            .workspaces
            .get(&key)
            .is_some_and(|workspace| workspace.update(cx, |workspace, cx| workspace.rename_terminal(term, name.clone(), cx)));
        if !renamed {
            terminals::rename_saved_term(&group, term, name);
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn cancel_agent_rename(&mut self, cx: &mut Context<Self>) {
        if self.agent_rename.take().is_some() {
            cx.notify();
        }
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

    /// The Agents panel: every agent, on every server, by workspace (in the
    /// column's order): its state's dot, its workspace and its state.
    pub(super) fn render_agents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        let several = self.agents.values().filter(|agents| !agents.is_empty()).count() > 1;
        let ordered: Vec<TaskKey> = self.ordered_all(cx).into_iter().map(|(key, _)| key).collect();
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

    /// An agent's row: its dot, its name if it was given one, and its
    /// workspace; what it's on, on hover. The dot is its state: no word for
    /// it ("working", "done"…), ever.
    fn render_agent(&self, host: &SharedString, agent: &AgentInfo, cx: &mut Context<Self>) -> AnyElement {
        let (dot, color, _) = self.agent_status(host, agent, cx);
        let theme = cx.theme();
        let title = agent_title(agent);
        let key = TaskKey { host: host.clone(), path: PathBuf::from(&agent.group) };
        let label = self.task(&key).map(row_label).unwrap_or_else(|| folder_name(&key.path).into());
        let name = terminals::saved_term_name(&agent.group, agent.term);
        let name_given = name.is_some();
        let (host, group, term) = (host.clone(), agent.group.clone(), agent.term);
        let input = self
            .agent_rename
            .as_ref()
            .filter(|rename| rename.host == host && rename.term == term)
            .map(|rename| rename.input.clone());
        let renaming = input.is_some();
        let weak = cx.entity().downgrade();
        h_flex()
            .id(SharedString::from(format!("agent-{host}-{term}")))
            .h(px(24.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
            .child(div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot))
            .map(|row| match input {
                Some(input) => row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.cancel_agent_rename(cx);
                            }
                        }))
                        .child(Input::new(&input).xsmall()),
                ),
                None => row
                    .children(name.map(|name| div().flex_none().max_w(px(160.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name)))
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .when(name_given, |el| el.text_color(theme.muted_foreground))
                            .child(label),
                    ),
            })
            .when(!renaming, |row| row.tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx)))
            .on_click(cx.listener({
                let (host, group) = (host.clone(), group.clone());
                move |this, _, window, cx| {
                    if !renaming {
                        this.open_agent(host.clone(), group.clone(), term, window, cx)
                    }
                }
            }))
            .context_menu(move |menu, _, _| {
                let (host, group) = (host.clone(), group.clone());
                menu.item(menu::item("Rename", &weak, move |this, window, cx| {
                    this.start_agent_rename(host.clone(), group.clone(), term, window, cx)
                }))
            })
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

#[cfg(test)]
mod tests {
    /// The workspaces' and the agents' rows show their state only as a dot.
    /// The words ("working", "done"…) were taken off four times and came
    /// back with rewrites of these rows: this fails if they do again.
    #[test]
    fn the_rows_have_no_word_for_the_state() {
        for (file, source) in [("app.rs", include_str!("../app.rs")), ("app/agents.rs", include_str!("agents.rs"))] {
            // The code, not its tests (this one names what it looks for).
            let code = source.split("#[cfg(test)]").next().unwrap_or_default();
            let code: String = code.lines().filter(|line| !line.trim_start().starts_with("//")).collect();
            for shown in [".child(state)", ".child(if state", "\"deleting\""] {
                assert!(!code.contains(shown), "{file} shows the agents' state in words ({shown}): only the dot says it");
            }
        }
    }
}
