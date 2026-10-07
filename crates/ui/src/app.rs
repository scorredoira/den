//! The den window: the open folder's workspace and, optionally, the tasks
//! column, grouped by server. Any folder can be opened; a task is a folder
//! that is a worktree of a known repo. Each open folder has its own
//! workspace, which is kept when switching from one to another. With none
//! open, a welcome screen offers to open one.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Weak},
    time::Duration,
};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme, ThemeMode, TitleBar, h_flex, h_resizable,
    button::{Button, ButtonVariants as _},
    highlighter::SyntaxColors,
    input::{Input, InputEvent, InputState},
    kbd::Kbd,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu},
    resizable_panel,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use crate::menu::PanelItems as _;
use proto::{Event, GitOp, Request, Response, TaskInfo};

use crate::{
    About, CheckForUpdates, NewTask, OpenCommandPalette, OpenShortcutsGuide, OpenFolder, OpenRecent, OpenRemoteFolder, OpenSettings, OpenTaskPicker,
    AddServer, NextActiveTask, NextTask, PreviousTask, ResetLayout, ShowShortcuts, ShowWelcome, ToggleTasks,
    config::{self, Config, DiffLayout, HostConfig, Panel, Place, SavedTask, SavedWindow, TextArea, ThemeChoice, UiText},
    menu,
    folder_picker::{FolderPicker, FolderPickerEvent},
    picker::{Picker, PickerEvent},
    shortcuts::{self, SHORTCUTS},
    workspace::{ACTIVITY_WIDTH, Badge, Item, OnActivity, TaskBadges, Workspace, WorkspacesPanel, activity_bar},
};

mod about;
mod agents;
mod commands;
mod confirm;
mod settings;
mod switcher;
mod theme;
pub(crate) use theme::selected_row;
mod welcome;

/// How often the task list is re-read (worktrees created elsewhere).
const REFRESH: Duration = Duration::from_secs(5);

/// Name of this machine in the tasks column.
pub const LOCAL: &str = "local";

/// The last choice of Open Folder on Server: a server not connected yet.
const ADD_SERVER: &str = "Add Server…";

/// What a server shows while connecting, unless it's doing something longer.
const CONNECTING: &str = "connecting…";

/// How many folders Open Recent remembers.
const RECENT: usize = 20;

/// Commands the Command Palette remembers (VS Code's default).
const RECENT_COMMANDS: usize = 50;

/// A task on a server.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
struct TaskKey {
    host: SharedString,
    path: PathBuf,
}

impl TaskKey {
    /// Key in `config.json`: the path locally, `server:path` on a server.
    fn config(&self) -> String {
        if self.host == LOCAL {
            self.path.to_string_lossy().into_owned()
        } else {
            format!("{}:{}", self.host, self.path.display())
        }
    }
}

enum HostStatus {
    /// What it's doing: connecting, or uploading the agent first.
    Connecting(&'static str),
    Connected,
    Failed(SharedString),
}

struct Host {
    name: SharedString,
    /// `None` on this machine.
    destination: Option<String>,
    client: Option<Arc<Client>>,
    status: HostStatus,
    tasks: Vec<TaskInfo>,
    /// Open folders that aren't one of its tasks.
    loose: Vec<TaskInfo>,
    repos: Vec<PathBuf>,
    /// Goes up with each new connection attempt: an earlier one still
    /// retrying notices and gives up.
    generation: u64,
}

impl Host {
    /// A server, not connected yet.
    fn remote(name: SharedString, destination: String) -> Self {
        Host {
            name,
            destination: Some(destination),
            client: None,
            status: HostStatus::Connecting(CONNECTING),
            tasks: Vec::new(),
            loose: Vec::new(),
            repos: Vec::new(),
            generation: 0,
        }
    }
}

/// A window opened with `den -s`, on a server, or with `den -n`, on a
/// server or this machine.
struct ServerWindow {
    /// The server, or `LOCAL`.
    name: SharedString,
    /// What to open once it connects: a folder (or a file) or, with no
    /// path, the home folder; taken then.
    start: Option<Option<PathBuf>>,
}

/// A new worktree being named, in a row under its repo.
struct NewTaskInput {
    host: SharedString,
    repo: PathBuf,
    input: Entity<InputState>,
    busy: bool,
    error: Option<SharedString>,
    _subscription: Subscription,
}

/// A task being dragged to reorder (within its server).
#[derive(Clone)]
struct TaskDrag {
    key: TaskKey,
    label: SharedString,
}

struct DragPreview(SharedString);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_ui(cx)
            .rounded(cx.theme().radius)
            .bg(cx.theme().sidebar_accent)
            .text_color(cx.theme().sidebar_foreground)
            .child(self.0.clone())
    }
}

/// The windows, and the local agent the main one talks to (kept for opening
/// it again after it's closed: the app goes on without it on macOS).
#[derive(Default)]
struct Main {
    /// The main window first.
    windows: Vec<OpenWindow>,
    agent: Option<Arc<Client>>,
    /// The local tasks, listed while the app started, for the first window
    /// to open on.
    start_tasks: Option<Vec<TaskInfo>>,
}

impl Global for Main {}

struct OpenWindow {
    handle: AnyWindowHandle,
    den: WeakEntity<Den>,
    /// The server of a window opened with `den -s` or `den -n` (`LOCAL` on
    /// this machine); none in the main one, whose workspaces are remembered.
    server: Option<SharedString>,
}

/// The local agent, for the main window.
pub fn set_agent(agent: Option<Arc<Client>>, cx: &mut App) {
    cx.default_global::<Main>().agent = agent;
}

pub fn set_start_tasks(tasks: Option<Vec<TaskInfo>>, cx: &mut App) {
    cx.default_global::<Main>().start_tasks = tasks;
}

/// Whether `client` is the app's connection to the local agent.
fn is_app_agent(client: &Arc<Client>, cx: &App) -> bool {
    cx.try_global::<Main>().and_then(|main| main.agent.as_ref()).is_some_and(|agent| Arc::ptr_eq(agent, client))
}

/// The open windows, the main one first.
fn windows(cx: &App) -> Vec<(AnyWindowHandle, Entity<Den>)> {
    let Some(main) = cx.try_global::<Main>() else {
        return Vec::new();
    };
    main.windows.iter().filter_map(|open| Some((open.handle, open.den.upgrade()?))).collect()
}

/// The Den menu's own (About, Check for Updates, Settings, Keyboard
/// Shortcuts): the app's, not a view's, so the menu has them even when
/// nothing in a window has the keyboard. They act on the window in front,
/// or on the first one.
pub fn register_app_actions(cx: &mut App) {
    // Deferred: from a key in a window, that window is busy dispatching it.
    fn in_front(cx: &mut App, act: impl FnOnce(&mut Den, &mut Window, &mut Context<Den>) + 'static) {
        cx.defer(move |cx| {
            let open = windows(cx);
            let active = cx.active_window();
            let Some((handle, den)) = open.iter().find(|(handle, _)| Some(*handle) == active).or(open.first()).cloned() else {
                return;
            };
            handle.update(cx, |_, window, cx| den.update(cx, |den, cx| act(den, window, cx))).ok();
        });
    }
    cx.on_action(|_: &About, cx| in_front(cx, |den, window, cx| den.open_about(window, cx)));
    cx.on_action(|_: &CheckForUpdates, cx| in_front(cx, |den, window, cx| den.check_for_updates(window, cx)));
    cx.on_action(|_: &OpenSettings, cx| in_front(cx, |den, window, cx| den.open_settings(window, cx)));
    cx.on_action(|_: &OpenShortcutsGuide, cx| in_front(cx, |den, window, cx| den.open_guide(window, cx)));
}

/// The main window, while it's open.
fn main_window(cx: &App) -> Option<(AnyWindowHandle, Entity<Den>)> {
    let main = cx.try_global::<Main>()?.windows.iter().find(|open| open.server.is_none())?;
    Some((main.handle, main.den.upgrade()?))
}

/// Where `den <path>` from a terminal on `host` outside den opens, when
/// several windows hear it: the first window connected to `host`.
fn opens_from_host(host: &str, den: &WeakEntity<Den>, cx: &App) -> bool {
    windows(cx)
        .into_iter()
        .find(|(_, other)| other.read(cx).client(host).is_some())
        .is_some_and(|(_, other)| other.entity_id() == den.entity_id())
}

/// Opens the window with `root` and `file` in it; with no `root`, the last
/// workspace if `resume`, or the welcome screen.
pub fn open_window(root: Option<PathBuf>, file: Option<PathBuf>, resume: bool, cx: &mut App) {
    let agent = cx.default_global::<Main>().agent.clone();
    let title = match &root {
        Some(root) => folder_name(root),
        None => "den".to_string(),
    };
    // The app draws the title bar itself (`TitleBar`), in the theme's color.
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitleBar::title_bar_options()
        }),
        window_bounds: Some(window_bounds(cx)),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Den::new(root.clone(), file.clone(), resume, agent, window, cx))
    });
    match opened {
        Ok((handle, den)) => {
            let main = &mut cx.default_global::<Main>().windows;
            main.retain(|open| open.den.upgrade().is_some());
            main.insert(0, OpenWindow { handle, den: den.downgrade(), server: None });
        }
        Err(err) => eprintln!("could not open the window: {err:#}"),
    }
}

/// The workspace `window` shows: what a menu in it acts on.
pub(crate) fn window_workspace(window: &Window, cx: &App) -> Option<Entity<Workspace>> {
    let handle = window.window_handle();
    let open = cx.try_global::<Main>()?.windows.iter().find(|open| open.handle == handle)?;
    open.den.upgrade()?.read(cx).active_workspace()
}

/// `den -s <server> [<path>]`: `path` on `server` in a window of its own
/// (relative to the home folder there), or that home folder; in the window
/// already open on that server, if there's one.
pub fn open_server_window(destination: String, path: Option<PathBuf>, cx: &mut App) {
    let name = server_name(&destination, cx);
    let open = cx.try_global::<Main>().and_then(|main| {
        main.windows.iter().find(|open| open.server.as_ref() == Some(&name)).and_then(|open| Some((open.handle, open.den.upgrade()?)))
    });
    if let Some((handle, den)) = open {
        handle
            .update(cx, |_, window, cx| {
                den.update(cx, |den, cx| den.open_start(path, window, cx));
                window.activate_window();
            })
            .ok();
        cx.activate(true);
        return;
    }
    open_host_window(name, cx, |window, cx| Den::for_server(destination, path, window, cx));
}

/// `den -n <path>` from a terminal of `host`: `root`, with `file` open in
/// it, in a window of its own; in the window it's open in, if it is.
pub fn open_new_window(host: SharedString, destination: Option<String>, root: PathBuf, file: Option<PathBuf>, cx: &mut App) {
    open_new_window_except(host, destination, root, file, None, cx)
}

/// `open_new_window`, but never in the window of `except`: a workspace's
/// Open in New Window, asked from the window it's open in.
fn open_new_window_except(
    host: SharedString,
    destination: Option<String>,
    root: PathBuf,
    file: Option<PathBuf>,
    except: Option<EntityId>,
    cx: &mut App,
) {
    let open = windows(cx).into_iter().find(|(_, den)| {
        if Some(den.entity_id()) == except {
            return false;
        }
        let den = den.read(cx);
        let key = TaskKey { host: host.clone(), path: den.workspace_containing(&host, &root) };
        den.workspaces.contains_key(&key)
    });
    if let Some((handle, den)) = open {
        handle
            .update(cx, |_, window, cx| {
                den.update(cx, |den, cx| den.open_from_terminal(host, root, file, window, cx));
                window.activate_window();
            })
            .ok();
        cx.activate(true);
        return;
    }
    let path = file.unwrap_or(root);
    match destination {
        Some(destination) => {
            let name = server_name(&destination, cx);
            open_host_window(name, cx, |window, cx| Den::for_server(destination, Some(path), window, cx));
        }
        None => open_host_window(LOCAL.into(), cx, |window, cx| Den::for_local(path, window, cx)),
    }
}

/// Opens a window of `den -s` or `den -n`, on the server called `name`.
fn open_host_window(name: SharedString, cx: &mut App, build: impl FnOnce(&mut Window, &mut Context<Den>) -> Den) {
    let title = match name.as_ref() {
        LOCAL => "den".into(),
        _ => name.clone(),
    };
    // Over the main window, a little down and to the right.
    let bounds = match window_bounds(cx) {
        WindowBounds::Windowed(bounds) => WindowBounds::Windowed(Bounds::new(bounds.origin + point(px(28.), px(28.)), bounds.size)),
        other => other,
    };
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitleBar::title_bar_options()
        }),
        window_bounds: Some(bounds),
        ..TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| cx.new(|cx| build(window, cx)));
    match opened {
        Ok((handle, den)) => {
            let windows = &mut cx.default_global::<Main>().windows;
            windows.retain(|open| open.den.upgrade().is_some());
            windows.push(OpenWindow { handle, den: den.downgrade(), server: Some(name) });
        }
        Err(err) => eprintln!("could not open the window: {err:#}"),
    }
    cx.activate(true);
}

/// What a server is called: the name it was added with, if it was.
/// Whether an agent's error says a path isn't there (ENOENT, or Windows'
/// file or path not found), rather than that it couldn't answer or read it.
fn not_found(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    // 3 is Windows' path not found; canonicalize never gives Unix's (ESRCH).
    text.contains("(os error 2)") || text.contains("(os error 3)")
}

fn server_name(destination: &str, cx: &App) -> SharedString {
    Config::get(cx)
        .hosts
        .iter()
        .find(|host| host.destination == destination)
        .map(|host| host.name.clone())
        .unwrap_or_else(|| destination.to_string())
        .into()
}

/// Clicking the Dock icon with the main window closed opens it again.
pub fn reopen(cx: &mut App) {
    if main_window(cx).is_none() {
        open_window(None, None, true, cx);
    }
}

/// A `den` command from this machine, heard by the app on `agent`: the main
/// window runs it. Its own connection to the agent is another after the
/// agent restarts, and hears its own: this one's it's handed. Returns
/// whether the main window is open.
pub fn run_local_command(
    agent: &Arc<Client>,
    id: u64,
    args: Vec<String>,
    cwd: PathBuf,
    term: Option<proto::TermId>,
    group: Option<String>,
    cx: &mut App,
) -> bool {
    let Some((handle, den)) = main_window(cx) else {
        return false;
    };
    let own = den.read(cx).client(LOCAL).is_some_and(|client| Arc::ptr_eq(&client, agent));
    if !own {
        let agent = agent.clone();
        handle
            .update(cx, |_, window, cx| {
                den.update(cx, |den, cx| {
                    let command = commands::Command { id, args, cwd, term, group };
                    den.run_command(LOCAL.into(), agent, command, window, cx)
                })
            })
            .ok();
    }
    true
}

/// `den <path>` in a terminal of this machine: `root` as a workspace, with
/// `file` open in it, and the main window to the front (opened if it was
/// closed).
pub fn handle_open(root: PathBuf, file: Option<PathBuf>, cx: &mut App) {
    match main_window(cx) {
        Some((handle, den)) => {
            handle
                .update(cx, |_, window, cx| {
                    den.update(cx, |den, cx| den.open_from_terminal(LOCAL.into(), root, file, window, cx));
                    window.activate_window();
                })
                .ok();
        }
        None => open_window(Some(root), file, false, cx),
    }
    cx.activate(true);
}

/// Cmd-Q: quits once there are no unsaved files in any window, or they're
/// saved. A window with some asks; once it's done, the next one.
pub fn quit(cx: &mut App) {
    for (handle, den) in windows(cx) {
        let ready = handle
            .update(cx, |_, window, cx| {
                let ready = den.update(cx, |den, cx| den.confirm_quit(Closing::App, window, cx));
                if !ready {
                    window.activate_window();
                }
                ready
            })
            .unwrap_or(true);
        if !ready {
            return;
        }
    }
    crate::update::relaunch_if_restarting(cx);
    cx.quit();
}

/// What the dialog about unsaved files is about to do.
#[derive(Clone, Copy, PartialEq)]
pub enum Closing {
    /// Quit den (Cmd-Q).
    App,
    /// Close the window: the app goes on.
    Window,
}

/// The window opens where it was closed; the first time, covering almost
/// the whole screen, which is the size people work at.
fn window_bounds(cx: &App) -> WindowBounds {
    if let Some(saved) = Config::get(cx).window {
        return saved.bounds();
    }
    let screen = cx
        .primary_display()
        .map(|display| display.bounds().size)
        .unwrap_or(size(px(1440.), px(900.)));
    WindowBounds::centered(size(screen.width * 0.92, screen.height * 0.9), cx)
}

pub struct Den {
    hosts: Vec<Host>,
    /// Opened with `den -s`: what's open in it isn't remembered.
    server: Option<ServerWindow>,
    active: Option<TaskKey>,
    workspaces: HashMap<TaskKey, Entity<Workspace>>,
    /// The terminals running a coding agent on each server: what each
    /// workspace is doing.
    agents: HashMap<SharedString, Vec<proto::AgentInfo>>,
    /// Agents that finished while their workspace wasn't in front.
    agents_attention: HashSet<(SharedString, proto::TermId)>,
    new_task: Option<NewTaskInput>,
    /// Worktree whose deletion is being confirmed, in a dialog, and what
    /// it would lose (unset while git is asked).
    confirm_remove: Option<(TaskKey, FocusHandle, Option<confirm::AtRisk>)>,
    /// Worktree git refused to delete, with why, asking whether to force it.
    confirm_force_remove: Option<(TaskKey, FocusHandle, SharedString)>,
    /// Server whose agent is about to be restarted, confirming in a dialog.
    confirm_restart: Option<(SharedString, FocusHandle)>,
    /// The update about to be restarted into, confirming in a dialog.
    confirm_update: Option<(SharedString, FocusHandle)>,
    removing: HashSet<TaskKey>,
    /// Last error from a column action, with the task it affects.
    error: Option<(TaskKey, SharedString)>,
    /// File to open in the first workspace (`den file`).
    open_file: Option<PathBuf>,
    /// Last task from the previous session, on a server not yet connected:
    /// it's entered on connecting (unless another was opened first).
    pending_last: Option<TaskKey>,
    /// The task before the active one, to go back to when it closes.
    previous: Option<TaskKey>,
    /// The server just added, until a folder is opened on it (or not, and
    /// it goes).
    adding_host: Option<SharedString>,
    /// Cmd-Alt-E held: the workspaces to go through.
    switcher: Option<switcher::Switcher>,
    /// What Cmd-E just entered, shown while Cmd is down.
    notice: Option<switcher::Notice>,
    /// Cmd-K: jump to a task by name.
    task_picker: Option<(Entity<Picker>, Subscription)>,
    /// Cmd-Shift-P and F1: run any command, with its shortcut beside it.
    command_palette: Option<(Entity<Picker>, Subscription)>,
    /// Cmd-Shift-O: a folder opened before.
    recent_picker: Option<(Entity<Picker>, Subscription)>,
    /// Add Server: one from `~/.ssh/config`, or typed.
    host_picker: Option<(Entity<Picker>, Subscription)>,
    /// A folder on a server, to open (or to make one there).
    folder_picker: Option<(Entity<FolderPicker>, Subscription)>,
    /// Quit dialog with unsaved files (its focus, for Esc and Enter), and
    /// whether it closes the window or quits.
    quit_confirm: Option<(FocusHandle, Closing)>,
    /// Closing anyway, without saving: the window doesn't ask again.
    discarded: bool,
    /// The window it's in.
    handle: AnyWindowHandle,
    /// Saving everything before quitting.
    quit_saving: bool,
    /// About, if open (its focus, for Esc).
    about: Option<FocusHandle>,
    /// The shortcuts guide, shown in place of the welcome screen while no
    /// workspace is open (with one, it's a tab of its own).
    guide: Option<Entity<gpui_kit::component::text::TextViewState>>,
    /// Tasks column and welcome, while no workspace is open.
    split: config::Split,
    /// The tasks column, drawn by the workspace where its panel is placed.
    workspaces_panel: Entity<WorkspacesPanel>,
    /// The agents panel, drawn by the workspace where it's placed.
    agents_panel: Entity<WorkspacesPanel>,
    /// Settings, if open.
    settings: Option<settings::Settings>,
    focus_handle: FocusHandle,
    /// Each host's events come to this window while its entry lives (the
    /// local connection outlives the window).
    watches: HashMap<SharedString, client::Watch>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Den {
    /// A window with `hosts`, connecting to the servers among them; with
    /// `server`, one opened with `den -s` (see `for_server`).
    fn with_hosts(
        hosts: Vec<Host>,
        server: Option<ServerWindow>,
        open_file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            if Config::get(cx).theme == ThemeChoice::System {
                Theme::sync_system_appearance(Some(window), cx);
                Self::apply_font_sizes(cx);
            }
        });
        let mut this = Self {
            hosts,
            server,
            active: None,
            workspaces: HashMap::new(),
            agents: HashMap::new(),
            agents_attention: HashSet::new(),
            new_task: None,
            confirm_remove: None,
            confirm_force_remove: None,
            confirm_restart: None,
            confirm_update: None,
            removing: HashSet::new(),
            error: None,
            open_file,
            pending_last: None,
            previous: None,
            switcher: None,
            notice: None,
            adding_host: None,
            task_picker: None,
            command_palette: None,
            recent_picker: None,
            host_picker: None,
            folder_picker: None,
            quit_confirm: None,
            discarded: false,
            handle: window.window_handle(),
            quit_saving: false,
            about: None,
            guide: None,
            split: config::Split::new(cx),
            workspaces_panel: {
                let den = cx.entity().downgrade();
                let actions = den.clone();
                cx.new(|_| {
                    WorkspacesPanel::new(move |_, cx| {
                        den.update(cx, |den, cx| den.render_column(false, cx).into_any_element())
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                    .with_actions(move |_, _| tasks_add_button(&actions).into_any_element())
                })
            },
            agents_panel: {
                let den = cx.entity().downgrade();
                cx.new(|_| {
                    WorkspacesPanel::new(move |_, cx| {
                        den.update(cx, |den, cx| den.render_agents(cx).into_any_element())
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                })
            },
            settings: None,
            focus_handle: cx.focus_handle(),
            watches: HashMap::new(),
            _tasks: Vec::new(),
            _subscriptions: vec![appearance, {
                // The switcher's keys come before any shortcut (Cmd-Shift-E
                // is the files' too), wherever the focus is.
                let den = cx.entity().downgrade();
                cx.intercept_keystrokes(move |event, _, cx| {
                    if den.update(cx, |this, cx| this.switcher_key(&event.keystroke, cx)).unwrap_or(false) {
                        cx.stop_propagation();
                    }
                })
            }],
        };
        Self::install_theme(cx);
        this.apply_theme(window, cx);

        let den = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            den.update(cx, |den, cx| den.discarded || den.confirm_quit(Closing::Window, window, cx)).unwrap_or(true)
        });
        // Its connections to servers go with it (this machine's is the
        // app's): a closed window's must not be sent the servers' `den`
        // commands.
        cx.on_release(move |this, _| {
            for host in this.hosts.iter().filter(|host| host.destination.is_some() || this.server.is_some()) {
                if let Some(client) = &host.client {
                    client.disconnect();
                }
            }
        })
        .detach();

        // A window of `den -n` on this machine has its own connection to the
        // agent: the `den` commands of its terminals come to it.
        let own = |host: &Host| host.destination.is_some() || this.server.is_some();
        for name in this.hosts.iter().filter(|host| own(host)).map(|host| host.name.clone()).collect::<Vec<_>>() {
            this.connect(name, window, cx);
        }

        let refresh = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update_in(cx, |this, window, cx| this.refresh_all(window, cx)).is_err() {
                    break;
                }
            }
        });
        this._tasks.push(refresh);
        this
    }

    /// `den -s <server> [<path>]`: a window with only that server, which
    /// opens `path` (or the home folder) once connected. Nothing open in
    /// it is remembered (see `keep`), and the workspaces column starts
    /// hidden.
    pub fn for_server(destination: String, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = server_name(&destination, cx);
        let host = Host::remote(name.clone(), destination);
        let server = ServerWindow { name, start: Some(path) };
        let this = Self::with_hosts(vec![host], Some(server), None, window, cx);
        this.focus_handle.focus(window, cx);
        this
    }

    /// `den -n <path>` on this machine: like `for_server`, a window with only
    /// this machine that opens `path` once connected.
    pub fn for_local(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let host = Host {
            name: LOCAL.into(),
            destination: None,
            client: None,
            status: HostStatus::Connecting(CONNECTING),
            tasks: Vec::new(),
            loose: Vec::new(),
            repos: Vec::new(),
            generation: 0,
        };
        let server = ServerWindow { name: LOCAL.into(), start: Some(Some(path)) };
        let this = Self::with_hosts(vec![host], Some(server), None, window, cx);
        this.focus_handle.focus(window, cx);
        this
    }

    pub fn new(
        root: Option<PathBuf>,
        open_file: Option<PathBuf>,
        resume: bool,
        client: Option<Arc<Client>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let local = Host {
            name: LOCAL.into(),
            destination: None,
            status: if client.is_some() {
                HostStatus::Connected
            } else {
                HostStatus::Failed("no agent".into())
            },
            client: client.clone(),
            tasks: Vec::new(),
            loose: Vec::new(),
            repos: Vec::new(),
            generation: 0,
        };
        let remotes: Vec<Host> = Config::get(cx)
            .hosts
            .iter()
            .map(|host| Host::remote(host.name.clone().into(), host.destination.clone()))
            .collect();
        let mut this = Self::with_hosts(std::iter::once(local).chain(remotes).collect(), None, open_file, window, cx);
        // Where it is goes on to the next session.
        let bounds = cx.observe_window_bounds(window, |_, window, cx| {
            let saved = SavedWindow::from_bounds(window.window_bounds());
            Config::update_quietly(cx, |config| config.window = saved);
        });
        this._subscriptions.push(bounds);

        let Some(client) = client else {
            match root {
                Some(root) => this.activate(TaskKey { host: LOCAL.into(), path: root }, window, cx),
                None => this.focus_handle.focus(window, cx),
            }
            return this;
        };
        this.watch_host(LOCAL.into(), client.clone(), window, cx);
        this.track(LOCAL.into(), &client, window, cx);

        // The tasks listed while the app started: it opens on them now, in
        // the first frame. Otherwise it reads them first.
        if let Some(tasks) = cx.default_global::<Main>().start_tasks.take() {
            this.enter_start(tasks, root, resume, window, cx);
            return this;
        }
        let startup = cx.spawn_in(window, async move |this, cx| {
            let tasks = list_tasks(&client).await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| this.enter_start(tasks, root, resume, window, cx)).ok();
        });
        this._tasks.push(startup);
        this
    }

    /// With the local tasks known: enters the one containing the folder, if
    /// any; otherwise the folder on its own, or the last one visited.
    fn enter_start(&mut self, tasks: Vec<TaskInfo>, root: Option<PathBuf>, resume: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.hosts[0].tasks = tasks;
        let task = root.as_ref().and_then(|root| {
            self.hosts[0]
                .tasks
                .iter()
                .filter(|task| root.starts_with(&task.path))
                .max_by_key(|task| task.path.components().count())
                .map(|task| task.path.clone())
        });
        // No folder at launch, and outside a task: go to the last one.
        let last = resume.then(|| Config::get(cx).last.clone()).flatten();
        let key = match (task, last) {
            (Some(path), _) => TaskKey { host: LOCAL.into(), path },
            (None, Some(last)) if last.host != LOCAL && self.host(&last.host).is_some() => {
                let key = TaskKey { host: last.host.into(), path: last.path };
                if self.client(&key.host).is_some() {
                    self.activate(key, window, cx);
                } else {
                    self.pending_last = Some(key);
                }
                return;
            }
            (None, Some(last)) if last.host == LOCAL && last.path.is_dir() => {
                TaskKey { host: LOCAL.into(), path: last.path }
            }
            (None, _) => match root {
                Some(root) => TaskKey { host: LOCAL.into(), path: root },
                // Nothing to open: the welcome screen.
                None => {
                    self.focus_handle.focus(window, cx);
                    return;
                }
            },
        };
        self.activate(key, window, cx);
    }

    /// The window's title with no workspace open.
    fn title(&self) -> String {
        match &self.server {
            Some(server) if server.name != LOCAL => server.name.to_string(),
            _ => "den".to_string(),
        }
    }

    fn host(&self, name: &str) -> Option<&Host> {
        self.hosts.iter().find(|host| host.name == name)
    }

    fn host_mut(&mut self, name: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|host| host.name == name)
    }

    fn client(&self, host: &str) -> Option<Arc<Client>> {
        self.host(host).and_then(|host| host.client.clone())
    }

    /// Connects to a server (over SSH, installing the agent if needed; or to
    /// the local agent), retrying with increasing backoff until it succeeds.
    fn connect(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.host_mut(&name) else {
            return;
        };
        host.generation += 1;
        let generation = host.generation;
        let destination = host.destination.clone();
        host.status = HostStatus::Connecting(CONNECTING);
        cx.notify();
        let agents = crate::agent::agents_dir().unwrap_or_default();
        // Uploading the agent takes a while: the column says so.
        let (step_tx, step_rx) = smol::channel::unbounded::<&'static str>();
        let step_name = name.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(step) = step_rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if let Some(host) = this.host_mut(&step_name)
                            && host.generation == generation
                            && matches!(host.status, HostStatus::Connecting(_))
                        {
                            host.status = HostStatus::Connecting(step);
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let mut delay = Duration::ZERO;
            loop {
                if !delay.is_zero() {
                    cx.background_executor().timer(delay).await;
                }
                let current = this
                    .update(cx, |this, _| this.host(&name).is_some_and(|host| host.generation == generation))
                    .unwrap_or(false);
                if !current {
                    return;
                }
                let (destination, agents, step_tx) = (destination.clone(), agents.clone(), step_tx.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        match destination {
                            Some(destination) => {
                                client::connect_ssh(&destination, &agents, &|step| {
                                    let _ = step_tx.try_send(step);
                                })
                            }
                            None => crate::agent::connect(),
                        }
                    })
                    .await;
                match result {
                    Ok(client) => {
                        this.update_in(cx, |this, window, cx| this.connected(name.clone(), client, window, cx))
                            .ok();
                        return;
                    }
                    Err(err) => {
                        let alive = this
                            .update(cx, |this, cx| {
                                if let Some(host) = this.host_mut(&name) {
                                    host.status = HostStatus::Failed(format!("{err:#}").into());
                                }
                                cx.notify();
                            })
                            .is_ok();
                        if !alive {
                            return;
                        }
                        delay = (delay * 2).clamp(Duration::from_secs(1), Duration::from_secs(30));
                    }
                }
            }
        })
        .detach();
    }

    /// There's a connection (the first, or after losing it): the server's things move to it.
    fn connected(&mut self, name: SharedString, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.host_mut(&name) else {
            return;
        };
        let previous = host.client.replace(client.clone());
        host.status = HostStatus::Connected;
        // Reconnect while connected: the previous connection goes (unless
        // it's the app's, which this machine's `den` commands come to).
        if let Some(previous) = previous
            && !Arc::ptr_eq(&previous, &client)
            && !is_app_agent(&previous, cx)
        {
            previous.disconnect();
        }
        // The agent wasn't up when den started: now that it is, `den <path>`
        // from other terminals comes to the app (and a window opened from
        // the Dock connects to it).
        if name == LOCAL && self.server.is_none() && cx.try_global::<Main>().is_none_or(|main| main.agent.is_none()) {
            set_agent(Some(client.clone()), cx);
            crate::listen_for_open(&client, cx);
        }
        self.watch_host(name.clone(), client.clone(), window, cx);
        self.track(name.clone(), &client, window, cx);
        let workspaces: Vec<Entity<Workspace>> = self
            .workspaces
            .iter()
            .filter(|(key, _)| key.host == name)
            .map(|(_, workspace)| workspace.clone())
            .collect();
        for workspace in workspaces {
            workspace.update(cx, |workspace, cx| workspace.set_client(client.clone(), window, cx));
        }
        if self.active.is_none()
            && let Some(key) = self.pending_last.take_if(|key| key.host == name)
        {
            self.activate(key, window, cx);
        }
        // `den -s`: what it was opened with.
        if let Some(server) = &mut self.server
            && server.name == name
            && let Some(path) = server.start.take()
        {
            self.open_start(path, window, cx);
        }
        // Just added: a folder to open on it.
        if self.adding_host.as_ref() == Some(&name) {
            self.open_folder_picker(name.clone(), window, cx);
        }
        self.refresh_all(window, cx);
        cx.notify();
    }

    /// Finds out when the connection to the server is lost.
    fn track(&mut self, name: SharedString, client: &Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = smol::channel::bounded::<()>(1);
        client.on_disconnect(move || {
            let _ = tx.try_send(());
        });
        let client = Arc::downgrade(client);
        cx.spawn_in(window, async move |this, cx| {
            if rx.recv().await.is_ok() {
                this.update_in(cx, |this, window, cx| this.lost(name, &client, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Restarts the server's agent with the new build; reconnecting finds it.
    fn restart_agent(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_restart.take().is_some() {
            self.focus_active(window, cx);
        }
        if let Some(client) = self.client(&name) {
            client.restart();
        }
        cx.notify();
    }

    /// The connection was lost: terminals are left disconnected (still alive
    /// in the agent) and it retries until it's back.
    fn lost(&mut self, name: SharedString, client: &Weak<Client>, window: &mut Window, cx: &mut Context<Self>) {
        // A connection already replaced (Reconnect), or a server removed.
        let Some(host) = self.host_mut(&name).filter(|host| {
            host.client.as_ref().is_some_and(|current| Weak::ptr_eq(&Arc::downgrade(current), client))
        }) else {
            return;
        };
        host.client = None;
        host.status = HostStatus::Failed("connection lost; reconnecting…".into());
        self.connect(name, window, cx);
    }

    /// Whether `client` is still the connection to the server `name`: a
    /// replaced or removed one's events are no longer this window's.
    fn is_current(&self, name: &str, client: &Arc<Client>) -> bool {
        self.client(name).is_some_and(|current| Arc::ptr_eq(&current, client))
    }

    /// Receives the agents running on the server and the tasks created with
    /// `den task` on it.
    fn watch_host(&mut self, name: SharedString, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        // The `den` commands run in its terminals come here.
        client.notify(Request::Serve);
        let (tx, rx) = smol::channel::unbounded::<Event>();
        let name_for_watch = name.clone();
        let watch = client.watch_scoped(move |event| {
            let event = match event {
                Event::OpenTask { path } => Event::OpenTask { path: path.clone() },
                Event::Agents { agents } => Event::Agents { agents: agents.clone() },
                Event::Command { command, args, cwd, term, group } => Event::Command {
                    command: *command,
                    args: args.clone(),
                    cwd: cwd.clone(),
                    term: *term,
                    group: group.clone(),
                },
                // This machine's come to the app (see `main`), even with no window.
                Event::Open { root, file } if name_for_watch != LOCAL => Event::Open {
                    root: root.clone(),
                    file: file.clone(),
                },
                _ => return,
            };
            let _ = tx.try_send(event);
        });
        // Replaces the lost connection's, if any.
        self.watches.insert(name.clone(), watch);
        cx.spawn_in(window, async move |this, cx| {
            // The agents running there (an outdated agent doesn't know).
            if let Ok(Response::Agents(agents)) = client.request(Request::AgentList).await {
                this.update(cx, |this, cx| {
                    if this.is_current(&name, &client) {
                        this.set_agents(name.clone(), agents, cx);
                    }
                })
                .ok();
            }
            while let Ok(event) = rx.recv().await {
                // The server was removed, or reconnected: no longer heard here.
                if !this.read_with(cx, |this, _| this.is_current(&name, &client)).unwrap_or(false) {
                    if let Event::Command { command, .. } = event {
                        let result = Err("den is no longer connected to this server".to_string());
                        client.notify(Request::CommandDone { command, result });
                    }
                    break;
                }
                let alive = match event {
                    Event::Agents { agents } => this.update(cx, |this, cx| this.set_agents(name.clone(), agents, cx)).is_ok(),
                    // From a terminal outside den: only one window opens it.
                    Event::Open { root, file } => {
                        if cx.update(|_, cx| opens_from_host(&name, &this, cx)).unwrap_or(false) {
                            this.update_in(cx, |this, window, cx| {
                                this.open_from_terminal(name.clone(), root, file, window, cx);
                                window.activate_window();
                                cx.activate(true);
                            })
                            .ok();
                        }
                        this.upgrade().is_some()
                    }
                    Event::Command { command, args, cwd, term, group } => {
                        let ran = this
                            .update_in(cx, |this, window, cx| {
                                let command = commands::Command { id: command, args, cwd, term, group };
                                this.run_command(name.clone(), client.clone(), command, window, cx)
                            })
                            .is_ok();
                        if !ran {
                            let result = Err("den's window is closed".to_string());
                            client.notify(Request::CommandDone { command, result });
                        }
                        ran
                    }
                    Event::OpenTask { path } => {
                        if !cx.update(|_, cx| opens_from_host(&name, &this, cx)).unwrap_or(false) {
                            continue;
                        }
                        let tasks = list_tasks(&client).await;
                        this.update_in(cx, |this, window, cx| {
                            if let (Ok(tasks), Some(host)) = (tasks, this.host_mut(&name)) {
                                host.tasks = tasks;
                            }
                            this.activate(TaskKey { host: name.clone(), path }, window, cx);
                        })
                        .is_ok()
                    }
                    _ => true,
                };
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Re-reads the tasks of the connected servers, and closes the folders
    /// open on them that are no longer there (a worktree deleted elsewhere).
    fn refresh_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (name, client) in self
            .hosts
            .iter()
            .filter_map(|host| Some((host.name.clone(), host.client.clone()?)))
            .collect::<Vec<_>>()
        {
            let open: Vec<PathBuf> =
                self.workspaces.keys().filter(|key| key.host == name).map(|key| key.path.clone()).collect();
            cx.spawn_in(window, async move |this, cx| {
                let Ok(tasks) = list_tasks(&client).await else {
                    return;
                };
                // A task is there; any other folder, if the server finds it.
                let mut gone = Vec::new();
                for path in open.into_iter().filter(|path| !tasks.iter().any(|task| task.path == *path)) {
                    // Only when the server says it isn't there: not when it
                    // can't answer, or can't read it.
                    if let Err(err) = client.request(Request::Resolve { path: path.clone() }).await
                        && not_found(&err)
                    {
                        gone.push(path);
                    }
                }
                this.update_in(cx, |this, window, cx| {
                    if let Some(host) = this.host_mut(&name)
                        && host.tasks != tasks
                    {
                        host.tasks = tasks;
                        cx.notify();
                    }
                    for path in gone {
                        this.forget_gone(TaskKey { host: name.clone(), path }, window, cx);
                    }
                })
                .ok();
            })
            .detach();
        }
    }

    /// A folder that's no longer there: closed, and off the column's order
    /// and the recent ones.
    fn forget_gone(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        // Its unsaved files stay open, to be saved somewhere else.
        if self.workspaces.get(&key).is_some_and(|workspace| !workspace.read(cx).unsaved().is_empty()) {
            return;
        }
        if self.remembers() {
            let config = key.config();
            Config::update(cx, |c| {
                c.order.retain(|other| *other != config);
                c.recent.retain(|recent| !(recent.host == key.host.as_ref() && recent.path == key.path));
            });
        }
        self.close_folder(&key, window, cx);
    }

    /// Keep the default light surfaces and use layered charcoal surfaces in
    /// dark mode, with VS Code's 2026 syntax colors in both modes.
    fn install_theme(cx: &mut App) {
        let mut colors: HashMap<String, SyntaxColors> =
            serde_json::from_str(include_str!("../assets/themes/vscode-2026.json")).expect("vscode-2026.json");
        if !cx.has_global::<Theme>() {
            Theme::change(ThemeMode::Light, None, cx);
        }
        let theme = Theme::global_mut(cx);
        for (config, mode) in [(&mut theme.light_theme, "light"), (&mut theme.dark_theme, "dark")] {
            let Some(syntax) = colors.remove(mode) else { continue };
            let mut updated = (**config).clone();
            let mut style = updated.highlight.clone().unwrap_or_default();
            style.syntax = syntax;
            updated.highlight = Some(style);
            if mode == "dark" {
                theme::dark_surfaces(&mut updated);
            }
            theme::selection(&mut updated, mode == "dark");
            *config = Rc::new(updated);
        }
    }

    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match Config::get(cx).theme {
            ThemeChoice::System => Theme::sync_system_appearance(Some(window), cx),
            ThemeChoice::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemeChoice::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
        Self::apply_font_sizes(cx);
        cx.notify();
    }

    /// Workspaces in list order, as Cmd-E and Cmd-K go through them: those
    /// of the hidden projects left out, unless one is in front.
    fn ordered<'a>(&'a self, cx: &App) -> Vec<(TaskKey, &'a TaskInfo)> {
        let mut entries = self.ordered_all(cx);
        let active = self.active.as_ref().and_then(|key| Some(project_of(key, self.task(key)?)));
        entries.retain(|(key, task)| {
            let project = project_of(key, task);
            active.as_ref() == Some(&project) || !self.is_hidden_project(&project, cx)
        });
        entries
    }

    /// Every workspace in list order: by server; within each, by project
    /// (a repo's checkout followed by its worktrees, or a folder), the
    /// projects and the worktrees within them in the dragged order.
    fn ordered_all<'a>(&'a self, cx: &App) -> Vec<(TaskKey, &'a TaskInfo)> {
        let config = Config::get(cx);
        let mut entries: Vec<(usize, TaskKey, &TaskInfo)> = Vec::new();
        for (ix, host) in self.hosts.iter().enumerate() {
            // A folder opened on its own that has since become a task shows once.
            let loose = host.loose.iter().filter(|loose| !host.tasks.iter().any(|task| task.path == loose.path));
            for task in host.tasks.iter().chain(loose) {
                let key = TaskKey {
                    host: host.name.clone(),
                    path: task.path.clone(),
                };
                // An agent's worktree, unless it's the one in front or has an
                // agent running: then it's where to find it.
                let hidden = config.only_own_worktrees
                    && !task.main
                    && self.active.as_ref() != Some(&key)
                    && !config.own_worktrees.contains(&key.config())
                    && self.workspace_agents(&key).is_empty();
                if !hidden {
                    entries.push((ix, key, task));
                }
            }
        }
        let position = |key: &TaskKey| {
            let key = key.config();
            config
                .order
                .iter()
                .position(|other| other == &key)
                .unwrap_or(usize::MAX)
        };
        // A repo goes where the first of its own was dragged; never dragged,
        // at the end, by name.
        let mut groups: HashMap<(usize, &Path), usize> = HashMap::new();
        for (host, key, task) in &entries {
            let task: &'a TaskInfo = task;
            let rank = groups.entry((*host, task.repo.as_path())).or_insert(usize::MAX);
            *rank = (*rank).min(position(key));
        }
        let sort_key = |(host, key, task): &(usize, TaskKey, &TaskInfo)| {
            let repo = folder_name(&task.repo).to_lowercase();
            let group = groups[&(*host, task.repo.as_path())];
            (*host, group, repo, task.repo.clone(), !task.main, position(key), folder_name(&task.path).to_lowercase())
        };
        entries.sort_by_cached_key(sort_key);
        entries.into_iter().map(|(_, key, task)| (key, task)).collect()
    }

    fn task(&self, key: &TaskKey) -> Option<&TaskInfo> {
        let host = self.host(&key.host)?;
        host.tasks.iter().chain(&host.loose).find(|task| task.path == key.path)
    }

    /// The project in front: its repo's checkout, or its folder.
    fn active_project(&self) -> Option<TaskKey> {
        let key = self.active.as_ref()?;
        Some(project_of(key, self.task(key)?))
    }

    /// Hidden itself or with its server.
    fn is_hidden_project(&self, project: &TaskKey, cx: &App) -> bool {
        Config::get(cx).hidden_projects.contains(&project.config()) || is_hidden_host(&project.host, cx)
    }

    /// Hides `project` from the Workspaces panel, Cmd-E and Cmd-K, or shows it
    /// again. Its agents still count in the dots.
    fn set_project_hidden(&mut self, project: &TaskKey, hidden: bool, cx: &mut Context<Self>) {
        let key = project.config();
        Config::update(cx, |config| {
            config.hidden_projects.retain(|other| *other != key);
            if hidden {
                config.hidden_projects.push(key);
            }
        });
        cx.notify();
    }

    /// Hides a server and its projects from the Workspaces panel, Cmd-E and
    /// Cmd-K, or shows them again. It stays connected: its agents still
    /// count in the dots.
    fn set_host_hidden(&mut self, host: &SharedString, hidden: bool, cx: &mut Context<Self>) {
        Config::update(cx, |config| {
            config.hidden_hosts.retain(|other| other != host.as_ref());
            if hidden {
                config.hidden_hosts.push(host.to_string());
            }
        });
        cx.notify();
    }

    fn toggle_hidden_projects(&mut self, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.show_hidden_projects = !config.show_hidden_projects);
        cx.notify();
    }

    /// Folds `project` to its row, or shows its workspaces under it.
    fn set_project_folded(&mut self, project: &TaskKey, folded: bool, cx: &mut Context<Self>) {
        let key = project.config();
        if Config::get(cx).folded_projects.contains(&key) == folded {
            return;
        }
        Config::update(cx, |config| match folded {
            true => config.folded_projects.push(key),
            false => config.folded_projects.retain(|other| *other != key),
        });
        cx.notify();
    }

    /// Whether `project` (on its server) is a git repo: it has a branch or
    /// worktrees.
    fn is_repo(&self, project: &TaskKey) -> bool {
        self.host(&project.host).is_some_and(|host| {
            host.tasks.iter().any(|task| task.repo == project.path && (task.branch.is_some() || !task.main))
        })
    }

    /// Enters `project`: the worktree of it used last, or its checkout (its
    /// first workspace, if it has none).
    fn enter_project(&mut self, project: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        let mine: Vec<TaskKey> = self
            .ordered_all(cx)
            .into_iter()
            .filter(|(key, task)| project_of(key, task) == project)
            .map(|(key, _)| key)
            .collect();
        let recent = Config::get(cx).recent.iter().map(|recent| TaskKey { host: recent.host.clone().into(), path: recent.path.clone() });
        let last = self.active.iter().chain(self.previous.iter()).cloned().chain(recent).find(|key| mine.contains(key));
        let key = last.or_else(|| mine.iter().find(|key| **key == project).cloned()).or_else(|| mine.first().cloned());
        if let Some(key) = key {
            self.activate(key, window, cx);
        }
    }

    /// Whether what's open in the window is remembered: not in one opened
    /// with `den -s`.
    fn remembers(&self) -> bool {
        self.server.is_none()
    }

    /// In a window opened with `den -s`, whether something in it would be
    /// forgotten: its server, or a folder open in it.
    fn unkept(&self, cx: &App) -> bool {
        if self.remembers() {
            return false;
        }
        let hosts = &Config::get(cx).hosts;
        self.hosts
            .iter()
            .any(|host| host.destination.is_some() && !hosts.iter().any(|saved| saved.name == host.name.as_ref()))
            || self.workspaces.keys().any(|key| self.is_loose(key))
    }

    /// A folder open on its own that its server doesn't keep (nor the repo
    /// it's in: that's what keeping it adds).
    fn is_loose(&self, key: &TaskKey) -> bool {
        self.host(&key.host).is_some_and(|host| !host.tasks.iter().any(|task| key.path.starts_with(&task.path)))
    }

    /// Keep in Workspaces, in a window opened with `den -s`: its servers and
    /// the folders open in it (or only `folder`) are remembered, and the main
    /// window lists them.
    fn keep(&mut self, folder: Option<TaskKey>, window: &mut Window, cx: &mut Context<Self>) {
        let servers: Vec<HostConfig> = self
            .hosts
            .iter()
            .filter(|host| folder.as_ref().is_none_or(|folder| folder.host == host.name))
            .filter_map(|host| {
                Some(HostConfig { name: host.name.to_string(), destination: host.destination.clone()? })
            })
            .collect();
        Config::update(cx, |config| {
            for server in &servers {
                if !config.hosts.iter().any(|saved| saved.name == server.name) {
                    config.hosts.push(server.clone());
                }
            }
        });
        let folders: Vec<TaskKey> = match folder {
            Some(folder) => vec![folder],
            None => self.workspaces.keys().filter(|key| self.is_loose(key)).cloned().collect(),
        };
        let adds: Vec<_> = folders
            .into_iter()
            .filter_map(|key| Some(self.client(&key.host)?.request(Request::RepoAdd { path: key.path })))
            .collect();
        // Once the servers have them: the main window reads them then.
        cx.spawn_in(window, async move |this, cx| {
            for add in adds {
                let _ = add.await;
            }
            this.update_in(cx, |this, window, cx| this.refresh_repos(window, cx)).ok();
            cx.update(|_, cx| {
                if let Some((handle, den)) = main_window(cx) {
                    handle
                        .update(cx, |_, window, cx| den.update(cx, |den, cx| den.add_kept(servers, window, cx)))
                        .ok();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Servers kept in a window opened with `den -s`: listed here too, with
    /// their folders.
    fn add_kept(&mut self, servers: Vec<HostConfig>, window: &mut Window, cx: &mut Context<Self>) {
        for server in servers {
            let name: SharedString = server.name.into();
            if self.host(&name).is_none() {
                self.hosts.push(Host::remote(name.clone(), server.destination));
                self.connect(name, window, cx);
            }
        }
        self.refresh_repos(window, cx);
    }

    /// The workspaces show: in the side column, or alone without a
    /// workspace open.
    fn tasks_shown(&self, cx: &App) -> bool {
        self.active_workspace().is_none_or(|workspace| workspace.read(cx).is_shown(Panel::Workspaces, cx))
    }

    fn show_tasks_column(&mut self, visible: bool, cx: &mut Context<Self>) {
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| match visible {
                true => workspace.show_panel(Panel::Workspaces, cx),
                false => workspace.hide_panel(Panel::Workspaces, cx),
            });
        }
        cx.notify();
    }

    fn label(&self, key: &TaskKey) -> String {
        let label = self
            .task(key)
            .map(task_label)
            .unwrap_or_else(|| key.path.display().to_string());
        if key.host == LOCAL { label } else { format!("{}: {label}", key.host) }
    }

    /// Enters task `key`; if it isn't one, it opens as a folder on its own.
    fn activate(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        // A folder opened for the first time: shown right away, and added to
        // the server's list so that it stays (a repo shows its worktrees).
        if self.task(&key).is_none()
            && let Some(host) = self.host_mut(&key.host)
        {
            host.loose.push(loose_task(&key.path));
            if self.remembers() {
                self.add_folder(key.host.clone(), key.path.clone(), window, cx);
            }
        }
        self.agents_seen(&key);
        let workspace = match self.workspaces.get(&key) {
            Some(workspace) => workspace.clone(),
            None => {
                let client = self.client(&key.host);
                let root = key.path.clone();
                let local = key.host == LOCAL;
                let workspace = cx.new(|cx| Workspace::new(root, client, local, key.config(), window, cx));
                workspace.update(cx, |workspace, cx| workspace.restore(window, cx));
                if let Some(file) = self.open_file.take() {
                    workspace.update(cx, |workspace, cx| workspace.open(file, true, window, cx));
                }
                self.workspaces.insert(key.clone(), workspace.clone());
                workspace
            }
        };
        workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
        window.set_window_title(&format!("{} — den", self.label(&key)));
        if self.remembers() {
            let last = SavedTask {
                host: key.host.to_string(),
                path: key.path.clone(),
            };
            Config::update(cx, |config| {
                config.recent.retain(|recent| *recent != last);
                config.recent.insert(0, last.clone());
                config.recent.truncate(RECENT);
                config.last = Some(last);
            });
        }
        self.pending_last = None;
        if let Some(old) = self.active.take().filter(|old| *old != key) {
            self.previous = Some(old);
        }
        self.active = Some(key);
        cx.notify();
    }

    /// Unsaved files across all tasks, relative to their own task.
    fn unsaved(&self, cx: &App) -> Vec<(TaskKey, PathBuf)> {
        let mut unsaved: Vec<(TaskKey, PathBuf)> = self
            .workspaces
            .iter()
            .flat_map(|(key, workspace)| workspace.read(cx).unsaved().into_iter().map(move |file| (key.clone(), file)))
            .collect();
        unsaved.sort_by_key(|(key, file)| (self.label(key), file.clone()));
        unsaved
    }

    /// Before quitting (Cmd-Q) or closing the window: if any task has unsaved
    /// files, asks and only goes on if confirmed. Returns whether it can go
    /// on right away.
    pub fn confirm_quit(&mut self, closing: Closing, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.discarded || self.unsaved(cx).is_empty() {
            return true;
        }
        match &mut self.quit_confirm {
            Some((_, asked)) => *asked = closing,
            None => {
                let focus = cx.focus_handle();
                focus.focus(window, cx);
                self.quit_confirm = Some((focus, closing));
            }
        }
        cx.notify();
        false
    }

    /// The dialog's Quit (or Close) Without Saving. Quitting, the other
    /// windows with unsaved files still ask.
    fn discard_and_close(&mut self, cx: &mut Context<Self>) {
        match self.quit_confirm.take() {
            Some((_, Closing::Window)) => self.close_window(cx),
            _ => {
                self.discarded = true;
                cx.defer(quit);
            }
        }
    }

    /// Closes its window, unsaved files and all.
    fn close_window(&mut self, cx: &mut Context<Self>) {
        self.discarded = true;
        let handle = self.handle;
        cx.defer(move |cx| {
            handle.update(cx, |_, window, _| window.remove_window()).ok();
        });
    }

    fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_confirm = None;
        crate::update::cancel_restart(cx);
        // Not quitting after all: a window that went on without saving asks
        // again.
        self.discarded = false;
        cx.defer(|cx| {
            for (_, den) in windows(cx) {
                den.update(cx, |den, _| den.discarded = false);
            }
        });
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Saves everything and quits; if something couldn't be saved, the dialog
    /// stays with what's left (and the tab says why).
    fn save_and_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_saving {
            return;
        }
        let saves: Vec<Task<bool>> = self
            .workspaces
            .values()
            .map(|workspace| workspace.update(cx, |workspace, cx| workspace.save_all(cx)))
            .collect();
        self.quit_saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut ok = true;
            for save in saves {
                ok &= save.await;
            }
            this.update(cx, |this, cx| {
                this.quit_saving = false;
                if ok && this.unsaved(cx).is_empty() {
                    match this.quit_confirm.take() {
                        Some((_, Closing::Window)) => this.close_window(cx),
                        // The other windows may have some too.
                        _ => cx.defer(quit),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Click on a file in the dialog: it closes and goes to its tab.
    fn go_to_unsaved(&mut self, key: TaskKey, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // Not quitting after all, as with Cancel.
        self.cancel_quit(window, cx);
        let path = key.path.join(&file);
        self.activate(key, window, cx);
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.open(path, true, window, cx));
        }
    }

    fn render_quit_confirm(&self, focus: &FocusHandle, closing: Closing, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let unsaved = self.unsaved(cx);
        let title = match unsaved.len() {
            1 => "There is 1 unsaved file".to_string(),
            n => format!("There are {n} unsaved files"),
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui_kit::black().opacity(0.25))
            .occlude()
            .child(
                v_flex()
                    .id("quit-confirm")
                    .track_focus(focus)
                    .w(px(480.))
                    .p_4()
                    .gap_3()
                    .rounded(theme.radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .text_ui(cx)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => this.cancel_quit(window, cx),
                            "enter" => this.save_and_quit(cx),
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .child(div().text_base().font_semibold().child(title))
                    .child(
                        v_flex()
                            .id("quit-confirm-files")
                            .max_h(px(240.))
                            .overflow_y_scroll()
                            .children(unsaved.into_iter().enumerate().map(|(ix, (key, file))| {
                                let label = self.label(&key);
                                let name = file.display().to_string();
                                h_flex()
                                    .id(("unsaved", ix))
                                    .px_2()
                                    .py_1()
                                    .gap_2()
                                    .rounded(theme.radius)
                                    .hover(|style| style.bg(theme.accent))
                                    .child(div().flex_none().text_color(theme.muted_foreground).child(label))
                                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(name))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.go_to_unsaved(key.clone(), file.clone(), window, cx)
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child("Click a file to go to it."),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(dialog_button("quit-cancel", "Cancel", cx).on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                            .child(
                                dialog_button(
                                    "quit-discard",
                                    if closing == Closing::App { "Quit Without Saving" } else { "Close Without Saving" },
                                    cx,
                                )
                                .text_color(theme.danger)
                                .on_click(cx.listener(|this, _, _, cx| this.discard_and_close(cx))),
                            )
                            .child(
                                div()
                                    .id("quit-save")
                                    .px_3()
                                    .py_1()
                                    .rounded(theme.radius)
                                    .bg(theme.primary)
                                    .text_color(theme.primary_foreground)
                                    .hover(|style| style.bg(theme.primary_hover))
                                    .child(match (self.quit_saving, closing) {
                                        (true, _) => "Saving…",
                                        (false, Closing::App) => "Save All and Quit",
                                        (false, Closing::Window) => "Save All and Close",
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.save_and_quit(cx))),
                            ),
                    ),
            )
    }

    /// One picker at a time: opening one closes the others, which would
    /// be drawn over it or keep the focus.
    fn close_pickers(&mut self) {
        self.task_picker = None;
        self.command_palette = None;
        self.recent_picker = None;
        self.host_picker = None;
        self.folder_picker = None;
    }

    /// Cmd-E: goes back to the previous task; again, to the one before (like Alt-Tab).
    fn open_task_picker(&mut self, _: &OpenTaskPicker, window: &mut Window, cx: &mut Context<Self>) {
        if self.task_picker.is_some() {
            return;
        }
        self.close_pickers();
        // The most recently used first, so Enter goes back to the previous
        // one; those never visited in the column's order; the one in front, last.
        let mut keys: Vec<TaskKey> = self.ordered(cx).into_iter().map(|(key, _)| key).collect();
        let recent = &Config::get(cx).recent;
        keys.sort_by_key(|key| {
            if self.active.as_ref() == Some(key) {
                usize::MAX
            } else {
                recent.iter().position(|task| task.host == key.host.to_string() && task.path == key.path).unwrap_or(RECENT)
            }
        });
        let labels = distinct_labels(keys.iter().map(|key| (self.label(key), key.path.as_path())));
        let picker = cx.new(|cx| Picker::new(Arc::new(labels.clone()), "Go to workspace…", false, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &PickerEvent, window, cx| {
            this.task_picker = None;
            match event {
                PickerEvent::Pick(label) => {
                    let key = labels.iter().position(|other| other == label).map(|ix| keys[ix].clone());
                    match key {
                        Some(key) => this.activate(key, window, cx),
                        None => this.focus_active(window, cx),
                    }
                }
                PickerEvent::Dismiss => this.focus_active(window, cx),
                PickerEvent::Close => {}
            }
            cx.notify();
        });
        self.task_picker = Some((picker, subscription));
        cx.notify();
    }

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            return;
        }
        self.close_pickers();
        // The commands run where the focus was, as if their keys were pressed there.
        let previous = window.focused(cx);
        // As in VS Code: those run lately, the most recent first, then the
        // rest by name.
        let mut commands: Vec<_> = SHORTCUTS
            .iter()
            .filter(|shortcut| !matches!(shortcut.id, "OpenCommandPalette" | "ShowShortcuts"))
            .collect();
        let recent_ids = &Config::get(cx).recent_commands;
        let rank = |id: &str| recent_ids.iter().position(|recent| recent == id);
        commands.sort_by(|a, b| match (rank(a.id), rank(b.id)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.label.to_lowercase().cmp(&b.label.to_lowercase()),
        });
        let recent: HashSet<String> =
            commands.iter().filter(|shortcut| rank(shortcut.id).is_some()).map(|shortcut| shortcut.label.to_string()).collect();
        let labels: Vec<String> = commands.iter().map(|shortcut| shortcut.label.to_string()).collect();
        let hints: HashMap<String, String> = commands
            .iter()
            .filter_map(|shortcut| Some((shortcut.label.to_string(), Kbd::format(&shortcuts::keys(shortcut, cx)?))))
            .collect();
        let picker =
            cx.new(|cx| Picker::new(Arc::new(labels), "Run a command…", false, window, cx).ordered(recent).with_hints(hints));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &PickerEvent, window, cx| {
            this.command_palette = None;
            let restore = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| match &previous {
                Some(focus) => window.focus(focus, cx),
                None => this.focus_active(window, cx),
            };
            match event {
                PickerEvent::Pick(label) => {
                    restore(this, window, cx);
                    if let Some(shortcut) = SHORTCUTS.iter().find(|shortcut| shortcut.label == label) {
                        Config::update(cx, |config| {
                            config.recent_commands.retain(|id| id != shortcut.id);
                            config.recent_commands.insert(0, shortcut.id.to_string());
                            config.recent_commands.truncate(RECENT_COMMANDS);
                        });
                        match &previous {
                            // Once this update is over, and outside `Den`:
                            // dispatching on a focus handle runs now, and the
                            // action may reach `Den` (Settings), which can't be
                            // updated from within itself (`defer_in` would be).
                            Some(focus) => {
                                let (focus, action) = (focus.clone(), shortcut.action());
                                window.defer(cx, move |window, cx| focus.dispatch_action(action.as_ref(), window, cx));
                            }
                            None => window.dispatch_action(shortcut.action(), cx),
                        }
                    }
                }
                PickerEvent::Dismiss => restore(this, window, cx),
                PickerEvent::Close => {}
            }
            cx.notify();
        });
        self.command_palette = Some((picker, subscription));
        cx.notify();
    }

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
        }
    }

    fn toggle_tasks(&mut self, _: &ToggleTasks, _: &mut Window, cx: &mut Context<Self>) {
        let visible = !self.tasks_shown(cx);
        self.show_tasks_column(visible, cx);
    }

    /// The tasks' state on the explorer's icon or the panels' headers: the
    /// most urgent of the workspaces (those of hidden projects too, and the
    /// one in front), of the project's worktrees, and of the agents. Idle
    /// says nothing.
    fn task_badges(&self, cx: &App) -> TaskBadges {
        let tasks = self.ordered_all(cx);
        let most_urgent = |tasks: &mut dyn Iterator<Item = &(TaskKey, &TaskInfo)>| {
            tasks
                .map(|(key, task)| self.status(key, task, cx))
                .filter(|(dot, color)| urgency(dot, *color, cx) > 0)
                .max_by_key(|(dot, color)| urgency(dot, *color, cx))
                .map(|(_, color)| color)
        };
        TaskBadges {
            workspaces: most_urgent(&mut tasks.iter()),
            agents: self.agents_badge(cx),
        }
    }

    /// Opens `path` on `host`: the task it is, or the folder on its own.
    fn open_path(&mut self, host: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(TaskKey { host, path }, window, cx);
    }

    /// `den <path>`: `root` as a workspace (the worktree containing it, if
    /// any), with `file` open in it. A server not yet connected enters it on
    /// connecting.
    fn open_from_terminal(
        &mut self,
        host: SharedString,
        root: PathBuf,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = self.workspace_containing(&host, &root);
        self.open_file = file;
        if self.client(&host).is_none() {
            self.pending_last = Some(TaskKey { host, path });
            return;
        }
        self.open_path(host, path, window, cx);
        // Already open: the file goes in it now.
        if let (Some(file), Some(workspace)) = (self.open_file.take(), self.active_workspace()) {
            workspace.update(cx, |workspace, cx| workspace.open(file, true, window, cx));
        }
    }

    /// `den -s`: opens `path` on the window's server (a file in its repo),
    /// relative to its home folder, or the home folder with none. Not
    /// there, the folder picker starts at it. Before the server connects,
    /// it waits.
    fn open_start(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.server.as_ref().map(|server| server.name.clone()) else {
            return;
        };
        let Some(client) = self.client(&name) else {
            if let Some(server) = &mut self.server {
                server.start = Some(path);
            }
            return;
        };
        let path = path.unwrap_or_else(|| PathBuf::from("~"));
        let path = if path.is_absolute() || path.starts_with("~") { path } else { Path::new("~").join(path) };
        cx.spawn_in(window, async move |this, cx| {
            let target = match client.request(Request::Resolve { path: path.clone() }).await {
                Ok(Response::Path(Some(resolved))) => remote_target(&client, resolved).await,
                _ => None,
            };
            this.update_in(cx, |this, window, cx| match target {
                Some((root, file)) => this.open_from_terminal(name, root, file, window, cx),
                None => this.open_folder_picker_at(name, path, window, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The worktree of `host` containing `path`, or `path` itself.
    fn workspace_containing(&self, host: &str, path: &Path) -> PathBuf {
        self.host(host)
            .and_then(|host| {
                host.tasks
                    .iter()
                    .filter(|task| path.starts_with(&task.path))
                    .max_by_key(|task| task.path.components().count())
                    .map(|task| task.path.clone())
            })
            .unwrap_or_else(|| path.to_path_buf())
    }

    /// Cmd-O: a local folder, with the system's dialog. In a window opened
    /// with `den -s`, a folder on its server.
    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(server) = &self.server
            && server.name != LOCAL
        {
            let name = server.name.clone();
            self.open_folder_picker(name, window, cx);
            return;
        }
        self.pick_local_folder("Open", window, cx, |this, path, window, cx| {
            this.open_path(LOCAL.into(), path, window, cx)
        });
    }

    /// A folder on `host` to open (or make): this machine's with the
    /// system's dialog, a server's browsed through its agent.
    fn open_folder_on(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if host == LOCAL {
            self.open_folder(&OpenFolder, window, cx);
        } else {
            self.open_folder_picker(host, window, cx);
        }
    }

    /// The system's dialog for choosing a folder on this machine.
    fn pick_local_folder(
        &mut self,
        prompt: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, PathBuf, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            if let Some(path) = paths.into_iter().next() {
                this.update_in(cx, |this, window, cx| then(this, path, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Cmd-Alt-O: a folder on a server, browsed through its agent. With
    /// several servers, it asks which first; with none, it offers to add one.
    fn open_remote_folder(&mut self, _: &OpenRemoteFolder, window: &mut Window, cx: &mut Context<Self>) {
        let hosts: Vec<String> = self
            .hosts
            .iter()
            .filter(|host| host.destination.is_some() && host.client.is_some())
            .map(|host| host.name.to_string())
            .collect();
        match hosts.as_slice() {
            [] => self.open_host_picker(window, cx),
            // The servers connected, and another to connect to.
            _ => {
                self.close_pickers();
                let mut choices = hosts;
                choices.push(ADD_SERVER.to_string());
                let picker = cx.new(|cx| Picker::new(Arc::new(choices), "Open a folder on…", false, window, cx));
                let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
                    this.host_picker = None;
                    match event {
                        PickerEvent::Pick(choice) if choice == ADD_SERVER => this.open_host_picker(window, cx),
                        PickerEvent::Pick(host) => {
                            this.open_folder_picker(host.clone().into(), window, cx)
                        }
                        PickerEvent::Dismiss => this.focus_active(window, cx),
                        PickerEvent::Close => {}
                    }
                    cx.notify();
                });
                self.host_picker = Some((picker, subscription));
                cx.notify();
            }
        }
    }

    /// Folders opened before, the most recent first (without the open one).
    fn recents(&self, cx: &App) -> Vec<(TaskKey, String)> {
        Config::get(cx)
            .recent
            .iter()
            .map(|recent| TaskKey { host: recent.host.clone().into(), path: recent.path.clone() })
            .filter(|key| self.active.as_ref() != Some(key) && self.host(&key.host).is_some())
            .map(|key| {
                let label = recent_label(&key);
                (key, label)
            })
            .collect()
    }

    /// Cmd-Shift-O: jump to a folder opened before.
    fn open_recent(&mut self, _: &OpenRecent, window: &mut Window, cx: &mut Context<Self>) {
        if self.recent_picker.is_some() {
            return;
        }
        self.close_pickers();
        let labels: Vec<String> = self.recents(cx).into_iter().map(|(_, label)| label).collect();
        let picker = cx.new(|cx| Picker::new(Arc::new(labels), "Open recent…", false, window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            this.recent_picker = None;
            match event {
                PickerEvent::Pick(label) => {
                    match this.recents(cx).into_iter().find(|(_, other)| other == label) {
                        Some((key, _)) => this.open_path(key.host, key.path, window, cx),
                        None => this.focus_active(window, cx),
                    }
                }
                PickerEvent::Dismiss => this.focus_active(window, cx),
                PickerEvent::Close => {}
            }
            cx.notify();
        });
        self.recent_picker = Some((picker, subscription));
        cx.notify();
    }

    /// Takes a folder (a repo's checkout, with its worktrees) off the server's
    /// list and closes it. Nothing on disk is touched; its terminals stay in
    /// the agent and come back if it's opened again.
    fn remove_folder(&mut self, key: &TaskKey, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(client) = self.client(&key.host) {
            cx.spawn_in(window, async move |this, cx| {
                let _ = client.request(Request::RepoRemove { path: folder }).await;
                this.update_in(cx, |this, window, cx| this.refresh_repos(window, cx)).ok();
            })
            .detach();
        }
        self.close_folder(key, window, cx);
    }

    /// Closes a folder's workspace.
    fn close_folder(&mut self, key: &TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(host) = self.host_mut(&key.host) {
            host.loose.retain(|task| task.path != key.path);
        }
        self.workspaces.remove(key);
        if self.previous.as_ref() == Some(key) {
            self.previous = None;
        }
        if self.active.as_ref() == Some(key) {
            self.active = None;
            if self.remembers() {
                Config::update(cx, |config| config.last = None);
            }
            window.set_window_title(&self.title());
            match self.previous.clone().filter(|key| self.task(key).is_some()) {
                Some(previous) => self.activate(previous, window, cx),
                None => self.focus_handle.focus(window, cx),
            }
        }
        cx.notify();
    }

    /// Adds `path` to the folders `host`'s agent keeps (its repo, if it's in
    /// one). Quietly: it's already open, and without an agent it just
    /// doesn't stay.
    fn add_folder(&mut self, host: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&host) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            if client.request(Request::RepoAdd { path }).await.is_ok() {
                this.update_in(cx, |this, window, cx| this.refresh_repos(window, cx)).ok();
            }
        })
        .detach();
    }

    /// Cmd-N: new task in the active task's repo.
    fn new_task_action(&mut self, _: &NewTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.active.clone() else {
            return;
        };
        if let Some(repo) = self.task(&key).map(|task| task.repo.clone()) {
            self.start_new_task(key.host, repo, window, cx);
        }
    }

    fn start_new_task(&mut self, host: SharedString, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.client(&host).is_none() {
            return;
        }
        // It's named under its project, unfolded, in the Workspaces panel.
        let project = TaskKey { host: host.clone(), path: repo.clone() };
        self.set_project_folded(&project, false, cx);
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.show_panel(Panel::Workspaces, cx));
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("branch name"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.create_task(window, cx),
            // Like a new file in the tree: clicking elsewhere drops it,
            // unless it's already being created.
            InputEvent::Blur if this.new_task.as_ref().is_some_and(|form| !form.busy) => {
                this.new_task = None;
                cx.notify();
            }
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.new_task = Some(NewTaskInput {
            host,
            repo,
            input,
            busy: false,
            error: None,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn create_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.new_task.as_ref() else {
            return;
        };
        let Some(client) = self.client(&form.host) else {
            return;
        };
        let form = self.new_task.as_mut().expect("form exists");
        let name = form.input.read(cx).value().trim().to_string();
        if name.is_empty() || form.busy {
            return;
        }
        form.busy = true;
        form.error = None;
        let (host, repo) = (form.host.clone(), form.repo.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(Request::TaskCreate { repo, name, open: false }).await;
            let tasks = list_tasks(&client).await;
            this.update_in(cx, |this, window, cx| {
                if let (Ok(tasks), Some(host)) = (tasks, this.host_mut(&host)) {
                    host.tasks = tasks;
                }
                match result {
                    Ok(Response::Task(task)) => {
                        this.new_task = None;
                        let key = TaskKey { host, path: task.path };
                        Config::update(cx, |config| config.own_worktrees.push(key.config()));
                        this.activate(key, window, cx);
                    }
                    Ok(other) => this.new_task_error(format!("Unexpected response: {other:?}"), window, cx),
                    Err(err) => this.new_task_error(format!("{err:#}"), window, cx),
                }
            })
            .ok();
        })
        .detach();
    }

    /// The name back in its row, to fix and try again.
    fn new_task_error(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.new_task {
            form.busy = false;
            form.error = Some(error.into());
            form.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Esc closes whatever is open in the column.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.confirm_remove = None;
        self.confirm_force_remove = None;
        self.error = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// With `force`, along with what git won't delete: uncommitted changes.
    fn remove_task(&mut self, key: TaskKey, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&key.host) else {
            return;
        };
        let asked = self.confirm_remove.take().is_some();
        if self.confirm_force_remove.take().is_some() || asked {
            self.focus_active(window, cx);
        }
        self.error = None;
        self.removing.insert(key.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let path = key.path.clone();
            let request = if force { Request::TaskForceRemove { path } } else { Request::TaskRemove { path } };
            let result = client.request(request).await;
            let tasks = list_tasks(&client).await;
            this.update_in(cx, |this, window, cx| {
                this.removing.remove(&key);
                if let (Ok(tasks), Some(host)) = (tasks, this.host_mut(&key.host)) {
                    host.tasks = tasks;
                }
                match result {
                    Ok(_) => {
                        this.workspaces.remove(&key);
                        let config = key.config();
                        Config::update(cx, |c| {
                            c.order.retain(|other| other != &config);
                            c.own_worktrees.retain(|other| other != &config);
                            c.sessions.remove(&config);
                        });
                        crate::notes::forget(&config, cx);
                        if this.active.as_ref() == Some(&key) {
                            this.active = None;
                            if let Some(next) = this.ordered(cx).first().map(|(key, _)| key.clone()) {
                                this.activate(next, window, cx);
                            }
                        }
                    }
                    // Whatever refused it (git, the repo's .den/remove), why
                    // goes in a dialog that offers to force it, never a dead
                    // end in the list; a force that fails too says why there.
                    Err(err) => {
                        let focus = this.confirm_focus(window, cx);
                        this.confirm_force_remove = Some((key, focus, format!("{err:#}").into()));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Makes a folder that isn't in a repo yet one: it then shows its branch
    /// and can have worktrees.
    fn init_git(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&key.host) else {
            return;
        };
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(Request::Git { path: key.path.clone(), op: GitOp::Init }).await;
            this.update_in(cx, |this, window, cx| {
                if let Err(err) = result {
                    this.error = Some((key, format!("{err:#}").into()));
                }
                this.refresh_repos(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Dropping `dragged` on `target`, and saves the order. A repo's checkout
    /// takes its worktrees along, before `target`'s repo; a worktree only
    /// moves within its repo, before `target` (first, dropped on the checkout).
    fn move_task(&mut self, dragged: &TaskKey, target: &TaskKey, cx: &mut Context<Self>) {
        if dragged == target || dragged.host != target.host {
            return;
        }
        let (Some(from), Some(to)) = (self.task(dragged), self.task(target)) else {
            return;
        };
        let (main, same_repo, onto_main) = (from.main, from.repo == to.repo, to.main);
        // The list as runs of the same repo, each its checkout first.
        let mut groups: Vec<Vec<TaskKey>> = Vec::new();
        let mut last: Option<(SharedString, PathBuf)> = None;
        for (key, task) in self.ordered_all(cx) {
            let group = (key.host.clone(), task.repo.clone());
            if last.as_ref() != Some(&group) {
                groups.push(Vec::new());
                last = Some(group);
            }
            groups.last_mut().expect("just pushed").push(key);
        }
        let find = |groups: &[Vec<TaskKey>], key: &TaskKey| groups.iter().position(|group| group.contains(key));
        let (Some(source), Some(dest)) = (find(&groups, dragged), find(&groups, target)) else {
            return;
        };
        if main {
            if same_repo {
                return;
            }
            let moved = groups.remove(source);
            let dest = find(&groups, target).unwrap_or(groups.len());
            groups.insert(dest, moved);
        } else {
            if !same_repo {
                return;
            }
            let group = &mut groups[dest];
            group.retain(|key| key != dragged);
            let checkout = usize::from(group.first().is_some_and(|key| self.task(key).is_some_and(|task| task.main)));
            let at = if onto_main {
                checkout
            } else {
                group.iter().position(|key| key == target).unwrap_or(group.len())
            };
            group.insert(at.max(checkout), dragged.clone());
        }
        let order: Vec<String> = groups.into_iter().flatten().map(|key| key.config()).collect();
        Config::update(cx, |c| c.order = order);
        cx.notify();
    }

    fn refresh_repos(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (name, client) in self
            .hosts
            .iter()
            .filter_map(|host| Some((host.name.clone(), host.client.clone()?)))
            .collect::<Vec<_>>()
        {
            cx.spawn_in(window, async move |this, cx| {
                let repos = client.request(Request::RepoList).await;
                let tasks = list_tasks(&client).await;
                this.update(cx, |this, cx| {
                    if let Some(host) = this.host_mut(&name) {
                        if let Ok(Response::Repos(repos)) = repos {
                            host.repos = repos;
                        }
                        if let Ok(tasks) = tasks {
                            host.tasks = tasks;
                        }
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Registers the server `destination` (from `~/.ssh/config`,
    /// `user@host` or, on Windows, `wsl:<distro>`) and connects; returns whether it was added.
    fn add_host_named(&mut self, destination: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if destination.is_empty() || destination.contains(char::is_whitespace) {
            return false;
        }
        let name: SharedString = destination.clone().into();
        if self.host(&name).is_some() {
            return false;
        }
        if self.remembers() {
            Config::update(cx, |c| {
                c.hosts.push(HostConfig {
                    name: name.to_string(),
                    destination: destination.clone(),
                })
            });
        }
        self.hosts.push(Host::remote(name.clone(), destination));
        // It stays if a folder is opened on it once it connects.
        self.adding_host = Some(name.clone());
        self.connect(name, window, cx);
        if !self.tasks_shown(cx) {
            self.show_tasks_column(true, cx);
        }
        true
    }

    /// Add Server: those in `~/.ssh/config` (and, on Windows, the WSL
    /// distros) not added yet, or any `user@host` typed. Listing the distros
    /// runs `wsl.exe`, which takes seconds if WSL isn't running: in the
    /// background.
    fn open_host_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let hosts = cx
                .background_executor()
                .spawn(async {
                    #[allow(unused_mut)]
                    let mut hosts = ssh_hosts();
                    #[cfg(windows)]
                    hosts.extend(client::wsl::destinations());
                    hosts
                })
                .await;
            this.update_in(cx, |this, window, cx| this.show_host_picker(hosts, window, cx)).ok();
        })
        .detach();
    }

    fn show_host_picker(&mut self, mut hosts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_pickers();
        hosts.retain(|host| self.host(host).is_none());
        let placeholder =
            if cfg!(windows) { "Server from ~/.ssh/config, WSL distro or user@host…" } else { "Server from ~/.ssh/config or user@host…" };
        let picker = cx.new(|cx| Picker::new(Arc::new(hosts), placeholder, false, window, cx).typed());
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            this.host_picker = None;
            if let PickerEvent::Pick(host) = event {
                this.add_host_named(host.clone(), window, cx);
            }
            cx.notify();
        });
        self.host_picker = Some((picker, subscription));
        cx.notify();
    }

    /// Browse the server's folders, starting next to its known or open
    /// folders (or in its home folder), to open one or make a new one.
    fn open_folder_picker(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let start = self
            .host(&host)
            .and_then(|host| host.repos.first().or(host.tasks.first().map(|task| &task.repo)))
            .and_then(|repo| repo.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("~"));
        self.open_folder_picker_at(host, start, window, cx);
    }

    fn open_folder_picker_at(&mut self, host: SharedString, start: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&host) else {
            return;
        };
        self.close_pickers();
        let title = format!("OPEN FOLDER ON {}", host.to_uppercase());
        let picker = cx.new(|cx| FolderPicker::new(client.clone(), title, start, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &FolderPickerEvent, window, cx| {
            this.folder_picker = None;
            let adding = this.adding_host.take_if(|adding| *adding == host);
            match event {
                // A server just added with no folder opened isn't kept.
                FolderPickerEvent::Dismiss if adding.is_some() => this.remove_host(host.clone(), window, cx),
                FolderPickerEvent::Dismiss => this.focus_active(window, cx),
                FolderPickerEvent::Pick(path) => this.open_path(host.clone(), path.clone(), window, cx),
            }
            cx.notify();
        });
        self.folder_picker = Some((picker, subscription));
        cx.notify();
    }

    fn remove_host(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if name == LOCAL {
            return;
        }
        if self.unsaved(cx).iter().any(|(key, _)| key.host == name) {
            let answer = window.prompt(
                PromptLevel::Warning,
                &format!("Remove {name} with unsaved files?"),
                Some("Save your changes before removing the server, or discard them."),
                &[PromptButton::new("Cancel"), PromptButton::new("Discard Changes"), PromptButton::new("Save and Remove")],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| {
                match answer.await {
                    Ok(1) => {
                        this.update_in(cx, |this, window, cx| this.forget_host(name, window, cx)).ok();
                    }
                    Ok(2) => {
                        let Ok(saves) = this.update(cx, |this, cx| {
                            this.workspaces.iter().filter(|(key, _)| key.host == name)
                                .map(|(_, workspace)| workspace.update(cx, |workspace, cx| workspace.save_all(cx)))
                                .collect::<Vec<_>>()
                        }) else { return };
                        let mut saved = true;
                        for save in saves {
                            saved &= save.await;
                        }
                        if saved {
                            // Recheck: another buffer may have changed during saving.
                            this.update_in(cx, |this, window, cx| this.remove_host(name, window, cx)).ok();
                        }
                    }
                    _ => {}
                }
            }).detach();
            return;
        }
        self.forget_host(name, window, cx);
    }

    fn forget_host(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        // Its connection goes with it: no more agents or `den` commands from it.
        if let Some(client) = self.client(&name) {
            client.disconnect();
        }
        self.hosts.retain(|host| host.name != name);
        self.agents.remove(&name);
        self.agents_attention.retain(|(host, _)| *host != name);
        self.workspaces.retain(|key, _| key.host != name);
        if self.remembers() {
            Config::update(cx, |c| c.hosts.retain(|host| host.name != name.as_ref()));
        }
        if self.active.as_ref().is_some_and(|key| key.host == name) {
            self.active = None;
            if let Some(next) = self.ordered(cx).first().map(|(key, _)| key.clone()) {
                self.activate(next, window, cx);
            }
        }
        cx.notify();
    }

    fn set_theme(&mut self, theme: ThemeChoice, window: &mut Window, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.theme = theme);
        self.apply_theme(window, cx);
    }

    /// Applies at once: every window repaints with the new size.
    fn set_font_size(&mut self, area: TextArea, size: Option<f32>, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.set_font_size(area, size));
        Self::apply_font_sizes(cx);
        cx.refresh_windows();
    }

    /// Hands the sizes that aren't read from the config where they're used:
    /// the editor's to the theme and the terminal's to ui-term.
    fn apply_font_sizes(cx: &mut App) {
        let config = Config::get(cx);
        let (editor, terminal) = (config.font_size(TextArea::Editor), config.font_size(TextArea::Terminal));
        Theme::global_mut(cx).mono_font_size = px(editor);
        cx.set_global(ui_term::TerminalFontSize(terminal));
    }

    fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.active.as_ref().and_then(|key| self.workspaces.get(key).cloned())
    }

    /// The workspaces: with `header`, its title above them, as without a
    /// workspace open (with one, its panel's header is the side column's).
    fn render_column(&self, header: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_tasks(cx);
        let header = header.then(|| self.render_tasks_header(cx));
        let theme = cx.theme();
        v_flex()
            .id("task-column")
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .children(header)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }

    /// The workspaces' title, with what adds to them.
    fn render_tasks_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let weak = cx.entity().downgrade();
        h_flex()
            .id("tasks-header")
            .h(px(34.))
            .flex_none()
            .px_3()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(div().flex_1().text_ui_small(cx).text_color(theme.muted_foreground).child("PROJECTS"))
            .child(tasks_add_button(&weak))
            .context_menu(move |menu, window, cx| column_menu(menu, &weak, window, cx))
            .into_any_element()
    }

    /// The activity bar, with only the explorer's icon; the workspaces, as
    /// wide as the side column; and the welcome.
    fn render_without_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let badge = self.task_badges(cx).workspaces.map(Badge::Dot);
        let click: OnActivity = Rc::new(|_, _, _| {});
        let bar = activity_bar(vec![(Item::Place(Place(Panel::Workspaces)), true, badge)], Vec::new(), click, cx);
        let width = Config::get(cx).layout.side_width;
        let state = self.split.state(window.viewport_size().width - px(ACTIVITY_WIDTH), [true, true], cx).clone();
        let split = h_resizable("den-split")
            .with_state(&state)
            .child(
                resizable_panel()
                    .size(config::width(width, 160., 800.))
                    .size_range(px(160.)..px(800.))
                    .child(self.render_column(true, cx)),
            )
            .child(resizable_panel().child(match &self.guide {
                Some(guide) => self.render_guide(guide, cx),
                None => self.render_welcome(cx),
            }))
;
        h_flex().size_full().child(bar).child(div().flex_1().min_w_0().h_full().child(split)).into_any_element()
    }

    fn render_host_header(&self, host: &Host, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // The connection's state is the icon's color.
        let (color, detail): (Hsla, Option<SharedString>) = match &host.status {
            HostStatus::Connected => (theme.success, None),
            HostStatus::Connecting(step) => (theme.muted_foreground, Some((*step).into())),
            HostStatus::Failed(err) => (theme.danger, Some(err.clone())),
        };
        let name = host.name.clone();
        let retry = matches!(host.status, HostStatus::Failed(_));
        let outdated = host.client.as_ref().is_some_and(|client| client.outdated());
        let restart = name.clone();
        let connected = host.client.is_some();
        let keep = self.unkept(cx);
        let weak = cx.entity().downgrade();
        let menu_name = name.clone();
        let hidden = is_hidden_host(&name, cx);
        v_flex()
            .px_3()
            .pt_2()
            .pb_1()
            .when(hidden, |el| el.opacity(0.5))
            .child(
                h_flex()
                    .id(SharedString::from(format!("host-{name}")))
                    .group("host")
                    .h(px(20.))
                    .gap_1()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(
                        svg()
                            .path(if host.destination.is_none() { "icons/monitor.svg" } else { "icons/server.svg" })
                            .size(px(12.))
                            .text_color(color),
                    )
                    .child(name.clone())
                    .child(div().flex_1())
                    .when(connected, |el| {
                        let open = name.clone();
                        el.child(
                            div()
                                .id(SharedString::from(format!("host-open-{name}")))
                                .invisible()
                                .group_hover("host", |style| style.visible())
                                .rounded(theme.radius)
                                .hover(|style| style.bg(theme.sidebar_accent))
                                .child(svg().path("icons/plus.svg").size(px(12.)).text_color(theme.muted_foreground))
                                .tooltip({
                                    let tip = if open == LOCAL { "Open Folder…".to_string() } else { format!("Open Folder on {open}…") };
                                    move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_folder_on(open.clone(), window, cx)
                                })),
                        )
                    })
                    .when(retry, |el| {
                        el.child(div().hover(|style| style.underline()).child("retry"))
                        .on_click(cx.listener(move |this, _, window, cx| this.connect(name.clone(), window, cx)))
                    })
                    .when(outdated, |el| {
                        el.child(
                            div()
                                .text_color(theme.warning)
                                .hover(|style| style.underline())
                                .child("outdated agent · restart"),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.ask_restart(restart.clone(), window, cx)))
                    })
                    .context_menu(move |menu, window, cx| {
                        let host = menu_name.clone();
                        host_menu(menu, &menu_name, connected, keep, &weak)
                            .item(menu::item(if hidden { "Show Server" } else { "Hide Server" }, &weak, move |this, _, cx| {
                                this.set_host_hidden(&host, !hidden, cx)
                            }))
                            .item(show_hidden_item(&weak, cx))
                            .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                    }),
            )
            .children(detail.map(|detail| {
                div()
                    .pl_3()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(detail)
            }))
            .into_any_element()
    }

    /// The projects, by server: a row each. Hidden ones only with Show
    /// Hidden Projects, or while in front.
    fn render_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        let ordered = self.ordered_all(cx);
        let show_hidden = Config::get(cx).show_hidden_projects;
        let active = self.active_project();
        let mut sections: Vec<AnyElement> = Vec::new();
        for host in &self.hosts {
            let entries: Vec<(TaskKey, &TaskInfo)> =
                ordered.iter().filter(|(key, _)| key.host == host.name).cloned().collect();
            let mut projects = Vec::new();
            for group in entries.chunk_by(|(_, a), (_, b)| a.repo == b.repo) {
                let Some((key, task)) = group.first() else {
                    continue;
                };
                let project = project_of(key, task);
                let hidden = self.is_hidden_project(&project, cx);
                if hidden && !show_hidden && active.as_ref() != Some(&project) {
                    continue;
                }
                projects.push(self.render_project(project, hidden, group, cx));
            }
            // A hidden server shows only for its project in front.
            if is_hidden_host(&host.name, cx) && !show_hidden && projects.is_empty() {
                continue;
            }
            sections.push(self.render_host_header(host, cx));
            sections.extend(projects);
        }
        let theme = cx.theme();
        // Nothing in it yet: say what it's for.
        let hint = self.hosts.iter().all(|host| host.tasks.is_empty() && host.loose.is_empty()).then(|| {
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
                .child(div().whitespace_normal().child(
                    "The folders you open stay here. A git repo shows its worktrees under it, each a workspace with its own terminals and Claude Code session.",
                ))
                .child(
                    div()
                        .id("hint-open-folder")
                        .text_color(theme.sidebar_foreground)
                        .hover(|style| style.underline())
                        .child("Open Folder…")
                        .on_click(cx.listener(|this, _, window, cx| this.open_folder(&OpenFolder, window, cx))),
                )
        });
        let weak = cx.entity().downgrade();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("task-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(sections)
                    .children(hint)
                    // The empty space below: right-click to add things.
                    .child(
                        div()
                            .id("task-list-space")
                            .flex_1()
                            .min_h(px(32.))
                            .context_menu(move |menu, window, cx| column_menu(menu, &weak, window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// A project's row, dimmed while hidden: a folder, or a repo's checkout
    /// with its worktrees under it, which its chevron folds away. A click
    /// enters the checkout; folded, the worktree of it used last, and its
    /// dot is the most urgent of all of them.
    fn render_project(&self, project: TaskKey, hidden: bool, group: &[(TaskKey, &TaskInfo)], cx: &mut Context<Self>) -> AnyElement {
        let new_task = self
            .new_task
            .as_ref()
            .filter(|form| form.host == project.host && form.repo == project.path)
            .map(|form| render_new_task(form, cx));
        let checkout = group.iter().find(|(key, _)| *key == project);
        let worktrees: Vec<&(TaskKey, &TaskInfo)> = group.iter().filter(|(key, _)| *key != project).collect();
        let tree = !worktrees.is_empty() || new_task.is_some();
        let folded = tree && Config::get(cx).folded_projects.contains(&project.config());
        let active = match folded {
            true => self.active_project().as_ref() == Some(&project),
            false => checkout.is_some() && self.active.as_ref() == Some(&project),
        };
        let own: Vec<&(TaskKey, &TaskInfo)> = match folded {
            true => group.iter().collect(),
            false => checkout.into_iter().collect(),
        };
        let (dot, color) = own
            .iter()
            .map(|(key, task)| self.status(key, task, cx))
            .filter(|(dot, _)| *dot != "…")
            .max_by_key(|(dot, color)| urgency(dot, *color, cx))
            .unwrap_or(("○", cx.theme().muted_foreground));
        let theme = cx.theme();
        let known = self
            .host(&project.host)
            .is_some_and(|host| host.tasks.iter().any(|other| other.path == project.path));
        let git = self.is_repo(&project);
        let local = project.host == LOCAL;
        let connected = self.client(&project.host).is_some();
        let name = folder_name(&project.path);
        let label: SharedString = name.clone().into();
        let weak = cx.entity().downgrade();
        // Removing it would close its checkout, unsaved changes and all.
        let unsaved = self
            .workspaces
            .get(&project)
            .is_some_and(|workspace| !workspace.read(cx).unsaved().is_empty());

        // On hover, as in its menu: a repo makes a worktree.
        let action = (git && connected).then(|| {
            let (host, repo) = (project.host.clone(), project.path.clone());
            row_action(
                format!("task-new-{}", project.config()),
                "icons/plus.svg",
                format!("New Worktree in {name}…"),
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.start_new_task(host.clone(), repo.clone(), window, cx)
                }),
                cx,
            )
        });
        let chevron = tree.then(|| {
            let path = match folded {
                true => "icons/tree-chevron-right.svg",
                false => "icons/tree-chevron-down.svg",
            };
            let project = project.clone();
            div()
                .id(SharedString::from(format!("project-fold-{}", project.config())))
                .flex_none()
                .child(svg().path(path).size(px(14.)).text_color(theme.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_project_folded(&project, !folded, cx);
                }))
        });

        let row = h_flex()
            .id(SharedString::from(format!("project-{}", project.config())))
            .group("task")
            .h(px(24.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .when(hidden, |row| row.opacity(0.5))
            .when(active, |el| el.bg(selected_row(cx)))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            // Its agents' state, and only the dot.
            .child(div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
            .child(div().flex_1())
            .children(action)
            .children(chevron)
            .on_drag(TaskDrag { key: project.clone(), label }, |drag, _, _, cx| cx.new(|_| DragPreview(drag.label.clone())))
            .drag_over::<TaskDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
            .on_drop(cx.listener({
                let project = project.clone();
                move |this, drag: &TaskDrag, _, cx| this.move_task(&drag.key, &project, cx)
            }))
            .on_click(cx.listener({
                let project = project.clone();
                let enters = checkout.is_some() && !folded;
                move |this, _, window, cx| match enters {
                    true => this.activate(project.clone(), window, cx),
                    false => this.enter_project(project.clone(), window, cx),
                }
            }))
            .tooltip({
                let path = project.path.display().to_string();
                move |window, cx| Tooltip::new(path.clone()).build(window, cx)
            })
            .context_menu({
                let project = project.clone();
                move |menu, window, cx| {
                    let (create, init, add, window_key, copy, finder, hide, close) = (
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                    );
                    menu.when(git, |menu| {
                        menu.item(
                            menu::item(format!("New Worktree in {name}…"), &weak, move |this, window, cx| {
                                this.start_new_task(create.host.clone(), create.path.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .when(known && !git, |menu| {
                        menu.item(
                            menu::item("Initialize Git Repository", &weak, move |this, window, cx| {
                                this.init_git(init.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .when(!known, |menu| {
                        menu.item(
                            menu::item("Add to Workspaces", &weak, move |this, window, cx| {
                                if this.remembers() {
                                    this.add_folder(add.host.clone(), add.path.clone(), window, cx)
                                } else {
                                    this.keep(Some(add.clone()), window, cx)
                                }
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .item(menu::item("Open in New Window", &weak, move |this, _, cx| {
                        let key = window_key.clone();
                        let destination = this.host(&key.host).and_then(|host| host.destination.clone());
                        let except = cx.entity_id();
                        cx.defer(move |cx| open_new_window_except(key.host, destination, key.path, None, Some(except), cx));
                    }))
                    .separator()
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path)).disabled(!local),
                    )
                    .separator()
                    // Its own hiding: one hidden with its server shows with it.
                    .item({
                        let hidden = Config::get(cx).hidden_projects.contains(&hide.config());
                        menu::item(if hidden { "Show Project" } else { "Hide Project" }, &weak, move |this, _, cx| {
                            this.set_project_hidden(&hide, !hidden, cx)
                        })
                    })
                    .item(
                        menu::item("Remove from Workspaces", &weak, move |this, window, cx| {
                            this.set_project_hidden(&close, false, cx);
                            if known {
                                this.remove_folder(&close, close.path.clone(), window, cx)
                            } else {
                                this.close_folder(&close, window, cx)
                            }
                        })
                        .disabled(unsaved),
                    )
                    .separator()
                    .item(show_hidden_item(&weak, cx))
                    .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                }
            });

        // What went wrong with it (its worktrees' under them).
        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| *target == project)
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));
        let rows: Vec<AnyElement> = match tree && !folded {
            true => worktrees.iter().map(|(key, task)| self.render_task(key, task, cx)).chain(new_task).collect(),
            false => Vec::new(),
        };
        let line = cx.theme().sidebar_border;
        v_flex()
            .child(row)
            .children(error)
            .when(!rows.is_empty(), |el| el.child(v_flex().ml(px(18.)).border_l_1().border_color(line).children(rows)))
            .into_any_element()
    }

    /// A workspace's state: being deleted, or the most urgent of its
    /// agents' (waiting for an answer, working, done unseen, idle).
    fn status(&self, key: &TaskKey, _: &TaskInfo, cx: &App) -> (&'static str, Hsla) {
        if self.removing.contains(key) {
            return ("…", cx.theme().muted_foreground);
        }
        let (dot, color, _) = self.workspace_state(key, cx);
        (dot, color)
    }

    /// A workspace's row under its project: its state's dot and its branch;
    /// on hover, Delete Worktree on a worktree.
    fn render_task(&self, key: &TaskKey, task: &TaskInfo, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active.as_ref() == Some(key);
        let (dot, color) = self.status(key, task, cx);
        let theme = cx.theme();
        let known = self
            .host(&key.host)
            .is_some_and(|host| host.tasks.iter().any(|other| other.path == key.path));
        let removable = known && !task.main;
        let local = key.host == LOCAL;
        let weak = cx.entity().downgrade();
        let connected = self.client(&key.host).is_some();
        let repo_name = folder_name(&task.repo);
        // Only a repo makes worktrees.
        let git = known && self.is_repo(&project_of(key, task));
        let label = worktree_label(task);
        // On hover, as in its menu: a worktree is deleted (its project's
        // row makes one).
        let action = if removable && !self.removing.contains(key) {
            let remove = key.clone();
            Some(row_action(
                format!("task-delete-{}", key.config()),
                "icons/trash.svg",
                "Delete Worktree…".into(),
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.ask_remove(remove.clone(), window, cx)
                }),
                cx,
            ))
        } else {
            None
        };

        let row = h_flex()
            .id(SharedString::from(format!("task-{}", key.config())))
            .group("task")
            // On its way out.
            .when(self.removing.contains(key), |row| row.opacity(0.5))
            .h(px(24.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .when(active, |el| el.bg(selected_row(cx)))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            // Its agents' state, the same dot as in the Agents panel, and
            // only the dot: no word for it ("working", "done"…), ever.
            .child(match dot {
                "…" => spinner(color).into_any_element(),
                _ => div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot).into_any_element(),
            })
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
            .child(div().flex_1())
            .children(action)
            .on_drag(
                TaskDrag {
                    key: key.clone(),
                    label: label.clone(),
                },
                |drag, _, _, cx| cx.new(|_| DragPreview(drag.label.clone())),
            )
            .drag_over::<TaskDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
            .on_drop(cx.listener({
                let key = key.clone();
                move |this, drag: &TaskDrag, _, cx| this.move_task(&drag.key, &key, cx)
            }))
            .on_click(cx.listener({
                let key = key.clone();
                move |this, _, window, cx| this.activate(key.clone(), window, cx)
            }))
            .tooltip({
                let path = key.path.display().to_string();
                move |window, cx| Tooltip::new(path.clone()).build(window, cx)
            })
            .context_menu({
                let key = key.clone();
                let repo = task.repo.clone();
                let repo_name = repo_name.clone();
                let branch = task.branch.clone();
                move |menu, window, cx| {
                    let (create, copy, finder, remove) = (key.clone(), key.clone(), key.clone(), key.clone());
                    let repo = repo.clone();
                    let (window_key, branch) = (key.clone(), branch.clone());
                    menu.when(git, |menu| {
                        menu.item(
                            menu::item(format!("New Worktree in {repo_name}…"), &weak, move |this, window, cx| {
                                this.start_new_task(create.host.clone(), repo.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    // What `den -n <path>` does: a window with only it (and
                    // its server).
                    .item(menu::item("Open in New Window", &weak, move |this, _, cx| {
                        let key = window_key.clone();
                        let destination = this.host(&key.host).and_then(|host| host.destination.clone());
                        let except = cx.entity_id();
                        cx.defer(move |cx| open_new_window_except(key.host, destination, key.path, None, Some(except), cx));
                    }))
                    .separator()
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .when_some(branch, |menu, branch| {
                        menu.item(menu::item("Copy Branch Name", &weak, move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()))
                        }))
                    })
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path))
                            .disabled(!local),
                    )
                    .when(removable, |menu| {
                        menu.separator().item(menu::item("Delete Worktree…", &weak, move |this, window, cx| {
                            this.ask_remove(remove.clone(), window, cx)
                        }))
                    })
                    .separator()
                    .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                }
            });


        // Under its row.
        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| target == key)
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));

        v_flex().child(row).children(error).into_any_element()
    }
}

/// Turning while something takes a while: a worktree made or deleted, a test
/// starting to be debugged.
pub(crate) fn spinner(color: Hsla) -> impl IntoElement {
    svg()
        .path("icons/loader.svg")
        .size(px(12.))
        .flex_none()
        .text_color(color)
        .with_animation("spinner", Animation::new(Duration::from_millis(900)).repeat(), |svg, delta| {
            svg.with_transformation(Transformation::rotate(percentage(delta)))
        })
}

/// How much a task's dot (see `Den::status`) asks to be looked at: waiting
/// for an answer, working, finished unseen, or nothing.
fn urgency(dot: &str, color: Hsla, cx: &App) -> u8 {
    match dot {
        "●" if color == cx.theme().danger => 3,
        "◐" => 2,
        "●" => 1,
        _ => 0,
    }
}

impl Render for Den {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = self.active_workspace() {
            let width = window.viewport_size().width - px(ACTIVITY_WIDTH);
            let branch = self.active.as_ref().and_then(|key| self.task(key)).and_then(|task| task.branch.clone());
            let panel = self.workspaces_panel.clone();
            let agents = self.agents_panel.clone();
            let badges = self.task_badges(cx);
            workspace.update(cx, |workspace, cx| {
                workspace.set_width(width, cx);
                workspace.set_branch(branch, cx);
                workspace.set_workspaces(&panel);
                workspace.set_agents(&agents);
                workspace.set_badges(badges, cx);
            });
        }
        let title = self.active.as_ref().map(|key| self.label(key)).unwrap_or_else(|| self.title());
        let color = self.active.as_ref().map(workspace_color);
        v_flex()
            .id("den")
            .key_context("Den")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_ui(cx)
            .on_action(cx.listener(Self::toggle_tasks))
            .on_action(cx.listener(|this, _: &ResetLayout, _, cx| {
                menu::reset_layout_now(cx);
                if let Some(workspace) = this.active_workspace() {
                    workspace.update(cx, |workspace, cx| workspace.reset_layout(cx));
                }
            }))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::open_remote_folder))
            .on_action(cx.listener(|this, _: &AddServer, window, cx| this.open_host_picker(window, cx)))
            .on_action(cx.listener(Self::open_recent))
            .on_action(cx.listener(Self::new_task_action))
            .on_action(cx.listener(Self::open_task_picker))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| this.open_command_palette(window, cx)))
            .on_action(cx.listener(|this, _: &ShowShortcuts, window, cx| this.open_command_palette(window, cx)))
            .on_action(cx.listener(Self::previous_task))
            .on_action(cx.listener(|this, _: &NextActiveTask, window, cx| this.next_task(true, window, cx)))
            .on_action(cx.listener(|this, _: &NextTask, window, cx| this.next_task(false, window, cx)))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                this.switcher_modifiers(&event.modifiers, window, cx)
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| this.switcher_key_up(event)))

            .on_action(cx.listener(|this, _: &ShowWelcome, window, cx| this.show_welcome(window, cx)))
            .relative()
            // Our own bar, in the theme's color (macOS's is gray): the traffic
            // lights on the left, the active task in the middle, and it drags
            // and zooms like the system one.
            .child(
                TitleBar::new()
                    // Windows and Linux: the menus, which only macOS draws itself.
                    .children(crate::app_menu::bar(cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_center()
                            // Centered on the window: as much on the right
                            // as the traffic lights take on the left.
                            .pr(px(80.))
                            .items_center()
                            .gap_2()
                            .text_ui(cx)
                            .text_color(cx.theme().muted_foreground)
                            .children(color.map(|color| div().flex_none().size(px(8.)).rounded_full().bg(color)))
                            .child(title),
                    )
                    .when(self.unkept(cx), |bar| {
                        bar.child(
                            div()
                                .id("keep-in-workspaces")
                                .flex_none()
                                .mr_2()
                                .px_2()
                                .rounded(cx.theme().radius)
                                .text_ui_small(cx)
                                .text_color(cx.theme().muted_foreground)
                                .hover(|style| style.bg(cx.theme().secondary_hover).text_color(cx.theme().foreground))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child("Keep in Workspaces")
                                .tooltip(|window, cx| {
                                    Tooltip::new("Opened with den -s, this window is forgotten when it closes: keep its server and folders in Workspaces.")
                                        .build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| this.keep(None, window, cx))),
                        )
                    })
                    .children(cx.try_global::<crate::update::Updates>().and_then(|updates| updates.ready().map(str::to_string)).map(|version| {
                        div()
                            .id("restart-to-update")
                            .flex_none()
                            .mr_2()
                            .px_2()
                            .rounded(cx.theme().radius)
                            .text_ui_small(cx)
                            .text_color(cx.theme().primary)
                            .hover(|style| style.bg(cx.theme().secondary_hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(format!("Restart to update to {version}"))
                            .tooltip(|window, cx| {
                                Tooltip::new("Installed: it asks before restarting, and everything reopens as it was.").build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| this.ask_update(version.clone().into(), window, cx)))
                    }))
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(match self.active_workspace() {
                        Some(workspace) => workspace.into_any_element(),
                        None => self.render_without_workspace(window, cx),
                    })
                    .children(self.render_notice(window, cx)),
            )
            .children(self.task_picker.as_ref().or(self.command_palette.as_ref()).or(self.recent_picker.as_ref()).map(|(picker, _)| {
                div()
                    .absolute()
                    .top(px(44.))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(picker.clone())
            }))
            .children(self.settings.as_ref().map(|settings| self.render_settings(settings, cx)))
            .children(self.render_switcher(cx))
            .children(
                self.host_picker
                    .as_ref()
                    .map(|(picker, _)| picker.clone().into_any_element())
                    .or_else(|| self.folder_picker.as_ref().map(|(picker, _)| picker.clone().into_any_element()))
                    .map(|picker| div().absolute().top(px(44.)).left_0().right_0().flex().justify_center().child(picker)),
            )
            .children(self.about.as_ref().map(|focus| self.render_about(focus, cx)))
            .children(self.confirm_remove.as_ref().map(|(key, focus, at_risk)| self.render_confirm_remove(key, focus, at_risk.as_ref(), cx)))
            .children(
                self.confirm_force_remove
                    .as_ref()
                    .map(|(key, focus, reason)| self.render_confirm_force_remove(key, focus, reason, cx)),
            )
            .children(self.confirm_restart.as_ref().map(|(name, focus)| self.render_confirm_restart(name, focus, cx)))
            .children(self.confirm_update.as_ref().map(|(version, focus)| self.render_confirm_update(version, focus, cx)))
            .children(self.quit_confirm.as_ref().map(|(focus, closing)| self.render_quit_confirm(focus, *closing, cx)))
    }
}

/// A dialog's secondary button.
fn dialog_button(id: &'static str, label: &'static str, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .hover(|style| style.bg(theme.secondary_hover))
        .child(label)
}

fn error_text(error: SharedString, cx: &App) -> impl IntoElement {
    div()
        .text_ui_small(cx)
        .text_color(cx.theme().danger)
        .whitespace_normal()
        .child(error)
}

/// What `den -s` opens for `path` on a server (see `proto::open_target`):
/// a folder as it is, a file in its repo. None if it isn't there.
async fn remote_target(client: &Client, path: PathBuf) -> Option<(PathBuf, Option<PathBuf>)> {
    let list = |dir: &Path| client.request(Request::ListDir { path: dir.to_path_buf() });
    let entries = |response: anyhow::Result<Response>| match response {
        Ok(Response::Dir(entries)) => Some(entries),
        _ => None,
    };
    let parent = path.parent()?.to_path_buf();
    let name = path.file_name()?.to_string_lossy().into_owned();
    let entry = entries(list(&parent).await)?.into_iter().find(|entry| entry.name == name)?;
    if entry.is_dir {
        return Some((path, None));
    }
    for dir in parent.ancestors() {
        if entries(list(dir).await).is_some_and(|entries| entries.iter().any(|entry| entry.name == ".git")) {
            return Some((dir.to_path_buf(), Some(path)));
        }
    }
    Some((parent, Some(path)))
}

pub async fn list_tasks(client: &Client) -> anyhow::Result<Vec<TaskInfo>> {
    match client.request(Request::TaskList).await? {
        Response::Tasks(tasks) => Ok(tasks),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// The column's + menu: what can be added to it.
fn add_menu_items(menu: PopupMenu, den: &WeakEntity<Den>) -> PopupMenu {
    menu.item(menu::item("Open Folder…", den, |this, window, cx| this.open_folder(&OpenFolder, window, cx)))
        .item(menu::item("Open Folder on Server…", den, |this, window, cx| {
            this.open_remote_folder(&OpenRemoteFolder, window, cx)
        }))
        .separator()
        .item(menu::item("Add Server…", den, |this, window, cx| this.open_host_picker(window, cx)))
}

/// Right-click on the tasks column's empty space or its title.
fn column_menu(menu: PopupMenu, den: &WeakEntity<Den>, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
    add_menu_items(menu, den)
        .separator()
        .item(show_hidden_item(den, cx))
        .panel_items(hide_panel(den, Panel::Workspaces), window, cx)
}

/// Right-click on a server's name in the tasks column, before Hide Panel.
/// With `keep`, the window's server and folders aren't remembered yet (see
/// `Den::keep`).
fn host_menu(menu: PopupMenu, name: &SharedString, connected: bool, keep: bool, den: &WeakEntity<Den>) -> PopupMenu {
    if name == LOCAL {
        return menu
            .item(menu::item("Open Folder…", den, |this, window, cx| this.open_folder(&OpenFolder, window, cx)))
            .separator();
    }
    let (open, reconnect, remove) = (name.clone(), name.clone(), name.clone());
    menu.when(keep, |menu| {
        menu.item(menu::item("Keep in Workspaces", den, |this, window, cx| this.keep(None, window, cx)))
            .separator()
    })
    .item(
        menu::item(format!("Open Folder on {name}…"), den, move |this, window, cx| {
            this.open_folder_picker(open.clone(), window, cx)
        })
        .disabled(!connected),
    )
    .separator()
    .item(menu::item("Reconnect", den, move |this, window, cx| this.connect(reconnect.clone(), window, cx)))
    .item(menu::item("Remove Server", den, move |this, window, cx| this.remove_host(remove.clone(), window, cx)))
    .separator()
}

/// Open Folder or Add Server: the workspaces' header's button.
fn tasks_add_button(den: &WeakEntity<Den>) -> impl IntoElement {
    let den = den.clone();
    Button::new("tasks-add")
        .ghost()
        .xsmall()
        .icon(Icon::default().path("icons/plus.svg"))
        .tooltip("Open Folder or Add Server")
        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| add_menu_items(menu, &den))
}

/// Hide Panel, at the end of every menu in the Workspaces
/// panels: takes `panel` off the side column.
fn hide_panel(den: &WeakEntity<Den>, panel: Panel) -> menu::PopupMenuItem {
    menu::item("Hide Panel", den, move |this, _, cx| {
        if let Some(workspace) = this.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.remove_panel(panel, cx));
        }
    })
}

/// `host` is hidden with its projects (Hide Server).
fn is_hidden_host(host: &SharedString, cx: &App) -> bool {
    Config::get(cx).hidden_hosts.iter().any(|other| other == host.as_ref())
}

/// Show Hidden Projects, checked while the Workspaces panel shows them.
fn show_hidden_item(den: &WeakEntity<Den>, cx: &App) -> menu::PopupMenuItem {
    menu::item("Show Hidden Projects", den, |this, _, cx| this.toggle_hidden_projects(cx))
        .checked(Config::get(cx).show_hidden_projects)
}

/// The project a workspace is of: its repo's checkout, or its folder.
fn project_of(key: &TaskKey, task: &TaskInfo) -> TaskKey {
    TaskKey { host: key.host.clone(), path: task.repo.clone() }
}

/// A workspace's name under its project: its branch, or its folder.
fn worktree_label(task: &TaskInfo) -> SharedString {
    match &task.branch {
        Some(branch) => branch.clone().into(),
        None => folder_name(&task.path).into(),
    }
}

/// In Open Recent: the path (`~/…` locally) and, on a server, its name.
fn recent_label(key: &TaskKey) -> String {
    let path = match std::env::home_dir().and_then(|home| key.path.strip_prefix(&home).ok().map(Path::to_path_buf)) {
        Some(rest) if key.host == LOCAL => format!("~/{}", rest.display()),
        _ => key.path.display().to_string(),
    };
    if key.host == LOCAL { path } else { format!("{}: {path}", key.host) }
}

/// A workspace's name in the lists: its repo and branch (`scl / agente`),
/// or the folder of one that isn't a repo.
fn row_label(task: &TaskInfo) -> SharedString {
    let repo = folder_name(&task.repo);
    match &task.branch {
        Some(branch) => format!("{repo} / {branch}").into(),
        None if !task.main => format!("{repo} / {}", folder_name(&task.path)).into(),
        None => folder_name(&task.path).into(),
    }
}

/// A workspace's icon: a folder, or a branch for a repo's worktree.
/// A workspace's name in the column: a worktree goes by its branch, what
/// its agent works on (its folder usually repeats the repo's name); a
/// checkout by its folder.
fn column_label(task: &TaskInfo) -> SharedString {
    match (&task.branch, task.main) {
        (Some(branch), false) => branch.clone().into(),
        _ => folder_name(&task.path).into(),
    }
}

/// A workspace's own color, to tell it at a glance: one of a few hues
/// apart from the states' red and green, picked by its server and path, so
/// it keeps it however the column is ordered.
fn workspace_color(key: &TaskKey) -> Hsla {
    const HUES: [f32; 6] = [215., 172., 265., 42., 330., 290.];
    // FNV-1a: the same on every run and every Rust.
    let hash = key.host.bytes().chain([0]).chain(key.path.as_os_str().as_encoded_bytes().iter().copied()).fold(
        0xcbf29ce484222325u64,
        |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100000001b3),
    );
    hsla(HUES[(hash % HUES.len() as u64) as usize] / 360., 0.6, 0.55, 1.)
}

fn kind_icon(task: &TaskInfo) -> &'static str {
    if task.main { "icons/folder.svg" } else { "icons/git-branch.svg" }
}

/// The worktree being named: a row at the end of its repo's worktrees, like
/// a new file in the tree.
fn render_new_task(form: &NewTaskInput, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let name = form.input.read(cx).value().trim().to_string();
    v_flex()
        .child(
            h_flex()
                .h(px(26.))
                .px_3()
                .gap_2()
                .text_ui(cx)
                .child(svg().path("icons/git-branch.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                .child(if form.busy {
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .text_color(theme.muted_foreground)
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(name))
                        .child(spinner(theme.muted_foreground))
                        .into_any_element()
                } else {
                    div().flex_1().min_w_0().child(Input::new(&form.input).xsmall()).into_any_element()
                }),
        )
        .children(form.error.clone().map(|error| div().px_3().pb_1().child(error_text(error, cx))))
        .into_any_element()
}

/// A button at the end of a workspace's row, shown while it's hovered.
fn row_action(
    id: String,
    icon: &'static str,
    tip: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    div()
        .id(SharedString::from(id))
        .flex_none()
        .size(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .invisible()
        .group_hover("task", |style| style.visible())
        .rounded(theme.radius)
        .hover(|style| style.bg(theme.sidebar_accent))
        .child(svg().path(icon).size(px(13.)).text_color(theme.muted_foreground))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(on_click)
        .into_any_element()
}

/// An open folder that doesn't belong to any known repo.
fn loose_task(root: &Path) -> TaskInfo {
    TaskInfo {
        repo: root.to_path_buf(),
        path: root.to_path_buf(),
        branch: None,
        main: true,
        working: false,
    }
}

/// A small icon button.
fn icon_button(id: impl Into<ElementId>, icon: &'static str, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .flex_none()
        .p_1()
        .rounded(theme.radius)
        .text_color(theme.muted_foreground)
        .hover(|style| style.bg(theme.sidebar_accent).text_color(theme.sidebar_foreground))
        .child(svg().path(icon).size(px(14.)).text_color(theme.muted_foreground))
}

/// The `Host`s in `~/.ssh/config` (excluding those with wildcards).
fn ssh_hosts() -> Vec<String> {
    let config = std::env::home_dir()
        .and_then(|home| std::fs::read_to_string(home.join(".ssh/config")).ok())
        .unwrap_or_default();
    parse_ssh_hosts(&config)
}

fn parse_ssh_hosts(config: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for line in config.lines() {
        let mut words = line.split_whitespace();
        if !words.next().is_some_and(|word| word.eq_ignore_ascii_case("host")) {
            continue;
        }
        for host in words {
            if !host.contains(['*', '?', '!']) && !hosts.iter().any(|known| known == host) {
                hosts.push(host.to_string());
            }
        }
    }
    hosts
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Labels to pick from, each with its path if another has the same (two
/// folders called `api`): which is picked is known by its label.
fn distinct_labels<'a>(labels: impl Iterator<Item = (String, &'a Path)>) -> Vec<String> {
    let labels: Vec<(String, &Path)> = labels.collect();
    labels
        .iter()
        .map(|(label, path)| match labels.iter().filter(|(other, _)| other == label).count() {
            1 => label.clone(),
            _ => format!("{label} · {}", path.display()),
        })
        .collect()
}

/// The checkout's folder name, or `repo/folder` for a worktree.
fn task_label(task: &TaskInfo) -> String {
    let repo = folder_name(&task.repo);
    if task.path == task.repo { repo } else { format!("{repo}/{}", folder_name(&task.path)) }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::Path};

    use gpui_kit::component::highlighter::SyntaxColors;

    #[test]
    fn workspaces_with_the_same_name_are_told_apart() {
        let paths = [Path::new("/work/api"), Path::new("/personal/api"), Path::new("/work/web")];
        let labels = super::distinct_labels(["api", "api", "web"].into_iter().map(String::from).zip(paths));
        assert_eq!(labels, ["api · /work/api", "api · /personal/api", "web"]);
    }

    #[test]
    fn ssh_config_hosts() {
        let config = "Host ws com\n  HostName 1.2.3.4\nhost *.internal bill\nHost ws\nMatch all\nHost !x v3\n";
        assert_eq!(super::parse_ssh_hosts(config), vec!["ws", "com", "bill", "v3"]);
    }

    #[test]
    fn code_colors_parse() {
        let colors: HashMap<String, SyntaxColors> =
            serde_json::from_str(include_str!("../assets/themes/vscode-2026.json")).unwrap();
        for mode in ["light", "dark"] {
            let syntax = &colors[mode];
            assert!(syntax.keyword.is_some() && syntax.string.is_some() && syntax.title.is_some(), "{mode}");
        }
    }
}

#[cfg(test)]
mod palette_tests {
    use core::prelude::v1::test;

    use gpui_kit::*;

    use super::Den;
    use crate::{config::Config, picker::PickerEvent};

    #[gpui_kit::test]
    fn removing_a_server_preserves_unsaved_files_until_discard_is_explicit(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        let key = super::TaskKey { host: "remote".into(), path: "/remote-project".into() };
        cx.update(|window, cx| {
            let workspace = crate::workspace::autosave_tests::dirty_workspace(key.path.clone(), window, cx);
            den.update(cx, |den, cx| {
                den.workspaces.insert(key.clone(), workspace);
                den.remove_host(key.host.clone(), window, cx);
            });
        });
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.workspaces.contains_key(&key)));
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, cx| !den.unsaved(cx).is_empty()));

        cx.update(|window, cx| den.update(cx, |den, cx| den.remove_host(key.host.clone(), window, cx)));
        cx.run_until_parked();
        cx.simulate_prompt_answer("Save and Remove");
        cx.run_until_parked();
        // There is no connection: saving fails, so all unsaved text remains.
        assert!(den.read_with(cx, |den, cx| den.workspaces.contains_key(&key) && !den.unsaved(cx).is_empty()));

        cx.update(|window, cx| den.update(cx, |den, cx| den.remove_host(key.host.clone(), window, cx)));
        cx.run_until_parked();
        cx.simulate_prompt_answer("Discard Changes");
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| !den.workspaces.contains_key(&key)));
    }

    /// A hidden project is left out of Cmd-E and Cmd-K, unless it's in
    /// front; entering a project goes to the worktree of it used last.
    #[gpui_kit::test]
    fn hidden_projects_and_entering_one(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        let task = |repo: &str, path: &str, main: bool| proto::TaskInfo {
            repo: repo.into(),
            path: path.into(),
            branch: Some(if main { "master".into() } else { path.rsplit('/').next().unwrap().into() }),
            main,
            working: false,
        };
        let key = |path: &str| super::TaskKey { host: super::LOCAL.into(), path: path.into() };
        cx.update(|window, cx| {
            den.update(cx, |den, cx| {
                den.hosts[0].tasks = vec![task("/den", "/den", true), task("/den", "/den-hide", false), task("/stride", "/stride", true)];
                den.activate(key("/den-hide"), window, cx);
                den.set_project_hidden(&key("/stride"), true, cx);
            })
        });
        let paths = |den: &Den, cx: &App| den.ordered(cx).into_iter().map(|(key, _)| key.path).collect::<Vec<_>>();
        den.read_with(cx, |den, cx| {
            assert_eq!(paths(den, cx), [std::path::PathBuf::from("/den"), "/den-hide".into()]);
            assert_eq!(den.ordered_all(cx).len(), 3);
            assert!(den.is_repo(&key("/den")));
        });
        // A repo folds its worktrees away, and unfolds them again.
        cx.update(|_, cx| {
            den.update(cx, |den, cx| {
                den.set_project_folded(&key("/den"), true, cx);
                assert_eq!(Config::get(cx).folded_projects, [key("/den").config()]);
                den.set_project_folded(&key("/den"), false, cx);
                assert!(Config::get(cx).folded_projects.is_empty());
            })
        });
        // In front, it's gone through as any other.
        cx.update(|window, cx| den.update(cx, |den, cx| den.enter_project(key("/stride"), window, cx)));
        den.read_with(cx, |den, cx| {
            assert_eq!(den.active, Some(key("/stride")));
            assert_eq!(paths(den, cx).len(), 3);
        });
        // Back to den: its worktree, not its checkout.
        cx.update(|window, cx| den.update(cx, |den, cx| den.enter_project(key("/den"), window, cx)));
        den.read_with(cx, |den, _| assert_eq!(den.active, Some(key("/den-hide"))));
        // A hidden server hides all its projects, but the one in front.
        cx.update(|window, cx| {
            den.update(cx, |den, cx| {
                den.set_project_hidden(&key("/stride"), false, cx);
                den.set_host_hidden(&super::LOCAL.into(), true, cx);
                den.activate(key("/den-hide"), window, cx);
            })
        });
        den.read_with(cx, |den, cx| {
            assert_eq!(paths(den, cx), [std::path::PathBuf::from("/den"), "/den-hide".into()]);
            assert!(den.is_hidden_project(&key("/den"), cx));
        });
    }

    /// Settings from the command palette: the action reaches `Den` itself,
    /// which must not be in the middle of an update then.
    #[gpui_kit::test]
    fn the_palette_opens_settings(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
            super::register_app_actions(cx);
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        as_app_window(&den, cx);
        cx.update(|window, cx| {
            den.update(cx, |den, cx| {
                den.focus_handle.focus(window, cx);
                den.open_command_palette(window, cx);
            })
        });
        cx.run_until_parked();
        let picker = den.read_with(cx, |den, _| den.command_palette.as_ref().map(|(picker, _)| picker.clone()).unwrap());
        picker.update(cx, |_, cx| cx.emit(PickerEvent::Pick("Settings".into())));
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.settings.is_some()));
    }

    /// One picker at a time: the one drawn is the one with the focus.
    #[gpui_kit::test]
    fn opening_a_picker_closes_the_others(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        cx.update(|window, cx| {
            den.update(cx, |den, cx| {
                den.open_task_picker(&crate::OpenTaskPicker, window, cx);
                den.open_command_palette(window, cx);
            })
        });
        assert!(den.read_with(cx, |den, _| den.task_picker.is_none() && den.command_palette.is_some()));
    }

    /// Going to an unsaved file from the quit dialog cancels quitting, as
    /// Cancel does: a window that went on without saving asks again.
    #[gpui_kit::test]
    fn going_to_an_unsaved_file_cancels_quitting(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        let key = super::TaskKey { host: super::LOCAL.into(), path: "/unsaved-project".into() };
        cx.update(|window, cx| {
            den.update(cx, |den, cx| {
                den.discarded = true;
                den.quit_confirm = Some((cx.focus_handle(), super::Closing::App));
                den.go_to_unsaved(key, "main.rs".into(), window, cx);
            })
        });
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.quit_confirm.is_none() && !den.discarded));
    }

    /// Counts `den`'s window among the app's, as opening it does.
    fn as_app_window(den: &Entity<Den>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            let open = super::OpenWindow { handle: window.window_handle(), den: den.downgrade(), server: None };
            cx.default_global::<super::Main>().windows.push(open);
        });
    }

    /// Settings and About are the app's: they work from the menu with
    /// nothing focused, and from a key in the window, without opening twice.
    #[gpui_kit::test]
    fn the_den_menu_works_without_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
            crate::update::init(cx);
            super::register_app_actions(cx);
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        as_app_window(&den, cx);
        cx.update(|_, cx| cx.dispatch_action(&crate::About));
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.about.is_some()));
    }

    /// Check for Updates opens About; a build that isn't installed says so
    /// instead of asking GitHub.
    #[gpui_kit::test]
    fn check_for_updates_opens_about(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
            crate::update::init(cx);
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        cx.update(|window, cx| den.update(cx, |den, cx| den.check_for_updates(window, cx)));
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.about.is_some()));
        assert!(cx.update(|_, cx| crate::update::status(cx) == crate::update::Status::NotInstalled));
    }

    /// Restart to update asks first; cancelling leaves everything as it was.
    #[gpui_kit::test]
    fn restarting_to_update_asks_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (den, cx) = cx.add_window_view(|window, cx| Den::new(None, None, false, None, window, cx));
        cx.update(|window, cx| den.update(cx, |den, cx| den.ask_update("9.9.9".into(), window, cx)));
        cx.run_until_parked();
        assert!(den.read_with(cx, |den, _| den.confirm_update.is_some()));
        cx.update(|window, cx| den.update(cx, |den, cx| den.cancel_confirm(window, cx)));
        assert!(den.read_with(cx, |den, _| den.confirm_update.is_none()));
    }

    /// Checking for updates is on unless turned off.
    #[test]
    fn checks_for_updates_unless_turned_off() {
        let config: Config = serde_json::from_str("{}").unwrap();
        assert!(config.checks_for_updates());
        let config: Config = serde_json::from_str(r#"{"check_for_updates": false}"#).unwrap();
        assert!(!config.checks_for_updates());
    }
}
