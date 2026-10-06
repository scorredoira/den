//! UI configuration: `config.json` in den's config folder. It lives as a GPUI
//! global and is saved on every change.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use gpui_kit::component::ResizableState;
use gpui_kit::{App, AppContext as _, Bounds, Entity, Global, Pixels, Styled, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

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
    /// The debugger's call stack, variables, watches and breakpoints, the
    /// row above its console; unset, three fifths of the tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_height: Option<f32>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            side: true,
            place: Place::default(),
            places: Group::ALL.into_iter().map(|group| group.panels().to_vec()).collect(),
            hidden: vec![Panel::Agents],
            collapsed: vec![Panel::Outline, Panel::References],
            heights: HashMap::new(),
            side_width: 260.,
            dock: Dock::Right,
            dock_width: None,
            dock_height: None,
            debug_height: None,
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
fn lenient<'de, D: serde::Deserializer<'de>, T: Deserialize<'de> + Default>(deserializer: D) -> Result<T, D::Error> {
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
    /// The first layout of debugging, made from the editing one: the side
    /// column closed, as everything of the debugger is in its tab.
    pub fn for_debugging(&self) -> Layout {
        Layout { side: false, ..self.clone() }
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

    /// What the side column shows in `place`, top to bottom.
    pub fn panels(&self, place: Place) -> Vec<Panel> {
        self.index(place.0).map(|ix| self.places[ix].iter().copied().filter(|panel| !self.hidden.contains(panel)).collect()).unwrap_or_default()
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
            Panel::Worktrees => 140.,
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
            Group::Explorer => &[Panel::Workspaces, Panel::Worktrees, Panel::Agents, Panel::Files, Panel::Outline],
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
    /// The projects: the folders and repos opened, by server. Its name in
    /// the config is from before it had the worktrees in a panel of their own.
    Workspaces,
    /// The checkout and the worktrees of the project in front.
    Worktrees,
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
    pub const ALL: [Panel; 12] = [
        Panel::Workspaces,
        Panel::Worktrees,
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

/// Window position and size.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct SavedWindow {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    maximized: bool,
}

impl SavedWindow {
    pub fn from_bounds(bounds: WindowBounds) -> Option<Self> {
        let (rect, maximized) = match bounds {
            WindowBounds::Windowed(rect) => (rect, false),
            WindowBounds::Maximized(rect) => (rect, true),
            // Coming back from full screen, it opens at its normal size.
            WindowBounds::Fullscreen(rect) => (rect, false),
        };
        Some(Self {
            x: rect.origin.x.into(),
            y: rect.origin.y.into(),
            width: rect.size.width.into(),
            height: rect.size.height.into(),
            maximized,
        })
    }

    pub fn bounds(&self) -> WindowBounds {
        let rect = Bounds::new(point(px(self.x), px(self.y)), size(px(self.width), px(self.height)));
        if self.maximized {
            WindowBounds::Maximized(rect)
        } else {
            WindowBounds::Windowed(rect)
        }
    }
}

/// A server connected to over SSH (or, on Windows, a WSL distro).
#[derive(Clone, Serialize, Deserialize)]
pub struct HostConfig {
    /// How it's shown in the tasks column.
    pub name: String,
    /// A name from `~/.ssh/config`, `user@host` or `wsl:<distro>`.
    pub destination: String,
}

/// An open tab, with its cursor.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    /// The editor group it's in: 1 is the second one of a split.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub group: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// What's open in a task, to reopen it.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<SavedTab>,
    pub active: Option<usize>,
    /// The code area split in two, side by side or one above the other.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<crate::splits::Axis>,
    /// What shows; unset, as in a new one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shows: Option<SavedPanels>,
}

/// What of a workspace shows besides the code and the side column.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPanels {
    pub terminals: bool,
}

impl Default for SavedPanels {
    /// A new workspace's: the terminals.
    fn default() -> Self {
        Self { terminals: true }
    }
}

/// The debugger's state of a task: what survives closing the app.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugSaved {
    pub breakpoints: Vec<SavedBreakpoint>,
    pub watches: Vec<String>,
    /// Stop at exceptions nothing catches, and at every exception.
    pub uncaught: bool,
    pub all: bool,
    /// The terminal the launch command ran in: the next launch reuses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<u64>,
    /// The target picked in the toolbar (`targets` in the launch file).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Default for DebugSaved {
    fn default() -> Self {
        Self { breakpoints: Vec::new(), watches: Vec::new(), uncaught: true, all: false, terminal: None, target: None }
    }
}

/// A breakpoint: `path` relative to the task, `line` 0-based.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedBreakpoint {
    pub path: PathBuf,
    pub line: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub condition: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hit: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub log: String,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

fn is_enabled(enabled: &bool) -> bool {
    *enabled
}

/// A task on a server (`local` is this machine).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedTask {
    pub host: String,
    pub path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeChoice,
    /// List order, set by dragging; those not in it go at the end. Each is
    /// its path locally or `server:path` on a server.
    pub order: Vec<String>,
    pub hosts: Vec<HostConfig>,
    pub window: Option<SavedWindow>,
    /// What's open in each task (same keys as `order`).
    pub sessions: HashMap<String, Session>,
    /// Breakpoints and watches of each task (same keys as `order`).
    pub debug: HashMap<String, DebugSaved>,
    /// The last task visited, to return to on launch.
    pub last: Option<SavedTask>,
    /// Folders and tasks opened, the most recent first (Open Recent).
    pub recent: Vec<SavedTask>,
    /// Where things go and their sizes: the layout in use, the editing one
    /// or, while the workspace in front debugs, the debugging one.
    #[serde(deserialize_with = "lenient")]
    pub layout: Layout,
    /// The debugging layout while it isn't in use, as the last debug session
    /// left it; unset, `Layout::for_debugging` of the editing one.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub debug_layout: Option<Layout>,
    /// The editing layout while the debugging one is in use: set only while
    /// debugging.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub edit_layout: Option<Layout>,
    /// Shortcuts changed in Settings: action → keys (`""` for no shortcut).
    pub keys: HashMap<String, String>,
    pub font_sizes: FontSizes,
    /// Long lines wrap in the editor (Opt-Z).
    pub word_wrap: bool,
    /// The files panel shows what git ignores too (`target/`,
    /// `node_modules`…).
    pub show_ignored: bool,
    /// Save edited files when their editor loses focus.
    pub auto_save_on_focus_loss: bool,
    /// Extensions (`json`, `ts`…) formatted on saving.
    pub format_on_save: Vec<String>,
    /// Look for new releases every few hours; unset, it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_for_updates: Option<bool>,
    /// The Worktrees panel shows only the worktrees made from it (New
    /// Worktree), not those the agents make on their own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_own_worktrees: bool,
    /// Projects hidden from the Projects panel, Cmd-E and Cmd-K: a repo's
    /// checkout or a folder (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_projects: Vec<String>,
    /// The Projects panel shows the hidden ones too, dimmed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub show_hidden_projects: bool,
    /// The worktrees made from the Worktrees panel (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub own_worktrees: Vec<String>,
    /// The History tab's sizes, as dragged.
    #[serde(deserialize_with = "lenient")]
    pub history: HistorySizes,
    /// The History tab shows every branch, tag and remote, not only the
    /// current branch (as gitk does without `--all`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_all_branches: bool,
    /// The History tab shows the selected commit's files beside the commits.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_files: bool,
    /// The groups of the Outline turned off with the icons at its top.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outline_hidden: Vec<OutlineGroup>,
    /// Diffs side by side, in one column, or side by side when there's
    /// room; chosen in a diff's menu or in Settings.
    #[serde(deserialize_with = "lenient")]
    pub diff_layout: DiffLayout,
    /// The width under which an automatic diff goes to one column; unset,
    /// `DEFAULT_SIDE_BY_SIDE_WIDTH`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side_by_side_width: Option<f32>,
    /// The commands run from the Command Palette, the most recent first (by
    /// their id): they head it, as in VS Code.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent_commands: Vec<String>,
}

/// The parts of the History tab and its columns, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySizes {
    /// The commits' height, over the selected one.
    pub commits: f32,
    /// The commit's files, beside it.
    pub files: f32,
    /// The commits' author and date columns.
    pub author: f32,
    pub date: f32,
}

impl Default for HistorySizes {
    fn default() -> Self {
        Self { commits: 345., files: 450., author: 340., date: 185. }
    }
}

/// How a diff shows its two sides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLayout {
    /// Side by side when there's room for two, else in one column.
    #[default]
    Automatic,
    SideBySide,
    OneColumn,
}

impl DiffLayout {
    /// Whether a diff `width` wide shows its two sides; automatic, from
    /// `min` on.
    pub fn side_by_side(self, width: Pixels, min: f32) -> bool {
        match self {
            Self::Automatic => width >= px(min),
            Self::SideBySide => true,
            Self::OneColumn => false,
        }
    }
}

/// Narrower than this, an automatic diff shows in one column.
pub const DEFAULT_SIDE_BY_SIDE_WIDTH: f32 = 1200.;
pub const MIN_SIDE_BY_SIDE_WIDTH: f32 = 600.;
pub const MAX_SIDE_BY_SIDE_WIDTH: f32 = 3000.;

/// What the icons at the top of the Outline show or hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutlineGroup {
    Constants,
    Interfaces,
    /// Classes and what's in them: their methods.
    Classes,
    Functions,
}

/// Text sizes chosen in Settings; unset, the default.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FontSizes {
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    editor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal: Option<f32>,
}

/// Each place with its own text size.
#[derive(Clone, Copy, PartialEq)]
pub enum TextArea {
    Interface,
    Editor,
    Preview,
    Terminal,
}

impl TextArea {
    /// Interface and code as in VS Code; rendered Markdown, larger, for reading.
    pub fn default_size(self) -> f32 {
        match self {
            Self::Preview => 16.,
            Self::Interface | Self::Editor | Self::Terminal => 13.,
        }
    }

    fn slot(self, sizes: &mut FontSizes) -> &mut Option<f32> {
        match self {
            Self::Interface => &mut sizes.interface,
            Self::Editor => &mut sizes.editor,
            Self::Preview => &mut sizes.preview,
            Self::Terminal => &mut sizes.terminal,
        }
    }
}

impl Global for Config {}

pub const MIN_FONT_SIZE: f32 = 10.;
pub const MAX_FONT_SIZE: f32 = 24.;

impl Config {
    pub fn font_size(&self, area: TextArea) -> f32 {
        let mut sizes = self.font_sizes;
        area.slot(&mut sizes).unwrap_or(area.default_size()).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    }

    /// The width under which an automatic diff shows in one column.
    pub fn side_by_side_width(&self) -> f32 {
        self.side_by_side_width.unwrap_or(DEFAULT_SIDE_BY_SIDE_WIDTH).clamp(MIN_SIDE_BY_SIDE_WIDTH, MAX_SIDE_BY_SIDE_WIDTH)
    }

    /// `None` goes back to the default.
    pub fn set_side_by_side_width(&mut self, width: Option<f32>) {
        let width = width.map(|width| width.clamp(MIN_SIDE_BY_SIDE_WIDTH, MAX_SIDE_BY_SIDE_WIDTH));
        self.side_by_side_width = width.filter(|width| *width != DEFAULT_SIDE_BY_SIDE_WIDTH);
    }

    /// Whether a diff `width` wide shows its two sides.
    pub fn diff_side_by_side(&self, width: Pixels) -> bool {
        self.diff_layout.side_by_side(width, self.side_by_side_width())
    }

    /// Whether it looks for new releases by itself.
    pub fn checks_for_updates(&self) -> bool {
        self.check_for_updates.unwrap_or(true)
    }

    /// Whether saving `path` formats it first.
    pub fn formats_on_save(&self, path: &std::path::Path) -> bool {
        let ext = path.extension().map(|ext| ext.to_string_lossy().to_lowercase()).unwrap_or_default();
        !ext.is_empty() && self.format_on_save.contains(&ext)
    }

    /// `None` goes back to the default.
    pub fn set_font_size(&mut self, area: TextArea, size: Option<f32>) {
        let size = size.map(|size| size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        *area.slot(&mut self.font_sizes) = size.filter(|size| *size != area.default_size());
    }

    /// None in tests: they never touch the real one.
    fn path() -> Option<PathBuf> {
        if cfg!(test) {
            return None;
        }
        proto::config_dir().ok().map(|dir| dir.join("config.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        let (mut config, whole) = Self::parse(&bytes);
        if !whole {
            // What couldn't be read is kept aside before the next save
            // writes over it.
            let _ = std::fs::write(path.with_extension("json.bad"), &bytes);
        }
        // no debug session outlives the app
        config.use_debug_layout(false);
        config.layout.repair();
        if let Some(layout) = &mut config.debug_layout {
            layout.repair();
        }
        config
    }

    /// The debugging layout is in use.
    pub fn debugging(&self) -> bool {
        self.edit_layout.is_some()
    }

    /// Puts the debugging layout in use, or the editing one back, each as it
    /// was left: what changes while debugging stays in the debugging one.
    pub fn use_debug_layout(&mut self, debugging: bool) {
        if debugging == self.debugging() {
            return;
        }
        if debugging {
            let debug = self.debug_layout.take().unwrap_or_else(|| self.layout.for_debugging());
            self.edit_layout = Some(std::mem::replace(&mut self.layout, debug));
        } else if let Some(edit) = self.edit_layout.take() {
            self.debug_layout = Some(std::mem::replace(&mut self.layout, edit));
        }
    }

    /// The config in `bytes`, and whether all of it was read. A value this
    /// version can't read (one written by another version, or edited by
    /// hand) goes back to its default without losing the others.
    fn parse(bytes: &[u8]) -> (Self, bool) {
        if let Ok(config) = serde_json::from_slice::<Self>(bytes) {
            return (config, true);
        }
        let Ok(serde_json::Value::Object(fields)) = serde_json::from_slice(bytes) else {
            return (Self::default(), false);
        };
        let mut read = serde_json::Map::new();
        for (key, value) in fields {
            read.insert(key.clone(), value);
            if serde_json::from_value::<Self>(serde_json::Value::Object(read.clone())).is_err() {
                read.remove(&key);
            }
        }
        (serde_json::from_value(serde_json::Value::Object(read)).unwrap_or_default(), false)
    }

    /// Saves it now (on quit: nothing would be left to write it later).
    fn save(&self) {
        if let Some(bytes) = self.to_save() {
            write_config(bytes);
        }
    }

    /// Saves it on a thread of its own: the disk never holds up the window.
    fn save_later(&self) {
        let Some(bytes) = self.to_save() else {
            return;
        };
        static WRITER: std::sync::OnceLock<std::sync::mpsc::Sender<(u64, Vec<u8>)>> = std::sync::OnceLock::new();
        let writer = WRITER.get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<(u64, Vec<u8>)>();
            std::thread::spawn(move || {
                while let Ok(mut latest) = rx.recv() {
                    // Only the newest of those waiting is written.
                    while let Ok(newer) = rx.try_recv() {
                        latest = newer;
                    }
                    write_generation(latest.0, latest.1);
                }
            });
            tx
        });
        let _ = writer.send(bytes);
    }

    /// The config as it's written, numbered so an older one never replaces
    /// a newer one.
    fn to_save(&self) -> Option<(u64, Vec<u8>)> {
        let bytes = serde_json::to_vec_pretty(self).ok()?;
        Some((SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed), bytes))
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Changes the config and saves it.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Self)) {
        let config = cx.global_mut::<Self>();
        change(config);
        config.save_later();
        // The panels drawn from cache read it too.
        cx.refresh_windows();
    }

    /// Changes something that moves often (panel sizes, the window, the
    /// session's cursor): it's saved with the next change or on quit. Nothing
    /// drawn from cache reads these (sizes change the bounds, which redraws
    /// it anyway), so windows aren't redrawn whole for it, with every key.
    pub fn update_quietly(cx: &mut App, change: impl FnOnce(&mut Self)) {
        change(cx.global_mut::<Self>());
    }

    /// Loads the config as a global and saves it on quit.
    pub fn init(cx: &mut App) {
        cx.set_global(Self::load());
        cx.on_app_quit(|cx| {
            cx.global::<Self>().save();
            async {}
        })
        .detach();
    }
}

/// How many times the config was prepared for saving: each save's number.
static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_config((generation, bytes): (u64, Vec<u8>)) {
    write_generation(generation, bytes);
}

/// Writes `bytes` unless a newer save was written already.
fn write_generation(generation: u64, bytes: Vec<u8>) {
    static WRITTEN: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);
    let mut written = WRITTEN.lock().unwrap_or_else(|err| err.into_inner());
    if written.is_some_and(|written| written > generation) {
        return;
    }
    let Some(path) = Config::path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Whole or not at all: a crash halfway leaves the one before.
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, &path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    *written = Some(generation);
}

/// Interface text at the size chosen in Settings.
pub trait UiText: Styled + Sized {
    fn text_ui(self, cx: &App) -> Self {
        self.text_size(px(Config::get(cx).font_size(TextArea::Interface)))
    }

    /// Secondary text: one point smaller.
    fn text_ui_small(self, cx: &App) -> Self {
        self.text_size(px(Config::get(cx).font_size(TextArea::Interface) - 1.))
    }
}

impl<T: Styled> UiText for T {}

/// Width a panel opens with: the saved one, within limits.
pub fn width(saved: f32, min: f32, max: f32) -> Pixels {
    px(saved.clamp(min, max))
}

/// State of a row of panels where, when the width changes, only the unsized
/// panel (the code) changes: the others go back to their saved width.
/// On its own, gpui-component spreads the change across all of them in proportion.
pub struct Split {
    state: Entity<ResizableState>,
    width: Option<Pixels>,
    key: Option<u64>,
}

impl Split {
    pub fn new(cx: &mut App) -> Self {
        Self {
            state: cx.new(|_| ResizableState::default()),
            width: None,
            key: None,
        }
    }

    /// The state for painting the row in a space of `width`, with the
    /// panels `key` says (which are visible, in which order); if either
    /// changed, it starts again from the saved sizes. A hidden panel keeps
    /// the size it was last painted at, and dragging would count it as
    /// taken: the dragged panel jumps back to its minimum with every move.
    pub fn state(&mut self, width: Pixels, key: impl Hash, cx: &mut App) -> &Entity<ResizableState> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let key = hasher.finish();
        if self.width.is_some_and(|last| last != width) || self.key.is_some_and(|last| last != key) {
            self.state.update(cx, |state, _| state.clear());
        }
        self.width = Some(width);
        self.key = Some(key);
        &self.state
    }
}

#[cfg(test)]
mod load_tests {
    use super::{Config, ThemeChoice};

    #[test]
    fn a_value_it_cant_read_loses_only_that_value() {
        let json = br#"{ "theme": "solarized", "order": ["/a", "/b"], "word_wrap": true }"#;
        let (config, whole) = Config::parse(json);
        assert!(!whole);
        assert!(matches!(config.theme, ThemeChoice::System));
        assert_eq!(config.order, ["/a", "/b"]);
        assert!(config.word_wrap);
    }

    #[test]
    fn a_cut_file_reads_as_the_default_and_says_so() {
        let (config, whole) = Config::parse(br#"{ "order": ["/a""#);
        assert!(!whole);
        assert!(config.order.is_empty());
        assert!(Config::parse(br#"{ "order": ["/a"] }"#).1);
    }

    #[test]
    fn diffs_go_to_one_column_when_narrow_unless_told() {
        use super::{DEFAULT_SIDE_BY_SIDE_WIDTH, DiffLayout, MAX_SIDE_BY_SIDE_WIDTH};
        use gpui_kit::px;
        let mut config = Config::default();
        assert_eq!(config.diff_layout, DiffLayout::Automatic);
        assert_eq!(config.side_by_side_width(), DEFAULT_SIDE_BY_SIDE_WIDTH);
        assert!(config.diff_side_by_side(px(1200.)) && !config.diff_side_by_side(px(1199.)));
        config.set_side_by_side_width(Some(800.));
        assert!(config.diff_side_by_side(px(900.)));
        // The default isn't kept, and the width stays within its range.
        config.set_side_by_side_width(Some(DEFAULT_SIDE_BY_SIDE_WIDTH));
        assert_eq!(config.side_by_side_width, None);
        config.set_side_by_side_width(Some(99999.));
        assert_eq!(config.side_by_side_width(), MAX_SIDE_BY_SIDE_WIDTH);
        config.diff_layout = DiffLayout::SideBySide;
        assert!(config.diff_side_by_side(px(10.)));
        config.diff_layout = DiffLayout::OneColumn;
        assert!(!config.diff_side_by_side(px(10000.)));
    }

    #[test]
    fn the_diff_layout_is_saved_and_an_unknown_one_reads_as_automatic() {
        use super::DiffLayout;
        let config = Config { diff_layout: DiffLayout::OneColumn, side_by_side_width: Some(900.), ..Config::default() };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""diff_layout":"one_column""#));
        let (read, whole) = Config::parse(json.as_bytes());
        assert!(whole);
        assert_eq!((read.diff_layout, read.side_by_side_width), (DiffLayout::OneColumn, Some(900.)));
        let (read, whole) = Config::parse(br#"{ "diff_layout": "sideways", "order": ["/a"] }"#);
        assert!(whole);
        assert_eq!(read.diff_layout, DiffLayout::Automatic);
        assert_eq!(read.order, ["/a"]);
    }
}

#[cfg(test)]
mod split_tests {
    use gpui_kit::component::{h_resizable, resizable_panel};
    use gpui_kit::*;
    use core::prelude::v1::test;

    use super::Split;

    /// Side panel, code and terminals, as in a workspace.
    struct Row {
        split: Split,
        terminals: bool,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let state = self.split.state(px(1000.), [true, true, self.terminals], cx).clone();
            div().w(px(1000.)).h(px(100.)).child(
                h_resizable("row")
                    .with_state(&state)
                    .child(resizable_panel().size(px(200.)).size_range(px(160.)..px(600.)).child(div().size_full()))
                    .child(resizable_panel().child(div().size_full()))
                    .child(
                        resizable_panel()
                            .size(px(300.))
                            .size_range(px(240.)..px(4000.))
                            .visible(self.terminals)
                            .child(div().size_full()),
                    ),
            )
        }
    }

    /// Terminals shown and then hidden: dragging the side panel follows the
    /// mouse instead of jumping back to its minimum, and the terminals come
    /// back at their size.
    #[gpui_kit::test]
    fn dragging_beside_hidden_terminals_follows_the_mouse(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|_, cx| Row { split: Split::new(cx), terminals: true });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        view.update(cx, |view, cx| {
            view.terminals = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        let state = view.update(cx, |view, cx| view.split.state(px(1000.), [true, true, false], cx).clone());
        for width in [260., 300., 340.] {
            cx.update(|window, cx| state.update(cx, |state, cx| state.resize_panel(0, px(width), window, cx)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(state.read_with(cx, |state, _| state.sizes()[0]), px(width));
        }
        // Shown again, the terminals come back at their size, not squeezed.
        view.update(cx, |view, cx| {
            view.terminals = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        let state = view.update(cx, |view, cx| view.split.state(px(1000.), [true, true, true], cx).clone());
        assert_eq!(state.read_with(cx, |state, _| state.sizes()[2]), px(300.));
    }
}

#[cfg(test)]
mod layout_tests {
    use super::{Group, Layout, Panel, SavedPanels, Session};
    use core::prelude::v1::test;

    #[test]
    fn every_side_panel_is_in_one_group() {
        for panel in Panel::ALL {
            let groups = Group::ALL.into_iter().filter(|group| group.panels().contains(&panel)).count();
            let side = !matches!(panel, Panel::Code | Panel::Terminals | Panel::Console | Panel::Notes);
            assert_eq!(groups, usize::from(side), "{panel:?}");
        }
        for group in Group::ALL {
            assert!(group.panels().contains(&group.filler()));
        }
        assert_eq!(Group::of(Panel::Console), None);
    }

    #[test]
    fn panels_go_anywhere_and_icons_reorder() {
        use super::Place;
        let mut layout = Layout::default();
        let explorer = Place(Panel::Workspaces);
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Worktrees, Panel::Files, Panel::Outline]);
        assert_eq!(layout.place_of(Panel::Files), Some(explorer));
        layout.move_panel(Panel::Files, explorer, Some(Panel::Workspaces));
        assert_eq!(layout.panels(explorer), [Panel::Files, Panel::Workspaces, Panel::Worktrees, Panel::Outline]);
        // Dropped past the icons, a panel gets one of its own, last.
        let mut last = layout.clone();
        last.own_place(Panel::Outline);
        let outline = last.place_of(Panel::Outline).unwrap();
        last.move_place(outline, None);
        assert_eq!(last.places().last(), Some(&Place(Panel::Outline)));
        // Dropped on its own icon, it stays where it is.
        let first = layout.places()[0];
        layout.move_place(first, Some(first));
        assert_eq!(layout.places()[0], first);
        // Named by its first panel: still the same place.
        let explorer = layout.place_of(Panel::Workspaces).unwrap();
        // Into another place, and hidden ones come back.
        let git = layout.place_of(Panel::Changes).unwrap();
        layout.move_panel(Panel::Agents, git, None);
        assert_eq!(layout.panels(git), [Panel::Changes, Panel::Agents]);
        // A place of its own, right after; merged back.
        layout.own_place(Panel::Workspaces);
        let workspaces = Place(Panel::Workspaces);
        assert_eq!(layout.panels(workspaces), [Panel::Workspaces]);
        assert_eq!(layout.places()[1], workspaces);
        layout.merge(workspaces, git);
        assert!(layout.places().len() == 3 && layout.panels(git).ends_with(&[Panel::Workspaces]));
        // Icons reorder.
        layout.move_place(git, Some(explorer));
        assert_eq!(layout.places()[0], git);
        layout.move_place(git, None);
        assert_eq!(layout.places().last(), Some(&git));
        // The last panel of a place leaves: the place goes.
        let search = Place(Panel::Search);
        layout.move_panel(Panel::Search, git, None);
        layout.move_panel(Panel::References, git, None);
        assert!(!layout.places().contains(&search));
    }

    #[test]
    fn the_column_stays_on_its_place_as_its_panels_move() {
        use super::Place;
        let mut layout = Layout::default();
        let git = layout.place_of(Panel::Changes).unwrap();
        // The panel naming the place showing goes elsewhere: the column stays.
        assert_eq!(layout.place, Place(Panel::Files));
        layout.move_panel(Panel::Files, git, None);
        assert_eq!(layout.current(), layout.place_of(Panel::Outline));
        assert_eq!(layout.panels(layout.current().unwrap()), [Panel::Workspaces, Panel::Worktrees, Panel::Outline]);
        // As a click names it, by its first panel in sight.
        layout.place = layout.current().unwrap();
        layout.move_panel(Panel::Workspaces, git, None);
        layout.move_panel(Panel::Worktrees, git, None);
        assert_eq!(layout.panels(layout.current().unwrap()), [Panel::Outline]);
        // The last one leaves: the column goes with it.
        layout.move_panel(Panel::Outline, git, None);
        assert_eq!(layout.current(), Some(git));
    }

    #[test]
    fn a_panel_brought_where_it_is_keeps_its_spot() {
        use super::Place;
        let mut layout = Layout::default();
        let explorer = Place(Panel::Workspaces);
        layout.move_panel(Panel::Files, explorer, None);
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Worktrees, Panel::Files, Panel::Outline]);
        // A hidden one comes back where it was.
        layout.move_panel(Panel::Agents, explorer, None);
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Worktrees, Panel::Agents, Panel::Files, Panel::Outline]);
        // Above another, it moves.
        layout.move_panel(Panel::Outline, explorer, Some(Panel::Workspaces));
        assert_eq!(layout.panels(explorer)[0], Panel::Outline);
    }

    #[test]
    fn the_debuggers_old_side_panels_load_as_gone() {
        let mut layout: Layout = serde_json::from_str(
            r#"{"place": "variables", "places": [["files"], ["callstack", "variables", "watch", "breakpoints"]], "hidden": ["watch", "agents"], "collapsed": ["breakpoints"], "heights": {"variables": 100, "files": 200}}"#,
        )
        .unwrap();
        layout.repair();
        assert!(layout.places.iter().flatten().all(|panel| Group::of(*panel).is_some()));
        assert_eq!(layout.hidden, [Panel::Agents]);
        assert!(layout.collapsed.is_empty());
        assert_eq!(layout.heights.len(), 1);
        assert_eq!(layout.current().map(|place| layout.panels(place).contains(&Panel::Files)), Some(true));
    }

    #[test]
    fn a_layout_edited_by_hand_is_mended() {
        let mut layout: Layout = serde_json::from_str(r#"{"places": [["files", "files", "code"], []]}"#).unwrap();
        layout.repair();
        assert_eq!(layout.places[0], [Panel::Files, Panel::Outline, Panel::Workspaces, Panel::Worktrees, Panel::Agents]);
        assert!(Panel::ALL.iter().filter(|panel| Group::of(**panel).is_some()).all(|panel| layout.place_of(*panel).is_some()));
        // The History and Commit Files panels of before the History tab are left out.
        let config: super::Config = serde_json::from_str(
            r#"{"layout": {"place": "history", "places": [["files"], ["changes", "history", "commit"]], "hidden": ["agents", "commit"], "collapsed": ["history"], "heights": {"history": 100, "changes": 200}}}"#,
        )
        .unwrap();
        let mut layout = config.layout;
        layout.repair();
        assert_eq!(layout.place, super::Place::default());
        assert_eq!(layout.places[1], [Panel::Changes]);
        assert_eq!(layout.hidden, [Panel::Agents]);
        assert!(layout.collapsed.is_empty());
        assert_eq!(layout.heights.into_iter().collect::<Vec<_>>(), [(Panel::Changes, 200.)]);
        // One from before the places reads as a new one, the rest of the config intact.
        let config: super::Config = serde_json::from_str(r#"{"order": ["/a"], "layout": {"place": {"group": "explorer"}, "alone": ["files"]}}"#).unwrap();
        assert_eq!(config.order, ["/a"]);
        assert_eq!(config.layout.place, super::Place::default());
    }

    #[test]
    fn a_session_from_before_the_groups_starts_as_a_new_one() {
        let old = r#"{"tabs": [], "layout": {"columns": [{"stacks": [{"panels": ["files"]}]}]}, "panels": {"shown": {"files": 1}, "closed": [], "clock": 1}}"#;
        let session: Session = serde_json::from_str(old).unwrap();
        assert_eq!(session.shows.unwrap_or_default(), SavedPanels::default());
        let layout: Layout = serde_json::from_str("{}").unwrap();
        assert!(layout == Layout::default());
    }
}
