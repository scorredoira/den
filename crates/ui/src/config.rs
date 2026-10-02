//! UI configuration: `config.json` in sik's config folder. It lives as a GPUI
//! global and is saved on every change.

use std::{collections::HashMap, path::PathBuf};

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

/// Panel widths, the same for every task.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    pub tasks: f32,
    pub side: f32,
    /// When unsaved, the terminals take half of the code area.
    pub terminals: Option<f32>,
    /// Height of the debugger panel under the code.
    #[serde(default = "default_debug_height")]
    pub debug: f32,
    /// Where the debugger goes: under everything or a column on the right.
    pub debug_at: PanelAt,
    /// Width of the debugger as a column.
    pub debug_width: f32,
    /// Where the terminals go: a column on the right or a row under the code.
    pub terminals_at: PanelAt,
    /// Height of the terminals as a row.
    pub terminals_height: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelAt {
    Bottom,
    Right,
}

fn default_debug_height() -> f32 {
    260.
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            tasks: 240.,
            side: 260.,
            terminals: None,
            debug: default_debug_height(),
            debug_at: PanelAt::Bottom,
            debug_width: 420.,
            terminals_at: PanelAt::Right,
            terminals_height: 280.,
        }
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
    /// The launch configuration last started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<String>,
    /// The terminal the launch command ran in: the next launch reuses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<u64>,
}

impl Default for DebugSaved {
    fn default() -> Self {
        Self { breakpoints: Vec::new(), watches: Vec::new(), uncaught: true, all: false, launch: None, terminal: None }
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

    fn path() -> Option<PathBuf> {
        proto::config_dir().ok().map(|dir| dir.join("config.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
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
    visible: Vec<bool>,
}

impl Split {
    pub fn new(cx: &mut App) -> Self {
        Self {
            state: cx.new(|_| ResizableState::default()),
            width: None,
            visible: Vec::new(),
        }
    }

    /// The state for painting the row in a space of `width`, with the
    /// panels `visible`; if either changed, it starts again from the saved
    /// sizes. A hidden panel keeps the size it was last painted at, and
    /// dragging would count it as taken: the dragged panel jumps back to
    /// its minimum with every move.
    pub fn state(&mut self, width: Pixels, visible: &[bool], cx: &mut App) -> &Entity<ResizableState> {
        if self.width.is_some_and(|last| last != width) || (!self.visible.is_empty() && self.visible != visible) {
            self.state.update(cx, |state, _| state.clear());
        }
        self.width = Some(width);
        self.visible = visible.to_vec();
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
            let state = self.split.state(px(1000.), &[true, true, self.terminals], cx).clone();
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
        let state = view.update(cx, |view, cx| view.split.state(px(1000.), &[true, true, false], cx).clone());
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
        let state = view.update(cx, |view, cx| view.split.state(px(1000.), &[true, true, true], cx).clone());
        assert_eq!(state.read_with(cx, |state, _| state.sizes()[2]), px(300.));
    }
}
