//! Terminal in GPUI: `alacritty_terminal` emulator and our own painting. The
//! pty lives outside (in the agent): the terminal only receives bytes and sends
//! keys through a [`TerminalBackend`].

mod backend;
mod colors;
mod element;
mod keys;
pub mod links;
mod platform;
mod terminal;
mod view;

use gpui_kit::{App, KeyBinding, actions};

pub use backend::{PtyEvent, TerminalBackend};
pub use terminal::{Terminal, TerminalEvent};
pub use view::{TerminalFontSize, TerminalView, TerminalViewEvent, grid_for};

actions!(terminal, [Copy, Paste, SendTab, SendBackTab, SendInterrupt]);

pub fn init(cx: &mut App) {
    cx.bind_keys(platform::keymap());
}

/// Shortcut in a terminal's context.
fn terminal_key(keys: &str, action: impl gpui_kit::Action) -> KeyBinding {
    KeyBinding::new(keys, action, Some("Terminal"))
}
