//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{Context, WeakEntity, Window};

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
