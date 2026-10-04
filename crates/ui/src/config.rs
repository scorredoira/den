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

/// Where the panels go and their sizes, the same for every task.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// Left to right. Missing (a config from before the columns), it's made
    /// from the old fields below.
    #[serde(default)]
    pub columns: Vec<Column>,
    // Read to carry an older layout over; never written.
    #[serde(skip_serializing)]
    tasks: Option<f32>,
    #[serde(skip_serializing)]
    side: Option<f32>,
    #[serde(skip_serializing)]
    terminals: Option<f32>,
    #[serde(skip_serializing)]
    debug: Option<f32>,
    #[serde(skip_serializing)]
    debug_at: Option<PanelAt>,
    #[serde(skip_serializing)]
    debug_width: Option<f32>,
    #[serde(skip_serializing)]
    terminals_at: Option<PanelAt>,
    #[serde(skip_serializing)]
    terminals_height: Option<f32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PanelAt {
    Bottom,
    Right,
}

/// What can be placed in a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Panel {
    Workspaces,
    Files,
    Changes,
    History,
    /// The files of the commit selected in the history. In the history's
    /// place, it's the lower part of the history (see `Layout::commit_in_history`).
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
    /// Configs from when the agents had a panel of their own (they're in
    /// the workspaces column now) name it: it's dropped on reading.
    Agents,
}

impl Panel {
    /// In the activity bar's order, unless it's dragged.
    pub const ALL: [Panel; 13] = [
        Panel::Workspaces,
        Panel::Files,
        Panel::Search,
        Panel::Changes,
        Panel::History,
        Panel::Commit,
        Panel::References,
        Panel::Outline,
        Panel::Code,
        Panel::Terminals,
        Panel::Debugger,
        Panel::Notes,
        Panel::Device,
    ];

    /// The width of a column of its own when it gets one: the lists' and
    /// the debugger's a set one; unset (the terminals'), half of what the
    /// others leave. The code's takes the rest anyway.
    fn width(self) -> Option<f32> {
        match self {
            Panel::Workspaces | Panel::Agents | Panel::Files | Panel::Changes | Panel::History | Panel::Commit | Panel::Search | Panel::References | Panel::Outline => Some(260.),
            Panel::Debugger => Some(420.),
            Panel::Device => Some(400.),
            Panel::Notes => Some(320.),
            Panel::Code | Panel::Terminals => None,
        }
    }
}

/// Off the activity bar until shown: what is always in sight or opens by
/// itself (the terminals show anyway, the references on Find References,
/// the debugger on debugging, the notes as a tab of the terminals').
const HIDDEN_ACTIVITY: [Panel; 4] = [Panel::Terminals, Panel::References, Panel::Debugger, Panel::Notes];

/// Panels one above the other. The one with the code takes the width left
/// by the others.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Column {
    /// Unset, half of the space left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    pub stacks: Vec<Stack>,
}

/// Panels in the same place, as tabs; one shows at a time. The one with the
/// code takes the height left by the others.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stack {
    /// Unset, half of the column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    pub panels: Vec<Panel>,
}

impl Stack {
    fn of(panels: &[Panel]) -> Self {
        Self { height: None, panels: panels.to_vec() }
    }
}

impl Column {
    fn of(width: Option<f32>, stacks: Vec<Stack>) -> Self {
        Self { width, stacks }
    }
}

/// Where a dragged panel goes, next to the stack it is dropped on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// One more tab, before that one (or the last).
    Tab(Option<Panel>),
    Left,
    Right,
    Top,
    Bottom,
}

impl Default for Layout {
    fn default() -> Self {
        let mut layout = Self {
            columns: Vec::new(),
            tasks: None,
            side: None,
            terminals: None,
            debug: None,
            debug_at: None,
            debug_width: None,
            terminals_at: None,
            terminals_height: None,
        };
        layout.carry_over();
        layout
    }
}

impl Layout {
    /// The columns as the fields before them said (or the defaults), and
    /// every panel exactly once.
    fn carry_over(&mut self) {
        if self.columns.is_empty() {
            let mut code = vec![Stack::of(&[Panel::Code])];
            let mut columns = vec![Column::of(
                Some(self.side.unwrap_or(260.)),
                vec![Stack::of(&[Panel::Files, Panel::Changes, Panel::History, Panel::Commit, Panel::Search, Panel::References, Panel::Outline])],
            )];
            // The debugger, a tab of the terminals' unless it was placed;
            // the notes, one too.
            let terminals = match self.debug_at {
                None => vec![Panel::Terminals, Panel::Debugger, Panel::Notes],
                Some(_) => vec![Panel::Terminals, Panel::Notes],
            };
            if self.terminals_at == Some(PanelAt::Bottom) {
                code.push(Stack { height: Some(self.terminals_height.unwrap_or(280.)), panels: terminals.clone() });
            }
            if self.debug_at == Some(PanelAt::Bottom) {
                code.push(Stack { height: Some(self.debug.unwrap_or(260.)), panels: vec![Panel::Debugger] });
            }
            columns.push(Column::of(None, code));
            if self.terminals_at != Some(PanelAt::Bottom) {
                columns.push(Column::of(self.terminals, vec![Stack { height: None, panels: terminals }]));
            }
            if self.debug_at == Some(PanelAt::Right) {
                columns.push(Column::of(Some(self.debug_width.unwrap_or(420.)), vec![Stack::of(&[Panel::Debugger])]));
            }
            self.columns = columns;
        }
        self.repair();
    }

    /// Mends a layout edited by hand: each panel once, nothing empty. The
    /// workspaces, missing (a config from before they were a panel), are a
    /// column on the left. The agents' old panel goes.
    fn repair(&mut self) {
        let mut seen = Vec::new();
        for stack in self.columns.iter_mut().flat_map(|column| &mut column.stacks) {
            stack.panels.retain(|panel| {
                let new = !seen.contains(panel) && *panel != Panel::Agents;
                seen.push(*panel);
                new
            });
        }
        self.prune();
        if !seen.contains(&Panel::Code) {
            self.columns = Self::default().columns;
            return;
        }
        for panel in Panel::ALL.into_iter().filter(|panel| !seen.contains(panel) && !matches!(panel, Panel::Workspaces | Panel::Agents)) {
            // The device, a column of its own on the right, as a phone beside
            // the code.
            if panel == Panel::Device {
                self.columns.push(Column::of(panel.width(), vec![Stack::of(&[panel])]));
                continue;
            }
            // The commit's files, missing (a config from before they were a
            // panel), go in the history; the notes, with the terminals.
            let home = match panel {
                Panel::Commit => Panel::History,
                Panel::Notes => Panel::Terminals,
                _ => Panel::Code,
            };
            let stack = self.columns.iter_mut().flat_map(|column| &mut column.stacks).find(|stack| match home {
                Panel::Code => !stack.panels.contains(&Panel::Code),
                _ => stack.panels.contains(&home),
            });
            match stack {
                Some(stack) => stack.panels.push(panel),
                None => self.columns.push(Column::of(None, vec![Stack::of(&[panel])])),
            }
        }
        if !seen.contains(&Panel::Workspaces) {
            self.columns.insert(0, Column::of(Some(self.tasks.unwrap_or(240.)), vec![Stack::of(&[Panel::Workspaces])]));
        }
    }

    fn prune(&mut self) {
        for column in &mut self.columns {
            column.stacks.retain(|stack| !stack.panels.is_empty());
        }
        self.columns.retain(|column| !column.stacks.is_empty());
    }

    /// The commit's files are in the history's place: the history shows
    /// them in its lower part, rather than as a panel of their own.
    pub fn commit_in_history(&self) -> bool {
        self.find(Panel::Commit).is_some_and(|place| self.find(Panel::History) == Some(place))
    }

    /// The column and the stack in it where `panel` is.
    pub fn find(&self, panel: Panel) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(column, col)| {
            col.stacks.iter().position(|stack| stack.panels.contains(&panel)).map(|stack| (column, stack))
        })
    }

    /// Moves `panel` to `side` of the stack with `anchor` (which can be
    /// `panel`'s own); false if that is where it already was or can't go.
    pub fn move_panel(&mut self, panel: Panel, anchor: Panel, side: Side) -> bool {
        let Some((column, stack)) = self.find(anchor).filter(|_| side != Side::Tab(Some(panel))) else {
            return false;
        };
        // Something that stays where the panel goes, to find the place again
        // once the panel is taken out.
        let stays = match side {
            Side::Left | Side::Right => self.columns[column].stacks.iter().flat_map(|stack| &stack.panels).copied().find(|p| *p != panel),
            _ => self.columns[column].stacks[stack].panels.iter().copied().find(|p| *p != panel),
        };
        let Some(stays) = stays else {
            return false;
        };
        let before = self.clone();
        // The commit's files go with the history they're part of.
        let carry = panel == Panel::History && self.commit_in_history();
        let (from_column, from_stack) = self.find(panel).expect("every panel is placed");
        self.columns[from_column].stacks[from_stack].panels.retain(|p| *p != panel);
        self.prune();
        let (column, stack) = self.find(stays).expect("it stays");
        match side {
            Side::Tab(tab) => {
                let panels = &mut self.columns[column].stacks[stack].panels;
                let at = tab.and_then(|tab| panels.iter().position(|p| *p == tab)).unwrap_or(panels.len());
                panels.insert(at, panel);
            }
            Side::Top | Side::Bottom => {
                let at = stack + usize::from(side == Side::Bottom);
                self.columns[column].stacks.insert(at, Stack::of(&[panel]));
            }
            Side::Left | Side::Right => {
                let at = column + usize::from(side == Side::Right);
                self.columns.insert(at, Column::of(panel.width(), vec![Stack::of(&[panel])]));
            }
        }
        if carry {
            let (column, stack) = self.find(Panel::Commit).expect("every panel is placed");
            self.columns[column].stacks[stack].panels.retain(|p| *p != Panel::Commit);
            self.prune();
            let (column, stack) = self.find(Panel::History).expect("it was just placed");
            self.columns[column].stacks[stack].panels.push(Panel::Commit);
        }
        *self != before
    }
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
    /// Repos whose worktrees are folded under their checkout (same keys as
    /// `order`, the checkout's).
    pub collapsed: Vec<String>,
    pub hosts: Vec<HostConfig>,
    pub layout: Layout,
    pub window: Option<SavedWindow>,
    /// What's open in each task (same keys as `order`).
    pub sessions: HashMap<String, Session>,
    /// Breakpoints and watches of each task (same keys as `order`).
    pub debug: HashMap<String, DebugSaved>,
    /// The last task visited, to return to on launch.
    pub last: Option<SavedTask>,
    /// Folders and tasks opened, the most recent first (Open Recent).
    pub recent: Vec<SavedTask>,
    /// The tasks column shown or hidden by hand; unset, it shows once
    /// there's more than folders to it (a server or a worktree).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tasks_column: Option<bool>,
    /// The activity bar's icons, top to bottom, set by dragging; those not
    /// in it go at the end.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub activity: Vec<Panel>,
    /// Icons taken off the activity bar from its right-click menu or View;
    /// their panels still open with their keys and menus. Unset, those of
    /// `HIDDEN_ACTIVITY`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_activity: Option<Vec<Panel>>,
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

    /// The activity bar's icons, top to bottom: each panel once, but the
    /// code, which as in any editor stays put and never closes, and the
    /// device, which only a workspace with a device file has (it goes last).
    pub fn activity(&self) -> Vec<Panel> {
        let mut order: Vec<Panel> = Vec::new();
        for panel in self.activity.iter().chain(&Panel::ALL) {
            if !matches!(panel, Panel::Code | Panel::Agents | Panel::Device) && !order.contains(panel) {
                order.push(*panel);
            }
        }
        order
    }

    /// The icons taken off the bar.
    pub fn hidden_activity(&self) -> Vec<Panel> {
        self.hidden_activity.clone().unwrap_or_else(|| HIDDEN_ACTIVITY.to_vec())
    }

    /// The icons the bar draws: `activity` without the hidden ones.
    pub fn shown_activity(&self) -> Vec<Panel> {
        let hidden = self.hidden_activity();
        self.activity().into_iter().filter(|panel| !hidden.contains(panel)).collect()
    }

    /// Shows `panel`'s icon on the bar, or hides it if it shows.
    pub fn toggle_activity(&mut self, panel: Panel) {
        let mut hidden = self.hidden_activity();
        if hidden.contains(&panel) {
            hidden.retain(|other| *other != panel);
        } else {
            hidden.push(panel);
        }
        self.hidden_activity = Some(hidden);
    }

    /// Puts `panel`'s icon where `target`'s is, or at the end.
    pub fn move_activity(&mut self, panel: Panel, target: Option<Panel>) {
        let mut order = self.activity();
        let Some(from) = order.iter().position(|p| *p == panel) else {
            return;
        };
        let to = target.and_then(|target| order.iter().position(|p| *p == target)).unwrap_or(order.len() - 1);
        order.remove(from);
        order.insert(to, panel);
        self.activity = order;
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
            .map(|mut config| {
                config.layout.carry_over();
                config
            })
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
    use super::{Layout, Panel, Side};
    use core::prelude::v1::test;

    fn places(layout: &Layout) -> Vec<Vec<Vec<Panel>>> {
        layout.columns.iter().map(|column| column.stacks.iter().map(|stack| stack.panels.clone()).collect()).collect()
    }

    use Panel::*;

    #[test]
    fn an_older_layout_carries_over() {
        let old = r#"{"tasks": 200, "side": 300, "terminals": 500, "debug_at": "right", "debug_width": 380, "terminals_at": "bottom", "terminals_height": 220}"#;
        let mut layout: Layout = serde_json::from_str(old).unwrap();
        layout.carry_over();
        assert_eq!(
            places(&layout),
            [vec![vec![Workspaces]], vec![vec![Files, Changes, History, Commit, Search, References, Outline]], vec![vec![Code], vec![Terminals, Notes]], vec![vec![Debugger]], vec![vec![Device]]]
        );
        assert_eq!(layout.columns[0].width, Some(200.));
        assert_eq!(layout.columns[1].width, Some(300.));
        assert_eq!(layout.columns[2].stacks[1].height, Some(220.));
        assert_eq!(layout.columns[3].width, Some(380.));
        // Written again, only the columns are.
        let written = serde_json::to_string(&layout).unwrap();
        assert!(!written.contains("terminals_at"), "{written}");
        let mut again: Layout = serde_json::from_str(&written).unwrap();
        again.carry_over();
        assert_eq!(places(&again), places(&layout));
    }

    #[test]
    fn a_layout_edited_by_hand_is_mended() {
        let edited = r#"{"columns": [{"stacks": [{"panels": ["files", "code", "files"]}, {"panels": ["search"]}]}, {"stacks": []}]}"#;
        let mut layout: Layout = serde_json::from_str(edited).unwrap();
        layout.carry_over();
        // The missing ones go with a panel other than the code; the
        // workspaces and the device, a column of their own.
        assert_eq!(
            places(&layout),
            [vec![vec![Workspaces]], vec![vec![Files, Code], vec![Search, Changes, History, Commit, References, Outline, Terminals, Debugger, Notes]], vec![vec![Device]]]
        );
        let mut layout: Layout = serde_json::from_str(r#"{"columns": [{"stacks": [{"panels": ["files"]}]}]}"#).unwrap();
        layout.carry_over();
        assert!(layout == Layout::default(), "without the code, it starts again");
        // The agents' old panel goes, and its place if it's left empty.
        let old = r#"{"columns": [{"stacks": [{"panels": ["files"]}, {"panels": ["agents"]}]}, {"stacks": [{"panels": ["code", "terminals"]}]}]}"#;
        let mut layout: Layout = serde_json::from_str(old).unwrap();
        layout.carry_over();
        assert_eq!(places(&layout)[1], [vec![Files, Search, Changes, History, Commit, References, Outline, Debugger]]);
    }

    #[test]
    fn moving_panels() {
        let mut layout = Layout::default();
        assert_eq!(
            places(&layout),
            [vec![vec![Workspaces]], vec![vec![Files, Changes, History, Commit, Search, References, Outline]], vec![vec![Code]], vec![vec![Terminals, Debugger, Notes]], vec![vec![Device]]]
        );
        // The changes, a column of their own after the files.
        assert!(layout.move_panel(Changes, Files, Side::Right));
        assert_eq!(places(&layout)[2], [vec![Changes]]);
        assert_eq!(layout.columns[2].width, Some(260.), "a list's own width, not half the window");
        // Back as a tab, before the search.
        assert!(layout.move_panel(Changes, Search, Side::Tab(Some(Search))));
        assert_eq!(places(&layout)[1], [vec![Files, History, Commit, Changes, Search, References, Outline]]);
        assert_eq!(layout.columns.len(), 5, "the empty column goes");
        // Under the files, in their column.
        assert!(layout.move_panel(References, Files, Side::Bottom));
        assert_eq!(places(&layout)[1], [vec![Files, History, Commit, Changes, Search, Outline], vec![References]]);
        // The terminals left of the files: the debugger and the notes keep
        // their place.
        assert!(layout.move_panel(Terminals, Files, Side::Left));
        assert_eq!(places(&layout)[1], [vec![Terminals]]);
        assert_eq!(layout.columns[1].width, None, "the terminals, half of what's left");
        assert_eq!(places(&layout)[4], [vec![Debugger, Notes]]);
        // The code and the workspaces go anywhere too: tabs with the files,
        // and the workspaces under the terminals.
        assert!(layout.move_panel(Code, Files, Side::Tab(None)));
        assert!(layout.move_panel(Workspaces, Terminals, Side::Bottom));
        assert_eq!(places(&layout)[0], [vec![Terminals], vec![Workspaces]]);
        assert_eq!(places(&layout)[1], [vec![Files, History, Commit, Changes, Search, Outline, Code], vec![References]]);
    }

    #[test]
    fn the_commit_files_are_part_of_the_history_until_moved_apart() {
        let mut layout = Layout::default();
        assert!(layout.commit_in_history());
        // A column of their own.
        assert!(layout.move_panel(Commit, Files, Side::Right));
        assert_eq!(places(&layout)[2], [vec![Commit]]);
        assert!(!layout.commit_in_history());
        // Dropped on the history, part of it again.
        assert!(layout.move_panel(Commit, History, Side::Tab(None)));
        assert!(layout.commit_in_history());
        // The history takes them along.
        assert!(layout.move_panel(History, Terminals, Side::Bottom));
        assert_eq!(places(&layout)[3], [vec![Terminals, Debugger, Notes], vec![History, Commit]]);
    }

    #[test]
    fn moves_that_change_nothing() {
        let mut layout = Layout::default();
        let before = places(&layout);
        // Alone in its column, beside itself or on itself.
        assert!(!layout.move_panel(Code, Code, Side::Left));
        assert!(!layout.move_panel(Code, Code, Side::Top));
        assert!(!layout.move_panel(Code, Code, Side::Tab(None)));
        // On its own tab, or as the last tab when it is.
        assert!(!layout.move_panel(Changes, Changes, Side::Tab(Some(Changes))));
        assert!(!layout.move_panel(Outline, Files, Side::Tab(None)));
        assert_eq!(places(&layout), before);
        // Next to its own stack, with others in it, it does move.
        assert!(layout.move_panel(Changes, Changes, Side::Bottom));
        assert_eq!(places(&layout)[1], [vec![Files, History, Commit, Search, References, Outline], vec![Changes]]);
    }

    #[test]
    fn the_activity_bar_order() {
        let mut config = super::Config::default();
        assert!(!config.activity().contains(&Code));
        // Some start off the bar.
        assert_eq!(config.shown_activity(), [Workspaces, Files, Search, Changes, History, Commit, Outline]);
        // Dragged down, an icon takes the place of the one dropped on; up, too.
        config.move_activity(Workspaces, Some(Changes));
        assert_eq!(config.activity()[..4], [Files, Search, Changes, Workspaces]);
        config.move_activity(Debugger, Some(Files));
        assert_eq!(config.activity()[..2], [Debugger, Files]);
        // Dropped past the icons, it goes last.
        config.move_activity(Debugger, None);
        assert_eq!(config.activity().last(), Some(&Debugger));
        // A hand-edited one: the code, repeated and missing ones mended.
        config.activity = vec![Code, Terminals, Terminals];
        assert_eq!(config.activity(), [Terminals, Workspaces, Files, Search, Changes, History, Commit, References, Outline, Debugger, Notes]);
        // A hidden icon keeps its place for when it's shown again.
        config.toggle_activity(Workspaces);
        assert_eq!(config.shown_activity()[..2], [Files, Search]);
        config.move_activity(Files, None);
        config.toggle_activity(Workspaces);
        assert_eq!(config.shown_activity()[..3], [Workspaces, Search, Changes]);
    }
}
