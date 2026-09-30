//! The task's terminal area: tabs, and in each tab terminals split into
//! resizable rows and columns. The processes live in the agent; here we only
//! store which terminal goes where, to restore it when the app is reopened.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use anyhow::Result;
use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, h_flex, h_resizable,
    menu::{ContextMenuExt as _, PopupMenu},
    resizable_panel, v_flex, v_resizable,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::TermId;
use serde::{Deserialize, Serialize};
use ui_term::{Terminal, TerminalView, TerminalViewEvent, grid_for};

use crate::{
    config::UiText,
    CloseTab, NewTerminal, SplitDown, SplitRight, agent, menu,
    splits::{Axis, Direction, Tree},
};

pub enum TerminalAreaEvent {
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// A message for the status bar.
    Message(SharedString),
}

struct TerminalTab {
    id: usize,
    tree: Tree,
    /// The tab's terminal that last had focus.
    active: TermId,
}

/// Where a new terminal goes.
#[derive(Clone, Copy)]
enum Place {
    NewTab,
    /// Next to the active terminal: to the right (`Row`) or below (`Column`).
    Split(Axis),
}

pub struct TerminalArea {
    client: Option<Arc<Client>>,
    /// Terminal group in the agent (the task's folder).
    group: String,
    cwd: PathBuf,
    tabs: Vec<TerminalTab>,
    views: HashMap<TermId, Entity<TerminalView>>,
    active: usize,
    next_id: usize,
    /// Size of the terminal area at the last paint, so each shell is created
    /// at its final size (otherwise it redraws the prompt when resized).
    body_size: Rc<Cell<Option<Size<Pixels>>>>,
    /// Terminals on this machine (not on a server).
    local: bool,
    /// For right-click menus.
    weak: WeakEntity<Self>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalAreaEvent> for TerminalArea {}

impl TerminalArea {
    pub fn new(cwd: PathBuf, client: Option<Arc<Client>>, local: bool, cx: &mut Context<Self>) -> Self {
        Self {
            client,
            group: cwd.to_string_lossy().into_owned(),
            cwd,
            tabs: Vec::new(),
            views: HashMap::new(),
            active: 0,
            next_id: 0,
            body_size: Rc::default(),
            local,
            weak: cx.entity().downgrade(),
            _subscriptions: Vec::new(),
        }
    }

    /// Restores the agent's live terminals with the saved layout; if there are
    /// none, opens one.
    pub fn restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let group = self.group.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<(Vec<Tree>, Vec<(TermId, Entity<Terminal>)>)> = async {
                let alive: HashSet<TermId> = agent::list(&client, group.clone()).await?.into_iter().collect();
                let saved = SavedLayouts::load().groups.remove(&group).unwrap_or_default();
                let mut trees: Vec<Tree> = saved
                    .tabs
                    .into_iter()
                    .map(SavedTab::into_tree)
                    .filter_map(|tree| tree?.retain(&|term| alive.contains(&term)))
                    .collect();
                // Those still alive but not saved, each in its own tab.
                let placed: HashSet<TermId> = trees.iter().flat_map(Tree::leaves).collect();
                let mut orphans: Vec<TermId> = alive.difference(&placed).copied().collect();
                orphans.sort();
                trees.extend(orphans.into_iter().map(Tree::Leaf));

                let mut terminals = Vec::new();
                for term in trees.iter().flat_map(Tree::leaves) {
                    terminals.push((term, agent::attach(client.clone(), term, cx).await?));
                }
                Ok((trees, terminals))
            }
            .await;

            this.update_in(cx, |this, window, cx| match result {
                Ok((trees, _)) if trees.is_empty() => this.open(Place::NewTab, window, cx),
                Ok((trees, terminals)) => {
                    for (term, terminal) in terminals {
                        this.add_view(term, terminal, window, cx);
                    }
                    for tree in trees {
                        let id = this.next_tab_id();
                        let active = tree.leaves()[0];
                        this.tabs.push(TerminalTab { id, tree, active });
                    }
                    this.active = 0;
                    this.save();
                    cx.notify();
                }
                Err(err) => this.error(format!("Couldn't restore terminals: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Switches to a new connection with the agent (after reconnecting): each
    /// terminal reattaches to its process; those that no longer exist are removed.
    pub fn set_client(&mut self, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        self.client = Some(client.clone());
        let group = self.group.clone();
        let terminals: Vec<(TermId, Entity<Terminal>)> = self
            .views
            .iter()
            .map(|(term, view)| (*term, view.read(cx).terminal().clone()))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let alive: HashSet<TermId> = agent::list(&client, group).await.unwrap_or_default().into_iter().collect();
            let mut gone = Vec::new();
            for (term, terminal) in terminals {
                if !alive.contains(&term) || agent::reattach(client.clone(), term, &terminal, cx).await.is_err() {
                    gone.push(term);
                }
            }
            this.update_in(cx, |this, window, cx| {
                for term in gone {
                    this.remove(term, window, cx);
                }
                if this.tabs.is_empty() {
                    this.open(Place::NewTab, window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn next_tab_id(&mut self) -> usize {
        self.next_id += 1;
        self.next_id
    }

    fn error(&mut self, message: String, cx: &mut Context<Self>) {
        cx.emit(TerminalAreaEvent::Message(message.into()));
    }

    fn add_view(&mut self, term: TermId, terminal: Entity<Terminal>, window: &mut Window, cx: &mut Context<Self>) -> Entity<TerminalView> {
        let local = self.local;
        let view = cx.new(|cx| TerminalView::new(terminal, local, window, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event, window, cx| match event {
            TerminalViewEvent::TitleChanged => cx.notify(),
            TerminalViewEvent::Exited => this.remove(term, window, cx),
            TerminalViewEvent::Focused => this.select(term, cx),
            TerminalViewEvent::OpenPath { path, line, column } => cx.emit(TerminalAreaEvent::OpenPath {
                path: path.clone(),
                line: *line,
                column: *column,
            }),
        });
        self._subscriptions.push(subscription);
        self.views.insert(term, view.clone());
        view
    }

    /// Creates a terminal in the agent and places it; once it arrives, it gets focus.
    fn open(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return self.error("No agent: can't open terminals".into(), cx);
        };
        let place = if self.tabs.is_empty() { Place::NewTab } else { place };
        let group = self.group.clone();
        let cwd = self.cwd.clone();
        let size = self.body_size.get().map_or((80, 24), |size| {
            // A terminal to the side or below takes half the space.
            let size = match place {
                Place::Split(Axis::Row) => gpui_kit::size(size.width / 2., size.height),
                Place::Split(Axis::Column) => gpui_kit::size(size.width, size.height / 2.),
                Place::NewTab => size,
            };
            grid_for(size, window, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = agent::create(client, group, cwd, size, cx).await;
            this.update_in(cx, |this, window, cx| {
                let (term, terminal) = match result {
                    Ok(created) => created,
                    Err(err) => return this.error(format!("Couldn't open terminal: {err:#}"), cx),
                };
                let view = this.add_view(term, terminal, window, cx);
                match place {
                    Place::Split(axis) if !this.tabs.is_empty() => {
                        let tab = &mut this.tabs[this.active];
                        tab.tree.split(tab.active, term, axis);
                        tab.active = term;
                    }
                    _ => {
                        let id = this.next_tab_id();
                        this.tabs.push(TerminalTab {
                            id,
                            tree: Tree::Leaf(term),
                            active: term,
                        });
                        this.active = this.tabs.len() - 1;
                    }
                }
                view.read(cx).focus_handle(cx).focus(window, cx);
                this.save();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Opens a terminal in a new tab and focuses it.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Place::NewTab, window, cx);
    }

    /// Splits the active terminal: to the right (`Row`) or below (`Column`).
    pub fn split(&mut self, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Place::Split(axis), window, cx);
    }

    /// Moves focus to the neighboring terminal in `direction`.
    pub fn focus_neighbor(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        if let Some(view) = tab
            .tree
            .neighbor(tab.active, direction)
            .and_then(|term| self.views.get(&term))
        {
            view.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Whether any terminal has focus.
    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.views
            .values()
            .any(|view| view.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Focuses the active terminal; if there is none, opens one.
    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_view() {
            Some(view) => view.read(cx).focus_handle(cx).focus(window, cx),
            None => self.new_terminal(window, cx),
        }
    }

    fn active_view(&self) -> Option<Entity<TerminalView>> {
        let tab = self.tabs.get(self.active)?;
        self.views.get(&tab.active).cloned()
    }

    /// Closes the active terminal and kills its process.
    pub fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(term) = self.tabs.get(self.active).map(|tab| tab.active) else {
            return;
        };
        if let Some(view) = self.views.get(&term) {
            view.read(cx).terminal().read(cx).kill();
        }
        self.remove(term, window, cx);
    }

    /// Closes a specific terminal and kills its process.
    fn close_term(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.views.get(&term) {
            view.read(cx).terminal().read(cx).kill();
        }
        self.remove(term, window, cx);
    }

    /// Closes a tab with all its terminals.
    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else {
            return;
        };
        for term in tab.tree.leaves() {
            self.close_term(term, window, cx);
        }
    }

    /// Splits next to `term` (the terminal that was clicked).
    fn split_at(&mut self, term: TermId, axis: Axis, window: &mut Window, cx: &mut Context<Self>) {
        self.select(term, cx);
        self.split(axis, window, cx);
    }

    fn pane_menu(&self, term: TermId, menu: PopupMenu, cx: &App) -> PopupMenu {
        let area = self.weak.clone();
        let view = self.views.get(&term).cloned();
        let has_selection = view.as_ref().is_some_and(|view| view.read(cx).has_selection(cx));
        let copy_view = view.clone();
        let paste_view = view.clone();
        // Copy and Paste have their shortcuts in the terminal's context.
        let menu = match &view {
            Some(view) => menu.action_context(view.focus_handle(cx)),
            None => menu,
        };
        menu.item(
            menu::item("Copy", &area, move |_, _, cx| {
                let text = copy_view
                    .as_ref()
                    .and_then(|view| view.read(cx).terminal().read(cx).selection_text());
                if let Some(text) = text {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            })
            .disabled(!has_selection)
            .action(Box::new(ui_term::Copy)),
        )
        .item(
            menu::item("Paste", &area, move |_, _, cx| {
                if let Some(view) = &paste_view {
                    view.update(cx, |view, cx| view.paste_clipboard(cx));
                }
            })
            .action(Box::new(ui_term::Paste)),
        )
        .separator()
        .item(
            menu::item("Split Right", &area, move |this, window, cx| {
                this.split_at(term, Axis::Row, window, cx)
            })
            .action(Box::new(SplitRight)),
        )
        .item(
            menu::item("Split Down", &area, move |this, window, cx| {
                this.split_at(term, Axis::Column, window, cx)
            })
            .action(Box::new(SplitDown)),
        )
        .item(menu::item("New Tab", &area, |this, window, cx| this.new_terminal(window, cx)).action(Box::new(NewTerminal)))
        .separator()
        .item(
            menu::item("Close Terminal", &area, move |this, window, cx| this.close_term(term, window, cx))
                .action(Box::new(CloseTab)),
        )
    }

    fn select(&mut self, term: TermId, cx: &mut Context<Self>) {
        if let Some(ix) = self.tabs.iter().position(|tab| tab.tree.leaves().contains(&term)) {
            self.active = ix;
            self.tabs[ix].active = term;
            cx.notify();
        }
    }

    fn remove(&mut self, term: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self
            .views
            .get(&term)
            .is_some_and(|view| view.read(cx).focus_handle(cx).is_focused(window));
        self.views.remove(&term);
        let Some(tab_ix) = self.tabs.iter().position(|tab| tab.tree.leaves().contains(&term)) else {
            return;
        };
        let tab = self.tabs.remove(tab_ix);
        match tab.tree.clone().remove(term) {
            Some(tree) => {
                // Focus moves to a neighbor of the one being closed.
                let active = [Direction::Left, Direction::Up, Direction::Right, Direction::Down]
                    .into_iter()
                    .find_map(|direction| tab.tree.neighbor(term, direction))
                    .filter(|_| tab.active == term)
                    .unwrap_or(if tab.active == term { tree.leaves()[0] } else { tab.active });
                self.tabs.insert(tab_ix, TerminalTab { id: tab.id, tree, active });
            }
            None => self.active = self.active.min(self.tabs.len().saturating_sub(1)),
        }
        if had_focus && let Some(next) = self.active_view() {
            next.read(cx).focus_handle(cx).focus(window, cx);
        }
        self.save();
        cx.notify();
    }

    fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        if let Some(view) = self.active_view() {
            view.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Saves which terminal goes where.
    fn save(&self) {
        let mut layouts = SavedLayouts::load();
        layouts.groups.insert(
            self.group.clone(),
            SavedLayout {
                tabs: self.tabs.iter().map(|tab| SavedTab::Tree(tab.tree.clone())).collect(),
            },
        );
        if let Err(err) = layouts.store() {
            eprintln!("couldn't save terminal layout: {err:#}");
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .id("terminal-tabs")
            .h(px(34.))
            .flex_none()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let active = ix == self.active;
                let title = self
                    .views
                    .get(&tab.active)
                    .map(|view| view.read(cx).title(cx))
                    .unwrap_or_default();
                let count = tab.tree.leaves().len();
                h_flex()
                    .id(("terminal-tab", tab.id))
                    .h_full()
                    .flex_none()
                    .max_w(px(220.))
                    .px_3()
                    .gap_1()
                    .text_ui(cx)
                    .border_r_1()
                    .border_color(theme.border)
                    .when(active, |el| el.bg(theme.tab_active).text_color(theme.tab_active_foreground))
                    .when(!active, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                    .when(count > 1, |el| {
                        el.child(div().text_ui_small(cx).text_color(theme.muted_foreground).child(format!("×{count}")))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.activate_tab(ix, window, cx)))
                    .context_menu({
                        let area = self.weak.clone();
                        move |menu, _, _| {
                            menu.item(
                                menu::item("New Terminal", &area, |this, window, cx| this.new_terminal(window, cx))
                                    .action(Box::new(NewTerminal)),
                            )
                            .item(
                                menu::item("Split Right", &area, move |this, window, cx| {
                                    this.activate_tab(ix, window, cx);
                                    this.split(Axis::Row, window, cx);
                                })
                                .action(Box::new(SplitRight)),
                            )
                            .item(
                                menu::item("Split Down", &area, move |this, window, cx| {
                                    this.activate_tab(ix, window, cx);
                                    this.split(Axis::Column, window, cx);
                                })
                                .action(Box::new(SplitDown)),
                            )
                                .separator()
                                .item(menu::item("Close Tab", &area, move |this, window, cx| {
                                    this.close_tab(ix, window, cx)
                                }))
                        }
                    })
            }))
            .child(
                div()
                    .id("new-terminal")
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .text_color(theme.muted_foreground)
                    .hover(|style| style.text_color(theme.foreground))
                    .child("+")
                    .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
            )
    }

    /// Renders a branch of the tree; `path` makes each split's ids unique.
    fn render_tree(&self, tab: &TerminalTab, tree: &Tree, path: String, cx: &App) -> AnyElement {
        match tree {
            Tree::Leaf(term) => {
                let Some(view) = self.views.get(term) else {
                    return div().into_any_element();
                };
                let split = matches!(tab.tree, Tree::Split { .. });
                let theme = cx.theme();
                let term = *term;
                let this = self.weak.clone();
                div()
                    .id(("terminal-pane", term))
                    .size_full()
                    .when(split, |el| {
                        el.border_1().border_color(if term == tab.active {
                            theme.primary.opacity(0.6)
                        } else {
                            theme.background
                        })
                    })
                    .child(view.clone())
                    .context_menu(move |menu, _, cx| match this.upgrade() {
                        Some(area) => area.read(cx).pane_menu(term, menu, cx),
                        None => menu,
                    })
                    .into_any_element()
            }
            Tree::Split { axis, children } => {
                let id = SharedString::from(format!("terminal-split-{}-{path}", tab.id));
                let group = match axis {
                    Axis::Row => h_resizable(id),
                    Axis::Column => v_resizable(id),
                };
                group
                    .children(children.iter().enumerate().map(|(ix, child)| {
                        resizable_panel().child(self.render_tree(tab, child, format!("{path}.{ix}"), cx))
                    }))
                    .into_any_element()
            }
        }
    }
}

impl Render for TerminalArea {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.tabs.get(self.active) {
            None => {
                let theme = cx.theme();
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_ui(cx)
                    .text_color(theme.muted_foreground)
                    .child(if self.client.is_some() {
                        "Cmd-T opens a terminal"
                    } else {
                        "No agent: no terminals"
                    })
                    .into_any_element()
            }
            Some(tab) => self.render_tree(tab, &tab.tree, "0".into(), cx),
        };
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(self.render_tab_bar(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .child({
                        let body_size = self.body_size.clone();
                        canvas(move |bounds, _, _| body_size.set(Some(bounds.size)), |_, _, _, _| {})
                            .absolute()
                            .size_full()
                    }),
            )
    }
}

/// Saved terminal layout, per group.
#[derive(Default, Serialize, Deserialize)]
struct SavedLayouts {
    groups: HashMap<String, SavedLayout>,
}

#[derive(Default, Serialize, Deserialize)]
struct SavedLayout {
    tabs: Vec<SavedTab>,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SavedTab {
    Tree(Tree),
    /// Old format: a column of terminals.
    Column(Vec<TermId>),
}

impl SavedTab {
    fn into_tree(self) -> Option<Tree> {
        match self {
            SavedTab::Tree(tree) => Some(tree),
            SavedTab::Column(terms) => match terms.as_slice() {
                [] => None,
                [term] => Some(Tree::Leaf(*term)),
                _ => Some(Tree::Split {
                    axis: Axis::Column,
                    children: terms.into_iter().map(Tree::Leaf).collect(),
                }),
            },
        }
    }
}

impl SavedLayouts {
    fn path() -> Result<PathBuf> {
        Ok(proto::config_dir()?.join("layout.json"))
    }

    fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn store(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
