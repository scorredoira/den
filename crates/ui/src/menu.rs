//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use std::rc::Rc;

pub use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::{App, Context, Global, WeakEntity, Window};

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

/// How to hide the panel the last right-click landed in, set by its place
/// before any menu in it opens; none in the code's, which is never hidden.
struct PanelUnder(Option<Rc<dyn Fn(&mut App)>>);

impl Global for PanelUnder {}

pub fn set_panel_under(hide: Option<Rc<dyn Fn(&mut App)>>, cx: &mut App) {
    cx.set_global(PanelUnder(hide));
}

/// The panel right-clicked: every menu in a panel ends with this item.
pub fn hide_panel() -> PopupMenuItem {
    PopupMenuItem::new("Hide Panel").on_click(|_, _, cx| {
        if let Some(hide) = cx.try_global::<PanelUnder>().and_then(|under| under.0.clone()) {
            hide(cx);
        }
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
