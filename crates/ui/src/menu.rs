//! Helper for right-click menus: an item that acts on an entity (if it is
//! still alive).

use std::rc::Rc;

pub use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{App, Context, Global, WeakEntity, Window, prelude::FluentBuilder as _};

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

/// How to hide the panel the last right-click landed in, and to give it an
/// icon of its own (a side panel with company), set by its place before any
/// menu in it opens; none in the code's, which is never hidden.
struct PanelUnder {
    hide: Option<Rc<dyn Fn(&mut App)>>,
    own_icon: Option<Rc<dyn Fn(&mut App)>>,
}

impl Global for PanelUnder {}

pub fn set_panel_under(hide: Option<Rc<dyn Fn(&mut App)>>, own_icon: Option<Rc<dyn Fn(&mut App)>>, cx: &mut App) {
    cx.set_global(PanelUnder { hide, own_icon });
}

/// The panel right-clicked: every menu in a panel ends with this item.
pub fn hide_panel() -> PopupMenuItem {
    PopupMenuItem::new("Hide Panel").on_click(|_, _, cx| {
        if let Some(hide) = cx.try_global::<PanelUnder>().and_then(|under| under.hide.clone()) {
            hide(cx);
        }
    })
}

/// How every panel's menu ends: its Hide Panel, Move to Its Own Icon (a
/// side panel with company), then Show Panel.
pub trait PanelItems {
    fn panel_items(self, hide: PopupMenuItem, window: &mut Window, cx: &mut Context<PopupMenu>) -> Self;
}

impl PanelItems for PopupMenu {
    fn panel_items(self, hide: PopupMenuItem, window: &mut Window, cx: &mut Context<PopupMenu>) -> Self {
        let own_icon = cx.try_global::<PanelUnder>().and_then(|under| under.own_icon.clone());
        self.item(hide)
            .when_some(own_icon, |menu, own_icon| {
                menu.item(PopupMenuItem::new("Move to Its Own Icon").on_click(move |_, _, cx| own_icon(cx)))
            })
            .submenu("Show Panel", window, cx, panels_menu)
    }
}

/// Every panel, checked while it's on: a click puts it on or takes it
/// off. The History tab, checked while it shows. And Reset Layout. Show Panel's, and the activity bar's menu.
pub fn panels_menu(menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    let Some(workspace) = crate::app::window_workspace(window, cx) else {
        return menu.item(reset_layout());
    };
    let panels = workspace.read(cx).menu_panels(cx);
    let weak = workspace.downgrade();
    panels
        .into_iter()
        .fold(menu, |menu, (panel, shown)| {
            menu.item(
                item(crate::workspace::panel_title(panel), &weak, move |this, window, cx| this.toggle_from_menu(panel, window, cx))
                    .checked(shown),
            )
        })
        .item(
            item("History", &weak, |this, window, cx| this.toggle_history(window, cx))
                .checked(workspace.read(cx).history_visible()),
        )
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

#[cfg(test)]
mod tests {
    use gpui_kit::component::{GlobalState, menu::ContextMenuExt as _, tooltip::Tooltip};
    use gpui_kit::*;
    use core::prelude::v1::test;

    struct Row;

    impl Render for Row {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                div()
                    .id("row")
                    .w(px(200.))
                    .h(px(24.))
                    .tooltip(|window, cx| Tooltip::new("what it's on").build(window, cx))
                    .context_menu(|menu, _, _| menu.item(gpui_kit::component::menu::PopupMenuItem::new("Rename"))),
            )
        }
    }

    /// A tooltip never shows over an open right-click menu (`Tooltip`
    /// renders nothing while a menu or a popover is open): the row under
    /// the mouse is still hovered, and its tooltip used to show through it.
    #[gpui_kit::test]
    fn an_open_menu_holds_tooltips_back(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Row);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|_, cx| assert!(!GlobalState::is_in_deferred_context(cx)));

        let press = point(px(10.), px(10.));
        cx.simulate_mouse_down(press, MouseButton::Right, Default::default());
        cx.simulate_mouse_up(press, MouseButton::Right, Default::default());
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|_, cx| assert!(GlobalState::is_in_deferred_context(cx), "the open menu holds tooltips back"));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|_, cx| assert!(!GlobalState::is_in_deferred_context(cx), "dismissed, tooltips come back"));
    }
}
