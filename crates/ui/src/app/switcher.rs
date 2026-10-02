//! Cmd-E, as macOS's Cmd-Tab: the workspaces, the most recently used first,
//! with the previous one selected; each E (or ↓) selects the next, Shift-E
//! (or ↑) the one before, letting go of Cmd enters it and Esc stays.

use super::*;

pub(super) struct Switcher {
    /// The most recently used first: the one in front.
    keys: Vec<TaskKey>,
    selected: usize,
}

impl Sik {
    /// Cmd-E: opens the switcher on the previous workspace or, open, selects
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

    /// Selects `by` further down the list (up if negative), round the ends.
    fn move_switcher(&mut self, by: isize, cx: &mut Context<Self>) {
        if let Some(switcher) = &mut self.switcher {
            let len = switcher.keys.len() as isize;
            switcher.selected = (switcher.selected as isize + by).rem_euclid(len) as usize;
            cx.notify();
        }
    }

    /// While it's open, its keys: E and Shift-E with Cmd, the arrows, Esc.
    /// True if the key was its.
    pub(super) fn switcher_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if self.switcher.is_none() {
            return false;
        }
        match keystroke.key.as_str() {
            "e" if keystroke.modifiers.shift => self.move_switcher(-1, cx),
            "e" => self.move_switcher(1, cx),
            "down" | "right" | "tab" => self.move_switcher(1, cx),
            "up" | "left" => self.move_switcher(-1, cx),
            "escape" => self.close_switcher(cx),
            _ => return false,
        }
        true
    }

    /// Every workspace in the column: the one in front, then the most
    /// recently used, then the rest in the column's order.
    fn recently_used(&self, cx: &App) -> Vec<TaskKey> {
        let column: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        let recent = Config::get(cx).recent.iter().map(|recent| TaskKey { host: recent.host.clone().into(), path: recent.path.clone() });
        let mut keys: Vec<TaskKey> = Vec::new();
        for key in self.active.iter().cloned().chain(recent).chain(column.iter().cloned()) {
            if !keys.contains(&key) && column.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    /// Cmd let go: enters the one selected.
    pub(super) fn switcher_modifiers(&mut self, modifiers: &Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        if modifiers.secondary() {
            return;
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

    pub(super) fn render_switcher(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let switcher = self.switcher.as_ref()?;
        let theme = cx.theme().clone();
        let rows = switcher.keys.iter().enumerate().map(|(ix, key)| {
            let selected = ix == switcher.selected;
            let task = self.task(key);
            let icon = task.map(kind_icon).unwrap_or("icons/folder.svg");
            // Named as in the column: a worktree by its branch, with its
            // repo and its server beside it.
            let name = task.map(column_label).unwrap_or_else(|| folder_name(&key.path).into());
            let repo = task.filter(|task| !task.main).map(|task| folder_name(&task.repo));
            let place = [repo, (key.host != LOCAL).then(|| key.host.to_string())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            let (dot, color) = task.map(|task| self.status(key, task, cx)).unwrap_or(("○", theme.muted_foreground));
            let key = key.clone();
            h_flex()
                .id(("switcher", ix))
                .h(px(30.))
                .flex_none()
                .px_3()
                .gap_2()
                .rounded(theme.radius)
                .when(selected, |el| el.bg(theme.accent))
                .child(svg().path(icon).size(px(14.)).flex_none().text_color(theme.muted_foreground))
                .child(div().flex_none().max_w(px(260.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
                .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_ui_small(cx).text_color(theme.muted_foreground).child(place))
                .child(div().flex_1())
                .when(dot != "○", |row| row.child(div().flex_none().text_ui_small(cx).text_color(color).child(dot)))
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
