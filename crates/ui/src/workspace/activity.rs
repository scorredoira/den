//! The activity bar, on the window's left edge: an icon for each group of
//! the side column and each panel with an icon of its own, which shows it
//! or, if it's the one showing, closes the column; the device's while
//! there's one; and at the bottom the notes, connecting to a server and
//! Settings. Its right-click menu, as Show Panel, lists every panel.
use super::*;
use super::layout::{PanelDrag, group_icon, icon, title};
use crate::config::{Group, Panel, Place};

pub(crate) const ACTIVITY_WIDTH: f32 = 48.;

/// The icons' size: VS Code's, drawn as thin (see `thin`).
const ICON: f32 = 24.;

/// `icons/x.svg`, with thinner lines (see `Assets`).
fn thin(path: &str) -> SharedString {
    format!("icons/thin/{}", path.strip_prefix("icons/").unwrap_or(path)).into()
}

/// What an icon tells besides whether what it shows shows.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Badge {
    Count(usize),
    Dot(Hsla),
}

/// What an icon of the bar shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Item {
    Place(Place),
    Device,
    Notes,
}

impl Item {
    /// What a place goes by: the panel it has alone, or else the group its
    /// panels stand for (see `Layout::kind`), or else its first panel.
    fn stands_for(self, cx: &App) -> Result<Panel, Group> {
        let Item::Place(place) = self else {
            return Ok(if self == Item::Device { Panel::Device } else { Panel::Notes });
        };
        let layout = &Config::get(cx).layout;
        match (&layout.panels(place)[..], layout.kind(place)) {
            ([panel], _) => Ok(*panel),
            (_, Some(group)) => Err(group),
            _ => Ok(place.0),
        }
    }

    fn icon(self, cx: &App) -> &'static str {
        match self.stands_for(cx) {
            Ok(panel) => icon(panel),
            Err(group) => group_icon(group),
        }
    }

    pub(crate) fn title(self, cx: &App) -> &'static str {
        match self.stands_for(cx) {
            Ok(panel) => title(panel),
            Err(group) => group.title(),
        }
    }
}

/// An icon of a place being dragged to another spot on the bar.
#[derive(Clone)]
pub(crate) struct PlaceDrag(pub Place);

/// Puts `place`'s icon before `before`'s (or last), in every window.
fn move_place(place: Place, before: Option<Place>, cx: &mut App) {
    Config::update(cx, |config| config.layout.move_place(place, before));
    cx.refresh_windows();
}

/// An icon: what it shows, whether it shows and its badge.
pub(crate) type Activity = (Item, bool, Option<Badge>);

pub(crate) type OnActivity = Rc<dyn Fn(Item, &mut Window, &mut App)>;

/// The bar with `icons` at the top and `bottom` above the server's and
/// Settings'.
pub(crate) fn activity_bar(icons: Vec<Activity>, bottom: Vec<Activity>, click: OnActivity, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let button = |(item, shown, badge): Activity, cx: &App| {
        let click = click.clone();
        div()
            .relative()
            .child(
                activity_button(item, shown, cx)
                    .when(cfg!(test), |el| el.debug_selector(move || format!("activity-{item:?}")))
                    .tooltip(move |window, cx| Tooltip::new(item.title(cx)).build(window, cx))
                    .on_click(move |_, window, cx| click(item, window, cx))
                    // A place's icon drags up or down the bar.
                    .map(|el| match item {
                        Item::Place(place) => el
                            .on_drag(PlaceDrag(place), move |_, _, _, cx| {
                                let title = item.title(cx);
                                cx.new(|_| TabDragPreview(title.into()))
                            })
                            .drag_over::<PlaceDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                            .on_drop(move |drag: &PlaceDrag, _, cx| {
                                cx.stop_propagation();
                                move_place(drag.0, Some(place), cx);
                            })
                            // A panel dropped on it goes into that place.
                            .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                            // The column stays on what it shows.
                            .on_drop(move |drag: &PanelDrag, _, cx| {
                                cx.stop_propagation();
                                Config::update(cx, |config| config.layout.move_panel(drag.0, place, None));
                                cx.refresh_windows();
                            }),
                        _ => el,
                    }),
            )
            .children(badge.map(|badge| render_badge(badge, cx)))
    };
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
        .children(icons.into_iter().map(|icon| button(icon, cx)))
        // Past the icons, it goes last.
        .child(div().flex_1().w_full().on_drop(|drag: &PlaceDrag, _, cx| move_place(drag.0, None, cx)))
        .children(bottom.into_iter().map(|icon| button(icon, cx)))
        // At the bottom, as in VS Code: connecting to a server, and Settings.
        .child(bottom_button("activity-remote", "icons/satellite-dish.svg", ADD_SERVER, Box::new(crate::AddServer), cx))
        .child(bottom_button("activity-settings", "icons/settings.svg", "Settings", Box::new(crate::OpenSettings), cx))
        .context_menu(menu::panels_menu)
        .into_any_element()
}

/// An icon in the foreground's color while what it shows shows, with a
/// bar on its left.
fn activity_button(item: Item, shown: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(SharedString::from(format!("activity-{item:?}")))
        .size(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.sidebar_accent))
        .when(shown, |el| {
            el.child(div().absolute().left(px(-4.)).top(px(8.)).bottom(px(8.)).w(px(2.)).rounded_full().bg(theme.primary))
        })
        .child(svg().path(thin(item.icon(cx))).size(px(ICON)).text_color(if shown { theme.sidebar_foreground } else { theme.muted_foreground }))
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
        let side = self.side_place(cx);
        let mut icons: Vec<Activity> = Config::get(cx)
            .layout
            .places()
            .into_iter()
            .map(|place| (Item::Place(place), side == Some(place), self.badge(place, cx)))
            .collect();
        if self.device.read(cx).available() {
            icons.push((Item::Device, self.is_shown(Panel::Device, cx), None));
        }
        let notes = self.notes.read(cx).filled().then(|| Badge::Dot(cx.theme().primary));
        let bottom = vec![(Item::Notes, self.is_shown(Panel::Notes, cx), notes)];
        let workspace = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |item, window, cx| {
            workspace.update(cx, |this, cx| this.click_activity(item, window, cx)).ok();
        });
        activity_bar(icons, bottom, click, cx)
    }

    /// What Show Panel and the activity bar's menu list: every side panel,
    /// checked while it's in the place the column shows; the terminals,
    /// the device and the notes, while they show.
    pub(crate) fn menu_panels(&self, cx: &App) -> Vec<(Panel, bool)> {
        let layout = &Config::get(cx).layout;
        let here = layout.current().map(|place| layout.panels(place)).unwrap_or_default();
        let mut panels: Vec<(Panel, bool)> =
            Group::ALL.into_iter().flat_map(|group| group.panels()).map(|panel| (*panel, here.contains(panel))).collect();
        panels.push((Panel::Terminals, self.is_shown(Panel::Terminals, cx)));
        if self.device.read(cx).available() {
            panels.push((Panel::Device, self.is_shown(Panel::Device, cx)));
        }
        panels.push((Panel::Notes, self.is_shown(Panel::Notes, cx)));
        panels
    }

    /// A panel of that menu: a side panel comes to the place the column
    /// shows, from wherever it is, or goes off it; the others show or hide.
    pub(crate) fn toggle_from_menu(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        match panel {
            Panel::Terminals => self.set_terminals_visible(!self.is_shown(panel, cx), window, cx),
            Panel::Notes => self.click_activity(Item::Notes, window, cx),
            Panel::Device => self.toggle_panel(panel, cx),
            _ if self.in_side(panel, cx) => self.remove_panel(panel, cx),
            _ => self.bring_panel(panel, None, cx),
        }
    }

    /// What goes on `place`'s icon: the first of its panels' news.
    fn badge(&self, place: Place, cx: &App) -> Option<Badge> {
        Config::get(cx).layout.panels(place).into_iter().find_map(|panel| match panel {
            Panel::Workspaces => self.badges.workspaces.map(Badge::Dot),
            Panel::Agents => self.badges.agents.map(Badge::Dot),
            Panel::Changes => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            Panel::CallStack => {
                let debugger = self.debugger.read(cx);
                if debugger.is_stopped() {
                    Some(Badge::Dot(cx.theme().warning))
                } else {
                    debugger.is_active().then(|| Badge::Dot(cx.theme().success))
                }
            }
            _ => None,
        })
    }

    /// A place shows, or closes the side column if it's the one showing;
    /// the device and the notes show or hide. The search gets the focus,
    /// as its key does.
    pub(crate) fn click_activity(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Place(place) if self.side_place(cx) != Some(place) && Config::get(cx).layout.panels(place).contains(&Panel::Search) => {
                self.show_search(&ShowSearch, window, cx)
            }
            Item::Place(place) => self.click_place(place, cx),
            Item::Device => self.toggle_panel(Panel::Device, cx),
            Item::Notes if self.is_shown(Panel::Notes, cx) => {
                self.hide_panel(Panel::Notes, cx);
                self.focus_ide(window, cx);
            }
            Item::Notes => self.show_notes(window, cx),
        }
    }

    /// The state of the app's tasks: the most urgent of the others' and of
    /// the agents', on the explorer's icon.
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
    /// The most urgent of the other workspaces.
    pub workspaces: Option<Hsla>,
    /// The most urgent of the agents.
    pub agents: Option<Hsla>,
}
