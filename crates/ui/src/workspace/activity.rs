//! The activity bar, on the window's left edge: an icon for each panel but
//! the code (which never closes). A click shows or hides the panel wherever
//! it's placed; dragging an icon reorders the bar, or places the panel as
//! dragging its tab does.
use super::*;
use super::layout::{PanelDrag, icon, title};
use crate::config::Panel;

const WIDTH: f32 = 40.;

/// What the bar takes of the window's width: nothing, hidden.
pub(crate) fn activity_width(cx: &App) -> Pixels {
    px(if Config::get(cx).shows_activity_bar() { WIDTH } else { 0. })
}

/// What an icon tells of its panel besides whether it shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Badge {
    Count(usize),
    Dot(Hsla),
}

/// An icon: its panel, whether it shows and its badge.
pub(crate) type Activity = (Panel, bool, Option<Badge>);

pub(crate) type OnActivity = Rc<dyn Fn(Panel, &mut Window, &mut App)>;

/// The bar with `icons`, in the order the config says.
pub(crate) fn activity_bar(icons: Vec<Activity>, click: OnActivity, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .id("activity-bar")
        .when(cfg!(test), |el| el.debug_selector(|| "activity-bar".into()))
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .py_1()
        .gap_1()
        .items_center()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .children(icons.into_iter().map(|(panel, shown, badge)| {
            let click = click.clone();
            div()
                .relative()
                .child(
                    mode_button(("activity", panel as usize), icon(panel), shown, cx)
                        .size(px(32.))
                        .when(cfg!(test), |el| el.debug_selector(move || format!("activity-{panel:?}")))
                        .tooltip(move |window, cx| Tooltip::new(title(panel)).build(window, cx))
                        .on_click(move |_, window, cx| click(panel, window, cx))
                        .on_drag(PanelDrag(panel), |drag, _, _, cx| cx.new(|_| TabDragPreview(title(drag.0).into())))
                        .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                        .on_drop(move |drag: &PanelDrag, _, cx| {
                            cx.stop_propagation();
                            reorder(drag.0, Some(panel), cx);
                        }),
                )
                .children(badge.map(|badge| render_badge(badge, cx)))
        }))
        // Past the icons, it goes last.
        .child(div().flex_1().w_full().on_drop(|drag: &PanelDrag, _, cx| reorder(drag.0, None, cx)))
        .into_any_element()
}

fn reorder(panel: Panel, target: Option<Panel>, cx: &mut App) {
    Config::update(cx, |config| config.move_activity(panel, target));
    // Every window draws the bar.
    cx.refresh_windows();
}

/// On the icon's bottom right corner: a count, or a dot in the state's color.
fn render_badge(badge: Badge, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let corner = div().absolute().bottom(px(1.)).right(px(1.));
    match badge {
        Badge::Dot(color) => corner.size(px(8.)).rounded_full().bg(color).border_1().border_color(theme.sidebar),
        Badge::Count(count) => corner
            .h(px(12.))
            .min_w(px(12.))
            .px(px(3.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .text_size(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(if count > 99 { "99+".to_string() } else { count.to_string() }),
    }
    .into_any_element()
}

impl Workspace {
    /// The bar, unless it was hidden.
    pub(super) fn render_activity_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let config = Config::get(cx);
        if !config.shows_activity_bar() {
            return None;
        }
        let icons = config.activity().into_iter().map(|panel| (panel, self.is_shown(panel, cx), self.badge(panel, cx))).collect();
        let workspace = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |panel, window, cx| {
            workspace.update(cx, |this, cx| this.click_activity(panel, window, cx)).ok();
        });
        Some(activity_bar(icons, click, cx))
    }

    fn badge(&self, panel: Panel, cx: &App) -> Option<Badge> {
        match panel {
            Panel::Workspaces => self.badges.workspaces.map(Badge::Dot),
            Panel::Terminals => self.badges.terminals.map(Badge::Dot),
            Panel::Changes => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            Panel::Debugger => {
                let debugger = self.debugger.read(cx);
                if debugger.is_stopped() {
                    Some(Badge::Dot(cx.theme().warning))
                } else {
                    debugger.is_active().then(|| Badge::Dot(cx.theme().success))
                }
            }
            _ => None,
        }
    }

    /// Shows the panel, or hides it if it shows. The terminals and the
    /// search get the focus, as their keys do.
    pub(super) fn click_activity(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.is_shown(panel, cx);
        match panel {
            Panel::Terminals => self.set_terminals_visible(!shown, window, cx),
            Panel::Search if !shown => self.show_search(&ShowSearch, window, cx),
            _ if shown => self.hide_panel(panel, cx),
            _ => self.show_panel(panel, cx),
        }
    }

    /// The state of the app's tasks: this one's, on the terminals' icon; the
    /// most urgent of the others, on the workspaces'.
    pub fn set_badges(&mut self, badges: TaskBadges, cx: &mut Context<Self>) {
        if self.badges != badges {
            self.badges = badges;
            cx.notify();
        }
    }
}

/// Dots in the color of Claude's state (see `set_badges`).
#[derive(Clone, Copy, Default, PartialEq)]
pub struct TaskBadges {
    pub terminals: Option<Hsla>,
    pub workspaces: Option<Hsla>,
}
