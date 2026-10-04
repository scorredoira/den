//! Cmd-Alt-E (Ctrl-Tab on Linux and Windows), as macOS's Cmd-Tab: the workspaces
//! being worked on (the previous one and those with a coding agent, the ones
//! waiting for an answer first), with the previous one selected; each E (or ↓)
//! selects the next, Shift-E (or ↑) the one before, letting go of Cmd enters
//! it and Esc stays.
//!
//! Cmd-E enters the next one with no list, so while Cmd is down the
//! workspaces column shows where it went (drawn over the window if it was
//! hidden, so nothing beside it moves) and, at the bottom, what's next there
//! and what its agents are on; held down, it waits to be let go.

use super::*;

/// The workspace Cmd-E just entered.
pub(super) struct Notice {
    key: TaskKey,
    /// E is still down: its repeats go nowhere. Off the Mac: there its
    /// key-up never comes while Cmd is down (see `repeating`).
    held: bool,
}

pub(super) struct Switcher {
    /// The most recently used first: the one in front.
    keys: Vec<TaskKey>,
    selected: usize,
}

impl Den {
    /// Cmd-Alt-E: opens the switcher on the previous workspace or, open, selects
    /// the next one.
    pub(super) fn previous_task(&mut self, _: &PreviousTask, window: &mut Window, cx: &mut Context<Self>) {
        if self.switcher.is_some() {
            self.move_switcher(1, cx);
            return;
        }
        let keys = self.recently_used(cx);
        if keys.len() < 2 {
            return;
        }
        // From the menu, with no Cmd to let go of: straight to it.
        if !window.modifiers().secondary() {
            self.activate(keys[1].clone(), window, cx);
            return;
        }
        self.switcher = Some(Switcher { keys, selected: 1 });
        cx.notify();
    }

    /// Cmd-E, and Cmd-Alt-Shift-E: straight into the next workspace in
    /// the column (the first after the last), only those with a coding agent
    /// or any of them.
    pub(super) fn next_task(&mut self, with_agent: bool, window: &mut Window, cx: &mut Context<Self>) {
        if repeating() || self.notice.as_ref().is_some_and(|notice| notice.held) {
            return;
        }
        let column: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        let start = self.active.as_ref().and_then(|active| column.iter().position(|key| key == active));
        let after = start.map_or(0, |ix| ix + 1);
        let round: Vec<&TaskKey> = column.iter().filter(|key| !with_agent || !self.workspace_agents(key).is_empty()).collect();
        let next = (0..column.len())
            .map(|step| &column[(after + step) % column.len()])
            .find(|key| Some(*key) != self.active.as_ref() && round.contains(key));
        let Some(key) = next.cloned() else {
            return;
        };
        self.activate(key.clone(), window, cx);
        // From the menu, with no Cmd to let go of: nothing to say.
        if window.modifiers().secondary() {
            self.notice = Some(Notice { key, held: !cfg!(target_os = "macos") });
            cx.notify();
        }
    }

    /// E let go: another Cmd-E goes to the next workspace.
    pub(super) fn switcher_key_up(&mut self, event: &KeyUpEvent) {
        if event.keystroke.key == "e"
            && let Some(notice) = &mut self.notice
        {
            notice.held = false;
        }
    }

    /// Cmd-E's notice: the workspaces column, over the window if it's
    /// hidden, and at the bottom the next of the workspace's notes and what
    /// its agents are on. Clicks go through it.
    pub(super) fn render_notice(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.notice.as_ref()?;
        let theme = cx.theme().clone();
        let column = (!self.tasks_shown(cx)).then(|| {
            // As wide as when it shows.
            let width = Config::get(cx).layout.side_width;
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(ACTIVITY_WIDTH))
                .w(config::width(width, 160., 800.))
                .border_r_1()
                .border_color(theme.border)
                // Only to its right, over what it covers.
                .shadow(vec![BoxShadow {
                    color: hsla(0., 0., 0., 0.25),
                    offset: point(px(6.), px(0.)),
                    blur_radius: px(12.),
                    spread_radius: px(-2.),
                    inset: false,
                }])
                .child(self.render_column(true, cx))
        });
        // What's next there, from its notes, and its agents.
        let next = crate::notes::first_line(&notice.key.config(), cx);
        let agents: Vec<_> = self
            .workspace_agents(&notice.key)
            .into_iter()
            .take(3)
            .map(|agent| {
                let (dot, color, _) = self.agent_status(&notice.key.host, agent, cx);
                h_flex()
                    .gap_2()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_none().w(px(14.)).text_color(color).child(dot))
                    .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(super::agents::agent_title(agent)))
            })
            .collect();
        let banner = (next.is_some() || !agents.is_empty()).then(|| {
            let width = f32::from(window.viewport_size().width);
            div().absolute().bottom(px(48.)).left_0().right_0().flex().justify_center().child(
                v_flex()
                    .max_w(px((width * 0.6).max(320.)))
                    .px_4()
                    .py_2()
                    .gap_1()
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .shadow_lg()
                    .text_ui(cx)
                    .text_color(theme.popover_foreground)
                    .children(next.map(|next| {
                        h_flex()
                            .gap_2()
                            .child(svg().path("icons/sticky-note-text.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(next))
                    }))
                    .children(agents),
            )
        });
        Some(div().absolute().top_0().left_0().size_full().children(column).children(banner).into_any_element())
    }

    /// Selects `by` further down the list (up if negative), round the ends.
    fn move_switcher(&mut self, by: isize, cx: &mut Context<Self>) {
        if let Some(switcher) = &mut self.switcher {
            let len = switcher.keys.len() as isize;
            switcher.selected = (switcher.selected as isize + by).rem_euclid(len) as usize;
            cx.notify();
        }
    }

    /// While it's open, its keys: its shortcut's key (E, or Tab off the Mac)
    /// and that with Shift, Tab and Shift-Tab, the arrows, Esc. True if the
    /// key was its.
    pub(super) fn switcher_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if self.switcher.is_none() {
            return false;
        }
        let own = SHORTCUTS
            .iter()
            .find(|shortcut| shortcut.id == "PreviousTask")
            .and_then(|shortcut| shortcuts::keys(shortcut, cx))
            .is_some_and(|keys| keys.key == keystroke.key);
        match keystroke.key.as_str() {
            _ if own && keystroke.modifiers.shift => self.move_switcher(-1, cx),
            _ if own => self.move_switcher(1, cx),
            "tab" if keystroke.modifiers.shift => self.move_switcher(-1, cx),
            "down" | "right" | "tab" => self.move_switcher(1, cx),
            "up" | "left" => self.move_switcher(-1, cx),
            "escape" => self.close_switcher(cx),
            _ => return false,
        }
        true
    }

    /// The workspaces being worked on: the one in front, the previous one (so
    /// a quick Cmd-Alt-E goes back), then those with a coding agent, the ones
    /// waiting for an answer first, each the most recently used first. With
    /// no agent anywhere else, every workspace in the column.
    fn recently_used(&self, cx: &App) -> Vec<TaskKey> {
        let column: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        let recent = Config::get(cx).recent.iter().map(|recent| TaskKey { host: recent.host.clone().into(), path: recent.path.clone() });
        let mut keys: Vec<TaskKey> = Vec::new();
        for key in self.active.iter().cloned().chain(recent).chain(column.iter().cloned()) {
            if !keys.contains(&key) && column.contains(&key) {
                keys.push(key);
            }
        }
        let rest = keys.split_off(keys.len().min(2));
        let (waiting, others): (Vec<TaskKey>, Vec<TaskKey>) = rest
            .iter()
            .filter(|key| !self.workspace_agents(key).is_empty())
            .cloned()
            .partition(|key| {
                let (dot, color, _) = self.workspace_state(key, cx);
                urgency(dot, color, cx) == 3
            });
        if waiting.is_empty() && others.is_empty() {
            keys.extend(rest);
        } else {
            keys.extend(waiting.into_iter().chain(others));
        }
        keys
    }

    /// Cmd let go: enters the one selected, and Cmd-E's notice goes.
    pub(super) fn switcher_modifiers(&mut self, modifiers: &Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        if modifiers.secondary() {
            return;
        }
        if self.notice.take().is_some() {
            cx.notify();
        }
        if let Some(switcher) = self.switcher.take() {
            if let Some(key) = switcher.keys.get(switcher.selected) {
                self.activate(key.clone(), window, cx);
            }
            cx.notify();
        }
    }

    /// Esc, or a click outside: stays where it was.
    pub(super) fn close_switcher(&mut self, cx: &mut Context<Self>) {
        if self.switcher.take().is_some() {
            cx.notify();
        }
    }

    /// A workspace's icon and name as in the column (a worktree by its
    /// branch), and its repo and server to go beside it.
    fn naming(&self, key: &TaskKey) -> (&'static str, SharedString, String) {
        let task = self.task(key);
        let icon = task.map(kind_icon).unwrap_or("icons/folder.svg");
        let name = task.map(column_label).unwrap_or_else(|| folder_name(&key.path).into());
        let repo = task.filter(|task| !task.main).map(|task| folder_name(&task.repo));
        let place = [repo, (key.host != LOCAL).then(|| key.host.to_string())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        (icon, name, place)
    }

    pub(super) fn render_switcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let switcher = self.switcher.as_ref()?;
        let theme = cx.theme().clone();
        let rows = switcher.keys.iter().enumerate().map(|(ix, key)| {
            let selected = ix == switcher.selected;
            let task = self.task(key);
            let (icon, name, place) = self.naming(key);
            let (dot, color) = task.map(|task| self.status(key, task, cx)).unwrap_or(("○", theme.muted_foreground));
            // Every workspace with an agent has its dot, an idle one too.
            let dotted = dot != "○" || !self.workspace_agents(key).is_empty();
            let key = key.clone();
            h_flex()
                .id(("switcher", ix))
                .h(px(30.))
                .flex_none()
                .px_3()
                .gap_2()
                .rounded(theme.radius)
                .when(selected, |el| el.bg(theme.accent))
                .child(svg().path(icon).size(px(14.)).flex_none().text_color(workspace_color(&key)))
                .child(div().flex_none().max_w(px(260.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_ui_small(cx).text_color(theme.muted_foreground).child(place))
                .child(div().flex_1())
                .when(dotted, |row| row.child(div().flex_none().text_ui_small(cx).text_color(color).child(dot)))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.switcher = None;
                    this.activate(key.clone(), window, cx);
                }))
        });
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .id("switcher")
                        .occlude()
                        .w(px(480.))
                        .max_h(px(560.))
                        .overflow_y_scroll()
                        .p_1()
                        .gap_0p5()
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius_lg)
                        .shadow_lg()
                        .text_color(theme.popover_foreground)
                        .children(rows),
                )
                .into_any_element(),
        )
    }
}

/// The key that ran the shortcut is held down, repeating. On the Mac a key
/// let go while Cmd is down has no key-up, so it's asked of the key-down
/// itself (a menu's click is no repeat).
#[cfg(target_os = "macos")]
fn repeating() -> bool {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    const KEY_DOWN: usize = 10;
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return false;
        }
        let kind: usize = msg_send![event, type];
        kind == KEY_DOWN && msg_send![event, isARepeat]
    }
}

#[cfg(not(target_os = "macos"))]
fn repeating() -> bool {
    false
}
