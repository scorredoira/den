//! Translation of keys to escape sequences.
//!
//! Copied and adapted from Zed (`crates/terminal/src/mappings/keys.rs`),
//! Copyright 2022-2026 Zed Industries, Inc., license GPL-3.0-or-later.

use std::borrow::Cow;

/// The mappings defined in this file where created from reading the alacritty source
use alacritty_terminal::term::TermMode;
use gpui_kit::Keystroke;

#[derive(Debug, PartialEq, Eq)]
enum TerminalModifiers {
    None,
    Alt,
    Ctrl,
    Shift,
    CtrlShift,
    Other,
}

impl TerminalModifiers {
    fn new(ks: &Keystroke) -> Self {
        match (
            ks.modifiers.alt,
            ks.modifiers.control,
            ks.modifiers.shift,
            ks.modifiers.platform,
        ) {
            (false, false, false, false) => TerminalModifiers::None,
            (true, false, false, false) => TerminalModifiers::Alt,
            (false, true, false, false) => TerminalModifiers::Ctrl,
            (false, false, true, false) => TerminalModifiers::Shift,
            (false, true, true, false) => TerminalModifiers::CtrlShift,
            _ => TerminalModifiers::Other,
        }
    }

    fn any(&self) -> bool {
        match &self {
            TerminalModifiers::None => false,
            TerminalModifiers::Alt => true,
            TerminalModifiers::Ctrl => true,
            TerminalModifiers::Shift => true,
            TerminalModifiers::CtrlShift => true,
            TerminalModifiers::Other => true,
        }
    }
}

pub(crate) fn to_esc_str(
    keystroke: &Keystroke,
    mode: TermMode,
    option_as_meta: bool,
) -> Option<Cow<'static, str>> {
    let modifiers = TerminalModifiers::new(keystroke);

    // Manual Bindings including modifiers
    let manual_esc_str: Option<&'static str> = match (keystroke.key.as_ref(), &modifiers) {
        //Basic special keys
        ("tab", TerminalModifiers::None) => Some("\x09"),
        ("escape", TerminalModifiers::None) => Some("\x1b"),
        ("enter", TerminalModifiers::None) => Some("\x0d"),
        ("enter", TerminalModifiers::Shift) => Some("\x0a"),
        ("enter", TerminalModifiers::Alt) => Some("\x1b\x0d"),
        ("backspace", TerminalModifiers::None) => Some("\x7f"),
        //Interesting escape codes
        ("tab", TerminalModifiers::Shift) => Some("\x1b[Z"),
        ("backspace", TerminalModifiers::Ctrl) => Some("\x08"),
        ("backspace", TerminalModifiers::Alt) => Some("\x1b\x7f"),
        ("backspace", TerminalModifiers::Shift) => Some("\x7f"),
        ("space", TerminalModifiers::Ctrl) => Some("\x00"),
        ("home", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOH"),
        ("home", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[H"),
        ("end", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOF"),
        ("end", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[F"),
        ("up", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOA"),
        ("up", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[A"),
        ("down", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOB"),
        ("down", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[B"),
        ("right", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOC"),
        ("right", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[C"),
        ("left", TerminalModifiers::None) if mode.contains(TermMode::APP_CURSOR) => Some("\x1bOD"),
        ("left", TerminalModifiers::None) if !mode.contains(TermMode::APP_CURSOR) => Some("\x1b[D"),
        ("back", TerminalModifiers::None) => Some("\x7f"),
        ("insert", TerminalModifiers::None) => Some("\x1b[2~"),
        ("delete", TerminalModifiers::None) => Some("\x1b[3~"),
        ("pageup", TerminalModifiers::None) => Some("\x1b[5~"),
        ("pagedown", TerminalModifiers::None) => Some("\x1b[6~"),
        ("f1", TerminalModifiers::None) => Some("\x1bOP"),
        ("f2", TerminalModifiers::None) => Some("\x1bOQ"),
        ("f3", TerminalModifiers::None) => Some("\x1bOR"),
        ("f4", TerminalModifiers::None) => Some("\x1bOS"),
        ("f5", TerminalModifiers::None) => Some("\x1b[15~"),
        ("f6", TerminalModifiers::None) => Some("\x1b[17~"),
        ("f7", TerminalModifiers::None) => Some("\x1b[18~"),
        ("f8", TerminalModifiers::None) => Some("\x1b[19~"),
        ("f9", TerminalModifiers::None) => Some("\x1b[20~"),
        ("f10", TerminalModifiers::None) => Some("\x1b[21~"),
        ("f11", TerminalModifiers::None) => Some("\x1b[23~"),
        ("f12", TerminalModifiers::None) => Some("\x1b[24~"),
        ("f13", TerminalModifiers::None) => Some("\x1b[25~"),
        ("f14", TerminalModifiers::None) => Some("\x1b[26~"),
        ("f15", TerminalModifiers::None) => Some("\x1b[28~"),
        ("f16", TerminalModifiers::None) => Some("\x1b[29~"),
        ("f17", TerminalModifiers::None) => Some("\x1b[31~"),
        ("f18", TerminalModifiers::None) => Some("\x1b[32~"),
        ("f19", TerminalModifiers::None) => Some("\x1b[33~"),
        ("f20", TerminalModifiers::None) => Some("\x1b[34~"),
        // NumpadEnter, Action::Esc("\n".into());
        //Mappings for caret notation keys
        ("a", TerminalModifiers::Ctrl) => Some("\x01"), //1
        ("A", TerminalModifiers::CtrlShift) => Some("\x01"), //1
        ("b", TerminalModifiers::Ctrl) => Some("\x02"), //2
        ("B", TerminalModifiers::CtrlShift) => Some("\x02"), //2
        ("c", TerminalModifiers::Ctrl) => Some("\x03"), //3
        ("C", TerminalModifiers::CtrlShift) => Some("\x03"), //3
        ("d", TerminalModifiers::Ctrl) => Some("\x04"), //4
        ("D", TerminalModifiers::CtrlShift) => Some("\x04"), //4
        ("e", TerminalModifiers::Ctrl) => Some("\x05"), //5
        ("E", TerminalModifiers::CtrlShift) => Some("\x05"), //5
        ("f", TerminalModifiers::Ctrl) => Some("\x06"), //6
        ("F", TerminalModifiers::CtrlShift) => Some("\x06"), //6
        ("g", TerminalModifiers::Ctrl) => Some("\x07"), //7
        ("G", TerminalModifiers::CtrlShift) => Some("\x07"), //7
        ("h", TerminalModifiers::Ctrl) => Some("\x08"), //8
        ("H", TerminalModifiers::CtrlShift) => Some("\x08"), //8
        ("i", TerminalModifiers::Ctrl) => Some("\x09"), //9
        ("I", TerminalModifiers::CtrlShift) => Some("\x09"), //9
        ("j", TerminalModifiers::Ctrl) => Some("\x0a"), //10
        ("J", TerminalModifiers::CtrlShift) => Some("\x0a"), //10
        ("k", TerminalModifiers::Ctrl) => Some("\x0b"), //11
        ("K", TerminalModifiers::CtrlShift) => Some("\x0b"), //11
        ("l", TerminalModifiers::Ctrl) => Some("\x0c"), //12
        ("L", TerminalModifiers::CtrlShift) => Some("\x0c"), //12
        ("m", TerminalModifiers::Ctrl) => Some("\x0d"), //13
        ("M", TerminalModifiers::CtrlShift) => Some("\x0d"), //13
        ("n", TerminalModifiers::Ctrl) => Some("\x0e"), //14
        ("N", TerminalModifiers::CtrlShift) => Some("\x0e"), //14
        ("o", TerminalModifiers::Ctrl) => Some("\x0f"), //15
        ("O", TerminalModifiers::CtrlShift) => Some("\x0f"), //15
        ("p", TerminalModifiers::Ctrl) => Some("\x10"), //16
        ("P", TerminalModifiers::CtrlShift) => Some("\x10"), //16
        ("q", TerminalModifiers::Ctrl) => Some("\x11"), //17
        ("Q", TerminalModifiers::CtrlShift) => Some("\x11"), //17
        ("r", TerminalModifiers::Ctrl) => Some("\x12"), //18
        ("R", TerminalModifiers::CtrlShift) => Some("\x12"), //18
        ("s", TerminalModifiers::Ctrl) => Some("\x13"), //19
        ("S", TerminalModifiers::CtrlShift) => Some("\x13"), //19
        ("t", TerminalModifiers::Ctrl) => Some("\x14"), //20
        ("T", TerminalModifiers::CtrlShift) => Some("\x14"), //20
        ("u", TerminalModifiers::Ctrl) => Some("\x15"), //21
        ("U", TerminalModifiers::CtrlShift) => Some("\x15"), //21
        ("v", TerminalModifiers::Ctrl) => Some("\x16"), //22
        ("V", TerminalModifiers::CtrlShift) => Some("\x16"), //22
        ("w", TerminalModifiers::Ctrl) => Some("\x17"), //23
        ("W", TerminalModifiers::CtrlShift) => Some("\x17"), //23
        ("x", TerminalModifiers::Ctrl) => Some("\x18"), //24
        ("X", TerminalModifiers::CtrlShift) => Some("\x18"), //24
        ("y", TerminalModifiers::Ctrl) => Some("\x19"), //25
        ("Y", TerminalModifiers::CtrlShift) => Some("\x19"), //25
        ("z", TerminalModifiers::Ctrl) => Some("\x1a"), //26
        ("Z", TerminalModifiers::CtrlShift) => Some("\x1a"), //26
        ("@", TerminalModifiers::Ctrl) => Some("\x00"), //0
        ("[", TerminalModifiers::Ctrl) => Some("\x1b"), //27
        ("\\", TerminalModifiers::Ctrl) => Some("\x1c"), //28
        ("]", TerminalModifiers::Ctrl) => Some("\x1d"), //29
        ("^", TerminalModifiers::Ctrl) => Some("\x1e"), //30
        ("_", TerminalModifiers::Ctrl) => Some("\x1f"), //31
        ("?", TerminalModifiers::Ctrl) => Some("\x7f"), //127
        _ => None,
    };
    if let Some(esc_str) = manual_esc_str {
        return Some(Cow::Borrowed(esc_str));
    }

    // Automated bindings applying modifiers
    if modifiers.any() {
        let modifier_code = modifier_code(keystroke);
        let modified_esc_str = match keystroke.key.as_ref() {
            "up" => Some(format!("\x1b[1;{}A", modifier_code)),
            "down" => Some(format!("\x1b[1;{}B", modifier_code)),
            "right" => Some(format!("\x1b[1;{}C", modifier_code)),
            "left" => Some(format!("\x1b[1;{}D", modifier_code)),
            "f1" => Some(format!("\x1b[1;{}P", modifier_code)),
            "f2" => Some(format!("\x1b[1;{}Q", modifier_code)),
            "f3" => Some(format!("\x1b[1;{}R", modifier_code)),
            "f4" => Some(format!("\x1b[1;{}S", modifier_code)),
            "f5" => Some(format!("\x1b[15;{}~", modifier_code)),
            "f6" => Some(format!("\x1b[17;{}~", modifier_code)),
            "f7" => Some(format!("\x1b[18;{}~", modifier_code)),
            "f8" => Some(format!("\x1b[19;{}~", modifier_code)),
            "f9" => Some(format!("\x1b[20;{}~", modifier_code)),
            "f10" => Some(format!("\x1b[21;{}~", modifier_code)),
            "f11" => Some(format!("\x1b[23;{}~", modifier_code)),
            "f12" => Some(format!("\x1b[24;{}~", modifier_code)),
            "f13" => Some(format!("\x1b[25;{}~", modifier_code)),
            "f14" => Some(format!("\x1b[26;{}~", modifier_code)),
            "f15" => Some(format!("\x1b[28;{}~", modifier_code)),
            "f16" => Some(format!("\x1b[29;{}~", modifier_code)),
            "f17" => Some(format!("\x1b[31;{}~", modifier_code)),
            "f18" => Some(format!("\x1b[32;{}~", modifier_code)),
            "f19" => Some(format!("\x1b[33;{}~", modifier_code)),
            "f20" => Some(format!("\x1b[34;{}~", modifier_code)),
            "insert" => Some(format!("\x1b[2;{}~", modifier_code)),
            "pageup" => Some(format!("\x1b[5;{}~", modifier_code)),
            "pagedown" => Some(format!("\x1b[6;{}~", modifier_code)),
            "end" => Some(format!("\x1b[1;{}F", modifier_code)),
            "home" => Some(format!("\x1b[1;{}H", modifier_code)),
            _ => None,
        };
        if let Some(esc_str) = modified_esc_str {
            return Some(Cow::Owned(esc_str));
        }
    }

    // Handling alt keys. These work for single ASCII characters.
    if (!cfg!(target_os = "macos") || option_as_meta)
        && keystroke.modifiers.alt
        && keystroke.key.is_ascii()
        && keystroke.key.len() == 1
    {
        let key = keystroke.key.chars().next().unwrap();

        if modifiers == TerminalModifiers::Alt {
            return Some(Cow::Owned(format!("\x1b{key}")));
        } else if keystroke.modifiers.shift {
            return Some(Cow::Owned(format!("\x1b{}", key.to_ascii_uppercase())));
        } else if keystroke.modifiers.control && key.is_ascii_lowercase() {
            return Some(Cow::Owned(format!(
                "\x1b{}",
                (key as u8 - b'a' + 1) as char
            )));
        }
    }

    None
}

///   Code     Modifiers
/// ---------+---------------------------
///    2     | Shift
///    3     | Alt
///    4     | Shift + Alt
///    5     | Control
///    6     | Shift + Control
///    7     | Alt + Control
///    8     | Shift + Alt + Control
/// ---------+---------------------------
/// from: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-PC-Style-Function-Keys
fn modifier_code(keystroke: &Keystroke) -> u32 {
    let mut modifier_code = 0;
    if keystroke.modifiers.shift {
        modifier_code |= 1;
    }
    if keystroke.modifiers.alt {
        modifier_code |= 1 << 1;
    }
    if keystroke.modifiers.control {
        modifier_code |= 1 << 2;
    }
    modifier_code + 1
}

