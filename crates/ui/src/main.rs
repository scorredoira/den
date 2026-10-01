#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod app;
mod app_menu;
mod assets;
mod config;
mod crash;
mod drag_drop;
mod diff;
mod editing;
mod changes;
mod commit_view;
mod completion;
mod file_tree;
mod language;
mod menu;
mod folder_picker;
mod picker;
mod search;
mod shortcuts;
mod signature;
mod splits;
mod terminals;
mod update;
mod workspace;

use std::path::{Path, PathBuf};

use gpui_kit::*;


actions!(
    app,
    [
        OpenSettings,
        Quit,
        Save,
        CloseTab,
        CloseAllTabs,
        NextTab,
        PrevTab,
        ToggleSidePanel,
        CollapseFileTree,
        ShowFiles,
        ShowChanges,
        ShowSearch,
        ShowReferences,
        GoToDefinition,
        GoToLine,
        ToggleWordWrap,
        FormatDocument,
        SplitEditorRight,
        SplitEditorDown,
        OpenPreviewToSide,
        About,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
        FindReferences,
        NavigateBack,
        NavigateForward,
        ToggleMarkdownSource,
        NewTerminal,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        OpenFileFinder,
        NextResult,
        PrevResult,
        ToggleTerminals,
        MaximizeTerminals,
        NewTask,
        OpenTaskPicker,
        OpenCommandPalette,
        ShowShortcuts,
        PreviousTask,
        ToggleTasks,
        OpenFolder,
        OpenRemoteFolder,
        OpenRecent,
    ]
);

/// `sik [folder | file]`: given a file, opens it in its repo (see
/// `proto::open_target`).
/// Opened from the Dock or the Finder with nothing to resume, there's no
/// folder: the welcome screen offers to open one.
fn main() {
    crash::install();
    let cwd = std::env::current_dir().expect("could not read the current folder");
    // Opened from the Dock or the Finder, the current folder is `/`; from the
    // Start menu, a shortcut or a double click, it's whatever they set (the
    // user's folder, the app's), not something to open: only a terminal
    // means it.
    let launched = cwd == Path::new("/") || !std::io::IsTerminal::is_terminal(&std::io::stdin());
    let cwd = match std::env::home_dir() {
        Some(home) if launched => home,
        _ => cwd,
    };
    // Old versions of macOS pass `-psn_…` to apps opened from the Finder.
    let arg = std::env::args_os()
        .nth(1)
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn"))
        .map(PathBuf::from);
    let arg = arg.map(|path| cwd.join(&path).canonicalize().unwrap_or(path));
    // With nothing to open, go back to the last workspace.
    let resume = arg.is_none();
    let (root, file) = match arg {
        Some(path) => {
            let (root, file) = proto::open_target(&path);
            (Some(root), file)
        }
        None if launched => (None, None),
        None => (Some(cwd), None),
    };

    // For `sik <path>` to start the app when it isn't running.
    if let (Ok(exe), Ok(file)) = (std::env::current_exe(), proto::app_file()) {
        let _ = std::fs::create_dir_all(file.parent().unwrap_or(&file));
        let _ = std::fs::write(file, exe.to_string_lossy().as_bytes());
    }

    // Terminals live in the agent; if it doesn't start, the app works without them.
    let agent = agent::connect()
        .inspect_err(|err| eprintln!("no agent: {err:#}"))
        .ok();

    let application = gpui_kit::application().with_assets(assets::Assets);
    // The Dock icon clicked with the window closed: it opens again.
    application.on_reopen(app::reopen);
    application.run(move |cx| {
        gpui_kit::init(cx);
        ui_term::init(cx);
        config::Config::init(cx);
        language::register();
        bind_keys(cx);
        app_menu::init(cx);
        app_menu::set(cx);

        update::init(cx);
        app::set_agent(agent.clone(), cx);
        if let Some(agent) = &agent {
            listen_for_open(agent, cx);
        }
        app::open_window(root.clone(), file.clone(), resume, cx);
        // With unsaved files, Cmd-Q asks before quitting. The action arrives
        // while a window is busy dispatching it, and it can't be entered
        // from there: ask right afterwards.
        cx.on_action(|_: &Quit, cx| cx.defer(app::quit));
        cx.activate(true);
    });
}

/// `sik <path>` in this machine's terminals, also with the window closed.
fn listen_for_open(agent: &std::sync::Arc<client::Client>, cx: &mut App) {
    let (tx, rx) = smol::channel::unbounded::<(PathBuf, Option<PathBuf>)>();
    agent.watch(move |event| {
        if let proto::Event::Open { root, file } = event {
            let _ = tx.try_send((root.clone(), file.clone()));
        }
    });
    cx.spawn(async move |cx| {
        while let Ok((root, file)) = rx.recv().await {
            cx.update(|cx| app::handle_open(app::LOCAL.into(), root, file, cx));
        }
    })
    .detach();
}

/// The app's shortcuts (see `shortcuts`) and the tree's, which only apply
/// while it has focus.
fn bind_keys(cx: &mut App) {
    cx.bind_keys(file_tree::keymap());
    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    shortcuts::apply(cx);
}
