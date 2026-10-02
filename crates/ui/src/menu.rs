//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{App, Context, WeakEntity, Window};

use crate::config::{Config, Layout, PanelAt};

pub fn item<T: 'static>(
    label: impl Into<gpui_kit::SharedString>,
    target: &WeakEntity<T>,
    action: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> PopupMenuItem {
    let target = target.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        target.update(cx, |this, cx| action(this, window, cx)).ok();
    })
}

/// The item that moves the terminals between a column on the right and a
/// row under the code.
pub fn move_terminals(cx: &App) -> PopupMenuItem {
    let (label, to) = match Config::get(cx).layout.terminals_at {
        PanelAt::Right => ("Move Terminals Under the Code", PanelAt::Bottom),
        PanelAt::Bottom => ("Move Terminals to the Right", PanelAt::Right),
    };
    move_panel(label, move |layout| layout.terminals_at = to)
}

/// The item that moves the debugger between under everything and a column
/// on the right.
pub fn move_debugger(cx: &App) -> PopupMenuItem {
    let (label, to) = match Config::get(cx).layout.debug_at {
        PanelAt::Bottom => ("Move Debugger to the Right", PanelAt::Right),
        PanelAt::Right => ("Move Debugger to the Bottom", PanelAt::Bottom),
    };
    move_panel(label, move |layout| layout.debug_at = to)
}

fn move_panel(label: &'static str, change: impl Fn(&mut Layout) + 'static) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        Config::update(cx, |config| change(&mut config.layout));
        cx.refresh_windows();
    })
}
