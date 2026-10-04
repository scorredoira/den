//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use std::rc::Rc;

pub use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, Context, Global, WeakEntity, Window};

use crate::config::Config;

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

/// How every panel's menu ends: its Hide Panel, then Show Panel.
pub trait PanelItems {
    fn panel_items(self, hide: PopupMenuItem, window: &mut Window, cx: &mut Context<PopupMenu>) -> Self;
}

impl PanelItems for PopupMenu {
    fn panel_items(self, hide: PopupMenuItem, window: &mut Window, cx: &mut Context<PopupMenu>) -> Self {
        self.item(hide).submenu("Show Panel", window, cx, |menu, window, cx| show_panel_menu(menu, window, cx))
    }
}

/// What the activity bar shows, checked while it shows: a click shows or
/// hides it, as its icon does. And Reset Layout.
fn show_panel_menu(menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let Some(workspace) = crate::app::window_workspace(window, cx) else {
        return menu.item(reset_layout());
    };
    let items = workspace.read(cx).menu_items(cx);
    let weak = workspace.downgrade();
    items
        .into_iter()
        .fold(menu, |menu, (activity, shown)| {
            menu.item(item(activity.title(), &weak, move |this, window, cx| this.click_activity(activity, window, cx)).checked(shown))
        })
        .separator()
        .item(reset_layout())
}

/// The item that puts the workspace's panels back where they start.
pub fn reset_layout() -> PopupMenuItem {
    PopupMenuItem::new("Reset Layout").on_click(|_, window, cx| window.dispatch_action(Box::new(crate::ResetLayout), cx))
}

/// What Reset Layout does beyond the workspace's panels (`Workspace::
/// reset_layout`): the places and sizes as they start, in every window.
pub fn reset_layout_now(cx: &mut App) {
    Config::update(cx, |config| config.layout = crate::config::Layout::default());
    // View checks where the terminals go.
    crate::app_menu::set(cx);
    cx.refresh_windows();
}
