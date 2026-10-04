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
/// column, whose group of panels its icon picks; the code in the middle;
/// the terminals on its right or under it; the device, on the far right.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub side_width: f32,
    pub dock: Dock,
    /// The terminals as a column; unset, half of what the others leave.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dock_width: Option<f32>,
    /// The terminals as a row; unset, a third of the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dock_height: Option<f32>,
    pub device_width: f32,
    /// The side panels' heights, as dragged.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub heights: HashMap<Panel, f32>,
    /// The side panels folded to their header.
    pub collapsed: Vec<Panel>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            side_width: 260.,
            dock: Dock::Right,
            dock_width: None,
            dock_height: None,
            device_width: 400.,
            heights: HashMap::new(),
            collapsed: vec![Panel::Outline, Panel::References, Panel::Breakpoints],
        }
    }
}

impl Layout {
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
    Debug,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Explorer, Group::Search, Group::Git, Group::Debug];

    /// Its panels, top to bottom.
    pub fn panels(self) -> &'static [Panel] {
        match self {
            Group::Explorer => &[Panel::Workspaces, Panel::Agents, Panel::Files, Panel::Outline],
            Group::Search => &[Panel::Search, Panel::References],
            Group::Git => &[Panel::Changes, Panel::History],
            Group::Debug => &[Panel::CallStack, Panel::Variables, Panel::Watch, Panel::Breakpoints],
        }
    }

    /// The panel that takes the height the others leave, while it's open.
    pub fn filler(self) -> Panel {
        match self {
            Group::Explorer => Panel::Files,
            Group::Search => Panel::Search,
            Group::Git => Panel::History,
            Group::Debug => Panel::Variables,
        }
    }

    /// The group `panel` is in: the history's for the commit's files (a
    /// part of it), the debug one for the debugger.
    pub fn of(panel: Panel) -> Option<Group> {
        match panel {
            Panel::Commit => Some(Group::Git),
            Panel::Debugger => Some(Group::Debug),
            _ => Group::ALL.into_iter().find(|group| group.panels().contains(&panel)),
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Group::Explorer => "Explorer",
            Group::Search => "Search",
            Group::Git => "Source Control",
            Group::Debug => "Run and Debug",
        }
    }
}

/// What shows in the window: the side column's panels, the code, the
/// terminals and what goes with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Workspaces,
    Files,
    Changes,
    History,
    /// The files of the commit selected in the history, its lower part.
    Commit,
    Search,
    References,
    /// The classes, functions, constants… of the file in front, without
    /// what's inside the functions.
    Outline,
    Code,
    Terminals,
    Debugger,
    /// The screen of a phone (see `device`).
    Device,
    /// What's next in the workspace (see `notes`).
    Notes,
    /// The terminals running a coding agent, in every workspace.
    Agents,
    /// The debugger's parts, in the side column.
    #[serde(rename = "callstack")]
    CallStack,
    Variables,
    Watch,
    Breakpoints,
    /// The debugger's console, a tab of the terminals'.
    Console,
}

impl Panel {
    pub const ALL: [Panel; 19] = [
        Panel::Workspaces,
        Panel::Agents,
        Panel::Files,
        Panel::Outline,
        Panel::Search,
        Panel::References,
        Panel::Changes,
        Panel::History,
        Panel::Commit,
        Panel::CallStack,
        Panel::Variables,
        Panel::Watch,
        Panel::Breakpoints,
        Panel::Code,
        Panel::Terminals,
        Panel::Console,
        Panel::Debugger,
        Panel::Notes,
        Panel::Device,
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

/// What of a workspace shows besides the code.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedPanels {
    /// The group of the side column, the last shown if it's hidden.
    pub group: Group,
    pub side: bool,
    pub terminals: bool,
    pub device: bool,
}

impl Default for SavedPanels {
    /// A new workspace's: the files, the code and the terminals.
    fn default() -> Self {
        Self { group: Group::Explorer, side: true, terminals: true, device: false }
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
}

impl Default for DebugSaved {
    fn default() -> Self {
        Self { breakpoints: Vec::new(), watches: Vec::new(), uncaught: true, all: false, terminal: None }
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
    /// Where things go and their sizes.
    pub layout: Layout,
    /// Shortcuts changed in Settings: action → keys (`""` for no shortcut).
    pub keys: HashMap<String, String>,
    pub font_sizes: FontSizes,
    /// Long lines wrap in the editor (Opt-Z).
    pub word_wrap: bool,
    /// Save edited files when their editor loses focus.
    pub auto_save_on_focus_loss: bool,
    /// Extensions (`json`, `ts`…) formatted on saving.
    pub format_on_save: Vec<String>,
    /// Look for new releases every few hours; unset, it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_for_updates: Option<bool>,
    /// The workspaces column shows only the worktrees made from it (New
    /// Worktree), not those the agents make on their own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_own_worktrees: bool,
    /// The worktrees made from the workspaces column (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub own_worktrees: Vec<String>,
    /// The history without the selected commit's files under its commits
    /// (Hide Files, Cmd-Alt-Shift-H).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_files_hidden: bool,
    /// The groups of the Outline turned off with the icons at its top.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outline_hidden: Vec<OutlineGroup>,
}

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
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Changes the config and saves it.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Self)) {
        let config = cx.global_mut::<Self>();
        change(config);
        config.save();
    }

    /// Changes something that moves often (panel sizes, the window): it's
    /// saved with the next change or on quit.
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
            let side = !matches!(panel, Panel::Code | Panel::Terminals | Panel::Console | Panel::Device | Panel::Notes | Panel::Debugger | Panel::Commit);
            assert_eq!(groups, usize::from(side), "{panel:?}");
        }
        for group in Group::ALL {
            assert!(group.panels().contains(&group.filler()));
        }
        assert_eq!(Group::of(Panel::Commit), Some(Group::Git));
        assert_eq!(Group::of(Panel::Debugger), Some(Group::Debug));
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
