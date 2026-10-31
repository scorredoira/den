//! App shortcuts that can be changed in Settings: their action, their name and
//! their default key combination. Changed ones are saved to `config.json` and
//! applied immediately, without touching the components' own (the editor, the
//! terminal).

use std::collections::VecDeque;

use gpui_kit::{Action, App, Global, KeyBinding, Keystroke, Modifiers, NoAction, is_no_action};

use crate::{config::Config, *};

pub struct Shortcut {
    /// Name of the action (without `app::`); it's the key in `config.json`.
    pub id: &'static str,
    pub label: &'static str,
    pub default: &'static str,
    /// What it's listed under in Keyboard Shortcuts (`SECTIONS`).
    pub section: &'static str,
    bind: fn(&str) -> KeyBinding,
    action: fn() -> Box<dyn Action>,
}

macro_rules! shortcuts {
    ($($section:literal: [$(($action:ident, $label:expr, $keys:expr)),* $(,)?]),* $(,)?) => {
        pub static SECTIONS: &[&str] = &[$($section),*];
        pub static SHORTCUTS: &[Shortcut] = &[$($(Shortcut {
            id: stringify!($action),
            label: $label,
            default: $keys,
            section: $section,
            bind: |keys| KeyBinding::new(keys, $action, None),
            action: || Box::new($action),
        }),*),*];
    };
}

// The app's use Cmd on Mac and Ctrl on Windows and Linux (`secondary`), and have
// no context: they mean the same thing wherever the focus is.
shortcuts![
    "General": [
        (OpenCommandPalette, "Command Palette", "secondary-shift-p"),
        (ShowShortcuts, "Show Shortcuts", "f1"),
        (OpenSettings, "Settings", "secondary-,"),
        (OpenKeyboardShortcuts, "Keyboard Shortcuts", ""),
        (OpenShortcutsGuide, "Keyboard Shortcuts Guide", ""),
        (ReloadWindow, "Reload Window", ""),
        (Quit, "Quit", "secondary-q"),
    ],
    "Workspaces": [
        (OpenTaskPicker, "Find Workspace", mac_or("cmd-k", "ctrl-shift-k")),
        (PreviousTask, "Switch Workspace", mac_or("cmd-alt-e", "ctrl-tab")),
        (NextActiveTask, "Next Workspace with an Agent", mac_or("cmd-e", "ctrl-alt-e")),
        (NextTask, "Next Workspace", "secondary-alt-shift-e"),
        (ToggleTasks, "Toggle Workspaces", "secondary-shift-b"),
        (ToggleInactiveWorkspaces, "Show or Hide Inactive Workspaces", "secondary-alt-b"),
        (NewTask, "New Worktree", "secondary-shift-n"),
        (OpenFolder, "Open Folder", "secondary-o"),
        (OpenRemoteFolder, "Open Folder on Server", "secondary-alt-o"),
        (OpenRecent, "Open Recent", "secondary-alt-r"),
    ],
    "Files and Tabs": [
        (OpenFileFinder, "Go to File", "secondary-p"),
        (OpenFile, "Open File", ""),
        (NewFile, "New File", "secondary-n"),
        (Save, "Save", "secondary-s"),
        (SaveAs, "Save As", "secondary-shift-s"),
        (CloseTab, "Close Tab or Terminal", mac_or("cmd-w", "ctrl-shift-w")),
        (CloseAllTabs, "Close All Tabs", "secondary-alt-w"),
        (NextTab, "Next Tab", "secondary-shift-]"),
        (PrevTab, "Previous Tab", "secondary-shift-["),
        (SplitEditorRight, "Split Editor Right", "secondary-alt-s"),
        (SplitEditorDown, "Split Editor Down", "secondary-alt-shift-s"),
        (ToggleDiffLayout, "Diffs: Side by Side or One Column", "secondary-alt-shift-d"),
    ],
    "Editing": [
        (ToggleWordWrap, "Toggle Word Wrap", "alt-z"),
        (FormatDocument, "Format Document", "shift-alt-f"),
        (ToggleMarkdownSource, "Markdown: Toggle Source or Preview", "secondary-shift-v"),
        (OpenPreviewToSide, "Markdown: Open Preview to the Side", "secondary-alt-v"),
        (ZoomInPreview, "Markdown: Zoom In", "secondary-="),
        (ZoomOutPreview, "Markdown: Zoom Out", "secondary--"),
        (ResetPreviewZoom, "Markdown: Reset Zoom", "secondary-0"),
        (ExportPreviewPdf, "Markdown: Export Preview as PDF", ""),
        (FitPreviewDiagrams, "Markdown: Fit Diagrams to Width", ""),
    ],
    "Code": [
        (GoToDefinition, "Go to Definition", "f12"),
        (FindReferences, "Find References", "shift-f12"),
        (GoToLine, "Go to Line", "ctrl-g"),
        (GoToSymbol, "Go to Symbol in File", "secondary-shift-o"),
        (GoToWorkspaceSymbol, "Go to Symbol in Workspace", "secondary-shift-t"),
        (NavigateBack, "Go Back", if cfg!(target_os = "macos") { "ctrl-alt-left" } else { "alt-left" }),
        (NavigateForward, "Go Forward", if cfg!(target_os = "macos") { "ctrl-alt-right" } else { "alt-right" }),
    ],
    "Side Panel": [
        (ToggleSidePanel, "Toggle Side Panel", "secondary-b"),
        (ShowFiles, "Panel: Files", "secondary-shift-e"),
        (CollapseFileTree, "Collapse All Folders", "secondary-alt-c"),
        (RefreshFiles, "Files: Refresh", ""),
        (ShowChanges, "Panel: Changes", "secondary-shift-g"),
        (ShowSearch, "Panel: Search", "secondary-shift-f"),
        (ShowReferences, "Panel: References", "secondary-shift-r"),
        (ShowOutline, "Panel: Outline", "secondary-shift-l"),
        (NextResult, "Next Result", "f4"),
        (PrevResult, "Previous Result", "shift-f4"),
        (ShowGraph, "Show Graph", "secondary-shift-h"),
    ],
    "Terminals": [
        (ToggleTerminals, "Toggle Terminals", mac_or("cmd-j", "ctrl-`")),
        (NewTerminal, "New Terminal", mac_or("cmd-t", "ctrl-shift-`")),
        (SplitRight, "Split Terminal Right", mac_or("cmd-alt-d", "ctrl-shift-5")),
        (SplitDown, "Split Terminal Down", mac_or("cmd-d", "ctrl-alt-d")),
        (FocusPaneLeft, "Focus Terminal Left", "secondary-alt-left"),
        (FocusPaneRight, "Focus Terminal Right", "secondary-alt-right"),
        (FocusPaneUp, "Focus Terminal Above", "secondary-alt-up"),
        (FocusPaneDown, "Focus Terminal Below", "secondary-alt-down"),
        (FocusNext, "Focus Next: Code or Terminal", mac_or("ctrl-tab", "f8")),
        (FocusPrevious, "Focus Previous: Code or Terminal", mac_or("ctrl-shift-tab", "shift-f8")),
        (MaximizeTerminals, "Maximize Terminals", "secondary-shift-j"),
        (MoveTerminals, "Move Terminals Right or Down", "secondary-alt-j"),
        (ToggleTerminalMode, "Terminal Mode", "secondary-alt-shift-j"),
        (ToggleNotes, "Toggle Notes", "secondary-alt-n"),
    ],
    "Debugging": [
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
    ],
];

/// Whether a plain editor has the action called `name` (`app::ShowFiles` or
/// `ShowFiles`): what needs a panel it doesn't have, the terminals, the
/// debugger, git or other workspaces isn't offered there.
pub fn in_plain_editor(name: &str) -> bool {
    let id = name.strip_prefix("app::").unwrap_or(name);
    !matches!(
        id,
        "ShowChanges"
            | "ShowGraph"
            | "OpenChanges"
            | "ShowFileHistory"
            | "ShowReferences"
            | "ShowOutline"
            | "FindReferences"
            | "NewTerminal"
            | "SplitRight"
            | "SplitDown"
            | "FocusPaneLeft"
            | "FocusPaneRight"
            | "FocusPaneUp"
            | "FocusPaneDown"
            | "ToggleTerminals"
            | "MaximizeTerminals"
            | "MoveTerminals"
            | "ToggleTerminalMode"
            | "terminal::Find"
            | "NewTask"
            | "OpenTaskPicker"
            | "PreviousTask"
            | "NextActiveTask"
            | "NextTask"
            | "ToggleTasks"
            | "ToggleInactiveWorkspaces"
            | "OpenFolder"
            | "OpenRemoteFolder"
            | "OpenRecent"
            | "AddServer"
            | "ResetLayout"
            | "ToggleNotes"
            | "DebugContinue"
            | "DebugStop"
            | "DebugRestart"
            | "DebugPause"
            | "StepOver"
            | "StepInto"
            | "StepOut"
            | "ToggleBreakpoint"
            | "RunToCursor"
            | "SetNextStatement"
            | "ToggleDebugPanel"
            | "AddConditionalBreakpoint"
            | "AddLogpoint"
            | "AddToWatch"
            | "EvaluateInConsole"
    )
}

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
    parse(Config::get(cx).keys.get(shortcut.id).map(String::as_str).unwrap_or(shortcut.default))
}

/// `""` is no shortcut: gpui parses it as a keystroke with no key, which
/// every removed shortcut would share.
pub fn parse(keys: &str) -> Option<Keystroke> {
    Keystroke::parse(keys).ok().filter(|keystroke| !keystroke.key.is_empty())
}

/// Gives the shortcut `id` these keys (`""`, none; the default's, back to
/// it). Keys another shortcut has are refused: the other's name.
pub fn set(id: &str, keys: &str, cx: &mut App) -> Result<(), &'static Shortcut> {
    let shortcut = SHORTCUTS.iter().find(|shortcut| shortcut.id == id).expect("a shortcut");
    if let Some(other) = parse(keys).and_then(|keystroke| owner(&keystroke, cx)).filter(|other| other.id != id) {
        return Err(other);
    }
    Config::update(cx, |config| {
        if parse(keys) == parse(shortcut.default) {
            config.keys.remove(id);
        } else {
            config.keys.insert(id.to_string(), keys.to_string());
        }
    });
    apply(cx);
    Ok(())
}

/// The last keystrokes the app got, with what each ran, oldest first: for
/// `den keys`, to see why a shortcut does nothing.
#[derive(Default)]
struct RecentKeys(VecDeque<String>);

impl Global for RecentKeys {}

const RECENT_KEYS: usize = 20;

/// Starts keeping the last keystrokes (`recent_keys`).
pub fn keep_recent_keys(cx: &mut App) {
    cx.set_global(RecentKeys::default());
    cx.observe_keystrokes(|event, _, cx| {
        let action = event.action.as_ref().map_or("nothing", |action| action.name());
        let context: Vec<&str> =
            event.context_stack.iter().filter_map(|context| context.primary()).map(|entry| entry.key.as_ref()).collect();
        let line = format!("{}\t{action}\t{}", event.keystroke.unparse(), context.join(" > "));
        let recent = &mut cx.global_mut::<RecentKeys>().0;
        if recent.len() == RECENT_KEYS {
            recent.pop_front();
        }
        recent.push_back(line);
    })
    .detach();
}

/// The last keystrokes, one a line: the keys, the action they ran (or
/// `nothing`) and where the focus was.
pub fn recent_keys(cx: &App) -> String {
    cx.try_global::<RecentKeys>().map(|recent| recent.0.iter().map(|line| format!("{line}\n")).collect()).unwrap_or_default()
}

/// Every shortcut, one a line: its name, its keys (`-`, none) and its label,
/// and the default when it was changed.
pub fn list(cx: &App) -> String {
    SHORTCUTS
        .iter()
        .map(|shortcut| {
            let keys = keys(shortcut, cx).map_or("-".to_string(), |keys| keys.unparse());
            let default = if changed(shortcut, cx) { format!(" (default {})", if shortcut.default.is_empty() { "-" } else { shortcut.default }) } else { String::new() };
            format!("{}\t{keys}\t{}{default}\n", shortcut.id, shortcut.label)
        })
        .collect()
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
    // Where + has a key of its own (a Spanish keyboard), it zooms in as =
    // does, while Zoom In has a key with = and + is no other shortcut's.
    let zoom_in = SHORTCUTS.iter().find(|shortcut| shortcut.id == "ZoomInPreview").expect("a shortcut");
    if let Some(mut plus) = keys(zoom_in, cx).filter(|keystroke| keystroke.key == "=") {
        plus.key = "+".into();
        if owner(&plus, cx).is_none() {
            cx.bind_keys([KeyBinding::new(&plus.unparse(), ZoomInPreview, None)]);
        }
    }
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

    /// What `den shortcuts set` and Settings do: keys another has are
    /// refused, and the default's go back to it.
    #[gpui_kit::test]
    fn a_shortcut_takes_keys_no_other_has(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(crate::config::Config::default());
            assert!(super::set("CloseAllTabs", "f16", cx).is_ok());
            assert_eq!(super::list(cx).lines().find(|line| line.starts_with("CloseAllTabs\t")), Some("CloseAllTabs\tf16\tClose All Tabs (default secondary-alt-w)"));
            assert_eq!(super::set("CollapseFileTree", "f16", cx).map_err(|other| other.id), Err("CloseAllTabs"));
            assert!(super::set("CloseAllTabs", "secondary-alt-w", cx).is_ok());
            assert!(crate::config::Config::get(cx).keys.is_empty(), "back to the default");
        });
    }

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
    fn empty_is_no_shortcut() {
        assert!(super::parse("").is_none());
        assert!(super::parse("cmd-alt-w").is_some());
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
