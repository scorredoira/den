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
    /// order: their sections, top to bottom, each a panel or several as
    /// tabs. Every side panel is in one.
    #[serde(serialize_with = "save_places", deserialize_with = "known_places")]
    pub places: Vec<Vec<Vec<Panel>>>,
    /// The tabs in front of their sections; a section without one shows
    /// its first.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "known_panels")]
    pub front: Vec<Panel>,
    /// The activity bar's icons in order: a place's (the next of `places`)
    /// or another's. Those missing go after the rest.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient")]
    pub bar: Vec<Slot>,
    /// The icons in the order they were dragged to, each on its own: a
    /// tab's among any others. Unset, as `bar` puts them.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient")]
    pub order: Vec<Icon>,
    /// The icons taken off the bar from its menu: a place by its panels.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "lenient")]
    pub off: Vec<Icon>,
    /// The side panels taken off the column (Hide Panel): they keep their
    /// spot for when they're shown again.
    #[serde(deserialize_with = "known_panels")]
    pub hidden: Vec<Panel>,
    /// The sections folded to their header, each by one of its panels.
    #[serde(deserialize_with = "known_panels")]
    pub collapsed: Vec<Panel>,
    /// The sections' heights, as dragged, each by one of its panels.
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
            places: Group::ALL.into_iter().map(|group| group.panels().iter().map(|panel| vec![*panel]).collect()).collect(),
            front: Vec::new(),
            bar: Vec::new(),
            order: Vec::new(),
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
    #[serde(alias = "History")]
    Graph,
    Debug,
    Notes,
}

/// An icon of the activity bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Icon {
    /// A place's, by a panel of it.
    Place(Panel),
    /// The Graph tab's (named History before the History panel).
    #[serde(alias = "History")]
    Graph,
    Debug,
    Notes,
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

/// Panels by name, without those this version doesn't have: the Commit
/// Files panel of before the Graph tab, or a newer den's.
fn known_panels<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Panel>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(names.into_iter().filter_map(|name| Panel::deserialize(name).ok()).collect())
}

/// The places as written: a section of one panel by its name, tabs as a
/// list of their names; any panel this version doesn't have left out.
fn known_places<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Vec<Vec<Panel>>>, D::Error> {
    let places = Vec::<Vec<serde_json::Value>>::deserialize(deserializer)?;
    let known = |names: Vec<serde_json::Value>| -> Vec<Panel> { names.into_iter().filter_map(|name| Panel::deserialize(name).ok()).collect() };
    Ok(places
        .into_iter()
        .map(|sections| {
            sections
                .into_iter()
                .map(|section| match section {
                    serde_json::Value::Array(tabs) => known(tabs),
                    name => known(vec![name]),
                })
                .filter(|tabs| !tabs.is_empty())
                .collect()
        })
        .collect())
}

fn save_places<S: serde::Serializer>(places: &[Vec<Vec<Panel>>], serializer: S) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    #[serde(untagged)]
    enum Section<'a> {
        Alone(Panel),
        Tabs(&'a [Panel]),
    }
    let places: Vec<Vec<Section>> = places
        .iter()
        .map(|sections| sections.iter().map(|tabs| if let [panel] = tabs[..] { Section::Alone(panel) } else { Section::Tabs(tabs) }).collect())
        .collect();
    places.serialize(serializer)
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
        self.front = from.front.clone();
        self.bar = from.bar.clone();
        self.order = from.order.clone();
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
            for tabs in place.iter_mut() {
                tabs.retain(|panel| {
                    let keep = Group::of(*panel).is_some() && !seen.contains(panel);
                    seen.push(*panel);
                    keep
                });
            }
            place.retain(|tabs| !tabs.is_empty());
        }
        for group in Group::ALL {
            for (ix, panel) in group.panels().iter().enumerate().filter(|(_, panel)| !seen.contains(panel)) {
                // With those of its group, right after the one before it
                // there (a new panel goes where it would in a new layout:
                // the tab after it, if it's a tab), or a place of its own.
                match self.places.iter_mut().find(|place| place.iter().flatten().any(|other| group.panels().contains(other))) {
                    Some(place) => {
                        let at = ix
                            .checked_sub(1)
                            .and_then(|before| place.iter().position(|tabs| tabs.contains(&group.panels()[before])));
                        match at {
                            Some(at) if place[at].len() > 1 => {
                                let before = place[at].iter().position(|tab| *tab == group.panels()[ix - 1]).unwrap_or(0);
                                place[at].insert(before + 1, *panel);
                            }
                            _ => place.insert(at.map_or(place.len(), |at| at + 1), vec![*panel]),
                        }
                    }
                    None => self.places.push(vec![vec![*panel]]),
                }
            }
        }
        self.places.retain(|place| !place.is_empty());
    }

    /// Which of `places` has `panel`.
    fn index(&self, panel: Panel) -> Option<usize> {
        self.places.iter().position(|place| place.iter().flatten().any(|other| *other == panel))
    }

    /// The section `panel` is in: its tabs, hidden or not.
    fn section(&self, panel: Panel) -> &[Panel] {
        self.places.iter().flatten().find(|tabs| tabs.contains(&panel)).map(|tabs| &tabs[..]).unwrap_or_default()
    }

    /// The place `panel` is in, as the first of its panels in sight names
    /// it.
    pub fn place_of(&self, panel: Panel) -> Option<Place> {
        self.index(panel).map(|ix| self.name(ix))
    }

    fn name(&self, ix: usize) -> Place {
        let mut panels = self.places[ix].iter().flatten().copied();
        let first = self.places[ix][0][0];
        Place(panels.find(|panel| !self.hidden.contains(panel)).unwrap_or(first))
    }

    /// What the side column shows in `place`, top to bottom and tab after
    /// tab: nothing if its icon is off the bar.
    pub fn panels(&self, place: Place) -> Vec<Panel> {
        self.sections(place).into_iter().flatten().collect()
    }

    /// What the side column shows in `place`: its sections, top to bottom,
    /// each with its tabs in sight.
    pub fn sections(&self, place: Place) -> Vec<Vec<Panel>> {
        let Some(ix) = self.index(place.0).filter(|ix| !self.is_off(*ix)) else {
            return Vec::new();
        };
        self.places[ix]
            .iter()
            .map(|tabs| tabs.iter().copied().filter(|panel| !self.hidden.contains(panel)).collect::<Vec<_>>())
            .filter(|tabs| !tabs.is_empty())
            .collect()
    }

    /// Whether `panel` shares its section with another in sight: it's a
    /// tab, with an icon of its own.
    pub fn is_tab(&self, panel: Panel) -> bool {
        !self.hidden.contains(&panel) && self.section(panel).iter().filter(|other| !self.hidden.contains(other)).count() > 1
    }

    /// Of a section's tabs in sight, the one in front.
    pub fn front_of(&self, tabs: &[Panel]) -> Panel {
        tabs.iter().copied().find(|panel| self.front.contains(panel)).unwrap_or(tabs[0])
    }

    /// Whether `panel` is in front of its section.
    pub fn in_front(&self, panel: Panel) -> bool {
        let tabs: Vec<Panel> = self.section(panel).iter().copied().filter(|other| !self.hidden.contains(other)).collect();
        tabs.contains(&panel) && self.front_of(&tabs) == panel
    }

    /// Puts `panel` in front of its section.
    pub fn bring_to_front(&mut self, panel: Panel) {
        let tabs = self.section(panel).to_vec();
        self.front.retain(|other| !tabs.contains(other));
        if tabs.len() > 1 {
            self.front.push(panel);
        }
    }

    /// Whether `panel`'s section is folded to its header.
    pub fn is_collapsed(&self, panel: Panel) -> bool {
        self.section_or(panel).iter().any(|other| self.collapsed.contains(other))
    }

    /// Folds `panel`'s section, or unfolds it.
    pub fn set_collapsed(&mut self, panel: Panel, collapsed: bool) {
        let tabs = self.section_or(panel);
        self.collapsed.retain(|other| !tabs.contains(other));
        if collapsed {
            self.collapsed.push(tabs[0]);
        }
    }

    /// `panel`'s section, or the panel alone if it isn't in one (Terminal
    /// Mode's).
    fn section_or(&self, panel: Panel) -> Vec<Panel> {
        let tabs = self.section(panel);
        if tabs.is_empty() { vec![panel] } else { tabs.to_vec() }
    }

    /// Whether `places[ix]`'s icon is off the bar.
    fn is_off(&self, ix: usize) -> bool {
        self.places[ix].iter().flatten().any(|panel| self.off.contains(&Icon::Place(*panel)))
    }

    /// A place's icons: one, or one for each tab it has, which shows it
    /// with that tab in front.
    fn place_icons(&self, ix: usize) -> Vec<Icon> {
        let tabs: Vec<Icon> = self.places[ix].iter().flatten().filter(|panel| self.is_tab(**panel)).map(|panel| Icon::Place(*panel)).collect();
        if tabs.is_empty() { vec![Icon::Place(self.name(ix).0)] } else { tabs }
    }

    /// The bar's icons in order, and whether each is on it (its menu's
    /// checks): every place with a panel not hidden, and the others.
    pub fn icons(&self) -> Vec<(Icon, bool)> {
        let mut places = (0..self.places.len())
            .filter(|ix| self.places[*ix].iter().flatten().any(|panel| !self.hidden.contains(panel)))
            .map(|ix| self.place_icons(ix).into_iter().map(move |icon| (icon, !self.is_off(ix))).collect::<Vec<_>>());
        let mut icons: Vec<(Icon, bool)> = Vec::new();
        let push = |icons: &mut Vec<(Icon, bool)>, icon: (Icon, bool)| {
            if !icons.iter().any(|(other, _)| *other == icon.0) {
                icons.push(icon);
            }
        };
        for slot in &self.bar {
            match slot {
                Slot::Place => {
                    for icon in places.next().unwrap_or_default() {
                        push(&mut icons, icon);
                    }
                }
                Slot::Graph => push(&mut icons, (Icon::Graph, true)),
                Slot::Debug => push(&mut icons, (Icon::Debug, true)),
                Slot::Notes => push(&mut icons, (Icon::Notes, true)),
            }
        }
        icons.extend(places.flatten());
        for other in [Icon::Graph, Icon::Debug, Icon::Notes] {
            push(&mut icons, (other, true));
        }
        for (icon, on) in &mut icons {
            if !matches!(icon, Icon::Place(_)) {
                *on = !self.off.contains(icon);
            }
        }
        self.in_order(icons)
    }

    /// `icons` in the order dragged to: each where its own was, or a place's
    /// where one of its panels' was; one that had none (a new tab) after the
    /// icon before it in `icons`.
    fn in_order(&self, icons: Vec<(Icon, bool)>) -> Vec<(Icon, bool)> {
        if self.order.is_empty() {
            return icons;
        }
        let mut claimed = vec![false; self.order.len()];
        let mut rank: Vec<Option<usize>> = icons
            .iter()
            .map(|(icon, _)| {
                let at = self.order.iter().position(|other| other == icon)?;
                claimed[at] = true;
                Some(at)
            })
            .collect();
        for (ix, (icon, _)) in icons.iter().enumerate() {
            if rank[ix].is_some() {
                continue;
            }
            let Icon::Place(panel) = icon else {
                continue;
            };
            let place = self.index(*panel);
            let at = self.order.iter().enumerate().position(|(at, other)| {
                !claimed[at] && matches!(other, Icon::Place(other) if place.is_some() && self.index(*other) == place)
            });
            if let Some(at) = at {
                claimed[at] = true;
                rank[ix] = Some(at);
            }
        }
        let mut ranked: Vec<(usize, usize)> = rank.iter().enumerate().filter_map(|(ix, at)| Some(((*at)?, ix))).collect();
        ranked.sort();
        let mut sorted: Vec<usize> = ranked.into_iter().map(|(_, ix)| ix).collect();
        for ix in (0..icons.len()).filter(|ix| rank[*ix].is_none()) {
            let at = ix.checked_sub(1).and_then(|before| sorted.iter().position(|other| *other == before)).map_or(0, |at| at + 1);
            sorted.insert(at, ix);
        }
        sorted.into_iter().map(|ix| icons[ix]).collect()
    }

    /// The icons on the bar, in order.
    pub fn bar(&self) -> Vec<Icon> {
        self.icons().into_iter().filter(|(_, on)| *on).map(|(icon, _)| icon).collect()
    }

    /// Puts `icon` on the bar or takes it off; a place's keeps its panels.
    pub fn set_on_bar(&mut self, icon: Icon, on: bool) {
        let icons: Vec<Icon> = match icon {
            Icon::Place(panel) => self.index(panel).map(|ix| self.places[ix].iter().flatten().map(|panel| Icon::Place(*panel)).collect()).unwrap_or_default(),
            _ => vec![icon],
        };
        self.off.retain(|other| !icons.contains(other));
        if !on {
            self.off.extend(icons);
        }
    }

    /// Puts `icon` before `before`, or last: only the icon moves, whatever
    /// it shows stays where it is.
    pub fn move_icon(&mut self, icon: Icon, before: Option<Icon>) {
        let mut icons: Vec<Icon> = self.icons().into_iter().map(|(icon, _)| icon).collect();
        if !icons.contains(&icon) || before == Some(icon) {
            return;
        }
        icons.retain(|other| *other != icon);
        let at = before.and_then(|before| icons.iter().position(|other| *other == before)).unwrap_or(icons.len());
        icons.insert(at, icon);
        self.order = icons;
    }

    /// The panel an icon brings, if it brings one: a tab's, or a place's
    /// with only one in sight.
    pub fn icon_panel(&self, icon: Icon) -> Option<Panel> {
        let Icon::Place(panel) = icon else {
            return None;
        };
        if self.is_tab(panel) {
            return Some(panel);
        }
        match self.panels(self.place_of(panel)?)[..] {
            [only] => Some(only),
            _ => None,
        }
    }

    /// The place the side column shows, or last showed; the first one, if
    /// that has nothing to show now.
    pub fn current(&self) -> Option<Place> {
        self.place_of(self.place.0).filter(|place| !self.panels(*place).is_empty()).or_else(|| self.places().first().copied())
    }

    /// The activity bar's places: those with something to show.
    pub fn places(&self) -> Vec<Place> {
        (0..self.places.len()).map(|ix| self.name(ix)).filter(|place| !self.panels(*place).is_empty()).collect()
    }

    /// Takes `panel` out of its section, which keeps how it's folded and
    /// sized if that went by it. A section or place left empty stays, for
    /// `tidy`.
    fn take(&mut self, panel: Panel) {
        let Some(tabs) = self.places.iter_mut().flatten().find(|tabs| tabs.contains(&panel)) else {
            return;
        };
        tabs.retain(|other| *other != panel);
        let Some(next) = tabs.first().copied() else {
            return;
        };
        if self.collapsed.contains(&panel) && !self.collapsed.contains(&next) {
            self.collapsed.push(next);
        }
        if let Some(height) = self.heights.get(&panel).copied() {
            self.heights.entry(next).or_insert(height);
        }
        self.front.retain(|other| *other != panel);
    }

    /// Drops the empty sections and places.
    fn tidy(&mut self) {
        for place in &mut self.places {
            place.retain(|tabs| !tabs.is_empty());
        }
        self.places.retain(|place| !place.is_empty());
    }

    /// The panel naming the place showing is leaving it: one in sight that
    /// stays names it, so the column doesn't follow the panel. With none,
    /// the place goes from the bar, and the column follows.
    fn keep_place(&mut self, panel: Panel) {
        if self.place.0 == panel
            && let Some(ix) = self.index(panel)
            && let Some(other) = self.places[ix].iter().flatten().find(|other| **other != panel && !self.hidden.contains(other))
        {
            self.place = Place(*other);
        }
    }

    /// Puts `panel` in `place`, a section of its own above `before`'s (or
    /// last, unless it's there already: then it keeps its spot); a place
    /// left empty goes. The column stays on the place it shows.
    pub fn move_panel(&mut self, panel: Panel, place: Place, before: Option<Panel>) {
        if self.index(place.0).is_none() {
            return;
        }
        // The place may be named by the panel that moves.
        let others: Vec<Panel> = self.index(place.0).map(|ix| self.places[ix].iter().flatten().copied().collect()).unwrap_or_default();
        let Some(anchor) = others.iter().copied().find(|other| *other != panel) else {
            return;
        };
        // Already there, hidden or not, it keeps its spot.
        if before.is_none() && others.contains(&panel) {
            self.hidden.retain(|other| *other != panel);
            return;
        }
        // Above a section of its own tabs: above what's left of it.
        let before = match before {
            Some(before) => match self.section(before).iter().copied().find(|other| *other != panel) {
                Some(other) => Some(other),
                None => return,
            },
            None => None,
        };
        self.keep_place(panel);
        self.take(panel);
        let ix = self.index(anchor).expect("it stays");
        let target = &mut self.places[ix];
        let at = before.and_then(|before| target.iter().position(|tabs| tabs.contains(&before))).unwrap_or(target.len());
        target.insert(at, vec![panel]);
        self.tidy();
        self.hidden.retain(|other| *other != panel);
        self.set_on_bar(Icon::Place(panel), true);
    }

    /// Puts `panel` among `with`'s tabs, before `before` (or last), and in
    /// front; a place left empty goes. The column stays on the place it
    /// shows.
    pub fn join(&mut self, panel: Panel, with: Panel, before: Option<Panel>) {
        if panel == with || self.index(with).is_none() {
            return;
        }
        let before = before.filter(|before| *before != panel);
        self.keep_place(panel);
        self.take(panel);
        // It goes as the section is.
        self.collapsed.retain(|other| *other != panel);
        self.heights.remove(&panel);
        let tabs = self.places.iter_mut().flatten().find(|tabs| tabs.contains(&with)).expect("it stays");
        let at = before.and_then(|before| tabs.iter().position(|other| *other == before)).unwrap_or(tabs.len());
        tabs.insert(at, panel);
        self.tidy();
        self.hidden.retain(|other| *other != panel);
        self.bring_to_front(panel);
        self.set_on_bar(Icon::Place(panel), true);
    }

    /// Gives `panel` a place of its own, right after the one it's in.
    pub fn own_place(&mut self, panel: Panel) {
        let Some(ix) = self.index(panel).filter(|ix| self.places[*ix].iter().flatten().count() > 1) else {
            return;
        };
        self.take(panel);
        self.places.insert(ix + 1, vec![vec![panel]]);
        self.tidy();
    }

    /// Puts `from`'s panels in `into`, after its own.
    pub fn merge(&mut self, from: Place, into: Place) {
        let (Some(from), Some(into)) = (self.index(from.0), self.index(into.0)) else {
            return;
        };
        if from == into {
            return;
        }
        let sections = std::mem::take(&mut self.places[from]);
        self.places[into].extend(sections);
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
        let sections = self.places.remove(from);
        let at = before.and_then(|before| self.index(before.0)).unwrap_or(self.places.len());
        self.places.insert(at, sections);
    }

    /// The group a place stands for, by the panels it has: its icon and
    /// title, unless it shows a single panel.
    pub fn kind(&self, place: Place) -> Option<Group> {
        let panels: Vec<Panel> = self.index(place.0).map(|ix| self.places[ix].iter().flatten().copied().collect()).unwrap_or_default();
        Group::ALL.into_iter().find(|group| panels.contains(&group.filler()))
            .or_else(|| Group::ALL.into_iter().find(|group| panels.iter().any(|panel| group.panels().contains(panel))))
    }

    /// Of the sections open, the one that takes the height the others
    /// leave: the first with a group filler among its tabs, or the last.
    pub fn filler(open: &[Vec<Panel>]) -> Option<usize> {
        open.iter()
            .position(|tabs| tabs.iter().any(|panel| Group::ALL.iter().any(|group| group.filler() == *panel)))
            .or(open.len().checked_sub(1))
    }

    /// The height of `panel`'s section: as dragged, or its first panel's
    /// own.
    pub fn height(&self, panel: Panel) -> f32 {
        let tabs = self.section_or(panel);
        tabs.iter().find_map(|panel| self.heights.get(panel).copied()).unwrap_or(match tabs[0] {
            Panel::Workspaces | Panel::Changes => 240.,
            Panel::Agents => 110.,
            _ => 180.,
        })
    }

    /// Sizes `panel`'s section.
    pub fn set_height(&mut self, panel: Panel, height: f32) {
        let tabs = self.section_or(panel);
        self.heights.retain(|other, _| !tabs.contains(other));
        self.heights.insert(tabs[0], height);
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
