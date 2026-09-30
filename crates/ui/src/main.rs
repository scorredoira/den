mod agent;
mod app;
mod assets;
mod config;
mod editing;
mod changes;
mod file_tree;
mod language;
mod menu;
mod folder_picker;
mod picker;
mod search;
mod shortcuts;
mod splits;
mod terminals;
mod workspace;

use std::path::{Path, PathBuf};

use gpui_kit::*;

use crate::app::Sik;

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
        ActivateTask1,
        ActivateTask2,
        ActivateTask3,
        ActivateTask4,
        ActivateTask5,
        ActivateTask6,
        ActivateTask7,
        ActivateTask8,
        ActivateTask9,
    ]
);

/// `editor [folder | file]`: given a file, opens it; the project is the
/// current folder if it contains the file, otherwise the file's folder.
fn main() {
    let cwd = std::env::current_dir().expect("could not read the current folder");
    // Opened from the Dock or the Finder, the current folder is `/`: home is better.
    let cwd = match std::env::home_dir() {
        Some(home) if cwd == Path::new("/") => home,
        _ => cwd,
    };
    // Old versions of macOS pass `-psn_…` to apps opened from the Finder.
    let arg = std::env::args_os()
        .nth(1)
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn"))
        .map(PathBuf::from);
    let arg = arg.map(|path| path.canonicalize().unwrap_or(path));
    // With nothing to open, go back to the last task.
    let resume = arg.is_none();
    let (root, file) = match arg {
        Some(path) if path.is_file() => {
            let root = if path.starts_with(&cwd) {
                cwd
            } else {
                path.parent().map(PathBuf::from).unwrap_or(cwd)
            };
            (root, Some(path))
        }
        Some(path) => (path, None),
        None => (cwd, None),
    };

    // Terminals live in the agent; if it doesn't start, the app works without them.
    let agent = agent::connect()
        .inspect_err(|err| eprintln!("no agent: {err:#}"))
        .ok();

    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            ui_term::init(cx);
            config::Config::init(cx);
            language::register();
            bind_keys(cx);

            let title = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            // The app draws the title bar itself (`TitleBar`), in the theme's color.
            let options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..gpui_kit::component::TitleBar::title_bar_options()
                }),
                window_bounds: Some(window_bounds(cx)),
                ..gpui_kit::component::TitleBar::window_options()
            };
            let (window, sik) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| Sik::new(root.clone(), file.clone(), resume, agent.clone(), window, cx))
            })
            .expect("could not open the window");
            // With unsaved files, Cmd-Q asks before quitting.
            let sik = sik.downgrade();
            // The action arrives while the window is busy dispatching it, and
            // it can't be entered from there: ask right afterwards.
            cx.on_action(move |_: &Quit, cx| {
                let sik = sik.clone();
                cx.defer(move |cx| {
                    let ready = window
                        .update(cx, |_, window, cx| sik.update(cx, |sik, cx| sik.confirm_quit(window, cx)))
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(true);
                    if ready {
                        cx.quit();
                    }
                });
            });
            cx.activate(true);
        });
}

/// The window opens where it was closed; the first time, covering almost the
/// whole screen, which is the size people work at.
fn window_bounds(cx: &App) -> WindowBounds {
    if let Some(saved) = config::Config::get(cx).window {
        return saved.bounds();
    }
    let screen = cx
        .primary_display()
        .map(|display| display.bounds().size)
        .unwrap_or(size(px(1440.), px(900.)));
    WindowBounds::centered(size(screen.width * 0.92, screen.height * 0.9), cx)
}

/// The app's shortcuts (see `shortcuts`) and the tree's, which only apply
/// while it has focus.
fn bind_keys(cx: &mut App) {
    cx.bind_keys(file_tree::keymap());
    shortcuts::apply(cx);
}
