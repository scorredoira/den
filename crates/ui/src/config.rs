//! UI configuration: `config.json` in den's config folder. It lives as a GPUI
//! global and is saved on every change.

mod layout;
pub mod store;

pub use layout::*;

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::PathBuf,
};

use gpui_kit::component::{
    ResizableState,
    input::{FindBarMemory, SearchOptions},
};
use gpui_kit::{App, AppContext as _, Bounds, Entity, Global, Pixels, Styled, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

use store::JsonFile;

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
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
    /// left it; unset, the editing one. Its side column and activity bar
    /// are the editing one's (`Layout::keep_column`).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub debug_layout: Option<Layout>,
    /// The editing layout while the debugging one is in use: set only while
    /// debugging.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub edit_layout: Option<Layout>,
    /// Terminal Mode's side column: shown or not, and its width.
    #[serde(deserialize_with = "lenient")]
    pub terminal_mode: TerminalModeLayout,
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
    /// Spaces per indentation level in a file that doesn't show its own
    /// (new, or nothing indented yet); unset, 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_size: Option<usize>,
    /// Such a file indents with tabs, not spaces.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub indent_with_tabs: bool,
    /// Look for new releases every few hours; unset, it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_for_updates: Option<bool>,
    /// The Workspaces panel shows only the worktrees made from it (New
    /// Worktree), not those the agents make on their own.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_own_worktrees: bool,
    /// A split terminal tab shows no title over each pane.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hide_pane_titles: bool,
    /// Projects hidden from the Workspaces panel, Cmd-E and Cmd-K: a repo's
    /// checkout or a folder (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_projects: Vec<String>,
    /// Servers hidden from the Workspaces panel, Cmd-E and Cmd-K, with all
    /// their projects (by name, `local` too).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hidden_hosts: Vec<String>,
    /// The Workspaces panel shows the hidden ones too, dimmed.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub show_hidden_projects: bool,
    /// Repos folded to their row in the Workspaces panel, their worktrees
    /// hidden (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folded_projects: Vec<String>,
    /// The worktrees made from the Workspaces panel (same keys as `order`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub own_worktrees: Vec<String>,
    /// The History tab's sizes, as dragged.
    #[serde(deserialize_with = "lenient")]
    pub history: HistorySizes,
    /// The History tab shows every branch, tag and remote, not only the
    /// current branch (as gitk does without `--all`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_all_branches: bool,
    /// The History tab's columns, as dragged by their headers; those not
    /// in it go where they go by default.
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "known_history_parts")]
    pub history_order: Vec<HistoryPart>,
    /// Files is off in the History tab: the selected commit's files don't
    /// show beside the commits, as they do by default.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub history_hide_files: bool,
    /// The groups of the Outline turned off with the icons at its top.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub outline_hidden: Vec<OutlineGroup>,
    /// The Outline lists the symbols in the order of the file, rather than
    /// by their group.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub outline_file_order: bool,
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
    /// The find bar's (Cmd-F) width, toggles and history.
    #[serde(deserialize_with = "lenient")]
    pub find: SearchSaved,
    /// The Search panel's (Shift-Cmd-F) toggles and history.
    #[serde(deserialize_with = "lenient")]
    pub search: SearchSaved,
}

/// What a search box remembers: its toggles and the queries searched, the
/// newest last.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSaved {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub case_sensitive: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub whole_word: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub regex: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub preserve_case: bool,
    /// The find bar's width, as dragged; unset, its default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
}

/// How many queries a search box remembers.
const SEARCH_HISTORY: usize = 50;

impl SearchSaved {
    /// `query` goes last in the history.
    pub fn remember(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        self.history.retain(|old| old != query);
        self.history.push(query.to_string());
        let extra = self.history.len().saturating_sub(SEARCH_HISTORY);
        self.history.drain(..extra);
    }
}

/// The find bar keeps its memory as a global of its own: it starts from the
/// config, and what changes in it goes back to the config.
pub fn sync_find_bar(cx: &mut App) {
    let saved = &Config::get(cx).find;
    cx.set_global(FindBarMemory {
        width: saved.width.map(px),
        history: saved.history.clone(),
        options: SearchOptions {
            case_insensitive: !saved.case_sensitive,
            whole_word: saved.whole_word,
            regex: saved.regex,
            preserve_case: saved.preserve_case,
        },
    });
    cx.observe_global::<FindBarMemory>(|cx| {
        let memory = cx.global::<FindBarMemory>();
        let find = SearchSaved {
            history: memory.history.clone(),
            case_sensitive: !memory.options.case_insensitive,
            whole_word: memory.options.whole_word,
            regex: memory.options.regex,
            preserve_case: memory.options.preserve_case,
            width: memory.width.map(f32::from),
        };
        Config::update_quietly(cx, |config| config.find = find);
    })
    .detach();
}

/// A column of the History tab's top: the commits, the selected one's
/// message and its files.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum HistoryPart {
    Commits,
    Message,
    Files,
}

impl HistoryPart {
    pub const ALL: [HistoryPart; 3] = [HistoryPart::Commits, HistoryPart::Message, HistoryPart::Files];
}

fn known_history_parts<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<HistoryPart>, D::Error> {
    let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(names.into_iter().filter_map(|name| HistoryPart::deserialize(name).ok()).collect())
}

/// The parts of the History tab and its columns, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySizes {
    /// The commits' height, over the selected one.
    pub commits: f32,
    /// The commit's message and its files, beside the commits.
    pub message: f32,
    pub files: f32,
    /// The commits' author and date columns.
    pub author: f32,
    pub date: f32,
}

impl Default for HistorySizes {
    fn default() -> Self {
        Self { commits: 345., message: 450., files: 450., author: 340., date: 185. }
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
pub const DEFAULT_TAB_SIZE: usize = 4;
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
    /// The indentation of a file that doesn't show its own.
    pub fn tab(&self) -> gpui_base::input::TabSize {
        gpui_base::input::TabSize { tab_size: self.tab_size.unwrap_or(DEFAULT_TAB_SIZE), hard_tabs: self.indent_with_tabs }
    }

    pub fn side_by_side_width(&self) -> f32 {
        self.side_by_side_width.unwrap_or(DEFAULT_SIDE_BY_SIDE_WIDTH).clamp(MIN_SIDE_BY_SIDE_WIDTH, MAX_SIDE_BY_SIDE_WIDTH)
    }

    /// `None` goes back to the default.
    pub fn set_side_by_side_width(&mut self, width: Option<f32>) {
        let width = width.map(|width| width.clamp(MIN_SIDE_BY_SIDE_WIDTH, MAX_SIDE_BY_SIDE_WIDTH));
        self.side_by_side_width = width.filter(|width| *width != DEFAULT_SIDE_BY_SIDE_WIDTH);
    }

    /// Whether a diff `width` wide has room for its two sides, whatever the
    /// layout chosen.
    pub fn room_for_two_sides(&self, width: Pixels) -> bool {
        width >= px(self.side_by_side_width())
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

    pub fn load() -> Self {
        let Some(bytes) = FILE.read() else {
            return Self::default();
        };
        let (mut config, whole) = Self::parse(&bytes);
        if !whole {
            FILE.keep_aside(&bytes);
        }
        // no debug session outlives the app
        config.use_debug_layout(false);
        config.layout.repair();
        if let Some(layout) = &mut config.debug_layout {
            layout.repair();
        }
        config
    }

    /// The History tab's columns in order: as saved, each once, with any
    /// missing where it goes by default.
    pub fn history_order(&self) -> [HistoryPart; 3] {
        let mut order: Vec<HistoryPart> = Vec::new();
        for part in self.history_order.iter().copied().chain(HistoryPart::ALL) {
            if !order.contains(&part) {
                order.push(part);
            }
        }
        [order[0], order[1], order[2]]
    }

    /// Swaps two of the History tab's columns.
    pub fn swap_history(&mut self, a: HistoryPart, b: HistoryPart) {
        let mut order = self.history_order();
        if let (Some(i), Some(j)) = (order.iter().position(|part| *part == a), order.iter().position(|part| *part == b)) {
            order.swap(i, j);
            self.history_order = order.to_vec();
        }
    }

    /// The debugging layout is in use.
    pub fn debugging(&self) -> bool {
        self.edit_layout.is_some()
    }

    /// Puts the debugging layout in use, or the editing one back, each as it
    /// was left: what changes while debugging stays in the debugging one,
    /// except the side column and the activity bar, which are one in both.
    pub fn use_debug_layout(&mut self, debugging: bool) {
        if debugging == self.debugging() {
            return;
        }
        let mut next = if debugging { self.debug_layout.take().unwrap_or_else(|| self.layout.clone()) } else { self.edit_layout.take().unwrap_or_default() };
        next.keep_column(&self.layout);
        let left = std::mem::replace(&mut self.layout, next);
        if debugging {
            self.edit_layout = Some(left);
        } else {
            self.debug_layout = Some(left);
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

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    /// Changes the config and saves it.
    pub fn update(cx: &mut App, change: impl FnOnce(&mut Self)) {
        let config = cx.global_mut::<Self>();
        change(config);
        // The panels drawn from cache read it too: if it changed, they're
        // drawn again.
        if FILE.save_later(config) {
            cx.refresh_windows();
        }
    }

    /// Changes something that moves often (panel sizes, the window, the
    /// session's cursor): it's saved with the next change or on quit. Nothing
    /// drawn from cache reads these (sizes change the bounds, which redraws
    /// it anyway), so windows aren't redrawn whole for it, with every key.
    pub fn update_quietly(cx: &mut App, change: impl FnOnce(&mut Self)) {
        change(cx.global_mut::<Self>());
    }

    /// Loads the config as a global and saves it on quit, with every file
    /// still to be written.
    pub fn init(cx: &mut App) {
        cx.set_global(Self::load());
        cx.on_app_quit(|cx| {
            FILE.save(cx.global::<Self>());
            // Waited for after every quit handler ran: what they saved
            // (the notes being typed) is written too.
            async { store::flush_all() }
        })
        .detach();
    }
}

static FILE: JsonFile = JsonFile::new("config.json");

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
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Files, Panel::Outline]);
        assert_eq!(layout.place_of(Panel::Files), Some(explorer));
        layout.move_panel(Panel::Files, explorer, Some(Panel::Workspaces));
        assert_eq!(layout.panels(explorer), [Panel::Files, Panel::Workspaces, Panel::Outline]);
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

    /// The bar: the places, then History, Debug and Notes; any of them
    /// moved anywhere, or off it and back.
    #[test]
    fn the_bar_s_icons() {
        use super::{Icon, Place};
        let mut layout = Layout::default();
        let places = [Icon::Place(Panel::Workspaces), Icon::Place(Panel::Search), Icon::Place(Panel::Changes)];
        assert_eq!(layout.bar(), [&places[..], &[Icon::History, Icon::Debug, Icon::Notes]].concat());
        layout.move_icon(Icon::Notes, Some(places[1]));
        layout.move_icon(places[0], None);
        assert_eq!(layout.bar(), [Icon::Notes, places[1], places[2], Icon::History, Icon::Debug, places[0]]);
        layout.set_on_bar(places[1], false);
        assert!(layout.panels(Place(Panel::Search)).is_empty() && !layout.bar().contains(&places[1]));
        assert!(layout.icons().contains(&(places[1], false)));
        layout.set_on_bar(places[1], true);
        assert_eq!(layout.bar()[1], places[1]);
        layout.set_on_bar(Icon::History, false);
        assert!(!layout.bar().contains(&Icon::History));
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
        assert_eq!(layout.panels(layout.current().unwrap()), [Panel::Workspaces, Panel::Outline]);
        // As a click names it, by its first panel in sight.
        layout.place = layout.current().unwrap();
        layout.move_panel(Panel::Workspaces, git, None);
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
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Files, Panel::Outline]);
        // A hidden one comes back where it was.
        layout.move_panel(Panel::Agents, explorer, None);
        assert_eq!(layout.panels(explorer), [Panel::Workspaces, Panel::Agents, Panel::Files, Panel::Outline]);
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
        assert_eq!(layout.places[0], [Panel::Files, Panel::Outline, Panel::Workspaces, Panel::Agents]);
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
