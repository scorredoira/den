//! What depends on the operating system. See "Platforms" in plan.md.

use gpui_kit::KeyBinding;

use crate::{Copy, Paste, SendBackTab, SendInterrupt, SendTab, terminal_key};

/// Copy and paste inside the terminal. On Mac it uses Cmd; on Linux and
/// Windows, Ctrl-Shift so Ctrl-C and Ctrl-V still go to the shell.
///
/// Tab, Shift-Tab and Ctrl-C have shortcuts at the window root (move focus,
/// copy); inside the terminal they're claimed for the shell.
pub fn keymap() -> Vec<KeyBinding> {
    let mut keys = vec![
        terminal_key("tab", SendTab),
        terminal_key("shift-tab", SendBackTab),
        terminal_key("ctrl-c", SendInterrupt),
    ];
    if cfg!(target_os = "macos") {
        keys.extend([terminal_key("cmd-c", Copy), terminal_key("cmd-v", Paste)]);
    } else {
        keys.extend([
            terminal_key("ctrl-shift-c", Copy),
            terminal_key("ctrl-shift-v", Paste),
        ]);
    }
    keys
}
