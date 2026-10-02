//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{Context, WeakEntity, Window};

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
    PopupMenuItem::new("Reset Layout").on_click(|_, _, cx| {
        Config::update(cx, |config| config.layout.columns = Layout::default().columns);
        cx.refresh_windows();
    })
}
