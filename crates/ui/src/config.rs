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
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            tasks: 240.,
            side: 260.,
            terminals: None,
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

/// A server connected to over SSH.
#[derive(Clone, Serialize, Deserialize)]
pub struct HostConfig {
    /// How it's shown in the tasks column.
    pub name: String,
    /// A name from `~/.ssh/config` or `user@host`.
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

/// A task on a server (`local` is this machine).
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedTask {
    pub host: String,
    pub path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeChoice,
    /// Tasks removed from the list (without deleting anything). Each is its
    /// path locally or `server:path` on a server.
    pub hidden: Vec<String>,
    /// List order, set by dragging (same keys as `hidden`); those not in it
    /// go at the end.
    pub order: Vec<String>,
    pub hosts: Vec<HostConfig>,
    pub layout: Layout,
    pub window: Option<SavedWindow>,
    /// What's open in each task (same keys as `hidden`).
    pub sessions: HashMap<String, Session>,
    /// The last task visited, to return to on launch.
    pub last: Option<SavedTask>,
    /// Shortcuts changed in Settings: action → keys (`""` for no shortcut).
    pub keys: HashMap<String, String>,
    pub font_sizes: FontSizes,
    /// Long lines wrap in the editor (Opt-Z).
    pub word_wrap: bool,
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
}

impl Split {
    pub fn new(cx: &mut App) -> Self {
        Self {
            state: cx.new(|_| ResizableState::default()),
            width: None,
        }
    }

    /// The state for painting the row in a space of `width`; if the space
    /// changed, it starts again from the saved sizes.
    pub fn state(&mut self, width: Pixels, cx: &mut App) -> &Entity<ResizableState> {
        if self.width.is_some_and(|last| last != width) {
            self.state.update(cx, |state, _| state.clear());
        }
        self.width = Some(width);
        &self.state
    }
}
