//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{App, Context, WeakEntity, Window};

use crate::config::{Config, Layout};

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

/// The item that puts every panel back where it starts.
pub fn reset_layout() -> PopupMenuItem {
    PopupMenuItem::new("Reset Layout").on_click(|_, _, cx| reset_layout_now(cx))
}

/// What Reset Layout does, from a right-click menu or View: every panel
/// back in its place, and only the files, the code and the terminals shown.
pub fn reset_layout_now(cx: &mut App) {
    Config::update(cx, |config| {
        config.layout.columns = Layout::default().columns;
        config.tasks_column = Some(false);
        // The activity bar too: its order and the icons on it.
        config.activity = Vec::new();
        config.hidden_activity = None;
    });
    crate::workspace::reset_panels(cx);
    // View > Activity Bar checks the icons.
    crate::app_menu::set(cx);
    cx.refresh_windows();
}
