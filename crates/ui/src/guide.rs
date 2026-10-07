//! Help → Keyboard Shortcuts: a guide in Markdown (`assets/docs/shortcuts.md`)
//! whose `{{…}}` are filled in with the keys as they are now, in this
//! platform's form: `{{OpenFileFinder}}` a shortcut from Settings (changed
//! or not), `{{Undo}}` one of the editor's own, `{{key:escape}}` a key, and
//! `{{alt}}`, `{{shift}}` or `{{secondary}}` a modifier for a click or a drag.

use gpui_kit::{App, Keystroke, component::kbd::Kbd};

use crate::shortcuts::{self, SHORTCUTS};

const TEMPLATE: &str = include_str!("../assets/docs/shortcuts.md");

/// The title of its tab.
pub const TITLE: &str = "Keyboard Shortcuts";

/// The editor's own keys, which Settings doesn't change (see `editing.rs`
/// and the editor's bindings in `vendor/gpui-base`).
fn editor_keys(id: &str) -> Option<&'static str> {
    let mac = cfg!(target_os = "macos");
    let windows = cfg!(target_os = "windows");
    Some(match id {
        "Undo" => "secondary-z",
        "Redo" if mac => "cmd-shift-z",
        "Redo" => "ctrl-y",
        "MoveLineUp" => "alt-up",
        "MoveLineDown" => "alt-down",
        "DuplicateLineUp" => "shift-alt-up",
        "DuplicateLineDown" => "shift-alt-down",
        "Indent" => "secondary-]",
        "Outdent" => "secondary-[",
        "SelectNextOccurrence" => "secondary-d",
        "AddCursorAbove" if mac => "cmd-alt-up",
        "AddCursorAbove" if windows => "ctrl-alt-up",
        "AddCursorBelow" if mac => "cmd-alt-down",
        "AddCursorBelow" if windows => "ctrl-alt-down",
        "Search" => "secondary-f",
        "Replace" if !mac => "ctrl-h",
        // The terminal's, in `ui-term`: off the Mac Ctrl-F is the shell's.
        "TerminalFind" if mac => "cmd-f",
        "TerminalFind" => "ctrl-alt-f",
        _ => return None,
    })
}

/// What an editor shortcut without keys on this platform says instead.
fn without_keys(id: &str) -> Option<String> {
    Some(match id {
        // On Linux Shift-Alt-↑/↓ copy the line (above).
        "AddCursorAbove" | "AddCursorBelow" => "the command palette".to_string(),
        // Cmd-Shift-F is the workspace search.
        "Replace" => format!("{}, then the replace toggle", keys("secondary-f")),
        _ => return None,
    })
}

/// `keys` (`secondary-shift-p`) as this platform writes them, as code.
fn keys(keys: &str) -> String {
    match Keystroke::parse(keys) {
        Ok(keystroke) => format!("`{}`", Kbd::format(&keystroke)),
        Err(_) => format!("`{keys}`"),
    }
}

fn modifier(name: &str) -> Option<&'static str> {
    let mac = cfg!(target_os = "macos");
    Some(match name {
        "alt" if mac => "⌥",
        "alt" => "Alt",
        "shift" if mac => "⇧",
        "shift" => "Shift",
        "secondary" if mac => "⌘",
        "secondary" => "Ctrl",
        _ => return None,
    })
}

/// What `{{name}}` stands for; `None` if it means nothing.
fn resolve(name: &str, cx: &App) -> Option<String> {
    if let Some(shortcut) = SHORTCUTS.iter().find(|shortcut| shortcut.id == name) {
        return Some(match shortcuts::keys(shortcut, cx) {
            Some(keystroke) => format!("`{}`", Kbd::format(&keystroke)),
            None => "*(no shortcut)*".to_string(),
        });
    }
    if let Some(key) = name.strip_prefix("key:") {
        return Some(keys(key));
    }
    if let Some(modifier) = modifier(name) {
        return Some(modifier.to_string());
    }
    editor_keys(name).map(keys).or_else(|| without_keys(name))
}

/// The guide, with every `{{…}}` filled in.
pub fn markdown(cx: &App) -> String {
    fill(TEMPLATE, |name| resolve(name, cx))
}

fn fill(template: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match resolve(name) {
            Some(text) => out.push_str(&text),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;
    use crate::config::Config;

    /// Every placeholder means something, and every shortcut from Settings
    /// is in the guide: a new one has to be added to it.
    #[gpui_kit::test]
    fn the_guide_is_complete(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| cx.set_global(Config::default()));
        let guide = cx.update(|cx| markdown(cx));
        assert!(!guide.contains("{{"), "unresolved placeholders in the guide");
        for shortcut in SHORTCUTS {
            assert!(TEMPLATE.contains(&format!("{{{{{}}}}}", shortcut.id)), "{} is not in the guide", shortcut.id);
        }
    }

    #[test]
    fn fills_what_it_knows() {
        let filled = fill("a {{x}} b {{y}} c {{z", |name| (name == "x").then(|| "X".to_string()));
        assert_eq!(filled, "a X b {{y}} c {{z");
    }
}
