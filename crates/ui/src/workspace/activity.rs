//! The activity bar, on the window's left edge: an icon for each group of
//! the side column and each panel with an icon of its own, which shows it
//! or, if it's the one showing, closes the column; a place with tabs has
//! one for each tab instead, which shows it with that tab in front; the Graph tab's, the
//! debugger's and the notes', which show them or hide them; each where it
//! was dragged, and on it unless taken off from its right-click menu. At
//! the bottom, connecting to a server and Settings.
use super::*;
use super::layout::{PanelDrag, group_icon, icon, notes_icon, title};
use crate::config::{Group, Icon, Panel, Place};

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
    /// A tab of the side column: its place, with it in front.
    Tab(Panel),
    /// The Graph tab.
    Graph,
    /// The debugger's tab, among the terminals'.
    Debug,
    /// The notes, and whether they have something (a sticky note written on).
    Notes { filled: bool },
}

impl Item {
    /// What it is on the bar, apart from what it shows now.
    fn spot(self) -> Icon {
        match self {
            Item::Place(place) => Icon::Place(place.0),
            Item::Tab(panel) => Icon::Place(panel),
            Item::Graph => Icon::Graph,
            Item::Debug => Icon::Debug,
            Item::Notes { .. } => Icon::Notes,
        }
    }

    /// What a place goes by: the panel it has alone, or else the group its
    /// panels stand for (see `Layout::kind`), or else its first panel.
    fn stands_for(self, cx: &App) -> Result<Panel, Group> {
        let place = match self {
            Item::Place(place) => place,
            Item::Tab(panel) => return Ok(panel),
            _ => return Ok(Panel::Notes),
        };
        let layout = &Config::get(cx).layout;
        match (&layout.panels(place)[..], layout.kind(place)) {
            ([panel], _) => Ok(*panel),
            (_, Some(group)) => Err(group),
            _ => Ok(place.0),
        }
    }

    fn icon(self, cx: &App) -> &'static str {
        match self {
            Item::Notes { filled } => return notes_icon(filled),
            Item::Graph => return "icons/git-graph.svg",
            Item::Debug => return icon(Panel::Console),
            Item::Place(_) | Item::Tab(_) => {}
        }
        match self.stands_for(cx) {
            Ok(panel) => icon(panel),
            Err(group) => group_icon(group),
        }
    }

    /// What its key does, for its tooltip: showing the panel or group it
    /// stands for.
    fn action(self, cx: &App) -> Option<Box<dyn Action>> {
        match self {
            Item::Notes { .. } => return Some(Box::new(ToggleNotes)),
            Item::Graph => return Some(Box::new(ShowGraph)),
            Item::Debug => return Some(Box::new(ToggleDebugPanel)),
            Item::Place(_) | Item::Tab(_) => {}
        }
        let action: Box<dyn Action> = match self.stands_for(cx) {
            Ok(Panel::Files) | Err(Group::Explorer) => Box::new(ShowFiles),
            Ok(Panel::Search) | Err(Group::Search) => Box::new(ShowSearch),
            Ok(Panel::References) => Box::new(ShowReferences),
            Ok(Panel::Changes) | Err(Group::Git) => Box::new(ShowChanges),
            Ok(Panel::History) => Box::new(ShowHistory),
            Ok(Panel::Outline) => Box::new(ShowOutline),
            _ => return None,
        };
        Some(action)
    }

    /// The place of the side column it shows, if it shows one.
    pub(crate) fn place(self, cx: &App) -> Option<Place> {
        match self {
            Item::Place(place) => Some(place),
            Item::Tab(panel) => Config::get(cx).layout.place_of(panel),
            _ => None,
        }
    }

    pub(crate) fn title(self, cx: &App) -> &'static str {
        match self {
            Item::Graph => return "Graph",
            Item::Debug => return title(Panel::Console),
            _ => {}
        }
        match self.stands_for(cx) {
            Ok(panel) => title(panel),
            Err(group) => group.title(),
        }
    }
}

/// An icon being dragged to another spot on the bar.
#[derive(Clone)]
pub(crate) struct IconDrag(pub Icon);

/// Puts `icon` before `before` (or last), in every window.
fn move_icon(icon: Icon, before: Option<Icon>, cx: &mut App) {
    Config::update(cx, |config| config.layout.move_icon(icon, before));
    cx.refresh_windows();
}

/// The bar's right-click menu: every panel, checked while it can be seen.
/// A side panel checked shows in its place, whose icon comes back if it
/// had gone; unchecked it's hidden, and the place's icon goes with its last
/// one. The terminals show or hide; Graph, Debug and Notes are their
/// icons, on the bar or off it. And Reset Layout.
fn icons_menu(menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let Some(workspace) = crate::app::window_workspace(window, cx) else {
        return menu.item(menu::reset_layout());
    };
    let weak = workspace.downgrade();
    let layout = &Config::get(cx).layout;
    let side: Vec<(Panel, bool)> = Group::ALL
        .into_iter()
        .flat_map(|group| group.panels())
        .map(|panel| (*panel, layout.place_of(*panel).is_some_and(|place| layout.panels(place).contains(panel))))
        .collect();
    let bar = layout.bar();
    let menu = side.into_iter().fold(menu, |menu, (panel, on)| {
        menu.item(
            menu::item(title(panel), &weak, move |this, _, cx| {
                if on {
                    this.remove_panel(panel, cx);
                } else {
                    this.show_panel(panel, cx);
                }
            })
            .checked(on),
        )
    });
    let terminals = workspace.read(cx).is_shown(Panel::Terminals, cx);
    let menu = menu.item(
        menu::item(title(Panel::Terminals), &weak, |this, window, cx| this.toggle_from_menu(Panel::Terminals, window, cx)).checked(terminals),
    );
    [(Icon::Debug, Item::Debug), (Icon::Notes, Item::Notes { filled: false }), (Icon::Graph, Item::Graph)]
        .into_iter()
        .fold(menu, |menu, (icon, item)| {
            let on = bar.contains(&icon);
            menu.item(
                menu::PopupMenuItem::new(item.title(cx))
                    .checked(on)
                    .on_click(move |_, _, cx| Config::update(cx, |config| config.layout.set_on_bar(icon, !on))),
            )
        })
        .separator()
        .item(menu::reset_layout())
}

/// An icon: what it shows, whether it shows and its badge.
pub(crate) type Activity = (Item, bool, Option<Badge>);

pub(crate) type OnActivity = Rc<dyn Fn(Item, &mut Window, &mut App)>;

/// The bar with `icons` at the top, the server's and Settings' at the
/// bottom.
pub(crate) fn activity_bar(icons: Vec<Activity>, click: OnActivity, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let button = |(item, shown, badge): Activity, cx: &App| {
        let click = click.clone();
        div()
            .relative()
            .child(
                activity_button(item, shown, cx)
                    .when(cfg!(test), |el| el.debug_selector(move || match item {
                        Item::Notes { .. } => "activity-Notes".to_string(),
                        _ => format!("activity-{item:?}"),
                    }))
                    // With its key, as VS Code's: what the user set, if anything.
                    .tooltip(move |window, cx| {
                        let tooltip = Tooltip::new(item.title(cx));
                        match item.action(cx) {
                            Some(action) => tooltip.action(action.as_ref(), None),
                            None => tooltip,
                        }
                        .build(window, cx)
                    })
                    .on_click(move |_, window, cx| click(item, window, cx))
                    // An icon drags up or down the bar.
                    .on_drag(IconDrag(item.spot()), move |_, _, _, cx| {
                        let title = item.title(cx);
                        cx.new(|_| TabDragPreview(title.into()))
                    })
                    .drag_over::<IconDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                    .on_drop(move |drag: &IconDrag, _, cx| {
                        cx.stop_propagation();
                        move_icon(drag.0, Some(item.spot()), cx);
                    })
                    .map(|el| match item {
                        // A panel dropped on a place's goes into that place.
                        Item::Place(place) => el
                            .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                            // The column stays on what it shows.
                            .on_drop(move |drag: &PanelDrag, _, cx| {
                                cx.stop_propagation();
                                Config::update(cx, |config| config.layout.move_panel(drag.0, place, None));
                                cx.refresh_windows();
                            }),
                        // On a tab's, among its tabs.
                        Item::Tab(tab) => el
                            .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                            .on_drop(move |drag: &PanelDrag, _, cx| {
                                cx.stop_propagation();
                                Config::update(cx, |config| config.layout.join(drag.0, tab, None));
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
        // Past the icons, an icon goes last; a panel gets an icon of its own there.
        .child(
            div()
                .flex_1()
                .w_full()
                .on_drop(|drag: &IconDrag, _, cx| move_icon(drag.0, None, cx))
                .drag_over::<PanelDrag>(|style, _, _, cx| style.bg(cx.theme().primary.opacity(0.25)))
                .on_drop(|drag: &PanelDrag, _, cx| {
                    let panel = drag.0;
                    Config::update(cx, |config| {
                        config.layout.own_place(panel);
                        if let Some(place) = config.layout.place_of(panel) {
                            config.layout.move_place(place, None);
                            config.layout.side = true;
                            config.layout.place = place;
                        }
                    });
                    cx.refresh_windows();
                }),
        )
        // At the bottom, as in VS Code: connecting to a server, and Settings.
        .child(bottom_button("activity-remote", "icons/satellite-dish.svg", ADD_SERVER, Box::new(crate::AddServer), cx))
        .child(bottom_button("activity-settings", "icons/settings.svg", "Settings", Box::new(crate::OpenSettings), cx))
        .context_menu(icons_menu)
        .into_any_element()
}

/// An icon in the foreground's color while what it shows shows, with a
/// bar on its left.
fn activity_button(item: Item, shown: bool, cx: &App) -> Stateful<Div> {
    icon_button(format!("activity-{item:?}").into(), item.icon(cx), shown, cx)
}

fn icon_button(id: SharedString, icon: &str, shown: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.sidebar_accent))
        .when(shown, |el| {
            el.child(div().absolute().left(px(-4.)).top(px(8.)).bottom(px(8.)).w(px(2.)).rounded_full().bg(theme.primary))
        })
        .child(svg().path(thin(icon)).size(px(ICON)).text_color(if shown { theme.sidebar_foreground } else { theme.muted_foreground }))
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
    /// A plain editor's bar: the files and the search, each showing or
    /// hiding its panel.
    pub(super) fn render_plain_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let button = |panel: Panel, action: Box<dyn Action>, cx: &mut Context<Self>| {
            icon_button(format!("plain-{panel:?}").into(), icon(panel), self.is_shown(panel, cx), cx)
                .when(cfg!(test), |el| el.debug_selector(move || format!("plain-{panel:?}")))
                .tooltip(move |window, cx| Tooltip::new(title(panel)).action(action.as_ref(), None).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| match panel {
                    Panel::Search if !this.is_shown(panel, cx) => this.show_search(&ShowSearch, window, cx),
                    _ => this.toggle_panel(panel, cx),
                }))
        };
        v_flex()
            .id("plain-bar")
            .flex_none()
            .w(px(ACTIVITY_WIDTH))
            .h_full()
            .py_1()
            .gap_1()
            .items_center()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(button(Panel::Files, Box::new(ShowFiles), cx))
            .child(button(Panel::Search, Box::new(ShowSearch), cx))
            .into_any_element()
    }

    pub(super) fn render_activity_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let icons = self.activities(cx);
        let workspace = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |item, window, cx| {
            workspace.update(cx, |this, cx| this.click_activity(item, window, cx)).ok();
        });
        activity_bar(icons, click, cx)
    }

    /// The bar's icons.
    pub(crate) fn activities(&self, cx: &App) -> Vec<Activity> {
        let layout = &Config::get(cx).layout;
        layout
            .bar()
            .into_iter()
            .map(|icon| {
                let (item, badge) = match icon {
                    Icon::Place(panel) if layout.is_tab(panel) => (Item::Tab(panel), self.tab_badge(panel, cx)),
                    Icon::Place(panel) => (Item::Place(Place(panel)), self.badge(Place(panel), cx)),
                    Icon::Graph => (Item::Graph, None),
                    Icon::Debug => (Item::Debug, None),
                    Icon::Notes => (Item::Notes { filled: self.notes.read(cx).filled() }, None),
                };
                (item, self.shows(item, cx), badge)
            })
            .collect()
    }

    /// Whether what `item` stands for shows.
    pub(crate) fn shows(&self, item: Item, cx: &App) -> bool {
        match item {
            Item::Place(place) => self.side_place(cx) == Some(place),
            Item::Tab(panel) => self.side_place(cx).is_some() && self.side_place(cx) == item.place(cx) && Config::get(cx).layout.in_front(panel),
            Item::Graph => self.graph_visible(),
            Item::Debug => self.is_shown(Panel::Console, cx),
            Item::Notes { .. } => self.is_shown(Panel::Notes, cx),
        }
    }

    /// What Show Panel and the activity bar's menu list: every side panel,
    /// checked while it's in the place the column shows (none while it's
    /// closed); the terminals and the notes, while they show.
    pub(crate) fn menu_panels(&self, cx: &App) -> Vec<(Panel, bool)> {
        let layout = &Config::get(cx).layout;
        let here = self.side_place(cx).map(|place| layout.panels(place)).unwrap_or_default();
        let mut panels: Vec<(Panel, bool)> =
            Group::ALL.into_iter().flat_map(|group| group.panels()).map(|panel| (*panel, here.contains(panel))).collect();
        panels.push((Panel::Terminals, self.is_shown(Panel::Terminals, cx)));
        panels.push((Panel::Console, self.is_shown(Panel::Console, cx)));
        panels.push((Panel::Notes, self.is_shown(Panel::Notes, cx)));
        panels
    }

    /// A panel of that menu: a side panel comes to the place the column
    /// shows, from wherever it is (one there already, or hidden there,
    /// keeps its spot), or goes off it; the others show or hide.
    pub(crate) fn toggle_from_menu(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        match panel {
            Panel::Terminals => self.set_terminals_visible(!self.is_shown(panel, cx), window, cx),
            Panel::Notes => self.toggle_notes(&ToggleNotes, window, cx),
            Panel::Console => self.toggle_panel(Panel::Console, cx),
            _ if self.in_side(panel, cx) => self.remove_panel(panel, cx),
            _ => self.bring_panel(panel, None, cx),
        }
    }

    /// What goes on `place`'s icon: the first of its panels' news. The
    /// agents' dots only while the place doesn't show: then their panels
    /// say it (on their header, folded).
    fn badge(&self, place: Place, cx: &App) -> Option<Badge> {
        let hidden = self.side_place(cx) != Some(place);
        let dot = |color: Option<Hsla>| color.filter(|_| hidden).map(Badge::Dot);
        Config::get(cx).layout.panels(place).into_iter().find_map(|panel| match panel {
            Panel::Workspaces => dot(self.badges.workspaces),
            Panel::Agents => dot(self.badges.agents),
            Panel::Changes => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            _ => None,
        })
    }

    /// What goes on a tab's icon: its panel's news; on its place's first
    /// one, also those of the panels that aren't tabs.
    fn tab_badge(&self, panel: Panel, cx: &App) -> Option<Badge> {
        let layout = &Config::get(cx).layout;
        let place = layout.place_of(panel)?;
        let hidden = !self.shows(Item::Tab(panel), cx);
        let dot = |color: Option<Hsla>| color.filter(|_| hidden).map(Badge::Dot);
        let first = layout.panels(place).into_iter().find(|other| layout.is_tab(*other)) == Some(panel);
        let mut panels: Vec<Panel> = vec![panel];
        if first {
            panels.extend(layout.panels(place).into_iter().filter(|other| !layout.is_tab(*other)));
        }
        panels.into_iter().find_map(|panel| match panel {
            Panel::Workspaces => dot(self.badges.workspaces),
            Panel::Agents => dot(self.badges.agents),
            Panel::Changes => Some(self.changes.read(cx).count()).filter(|count| *count > 0).map(Badge::Count),
            _ => None,
        })
    }

    /// A place shows, or closes the side column if it's the one showing;
    /// a tab's, its place with it in front;
    /// the Graph tab, the debugger and the notes show or hide. The search gets the focus,
    /// as its key does.
    pub(crate) fn click_activity(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Place(place) if self.side_place(cx) != Some(place) && Config::get(cx).layout.panels(place).contains(&Panel::Search) => {
                self.show_search(&ShowSearch, window, cx)
            }
            Item::Place(place) => self.click_place(place, cx),
            Item::Tab(panel) if self.shows(item, cx) => self.hide_panel(panel, cx),
            Item::Tab(Panel::Search) => self.show_search(&ShowSearch, window, cx),
            Item::Tab(panel) => self.show_panel(panel, cx),
            Item::Graph => self.toggle_graph(window, cx),
            Item::Debug => self.toggle_panel(Panel::Console, cx),
            Item::Notes { .. } => self.toggle_notes(&ToggleNotes, window, cx),
        }
    }

    /// The state of the app's tasks: the most urgent of the others', of the
    /// project's and of the agents', on the explorer's icon or its headers.
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
    /// The most urgent of the workspaces.
    pub workspaces: Option<Hsla>,
    /// The most urgent of the agents.
    pub agents: Option<Hsla>,
}
