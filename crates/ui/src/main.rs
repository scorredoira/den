#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agent;
mod app;
mod app_menu;
mod assets;
mod config;
mod crash;
mod debug;
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
mod guide;
mod picker;
mod search;
mod shortcuts;
mod signature;
mod symbol_picker;
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
        ShowHistory,
        ShowSearch,
        ShowReferences,
        GoToDefinition,
        GoToLine,
        GoToSymbol,
        GoToWorkspaceSymbol,
        ToggleWordWrap,
        FormatDocument,
        SplitEditorRight,
        SplitEditorDown,
        OpenPreviewToSide,
        About,
        CheckForUpdates,
        ShowWelcome,
        OpenShortcutsGuide,
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
        AddServer,
        OpenRecent,
        DebugContinue,
        DebugStop,
        DebugRestart,
        DebugPause,
        StepOver,
        StepInto,
        StepOut,
        ToggleBreakpoint,
        RunToCursor,
        SetNextStatement,
        ToggleDebugPanel,
        ResetLayout,
    ]
);

/// Shows a panel's icon on the activity bar, or hides it (View > Activity Bar).
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = app, no_json)]
pub struct ToggleActivityIcon(pub config::Panel);

/// `den [folder | file]`: given a file, opens it in its repo (see
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

    // For `den <path>` to start the app when it isn't running.
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

/// `den <path>` in this machine's terminals, also with the window closed.
/// Other `den` commands run in the window (see `app::commands`): while it's
/// closed, they're answered here. When the agent restarts, it listens on the
/// new one.
fn listen_for_open(agent: &std::sync::Arc<client::Client>, cx: &mut App) {
    enum Received {
        Open(PathBuf, Option<PathBuf>),
        Command(u64, Vec<String>),
        Lost,
    }
    let (tx, rx) = smol::channel::unbounded::<Received>();
    let lost = tx.clone();
    agent.on_disconnect(move || {
        let _ = lost.try_send(Received::Lost);
    });
    agent.watch(move |event| match event {
        proto::Event::Open { root, file } => {
            let _ = tx.try_send(Received::Open(root.clone(), file.clone()));
        }
        proto::Event::Command { command, args, .. } => {
            let _ = tx.try_send(Received::Command(*command, args.clone()));
        }
        _ => {}
    });
    let agent = agent.clone();
    cx.spawn(async move |cx| {
        while let Ok(received) = rx.recv().await {
            match received {
                Received::Open(root, file) => cx.update(|cx| app::handle_open(root, file, cx)),
                Received::Command(command, args) => {
                    if !cx.update(|cx| app::has_window(cx)) {
                        let result = match args.as_slice() {
                            // `den <path>` in a terminal: the window opens with it.
                            [open, root, file @ ..] if open == "open" && file.len() <= 1 => {
                                let (root, file) = (PathBuf::from(root), file.first().map(PathBuf::from));
                                cx.update(|cx| app::handle_open(root, file, cx));
                                Ok(String::new())
                            }
                            _ => Err("den's window is closed".to_string()),
                        };
                        agent.notify(proto::Request::CommandDone { command, result });
                    }
                }
                Received::Lost => break,
            }
        }
        let mut delay = std::time::Duration::from_secs(1);
        loop {
            cx.background_executor().timer(delay).await;
            let connected = cx.background_executor().spawn(async { crate::agent::connect() }).await;
            match connected {
                Ok(agent) => {
                    cx.update(|cx| {
                        app::set_agent(Some(agent.clone()), cx);
                        listen_for_open(&agent, cx);
                    });
                    return;
                }
                Err(err) => eprintln!("no agent: {err:#}"),
            }
            delay = (delay * 2).min(std::time::Duration::from_secs(30));
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
