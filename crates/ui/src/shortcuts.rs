//! App shortcuts that can be changed in Settings: their action, their name and
//! their default key combination. Changed ones are saved to `config.json` and
//! applied immediately, without touching the components' own (the editor, the
//! terminal).

use gpui_kit::{Action, App, KeyBinding, Keystroke};

use crate::{config::Config, *};

pub struct Shortcut {
    /// Name of the action (without `app::`); it's the key in `config.json`.
    pub id: &'static str,
    pub label: &'static str,
    pub default: &'static str,
    bind: fn(&str) -> KeyBinding,
    action: fn() -> Box<dyn Action>,
}

macro_rules! shortcuts {
    ($(($action:ident, $label:expr, $keys:expr)),* $(,)?) => {
        pub static SHORTCUTS: &[Shortcut] = &[$(Shortcut {
            id: stringify!($action),
            label: $label,
            default: $keys,
            bind: |keys| KeyBinding::new(keys, $action, None),
            action: || Box::new($action),
        }),*];
    };
}

// The app's use Cmd on Mac and Ctrl on Windows and Linux (`secondary`), and have
// no context: they mean the same thing wherever the focus is.
shortcuts![
    (OpenCommandPalette, "Command Palette", "secondary-shift-p"),
    (ShowShortcuts, "Show Shortcuts", "f1"),
    (OpenSettings, "Settings", "secondary-,"),
    (Quit, "Quit", "secondary-q"),
    (Save, "Save", "secondary-s"),
    (CloseTab, "Close Tab or Terminal", "secondary-w"),
    (CloseAllTabs, "Close All Tabs", "secondary-alt-w"),
    (NextTab, "Next Tab", "secondary-shift-]"),
    (PrevTab, "Previous Tab", "secondary-shift-["),
    (OpenFileFinder, "Go to File", "secondary-p"),
    (ToggleSidePanel, "Toggle Side Panel", "secondary-b"),
    (ShowFiles, "Panel: Files", "secondary-shift-e"),
    (CollapseFileTree, "Collapse All Folders", "secondary-alt-c"),
    (ShowChanges, "Panel: Changes", "secondary-shift-g"),
    (ShowSearch, "Panel: Search", "secondary-shift-f"),
    (ShowReferences, "Panel: References", "secondary-shift-r"),
    (NextResult, "Next Result", "f4"),
    (PrevResult, "Previous Result", "shift-f4"),
    (GoToDefinition, "Go to Definition", "f12"),
    (FindReferences, "Find References", "shift-f12"),
    (NavigateBack, "Go Back", "ctrl-alt-left"),
    (NavigateForward, "Go Forward", "ctrl-alt-right"),
    (ToggleMarkdownSource, "Markdown: Toggle Source or Preview", "secondary-shift-v"),
    (NewTerminal, "New Terminal", "secondary-t"),
    (SplitRight, "Split Terminal Right", "secondary-d"),
    (SplitDown, "Split Terminal Down", "secondary-shift-d"),
    (FocusPaneLeft, "Focus Terminal Left", "secondary-alt-left"),
    (FocusPaneRight, "Focus Terminal Right", "secondary-alt-right"),
    (FocusPaneUp, "Focus Terminal Above", "secondary-alt-up"),
    (FocusPaneDown, "Focus Terminal Below", "secondary-alt-down"),
    (ToggleTerminals, "Toggle Terminals", "secondary-j"),
    (MaximizeTerminals, "Maximize Terminals", "secondary-shift-j"),
    (NewTask, "New Task", "secondary-n"),
    (OpenTaskPicker, "Find Task", "secondary-k"),
    (PreviousTask, "Previous Task", "secondary-e"),
    (ToggleTasks, "Toggle Tasks Column", "secondary-shift-b"),
    (ActivateTask1, "Go to Task 1", "secondary-1"),
    (ActivateTask2, "Go to Task 2", "secondary-2"),
    (ActivateTask3, "Go to Task 3", "secondary-3"),
    (ActivateTask4, "Go to Task 4", "secondary-4"),
    (ActivateTask5, "Go to Task 5", "secondary-5"),
    (ActivateTask6, "Go to Task 6", "secondary-6"),
    (ActivateTask7, "Go to Task 7", "secondary-7"),
    (ActivateTask8, "Go to Task 8", "secondary-8"),
    (ActivateTask9, "Go to Task 9", "secondary-9"),
];

impl Shortcut {
    pub fn action(&self) -> Box<dyn Action> {
        (self.action)()
    }
}

/// A shortcut's key combination, already in the platform's form (`cmd-s`);
/// `None` if it was removed.
pub fn keys(shortcut: &Shortcut, cx: &App) -> Option<Keystroke> {
    let keys = Config::get(cx).keys.get(shortcut.id).map(String::as_str).unwrap_or(shortcut.default);
    Keystroke::parse(keys).ok()
}

/// Whether the shortcut differs from the default.
pub fn changed(shortcut: &Shortcut, cx: &App) -> bool {
    Config::get(cx).keys.contains_key(shortcut.id)
}

/// The shortcut that uses `keystroke`, if any.
pub fn owner(keystroke: &Keystroke, cx: &App) -> Option<&'static Shortcut> {
    SHORTCUTS.iter().find(|shortcut| keys(shortcut, cx).as_ref() == Some(keystroke))
}

/// Registers the app's shortcuts according to the config, replacing any that
/// were there; the components' own stay as they were.
pub fn apply(cx: &mut App) {
    let ours = |name: &str| {
        name.strip_prefix("app::")
            .is_some_and(|id| SHORTCUTS.iter().any(|shortcut| shortcut.id == id))
    };
    let others: Vec<KeyBinding> = cx
        .key_bindings()
        .borrow()
        .bindings()
        .filter(|binding| !ours(binding.action().name()))
        .cloned()
        .collect();
    let bindings: Vec<KeyBinding> = SHORTCUTS
        .iter()
        .filter_map(|shortcut| Some((shortcut.bind)(&keys(shortcut, cx)?.unparse())))
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(others);
    cx.bind_keys(bindings);
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::Keystroke;

    use super::SHORTCUTS;

    #[test]
    fn defaults_parse_and_do_not_clash() {
        let mut seen = HashSet::new();
        for shortcut in SHORTCUTS {
            let keys = Keystroke::parse(shortcut.default).unwrap_or_else(|_| panic!("{}", shortcut.default));
            assert!(seen.insert(keys.unparse()), "{} repeated", shortcut.default);
        }
        let ids: HashSet<&str> = SHORTCUTS.iter().map(|shortcut| shortcut.id).collect();
        assert_eq!(ids.len(), SHORTCUTS.len());
    }
}
