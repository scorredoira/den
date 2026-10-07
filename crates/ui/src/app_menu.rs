//! The menu bar, as VS Code's: every entry is an action that already has a
//! shortcut, and the menu shows it next to it. Items whose action nobody
//! handles where the focus is (Undo in a terminal) show up disabled.
//!
//! macOS draws it at the top of the screen; on Windows and Linux nobody
//! does, so the window draws it in its title bar (`bar`), and what the Den
//! menu has there goes in File and Help.

use gpui_base::input;
use gpui_kit::component::{GlobalState, menu::AppMenuBar};
use gpui_kit::{App, Entity, Global, Menu, MenuItem, OsAction, SystemMenuType};

use crate::{
    config::Config,
    editing::{DuplicateLineDown, DuplicateLineUp, MoveLineDown, MoveLineUp, SelectNextOccurrence},
    *,
};

/// Sets the menus; again after something they show changes (Word Wrap's check).
pub fn set(cx: &mut App) {
    let config = Config::get(cx);
    let wrap = config.word_wrap;
    let bottom = config.layout.dock == crate::config::Dock::Bottom;
    let mac = cfg!(target_os = "macos");
    let mut menus = Vec::new();
    if mac {
        menus.push(Menu::new("Den").items([
            MenuItem::action("About Den", About),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Keyboard Shortcuts", OpenShortcutsGuide),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Den", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Den", Quit),
        ]));
    }
    menus.extend([
        Menu::new("File").items([
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Open Folder on Server…", OpenRemoteFolder),
            MenuItem::action("Add Server…", AddServer),
            MenuItem::action("Open Recent…", OpenRecent),
            MenuItem::separator(),
            MenuItem::action("New File…", NewFile),
            MenuItem::action("New Worktree…", NewTask),
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
            MenuItem::action("Find in Workspace", ShowSearch),
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
            MenuItem::action("History", ShowHistory),
            MenuItem::action("Search", ShowSearch),
            MenuItem::action("References", ShowReferences),
            MenuItem::action("Outline", ShowOutline),
            MenuItem::action("Toggle Side Panel", ToggleSidePanel),
            MenuItem::separator(),
            MenuItem::action("Toggle Workspaces", ToggleTasks),
            MenuItem::action("Toggle Terminals", ToggleTerminals),
            MenuItem::action("Maximize Terminals", MaximizeTerminals),
            MenuItem::action("Terminals Under the Code", MoveTerminals).checked(bottom),
            MenuItem::action("Toggle Notes", ToggleNotes),
            MenuItem::separator(),
            MenuItem::action("Reset Layout", ResetLayout),
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
            MenuItem::action("Go to Symbol in File…", GoToSymbol),
            MenuItem::action("Go to Symbol in Workspace…", GoToWorkspaceSymbol),
            MenuItem::action("Go to Definition", GoToDefinition),
            MenuItem::action("Find References", FindReferences),
            MenuItem::separator(),
            MenuItem::action("Next Result", NextResult),
            MenuItem::action("Previous Result", PrevResult),
            MenuItem::separator(),
            MenuItem::action("Find Workspace…", OpenTaskPicker),
            MenuItem::action("Switch Workspace", PreviousTask),
            MenuItem::action("Next Workspace with an Agent", NextActiveTask),
            MenuItem::action("Next Workspace", NextTask),
        ]),
        Menu::new("Debug").items([
            MenuItem::action("Start or Continue", DebugContinue),
            MenuItem::action("Stop", DebugStop),
            MenuItem::action("Restart", DebugRestart),
            MenuItem::action("Pause", DebugPause),
            MenuItem::separator(),
            MenuItem::action("Step Over", StepOver),
            MenuItem::action("Step Into", StepInto),
            MenuItem::action("Step Out", StepOut),
            MenuItem::action("Run to Cursor", RunToCursor),
            MenuItem::action("Set Next Statement", SetNextStatement),
            MenuItem::action("Add to Watch", AddToWatch),
            MenuItem::action("Evaluate in Console", EvaluateInConsole),
            MenuItem::separator(),
            MenuItem::action("Toggle Breakpoint", ToggleBreakpoint),
            MenuItem::action("Add Conditional Breakpoint", AddConditionalBreakpoint),
            MenuItem::action("Add Logpoint", AddLogpoint),
            MenuItem::action("Toggle Debug Panel", ToggleDebugPanel),
        ]),
        Menu::new("Terminal").items([
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::action("Split Terminal Right", SplitRight),
            MenuItem::action("Split Terminal Down", SplitDown),
            MenuItem::separator(),
            MenuItem::action("Find in Terminal", ui_term::Find),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PrevTab),
        ]),
        Menu::new("Help").items([
            MenuItem::action("Welcome", ShowWelcome),
            MenuItem::action("Keyboard Shortcuts", OpenShortcutsGuide),
        ]),
    ]);
    if !mac {
        for menu in &mut menus {
            match menu.name.as_ref() {
                "File" => menu.items.extend([
                    MenuItem::separator(),
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Exit", Quit),
                ]),
                "Help" => menu.items.extend([
                    MenuItem::separator(),
                    MenuItem::action("Check for Updates…", CheckForUpdates),
                    MenuItem::action("About Den", About),
                ]),
                _ => {}
            }
        }
    }
    cx.set_menus(menus);
    if !mac {
        if let Some(menus) = cx.get_menus() {
            GlobalState::global_mut(cx).set_app_menus(menus);
        }
        match cx.try_global::<Bar>().map(|bar| bar.0.clone()) {
            Some(bar) => bar.update(cx, |bar, cx| bar.reload(cx)),
            None => {
                let bar = AppMenuBar::new(cx);
                cx.set_global(Bar(bar));
            }
        }
    }
}

/// The menu bar the window draws, on Windows and Linux.
struct Bar(Entity<AppMenuBar>);

impl Global for Bar {}

/// The menu bar for the title bar; `None` on macOS, which draws its own.
pub fn bar(cx: &App) -> Option<Entity<AppMenuBar>> {
    cx.try_global::<Bar>().map(|bar| bar.0.clone())
}

/// The Den menu's actions, which belong to the app rather than to a view.
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
}
