//! Where the panels go: the side column's places, the debugger's grid,
//! the activity bar's icons, saved in the config.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::splits::{Axis, Tree};

/// Where things go, the same in every workspace: on the left the side
/// column, which shows one place's panels; the code in the middle; the
/// terminals on its right or under it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// The side column shows, and the place it shows (or last showed):
    /// the same in every workspace, so that going from one to another
    /// doesn't move it.
    pub side: bool,
    #[serde(deserialize_with = "lenient")]
    pub place: Place,
    /// The side column's places, an icon each on the activity bar in this
    /// order: their panels, top to bottom. Every side panel is in one.
    #[serde(deserialize_with = "known_places")]
    pub places: Vec<Vec<Panel>>,
    /// The activity bar's icons in order: a place's (the next of `places`)
    /// or another's. Those missing go after the rest.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient")]
    pub bar: Vec<Slot>,
    /// The icons taken off the bar from its menu: a place by its panels.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient")]
    pub off: Vec<Icon>,
    /// The side panels taken off the column (Hide Panel): they keep their
    /// spot for when they're shown again.
    #[serde(deserialize_with = "known_panels")]
    pub hidden: Vec<Panel>,
    /// The side panels folded to their header.
    #[serde(deserialize_with = "known_panels")]
    pub collapsed: Vec<Panel>,
    /// The side panels' heights, as dragged.
    #[serde(skip_serializing_if = "HashMap::is_empty", deserialize_with = "known_heights")]
    pub heights: HashMap<Panel, f32>,
    pub side_width: f32,
    pub dock: Dock,
    /// The terminals as a column; unset, half of what the others leave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dock_width: Option<f32>,
    /// The terminals as a row; unset, a third of the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dock_height: Option<f32>,
    /// The debugger's tab: where each part goes, the ones hidden, and the
    /// sizes dragged.
    #[serde(deserialize_with = "lenient")]
    pub debug_grid: DebugGrid,
}

/// A part of the debugger, a cell of its tab.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum DebugPart {
    Stack,
    Variables,
    Watch,
    Breakpoints,
    Terminal,
    Console,
}

impl DebugPart {
    pub const ALL: [DebugPart; 6] =
        [DebugPart::Stack, DebugPart::Variables, DebugPart::Watch, DebugPart::Breakpoints, DebugPart::Terminal, DebugPart::Console];
}

/// The debugger's tab as splits (crate::splits): its parts side by side or
/// one above the other, each where it was dragged; those hidden, which keep
/// their place; and the sizes as dragged, by the parts of each split.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugGrid {
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub tree: Option<Tree<DebugPart>>,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "known_parts")]
    pub hidden: Vec<DebugPart>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub sizes: HashMap<String, Vec<f32>>,
}

impl DebugGrid {
    /// The parts as placed: as saved, each once, with any missing under the
    /// rest. Unset, three rows of two: the call stack and the variables, the
    /// watches and the console (what evaluates, together, at mid height), the
    /// terminal and the breakpoints.
    pub fn tree(&self) -> Tree<DebugPart> {
        use DebugPart::*;
        let row = |a, b| Tree::Split { axis: Axis::Row, children: vec![Tree::Leaf(a), Tree::Leaf(b)] };
        let Some(saved) = self.tree.clone() else {
            return Tree::Split {
                axis: Axis::Column,
                children: vec![row(Stack, Variables), row(Watch, Console), row(Terminal, Breakpoints)],
            };
        };
        let seen = std::cell::RefCell::new(Vec::new());
        let tree = saved.retain(&|part| {
            let mut seen = seen.borrow_mut();
            let first = !seen.contains(&part);
            seen.push(part);
            first
        });
        let seen = seen.into_inner();
        let missing = DebugPart::ALL.into_iter().filter(|part| !seen.contains(part)).map(Tree::Leaf);
        match tree {
            Some(Tree::Split { axis: Axis::Column, mut children }) => {
                children.extend(missing);
                Tree::Split { axis: Axis::Column, children }
            }
            Some(tree) => {
                let children: Vec<_> = std::iter::once(tree).chain(missing).collect();
                if children.len() == 1 { children.into_iter().next().unwrap() } else { Tree::Split { axis: Axis::Column, children } }
            }
            None => Tree::Split { axis: Axis::Column, children: missing.collect() },
        }
    }

    /// `dragged` dropped on `target`: beside it, along `axis` (before it or
    /// after), or with no axis in its place, `target` going to its own.
    pub fn place(&mut self, dragged: DebugPart, target: DebugPart, beside: Option<(Axis, bool)>) {
        if dragged == target {
            return;
        }
        let mut tree = self.tree();
        match beside {
            None => tree.swap(dragged, target),
            Some((axis, before)) => {
                let Some(mut rest) = tree.remove(dragged) else {
                    return;
                };
                rest.insert(target, &Tree::Leaf(dragged), axis, before);
                tree = rest;
            }
        }
        self.tree = Some(tree);
    }

    /// Hides a part, or shows it again in its place.
    pub fn set_hidden(&mut self, part: DebugPart, hidden: bool) {
        self.hidden.retain(|other| *other != part);
        if hidden {
            self.hidden.push(part);
        }
    }
}

fn known_parts<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<DebugPart>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(names.into_iter().filter_map(|name| DebugPart::deserialize(name).ok()).collect())
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            side: true,
            place: Place::default(),
            places: Group::ALL.into_iter().map(|group| group.panels().to_vec()).collect(),
            bar: Vec::new(),
            off: Vec::new(),
            hidden: vec![Panel::Agents],
            collapsed: vec![Panel::Outline, Panel::References],
            heights: HashMap::new(),
            side_width: 260.,
            dock: Dock::Right,
            dock_width: None,
            dock_height: None,
            debug_grid: DebugGrid::default(),
        }
    }
}

/// Terminal Mode's side column (the workspaces and the agents), apart from
/// the layout's: hiding it or sizing it there leaves the other as it is.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalModeLayout {
    pub side: bool,
    pub side_width: f32,
}

impl Default for TerminalModeLayout {
    fn default() -> Self {
        Self { side: true, side_width: 260. }
    }
}

/// A spot on the activity bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Slot {
    /// The next place's.
    Place,
    History,
    Debug,
    Notes,
}

/// An icon of the activity bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Icon {
    /// A place's, by a panel of it.
    Place(Panel),
    History,
    Debug,
    Notes,
}

impl Icon {
    fn slot(self) -> Slot {
        match self {
            Icon::Place(_) => Slot::Place,
            Icon::History => Slot::History,
            Icon::Debug => Slot::Debug,
            Icon::Notes => Slot::Notes,
        }
    }
}

/// A place of the side column: the one with this panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Place(pub Panel);

impl Default for Place {
    fn default() -> Self {
        Place(Panel::Files)
    }
}

/// A value that, written by another version of den, reads as its default
/// rather than losing the whole config.
pub(super) fn lenient<'de, D: serde::Deserializer<'de>, T: Deserialize<'de> + Default>(deserializer: D) -> Result<T, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

/// Panels by name, without those this version doesn't have: the History
/// and Commit Files panels of before the History tab, or a newer den's.
fn known_panels<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Panel>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(names.into_iter().filter_map(|name| Panel::deserialize(name).ok()).collect())
}

fn known_places<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Vec<Panel>>, D::Error> {
    let places = Vec::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    Ok(places
        .into_iter()
        .map(|names| names.into_iter().filter_map(|name| Panel::deserialize(name).ok()).collect())
        .collect())
}

fn known_heights<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<HashMap<Panel, f32>, D::Error> {
    let heights = HashMap::<String, f32>::deserialize(deserializer)?;
    Ok(heights
        .into_iter()
        .filter_map(|(name, height)| Some((Panel::deserialize(serde_json::Value::String(name)).ok()?, height)))
        .collect())
}

impl Layout {
    /// The side column and the activity bar of `from`: one in every
    /// workspace, whether it debugs or not, so that going from one to
    /// another moves neither.
    pub fn keep_column(&mut self, from: &Layout) {
        self.side = from.side;
        self.place = from.place;
        self.places = from.places.clone();
        self.bar = from.bar.clone();
        self.off = from.off.clone();
        self.hidden = from.hidden.clone();
        self.collapsed = from.collapsed.clone();
        self.heights = from.heights.clone();
        self.side_width = from.side_width;
    }

    /// Mends one edited by hand: every side panel in one place, once.
    pub fn repair(&mut self) {
        let mut seen = Vec::new();
        for place in &mut self.places {
            place.retain(|panel| {
                let keep = Group::of(*panel).is_some() && !seen.contains(panel);
                seen.push(*panel);
                keep
            });
        }
        for group in Group::ALL {
            for (ix, panel) in group.panels().iter().enumerate().filter(|(_, panel)| !seen.contains(panel)) {
                // With those of its group, right after the one before it
                // there (a new panel goes where it would in a new layout),
                // or a place of its own.
                match self.places.iter_mut().find(|place| place.iter().any(|other| group.panels().contains(other))) {
                    Some(place) => {
                        let at = ix.checked_sub(1).and_then(|before| place.iter().position(|other| *other == group.panels()[before]));
                        place.insert(at.map_or(place.len(), |at| at + 1), *panel);
                    }
                    None => self.places.push(vec![*panel]),
                }
            }
        }
        self.places.retain(|place| !place.is_empty());
    }

    /// Which of `places` has `panel`.
    fn index(&self, panel: Panel) -> Option<usize> {
        self.places.iter().position(|place| place.contains(&panel))
    }

    /// The place `panel` is in, as the first of its panels in sight names
    /// it.
    pub fn place_of(&self, panel: Panel) -> Option<Place> {
        self.index(panel).map(|ix| self.name(&self.places[ix]))
    }

    fn name(&self, panels: &[Panel]) -> Place {
        Place(panels.iter().copied().find(|panel| !self.hidden.contains(panel)).unwrap_or(panels[0]))
    }

    /// What the side column shows in `place`, top to bottom: nothing if
    /// its icon is off the bar.
    pub fn panels(&self, place: Place) -> Vec<Panel> {
        let Some(ix) = self.index(place.0).filter(|ix| !self.is_off(*ix)) else {
            return Vec::new();
        };
        self.places[ix].iter().copied().filter(|panel| !self.hidden.contains(panel)).collect()
    }

    /// Whether `places[ix]`'s icon is off the bar.
    fn is_off(&self, ix: usize) -> bool {
        self.places[ix].iter().any(|panel| self.off.contains(&Icon::Place(*panel)))
    }

    /// The bar's icons in order, and whether each is on it (its menu's
    /// checks): every place with a panel not hidden, and the others.
    pub fn icons(&self) -> Vec<(Icon, bool)> {
        let mut places = (0..self.places.len())
            .filter(|ix| self.places[*ix].iter().any(|panel| !self.hidden.contains(panel)))
            .map(|ix| (Icon::Place(self.name(&self.places[ix]).0), !self.is_off(ix)));
        let mut icons = Vec::new();
        for slot in &self.bar {
            let icon = match slot {
                Slot::Place => match places.next() {
                    Some(place) => place,
                    None => continue,
                },
                Slot::History => (Icon::History, true),
                Slot::Debug => (Icon::Debug, true),
                Slot::Notes => (Icon::Notes, true),
            };
            if !icons.iter().any(|(other, _)| *other == icon.0) {
                icons.push(icon);
            }
        }
        icons.extend(places);
        for other in [Icon::History, Icon::Debug, Icon::Notes] {
            if !icons.iter().any(|(icon, _)| *icon == other) {
                icons.push((other, true));
            }
        }
        for (icon, on) in &mut icons {
            if !matches!(icon, Icon::Place(_)) {
                *on = !self.off.contains(icon);
            }
        }
        icons
    }

    /// The icons on the bar, in order.
    pub fn bar(&self) -> Vec<Icon> {
        self.icons().into_iter().filter(|(_, on)| *on).map(|(icon, _)| icon).collect()
    }

    /// Puts `icon` on the bar or takes it off; a place's keeps its panels.
    pub fn set_on_bar(&mut self, icon: Icon, on: bool) {
        let icons: Vec<Icon> = match icon {
            Icon::Place(panel) => self.index(panel).map(|ix| self.places[ix].iter().map(|panel| Icon::Place(*panel)).collect()).unwrap_or_default(),
            _ => vec![icon],
        };
        self.off.retain(|other| !icons.contains(other));
        if !on {
            self.off.extend(icons);
        }
    }

    /// Puts `icon` before `before`, or last.
    pub fn move_icon(&mut self, icon: Icon, before: Option<Icon>) {
        if before == Some(icon) {
            return;
        }
        let mut icons: Vec<Icon> = self.icons().into_iter().map(|(icon, _)| icon).collect();
        let Some(from) = icons.iter().position(|other| *other == icon) else {
            return;
        };
        icons.remove(from);
        let at = before.and_then(|before| icons.iter().position(|other| *other == before)).unwrap_or(icons.len());
        icons.insert(at, icon);
        // The places in the bar's order; any not on it (all its panels
        // hidden) after them.
        let mut places: Vec<Vec<Panel>> = icons
            .iter()
            .filter_map(|icon| match icon {
                Icon::Place(panel) => self.index(*panel).map(|ix| self.places[ix].clone()),
                _ => None,
            })
            .collect();
        places.extend(self.places.iter().filter(|place| !places.contains(place)).cloned().collect::<Vec<_>>());
        self.places = places;
        self.bar = icons.into_iter().map(Icon::slot).collect();
    }

    /// The place the side column shows, or last showed; the first one, if
    /// that has nothing to show now.
    pub fn current(&self) -> Option<Place> {
        self.place_of(self.place.0).filter(|place| !self.panels(*place).is_empty()).or_else(|| self.places().first().copied())
    }

    /// The activity bar's places: those with something to show.
    pub fn places(&self) -> Vec<Place> {
        self.places.iter().map(|place| self.name(place)).filter(|place| !self.panels(*place).is_empty()).collect()
    }

    /// Puts `panel` in `place`, above `before` (or last, unless it's there
    /// already: then it keeps its spot); a place left empty goes. The
    /// column stays on the place it shows.
    pub fn move_panel(&mut self, panel: Panel, place: Place, before: Option<Panel>) {
        if before == Some(panel) || self.index(place.0).is_none() {
            return;
        }
        // The place may be named by the panel that moves.
        let others: Vec<Panel> = self.index(place.0).map(|ix| self.places[ix].clone()).unwrap_or_default();
        let Some(anchor) = others.iter().copied().find(|other| *other != panel) else {
            return;
        };
        // Already there, hidden or not, it keeps its spot.
        if before.is_none() && others.contains(&panel) {
            self.hidden.retain(|other| *other != panel);
            return;
        }
        // The panel naming the place showing leaves: one in sight that
        // stays names it, so the column doesn't follow the panel. With none,
        // the place goes from the bar, and the column follows.
        if self.place.0 == panel
            && let Some(ix) = self.index(panel)
            && let Some(other) = self.places[ix].iter().find(|other| **other != panel && !self.hidden.contains(other))
        {
            self.place = Place(*other);
        }
        for place in &mut self.places {
            place.retain(|other| *other != panel);
        }
        let ix = self.index(anchor).expect("it stays");
        let target = &mut self.places[ix];
        let at = before.and_then(|before| target.iter().position(|other| *other == before)).unwrap_or(target.len());
        target.insert(at, panel);
        self.places.retain(|place| !place.is_empty());
        self.hidden.retain(|other| *other != panel);
        self.set_on_bar(Icon::Place(panel), true);
    }

    /// Gives `panel` a place of its own, right after the one it's in.
    pub fn own_place(&mut self, panel: Panel) {
        let Some(ix) = self.index(panel).filter(|ix| self.places[*ix].len() > 1) else {
            return;
        };
        self.places[ix].retain(|other| *other != panel);
        self.places.insert(ix + 1, vec![panel]);
    }

    /// Puts `from`'s panels in `into`, after its own.
    pub fn merge(&mut self, from: Place, into: Place) {
        let (Some(from), Some(into)) = (self.index(from.0), self.index(into.0)) else {
            return;
        };
        if from == into {
            return;
        }
        let panels = std::mem::take(&mut self.places[from]);
        self.places[into].extend(panels);
        self.places.retain(|place| !place.is_empty());
    }

    /// Puts `place`'s icon before `before`'s, or last.
    pub fn move_place(&mut self, place: Place, before: Option<Place>) {
        let Some(from) = self.index(place.0) else {
            return;
        };
        // Dropped on itself: it stays.
        if before.and_then(|before| self.index(before.0)) == Some(from) {
            return;
        }
        let panels = self.places.remove(from);
        let at = before.and_then(|before| self.index(before.0)).unwrap_or(self.places.len());
        self.places.insert(at, panels);
    }

    /// The group a place stands for, by the panels it has: its icon and
    /// title, unless it shows a single panel.
    pub fn kind(&self, place: Place) -> Option<Group> {
        let panels = self.index(place.0).map(|ix| self.places[ix].clone()).unwrap_or_default();
        Group::ALL.into_iter().find(|group| panels.contains(&group.filler()))
            .or_else(|| Group::ALL.into_iter().find(|group| panels.iter().any(|panel| group.panels().contains(panel))))
    }

    /// The panel that takes the height the others leave: the first group
    /// filler among `open`, or the last.
    pub fn filler(open: &[Panel]) -> Option<Panel> {
        open.iter().copied().find(|panel| Group::ALL.iter().any(|group| group.filler() == *panel)).or(open.last().copied())
    }

    /// A side panel's height: as dragged, or its own.
    pub fn height(&self, panel: Panel) -> f32 {
        self.heights.get(&panel).copied().unwrap_or(match panel {
            Panel::Workspaces | Panel::Changes => 240.,
            Panel::Agents => 110.,
            _ => 180.,
        })
    }
}

/// Where the terminals go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dock {
    /// A column on the right of the code.
    #[default]
    Right,
    /// A row under the code.
    Bottom,
}

/// The groups of the side column, an icon each on the activity bar: one
/// shows at a time, its panels one above the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    #[default]
    Explorer,
    Search,
    Git,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Explorer, Group::Search, Group::Git];

    /// Its panels, top to bottom.
    pub fn panels(self) -> &'static [Panel] {
        match self {
            Group::Explorer => &[Panel::Workspaces, Panel::Agents, Panel::Files, Panel::Outline],
            Group::Search => &[Panel::Search, Panel::References],
            Group::Git => &[Panel::Changes],
        }
    }

    /// The panel that takes the height the others leave, while it's open.
    pub fn filler(self) -> Panel {
        match self {
            Group::Explorer => Panel::Files,
            Group::Search => Panel::Search,
            Group::Git => Panel::Changes,
        }
    }

    /// The group `panel` is in, if it's a side panel.
    pub fn of(panel: Panel) -> Option<Group> {
        Group::ALL.into_iter().find(|group| group.panels().contains(&panel))
    }

    pub fn title(self) -> &'static str {
        match self {
            Group::Explorer => "Explorer",
            Group::Search => "Search",
            Group::Git => "Source Control",
        }
    }
}

/// What shows in the window: the side column's panels, the code, the
/// terminals and what goes with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    /// The folders and repos opened, by server, each repo with its
    /// worktrees under it.
    Workspaces,
    Files,
    Changes,
    Search,
    References,
    /// The classes, functions, constants… of the file in front, without
    /// what's inside the functions.
    Outline,
    Code,
    Terminals,
    /// What's next in the workspace (see `notes`).
    Notes,
    /// The terminals running a coding agent, in every workspace.
    Agents,
    /// The debugger: a tab of the terminals' with its toolbar, its call
    /// stack, variables, watches and breakpoints side by side, and its
    /// console under them.
    Console,
}

impl Panel {
    pub const ALL: [Panel; 11] = [
        Panel::Workspaces,
        Panel::Agents,
        Panel::Files,
        Panel::Outline,
        Panel::Search,
        Panel::References,
        Panel::Changes,
        Panel::Code,
        Panel::Terminals,
        Panel::Console,
        Panel::Notes,
    ];
}
