//! The menu bar (macOS), as VS Code's: every entry is an action that already
//! has a shortcut, and the menu shows it next to it. Items whose action nobody
//! handles where the focus is (Undo in a terminal) show up disabled.

use gpui_base::input;
use gpui_kit::{App, Menu, MenuItem, OsAction, SystemMenuType};

use crate::{
    config::Config,
    editing::{DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, SelectNextOccurrence},
    *,
};

/// Sets the menus; again after something they show changes (Word Wrap's check).
pub fn set(cx: &mut App) {
    let wrap = Config::get(cx).word_wrap;
    cx.set_menus(vec![
        Menu::new("Sik").items([
            MenuItem::action("About Sik", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Keyboard Shortcuts", ShowShortcuts),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Sik", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Sik", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Open Folder on Server…", OpenRemoteFolder),
            MenuItem::action("Open Recent…", OpenRecent),
            MenuItem::separator(),
            MenuItem::action("New Task…", NewTask),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::separator(),
            MenuItem::action("Go to File…", OpenFileFinder),
            MenuItem::separator(),
            MenuItem::action("Save", Save),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close All Tabs", CloseAllTabs),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
            MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
            MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
            MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
            MenuItem::separator(),
            MenuItem::action("Find", input::Search),
            MenuItem::action("Replace", input::Replace),
            MenuItem::separator(),
            MenuItem::action("Find in Task", ShowSearch),
            MenuItem::separator(),
            MenuItem::action("Format Document", FormatDocument),
        ]),
        Menu::new("Selection").items([
            MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Add Next Occurrence", SelectNextOccurrence),
            MenuItem::action("Add Cursor Above", input::AddCursorAbove),
            MenuItem::action("Add Cursor Below", input::AddCursorBelow),
            MenuItem::separator(),
            MenuItem::action("Move Line Up", MoveLineUp),
            MenuItem::action("Move Line Down", MoveLineDown),
            MenuItem::action("Copy Line Up", DuplicateLineUp),
            MenuItem::action("Copy Line Down", DuplicateLineDown),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette…", OpenCommandPalette),
            MenuItem::separator(),
            MenuItem::action("Files", ShowFiles),
            MenuItem::action("Changes", ShowChanges),
            MenuItem::action("Search", ShowSearch),
            MenuItem::action("References", ShowReferences),
            MenuItem::action("Toggle Side Panel", ToggleSidePanel),
            MenuItem::separator(),
            MenuItem::action("Toggle Tasks Column", ToggleTasks),
            MenuItem::action("Toggle Terminals", ToggleTerminals),
            MenuItem::action("Maximize Terminals", MaximizeTerminals),
            MenuItem::separator(),
            MenuItem::action("Split Editor Right", SplitEditorRight),
            MenuItem::action("Split Editor Down", SplitEditorDown),
            MenuItem::separator(),
            MenuItem::action("Word Wrap", ToggleWordWrap).checked(wrap),
            MenuItem::action("Markdown: Toggle Source or Preview", ToggleMarkdownSource),
            MenuItem::action("Markdown: Open Preview to the Side", OpenPreviewToSide),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Back", NavigateBack),
            MenuItem::action("Forward", NavigateForward),
            MenuItem::separator(),
            MenuItem::action("Go to File…", OpenFileFinder),
            MenuItem::action("Go to Line…", GoToLine),
            MenuItem::action("Go to Definition", GoToDefinition),
            MenuItem::action("Find References", FindReferences),
            MenuItem::separator(),
            MenuItem::action("Next Result", NextResult),
            MenuItem::action("Previous Result", PrevResult),
            MenuItem::separator(),
            MenuItem::action("Find Task…", OpenTaskPicker),
            MenuItem::action("Previous Task", PreviousTask),
        ]),
        Menu::new("Terminal").items([
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::action("Split Terminal Right", SplitRight),
            MenuItem::action("Split Terminal Down", SplitDown),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PrevTab),
        ]),
        Menu::new("Help").items([MenuItem::action("Keyboard Shortcuts", ShowShortcuts)]),
    ]);
}

/// The Sik menu's actions, which belong to the app rather than to a view.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.minimize_window()).ok();
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });
    // The action arrives while the window is busy dispatching it, and it
    // can't be entered from there: show it right afterwards.
    cx.on_action(|_: &About, cx| {
        cx.defer(|cx| {
            if let Some(window) = cx.active_window() {
                window
                    .update(cx, |_, window, cx| {
                        let detail = format!("Version {}", env!("CARGO_PKG_VERSION"));
                        let _ = window.prompt(gpui_kit::PromptLevel::Info, "Sik", Some(&detail), &["OK"], cx);
                    })
                    .ok();
            }
        });
    });
}
