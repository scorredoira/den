//! The activity bar, on the window's left edge: an icon for each group of
//! the side column, which shows it or, if it's the one showing, closes the
//! column; the device's while there's one; and at the bottom the notes,
//! connecting to a server and Settings.
use super::*;
use super::layout::{group_icon, icon};
use crate::config::{Group, Panel};

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
    Group(Group),
    Device,
    Notes,
}

impl Item {
    fn icon(self) -> &'static str {
        match self {
            Item::Group(group) => group_icon(group),
            Item::Device => icon(Panel::Device),
            Item::Notes => icon(Panel::Notes),
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Item::Group(group) => group.title(),
            Item::Device => "Device",
            Item::Notes => "Notes",
        }
    }
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
                    .tooltip(move |window, cx| Tooltip::new(item.title()).build(window, cx))
                    .on_click(move |_, window, cx| click(item, window, cx)),
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
        .child(div().flex_1())
        .children(bottom.into_iter().map(|icon| button(icon, cx)))
        // At the bottom, as in VS Code: connecting to a server, and Settings.
        .child(bottom_button("activity-remote", "icons/satellite-dish.svg", ADD_SERVER, Box::new(crate::AddServer), cx))
        .child(bottom_button("activity-settings", "icons/settings.svg", "Settings", Box::new(crate::OpenSettings), cx))
        .context_menu(|popup, _, _| popup.item(menu::reset_layout()))
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
        .child(svg().path(thin(item.icon())).size(px(ICON)).text_color(if shown { theme.sidebar_foreground } else { theme.muted_foreground }))
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
        let side = self.side_group();
        let mut icons: Vec<Activity> =
            Group::ALL.into_iter().map(|group| (Item::Group(group), side == Some(group), self.badge(Item::Group(group), cx))).collect();
        if self.device.read(cx).available() {
            icons.push((Item::Device, self.is_shown(Panel::Device, cx), None));
        }
        let bottom = vec![(Item::Notes, self.is_shown(Panel::Notes, cx), self.badge(Item::Notes, cx))];
        let workspace = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |item, window, cx| {
            workspace.update(cx, |this, cx| this.click_activity(item, window, cx)).ok();
        });
        activity_bar(icons, bottom, click, cx)
    }

    /// What Show Panel lists: the groups, the terminals, the device and the
    /// notes, and whether each shows.
    pub(crate) fn menu_items(&self, cx: &App) -> Vec<(Item, bool)> {
        let side = self.side_group();
        let mut items: Vec<(Item, bool)> = Group::ALL.into_iter().map(|group| (Item::Group(group), side == Some(group))).collect();
        if self.device.read(cx).available() {
            items.push((Item::Device, self.is_shown(Panel::Device, cx)));
        }
        items.push((Item::Notes, self.is_shown(Panel::Notes, cx)));
        items
    }

    fn badge(&self, item: Item, cx: &App) -> Option<Badge> {
        match item {
            Item::Group(Group::Explorer) => self.badges.workspaces.or(self.badges.agents).map(Badge::Dot),
            Item::Group(Group::Git) => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            Item::Group(Group::Debug) => {
                let debugger = self.debugger.read(cx);
                if debugger.is_stopped() {
                    Some(Badge::Dot(cx.theme().warning))
                } else {
                    debugger.is_active().then(|| Badge::Dot(cx.theme().success))
                }
            }
            Item::Group(Group::Search) | Item::Device => None,
            Item::Notes => self.notes.read(cx).filled().then(|| Badge::Dot(cx.theme().primary)),
        }
    }

    /// A group shows, or closes the side column if it's the one showing;
    /// the device and the notes show or hide. The search gets the focus,
    /// as its key does.
    pub(crate) fn click_activity(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Group(Group::Search) if self.side_group() != Some(Group::Search) => {
                self.show_search(&ShowSearch, window, cx)
            }
            Item::Group(group) => self.click_group(group, cx),
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
