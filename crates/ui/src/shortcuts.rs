//! App shortcuts that can be changed in Settings: their action, their name and
//! their default key combination. Changed ones are saved to `config.json` and
//! applied immediately, without touching the components' own (the editor, the
//! terminal).

use gpui_kit::{Action, App, KeyBinding, Keystroke, Modifiers, NoAction, is_no_action};

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
    (ReloadWindow, "Reload Window", ""),
    (Save, "Save", "secondary-s"),
    (CloseTab, "Close Tab or Terminal", mac_or("cmd-w", "ctrl-shift-w")),
    (CloseAllTabs, "Close All Tabs", "secondary-alt-w"),
    (NextTab, "Next Tab", "secondary-shift-]"),
    (PrevTab, "Previous Tab", "secondary-shift-["),
    (OpenFileFinder, "Go to File", "secondary-p"),
    (ToggleSidePanel, "Toggle Side Panel", "secondary-b"),
    (ShowFiles, "Panel: Files", "secondary-shift-e"),
    (CollapseFileTree, "Collapse All Folders", "secondary-alt-c"),
    (RefreshFiles, "Files: Refresh", ""),
    (ShowChanges, "Panel: Changes", "secondary-shift-g"),
    (ShowHistory, "Show History", "secondary-shift-h"),
    (ShowSearch, "Panel: Search", "secondary-shift-f"),
    (ShowReferences, "Panel: References", "secondary-shift-r"),
    (ShowOutline, "Panel: Outline", "secondary-shift-l"),
    (NextResult, "Next Result", "f4"),
    (PrevResult, "Previous Result", "shift-f4"),
    (GoToDefinition, "Go to Definition", "f12"),
    (GoToLine, "Go to Line", "ctrl-g"),
    (GoToSymbol, "Go to Symbol in File", "secondary-shift-o"),
    (GoToWorkspaceSymbol, "Go to Symbol in Workspace", "secondary-shift-t"),
    (FindReferences, "Find References", "shift-f12"),
    (NavigateBack, "Go Back", if cfg!(target_os = "macos") { "ctrl-alt-left" } else { "alt-left" }),
    (NavigateForward, "Go Forward", if cfg!(target_os = "macos") { "ctrl-alt-right" } else { "alt-right" }),
    (ToggleMarkdownSource, "Markdown: Toggle Source or Preview", "secondary-shift-v"),
    (OpenPreviewToSide, "Markdown: Open Preview to the Side", "secondary-alt-v"),
    (ToggleWordWrap, "Toggle Word Wrap", "alt-z"),
    (FormatDocument, "Format Document", "shift-alt-f"),
    (SplitEditorRight, "Split Editor Right", "secondary-alt-s"),
    (SplitEditorDown, "Split Editor Down", "secondary-alt-shift-s"),
    (NewTerminal, "New Terminal", mac_or("cmd-t", "ctrl-shift-`")),
    (SplitRight, "Split Terminal Right", mac_or("cmd-alt-d", "ctrl-shift-5")),
    (SplitDown, "Split Terminal Down", mac_or("cmd-d", "ctrl-alt-d")),
    (FocusPaneLeft, "Focus Terminal Left", "secondary-alt-left"),
    (FocusPaneRight, "Focus Terminal Right", "secondary-alt-right"),
    (FocusPaneUp, "Focus Terminal Above", "secondary-alt-up"),
    (FocusPaneDown, "Focus Terminal Below", "secondary-alt-down"),
    (ToggleTerminals, "Toggle Terminals", mac_or("cmd-j", "ctrl-`")),
    (MaximizeTerminals, "Maximize Terminals", "secondary-shift-j"),
    (MoveTerminals, "Move Terminals Right or Down", "secondary-alt-j"),
    (ToggleTerminalMode, "Terminal Mode", "secondary-alt-shift-j"),
    (NewFile, "New File", "secondary-n"),
    (NewTask, "New Worktree", "secondary-shift-n"),
    (OpenTaskPicker, "Find Workspace", mac_or("cmd-k", "ctrl-shift-k")),
    (PreviousTask, "Switch Workspace", mac_or("cmd-alt-e", "ctrl-tab")),
    (NextActiveTask, "Next Workspace with an Agent", mac_or("cmd-e", "ctrl-alt-e")),
    (NextTask, "Next Workspace", "secondary-alt-shift-e"),
    (ToggleTasks, "Toggle Workspaces", "secondary-shift-b"),
    (OpenFolder, "Open Folder", "secondary-o"),
    (OpenRemoteFolder, "Open Folder on Server", "secondary-alt-o"),
    (OpenRecent, "Open Recent", "secondary-alt-r"),
    (DebugContinue, "Debug: Start or Continue", "f5"),
    (DebugStop, "Debug: Stop", "shift-f5"),
    (DebugRestart, "Debug: Restart", "secondary-shift-f5"),
    (DebugPause, "Debug: Pause", "f6"),
    (StepOver, "Debug: Step Over", "f10"),
    (StepInto, "Debug: Step Into", "f11"),
    (StepOut, "Debug: Step Out", "shift-f11"),
    (ToggleBreakpoint, "Debug: Toggle Breakpoint", "f9"),
    (RunToCursor, "Debug: Run to Cursor", "ctrl-f10"),
    (SetNextStatement, "Debug: Set Next Statement", "ctrl-shift-f10"),
    (ToggleDebugPanel, "Debug: Toggle Panel", "secondary-shift-d"),
    (ToggleNotes, "Toggle Notes", "secondary-alt-n"),
];

/// `mac` on the Mac; `other` on Linux and Windows, for the shortcuts used from
/// the terminal, where Ctrl plus a letter is the shell's (see `for_the_shell`).
const fn mac_or(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") { mac } else { other }
}

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
/// were there, and then the code editor's; the components' own stay as they were.
pub fn apply(cx: &mut App) {
    let ours = |name: &str| {
        name.starts_with("editing::")
            || name.strip_prefix("app::")
                .is_some_and(|id| SHORTCUTS.iter().any(|shortcut| shortcut.id == id))
    };
    let others: Vec<KeyBinding> = cx
        .key_bindings()
        .borrow()
        .bindings()
        .filter(|binding| !ours(binding.action().name()) && !is_no_action(binding.action()))
        .cloned()
        .collect();
    let keystrokes: Vec<Keystroke> = SHORTCUTS.iter().filter_map(|shortcut| keys(shortcut, cx)).collect();
    let bindings: Vec<KeyBinding> = SHORTCUTS
        .iter()
        .filter_map(|shortcut| Some((shortcut.bind)(&keys(shortcut, cx)?.unparse())))
        .collect();
    cx.clear_key_bindings();
    cx.bind_keys(others);
    cx.bind_keys(bindings);
    // After the app's, so they win inside the editor (see `editing::keymap`).
    cx.bind_keys(crate::editing::keymap());
    // Also after the app's: a binding without context and one in the focused
    // terminal are equally deep, and the later one wins.
    if !cfg!(target_os = "macos") {
        cx.bind_keys(
            keystrokes
                .iter()
                .filter(|keystroke| for_the_shell(keystroke))
                .map(|keystroke| KeyBinding::new(&keystroke.unparse(), NoAction, Some("Terminal"))),
        );
    }
}

/// Whether, inside the terminal, `keystroke` goes to the shell rather than to
/// the app's shortcut. Off the Mac the app's shortcuts use Ctrl, and Ctrl plus
/// a letter is the shell's (EOF, delete word, history, readline); with Shift
/// or Alt they stay the app's, as copy and paste do (see `ui-term`'s platform).
fn for_the_shell(keystroke: &Keystroke) -> bool {
    keystroke.modifiers == Modifiers::control()
        && keystroke.key.len() == 1
        && keystroke.key.chars().all(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gpui_kit::Keystroke;

    use super::SHORTCUTS;

    #[test]
    fn defaults_parse_and_do_not_clash() {
        let mut seen = HashSet::new();
        // "" has no keys: the command palette runs it.
        for shortcut in SHORTCUTS.iter().filter(|shortcut| !shortcut.default.is_empty()) {
            let keys = Keystroke::parse(shortcut.default).unwrap_or_else(|_| panic!("{}", shortcut.default));
            assert!(seen.insert(keys.unparse()), "{} repeated", shortcut.default);
        }
        let ids: HashSet<&str> = SHORTCUTS.iter().map(|shortcut| shortcut.id).collect();
        assert_eq!(ids.len(), SHORTCUTS.len());
    }

    #[test]
    fn only_ctrl_and_a_letter_goes_to_the_shell() {
        let shell = |keys: &str| super::for_the_shell(&Keystroke::parse(keys).unwrap());
        assert!(shell("ctrl-d"));
        assert!(shell("ctrl-w"));
        assert!(!shell("ctrl-shift-d"));
        assert!(!shell("ctrl-alt-w"));
        assert!(!shell("ctrl-,"));
        assert!(!shell("ctrl-f10"));
        assert!(!shell("cmd-d"));
    }
}
