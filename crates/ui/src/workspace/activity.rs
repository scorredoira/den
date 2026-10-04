//! The activity bar, on the window's left edge: an icon for each panel but
//! the code, which stays put and never closes (the others go around it). A
//! click shows or hides the panel wherever it's placed; dragging an icon
//! reorders the bar, or places the panel. The places have no tabs: the bar
//! does their job. Its right-click menu (and View > Activity Bar) takes
//! icons off it.
use super::*;
use super::layout::{PanelDrag, icon, title};
use crate::config::Panel;
use gpui_kit::component::menu::PopupMenuItem;

pub(crate) const ACTIVITY_WIDTH: f32 = 48.;

/// The icons' size: VS Code's, drawn as thin (see `thin`).
const ICON: f32 = 24.;

/// `icons/x.svg`, with thinner lines (see `Assets`).
fn thin(path: &str) -> SharedString {
    format!("icons/thin/{}", path.strip_prefix("icons/").unwrap_or(path)).into()
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
        .w(px(ACTIVITY_WIDTH))
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
                    activity_button(panel, shown, cx)
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
        // At the bottom, as in VS Code: connecting to a server, and Settings.
        .child(bottom_button("activity-remote", "icons/satellite-dish.svg", ADD_SERVER, Box::new(crate::AddServer), cx))
        .child(bottom_button("activity-settings", "icons/settings.svg", "Settings", Box::new(crate::OpenSettings), cx))
        .context_menu(|popup, _, cx| {
            let config = Config::get(cx);
            let hidden = config.hidden_activity();
            config
                .activity()
                .into_iter()
                .fold(popup, |popup, panel| {
                    popup.item(
                        PopupMenuItem::new(title(panel))
                            .checked(!hidden.contains(&panel))
                            .on_click(move |_, _, cx| toggle_activity_icon(panel, cx)),
                    )
                })
                .separator()
                .item(menu::reset_layout())
        })
        .into_any_element()
}

/// Puts `panel`'s icon on the bar, or takes it off.
pub(crate) fn toggle_activity_icon(panel: Panel, cx: &mut App) {
    Config::update(cx, |config| config.toggle_activity(panel));
    // View > Activity Bar checks it.
    crate::app_menu::set(cx);
    cx.refresh_windows();
}

/// An icon in the foreground's color while its panel shows: unlike a tab,
/// several show at once, so none is filled in as if selected.
fn activity_button(panel: Panel, shown: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(("activity", panel as usize))
        .size(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.sidebar_accent))
        .child(svg().path(thin(icon(panel))).size(px(ICON)).text_color(if shown { theme.sidebar_foreground } else { theme.muted_foreground }))
}

const ADD_SERVER: &str = if cfg!(windows) { "Add Server or WSL Distro…" } else { "Add Server…" };

/// An icon at the bottom of the bar that runs `action`.
fn bottom_button(id: &'static str, path: &'static str, tip: &'static str, action: Box<dyn Action>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(40.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.sidebar_accent))
        .child(svg().path(thin(path)).size(px(ICON)).text_color(theme.muted_foreground))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
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
    pub(super) fn render_activity_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panels = Config::get(cx).shown_activity();
        if self.device.read(cx).available() {
            panels.push(Panel::Device);
        }
        let icons = panels.into_iter().map(|panel| (panel, self.is_shown(panel, cx), self.badge(panel, cx))).collect();
        let workspace = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |panel, window, cx| {
            workspace.update(cx, |this, cx| this.click_activity(panel, window, cx)).ok();
        });
        activity_bar(icons, click, cx)
    }

    fn badge(&self, panel: Panel, cx: &App) -> Option<Badge> {
        match panel {
            Panel::Workspaces => self.badges.workspaces.map(Badge::Dot),
            Panel::Terminals => self.badges.terminals.map(Badge::Dot),
            Panel::Changes => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            Panel::Notes => self.notes.read(cx).filled().then(|| Badge::Dot(cx.theme().primary)),
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
