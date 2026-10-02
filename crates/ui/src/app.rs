//! The sik window: the open folder's workspace and, optionally, the tasks
//! column, grouped by server. Any folder can be opened; a task is a folder
//! that is a worktree of a known repo. Each open folder has its own
//! workspace, which is kept when switching from one to another. With none
//! open, a welcome screen offers to open one.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
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
use proto::{Event, GitOp, Request, Response, TaskInfo};

use crate::{
    About, CheckForUpdates, NewTask, OpenCommandPalette, OpenShortcutsGuide, OpenFolder, OpenRecent, OpenRemoteFolder, OpenSettings, OpenTaskPicker,
    PreviousTask, ShowShortcuts, ShowWelcome, ToggleTasks,
    config::{self, Config, HostConfig, Panel, SavedTask, SavedWindow, TextArea, ThemeChoice, UiText},
    menu,
    folder_picker::{FolderPicker, FolderPickerEvent},
    picker::{Picker, PickerEvent},
    shortcuts::{self, SHORTCUTS},
    workspace::{ACTIVITY_WIDTH, Badge, OnActivity, TaskBadges, Workspace, WorkspacesPanel, activity_bar},
};

mod about;
mod commands;
mod confirm;
mod settings;
mod theme;
mod welcome;

/// How often the task list is re-read (worktrees created elsewhere).
const REFRESH: Duration = Duration::from_secs(5);

/// Name of this machine in the tasks column.
pub const LOCAL: &str = "local";

/// Width of the fold arrow at the end of a repo with worktrees.
const FOLD_WIDTH: f32 = 12.;

/// How far a server's workspaces sit in from its name.
const ROW_INDENT: f32 = 24.;

/// What a server shows while connecting, unless it's doing something longer.
const CONNECTING: &str = "connecting…";

/// How many folders Open Recent remembers.
const RECENT: usize = 20;

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

/// The window, and the local agent it talks to (kept for opening it again
/// after it's closed: the app goes on without it on macOS).
#[derive(Default)]
struct Main {
    window: Option<(AnyWindowHandle, WeakEntity<Sik>)>,
    agent: Option<Arc<Client>>,
}

impl Global for Main {}

/// The local agent, for the window.
pub fn set_agent(agent: Option<Arc<Client>>, cx: &mut App) {
    cx.default_global::<Main>().agent = agent;
}

/// The window, while it's open.
fn main_window(cx: &App) -> Option<(AnyWindowHandle, Entity<Sik>)> {
    let (handle, sik) = cx.try_global::<Main>()?.window.as_ref()?;
    Some((*handle, sik.upgrade()?))
}

/// Opens the window with `root` and `file` in it; with no `root`, the last
/// workspace if `resume`, or the welcome screen.
pub fn open_window(root: Option<PathBuf>, file: Option<PathBuf>, resume: bool, cx: &mut App) {
    let agent = cx.default_global::<Main>().agent.clone();
    let title = match &root {
        Some(root) => folder_name(root),
        None => "sik".to_string(),
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
        cx.new(|cx| Sik::new(root.clone(), file.clone(), resume, agent, window, cx))
    });
    match opened {
        Ok((handle, sik)) => cx.default_global::<Main>().window = Some((handle, sik.downgrade())),
        Err(err) => eprintln!("could not open the window: {err:#}"),
    }
}

/// Clicking the Dock icon with the window closed opens it again.
pub fn reopen(cx: &mut App) {
    if main_window(cx).is_none() {
        open_window(None, None, true, cx);
    }
}

/// Whether the window is open: the `sik` commands run in it.
pub fn has_window(cx: &App) -> bool {
    main_window(cx).is_some()
}

/// `sik <path>` in a terminal of `host`: `root` as a workspace, with `file`
/// open in it, and the window to the front (opened if it was closed).
pub fn handle_open(host: SharedString, root: PathBuf, file: Option<PathBuf>, cx: &mut App) {
    match main_window(cx) {
        Some((handle, sik)) => {
            handle
                .update(cx, |_, window, cx| {
                    sik.update(cx, |sik, cx| sik.open_from_terminal(host, root, file, window, cx));
                    window.activate_window();
                })
                .ok();
        }
        None if host == LOCAL => open_window(Some(root), file, false, cx),
        None => {
            open_window(None, None, false, cx);
            if let Some((handle, sik)) = main_window(cx) {
                handle
                    .update(cx, |_, window, cx| {
                        sik.update(cx, |sik, cx| sik.open_from_terminal(host, root, file, window, cx))
                    })
                    .ok();
            }
        }
    }
    cx.activate(true);
}

/// Cmd-Q: quits once there are no unsaved files, or they're saved.
pub fn quit(cx: &mut App) {
    if let Some((handle, sik)) = main_window(cx) {
        let ready = handle
            .update(cx, |_, window, cx| sik.update(cx, |sik, cx| sik.confirm_quit(window, cx)))
            .unwrap_or(true);
        if !ready {
            return;
        }
    }
    crate::update::relaunch_if_restarting(cx);
    cx.quit();
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

pub struct Sik {
    hosts: Vec<Host>,
    active: Option<TaskKey>,
    workspaces: HashMap<TaskKey, Entity<Workspace>>,
    /// Tasks that finished working without being looked at.
    attention: HashSet<TaskKey>,
    /// Tasks waiting for an answer (Claude is asking something): in red.
    blocked: HashSet<TaskKey>,
    new_task: Option<NewTaskInput>,
    /// Worktree whose deletion is being confirmed, in a dialog.
    confirm_remove: Option<(TaskKey, FocusHandle)>,
    /// Server whose agent is about to be restarted, confirming in a dialog.
    confirm_restart: Option<(SharedString, FocusHandle)>,
    /// The update about to be restarted into, confirming in a dialog.
    confirm_update: Option<(SharedString, FocusHandle)>,
    removing: HashSet<TaskKey>,
    /// Last error from a column action, with the task it affects.
    error: Option<(TaskKey, SharedString)>,
    /// File to open in the first workspace (`sik file`).
    open_file: Option<PathBuf>,
    /// Last task from the previous session, on a server not yet connected:
    /// it's entered on connecting (unless another was opened first).
    pending_last: Option<TaskKey>,
    /// The task before the active one, for Cmd-E.
    previous: Option<TaskKey>,
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
    /// Quit dialog with unsaved files (its focus, for Esc and Enter).
    quit_confirm: Option<FocusHandle>,
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
    /// Settings, if open.
    settings: Option<settings::Settings>,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Sik {
    pub fn new(
        root: Option<PathBuf>,
        open_file: Option<PathBuf>,
        resume: bool,
        client: Option<Arc<Client>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let bounds = cx.observe_window_bounds(window, |_, window, cx| {
            let saved = SavedWindow::from_bounds(window.window_bounds());
            Config::update_quietly(cx, |config| config.window = saved);
        });
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            if Config::get(cx).theme == ThemeChoice::System {
                Theme::sync_system_appearance(Some(window), cx);
                Self::apply_font_sizes(cx);
            }
        });
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
            .map(|host| Host {
                name: host.name.clone().into(),
                destination: Some(host.destination.clone()),
                client: None,
                status: HostStatus::Connecting(CONNECTING),
                tasks: Vec::new(),
                loose: Vec::new(),
                repos: Vec::new(),
                generation: 0,
            })
            .collect();
        let mut this = Self {
            hosts: std::iter::once(local).chain(remotes).collect(),
            active: None,
            workspaces: HashMap::new(),
            attention: HashSet::new(),
            blocked: HashSet::new(),
            new_task: None,
            confirm_remove: None,
            confirm_restart: None,
            confirm_update: None,
            removing: HashSet::new(),
            error: None,
            open_file,
            pending_last: None,
            previous: None,
            task_picker: None,
            command_palette: None,
            recent_picker: None,
            host_picker: None,
            folder_picker: None,
            quit_confirm: None,
            quit_saving: false,
            about: None,
            guide: None,
            split: config::Split::new(cx),
            workspaces_panel: {
                let sik = cx.entity().downgrade();
                cx.new(|_| {
                    WorkspacesPanel::new(move |_, cx| {
                        sik.update(cx, |sik, cx| sik.render_column(cx).into_any_element())
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                })
            },
            settings: None,
            focus_handle: cx.focus_handle(),
            _tasks: Vec::new(),
            _subscriptions: vec![appearance, bounds],
        };
        Self::install_theme(cx);
        this.apply_theme(window, cx);

        let sik = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            sik.update(cx, |sik, cx| sik.confirm_quit(window, cx)).unwrap_or(true)
        });

        for name in this.hosts.iter().skip(1).map(|host| host.name.clone()).collect::<Vec<_>>() {
            this.connect(name, window, cx);
        }

        let refresh = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |this, cx| this.refresh_all(cx)).is_err() {
                    break;
                }
            }
        });
        this._tasks.push(refresh);

        let Some(client) = client else {
            match root {
                Some(root) => this.activate(TaskKey { host: LOCAL.into(), path: root }, window, cx),
                None => this.focus_handle.focus(window, cx),
            }
            return this;
        };
        this.watch_host(LOCAL.into(), client.clone(), window, cx);
        this.track(LOCAL.into(), &client, window, cx);

        // Reads the tasks and enters the one containing the folder, if any;
        // otherwise the folder on its own.
        let startup = cx.spawn_in(window, async move |this, cx| {
            let tasks = list_tasks(&client).await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                this.hosts[0].tasks = tasks;
                let task = root.as_ref().and_then(|root| {
                    this.hosts[0]
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
                    (None, Some(last)) if last.host != LOCAL && this.host(&last.host).is_some() => {
                        let key = TaskKey { host: last.host.into(), path: last.path };
                        if this.client(&key.host).is_some() {
                            this.activate(key, window, cx);
                        } else {
                            this.pending_last = Some(key);
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
                            this.focus_handle.focus(window, cx);
                            return;
                        }
                    },
                };
                this.activate(key, window, cx);
            })
            .ok();
        });
        this._tasks.push(startup);
        this
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
        let agents = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
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
        host.client = Some(client.clone());
        host.status = HostStatus::Connected;
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
        self.refresh_all(cx);
        cx.notify();
    }

    /// Finds out when the connection to the server is lost.
    fn track(&mut self, name: SharedString, client: &Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, rx) = smol::channel::bounded::<()>(1);
        client.on_disconnect(move || {
            let _ = tx.try_send(());
        });
        cx.spawn_in(window, async move |this, cx| {
            if rx.recv().await.is_ok() {
                this.update_in(cx, |this, window, cx| this.lost(name, window, cx)).ok();
            }
        })
        .detach();
    }

    /// Shuts down the server's agent; reconnecting starts the new one.
    fn restart_agent(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_restart.take().is_some() {
            self.focus_active(window, cx);
        }
        if let Some(client) = self.client(&name) {
            client.notify(Request::Shutdown);
        }
        cx.notify();
    }

    /// The connection was lost: terminals are left disconnected (still alive
    /// in the agent) and it retries until it's back.
    fn lost(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(host) = self.host_mut(&name) {
            host.client = None;
            host.status = HostStatus::Failed("connection lost; reconnecting…".into());
        }
        self.connect(name, window, cx);
    }

    /// Receives activity from the server's tasks and the tasks created with
    /// `sik task` on it.
    fn watch_host(&mut self, name: SharedString, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
        // The `sik` commands run in its terminals come here.
        client.notify(Request::Serve);
        let (tx, rx) = smol::channel::unbounded::<Event>();
        let name_for_watch = name.clone();
        client.watch(move |event| {
            let event = match event {
                Event::Activity { group, working } => Event::Activity {
                    group: group.clone(),
                    working: *working,
                },
                Event::Blocked { group, blocked } => Event::Blocked {
                    group: group.clone(),
                    blocked: *blocked,
                },
                Event::OpenTask { path } => Event::OpenTask { path: path.clone() },
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
        cx.spawn_in(window, async move |this, cx| {
            // Those already waiting for an answer on connecting (an outdated
            // agent doesn't know: they stay as they were).
            if let Ok(Response::Files(groups)) = client.request(Request::BlockedList).await {
                this.update(cx, |this, cx| {
                    this.blocked.retain(|key| key.host != name);
                    for group in groups {
                        this.blocked.insert(TaskKey { host: name.clone(), path: PathBuf::from(group) });
                    }
                    cx.notify();
                })
                .ok();
            }
            while let Ok(event) = rx.recv().await {
                let alive = match event {
                    Event::Blocked { group, blocked } => this
                        .update(cx, |this, cx| {
                            let key = TaskKey { host: name.clone(), path: PathBuf::from(group) };
                            if blocked {
                                this.blocked.insert(key);
                            } else {
                                this.blocked.remove(&key);
                            }
                            cx.notify();
                        })
                        .is_ok(),
                    Event::Activity { group, working } => this
                        .update(cx, |this, cx| {
                            let key = TaskKey {
                                host: name.clone(),
                                path: PathBuf::from(group),
                            };
                            this.set_working(&key, working, cx)
                        })
                        .is_ok(),
                    Event::Open { root, file } => {
                        let host = name.clone();
                        this.update(cx, |_, cx| cx.defer(move |cx| handle_open(host, root, file, cx))).is_ok()
                    }
                    Event::Command { command, args, cwd, term, group } => this
                        .update_in(cx, |this, window, cx| {
                            let command = commands::Command { id: command, args, cwd, term, group };
                            this.run_command(name.clone(), client.clone(), command, window, cx)
                        })
                        .is_ok(),
                    Event::OpenTask { path } => {
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

    /// Re-reads the tasks of the connected servers.
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        for (name, client) in self
            .hosts
            .iter()
            .filter_map(|host| Some((host.name.clone(), host.client.clone()?)))
            .collect::<Vec<_>>()
        {
            cx.spawn(async move |this, cx| {
                let tasks = list_tasks(&client).await;
                this.update(cx, |this, cx| {
                    if let (Ok(tasks), Some(host)) = (tasks, this.host_mut(&name))
                        && host.tasks != tasks
                    {
                        host.tasks = tasks;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
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

    fn set_working(&mut self, key: &TaskKey, working: bool, cx: &mut Context<Self>) {
        let active = self.active.as_ref() == Some(key);
        let Some(task) = self
            .hosts
            .iter_mut()
            .find(|host| host.name == key.host)
            .and_then(|host| host.tasks.iter_mut().find(|task| task.path == key.path))
        else {
            return;
        };
        let finished = task.working && !working;
        task.working = working;
        if finished && !active {
            self.attention.insert(key.clone());
        }
        cx.notify();
    }

    /// Workspaces in list order (the one for Cmd-1…9): by server;
    /// within each, a repo's checkout followed by its worktrees, the repos
    /// and the worktrees within them in the dragged order.
    fn ordered<'a>(&'a self, cx: &App) -> Vec<(TaskKey, &'a TaskInfo)> {
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
                entries.push((ix, key, task));
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

    /// Whether the tasks column shows: as last chosen or, if never chosen,
    /// once there's more than folders to it (a server or a worktree).
    fn tasks_visible(&self, cx: &App) -> bool {
        Config::get(cx)
            .tasks_column
            .unwrap_or_else(|| self.hosts.len() > 1 || self.hosts.iter().any(|host| host.tasks.iter().any(|task| !task.main)))
    }

    /// The tasks column shows: with a workspace, also in front of the panels
    /// it shares a place with.
    fn tasks_shown(&self, cx: &App) -> bool {
        match self.active_workspace() {
            Some(workspace) => workspace.read(cx).is_shown(Panel::Workspaces, cx),
            None => self.tasks_visible(cx),
        }
    }

    fn show_tasks_column(&mut self, visible: bool, cx: &mut Context<Self>) {
        match self.active_workspace() {
            Some(workspace) => workspace.update(cx, |workspace, cx| match visible {
                true => workspace.show_panel(Panel::Workspaces, cx),
                false => workspace.hide_panel(Panel::Workspaces, cx),
            }),
            None => Config::update(cx, |config| config.tasks_column = Some(visible)),
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
            self.add_folder(key.host.clone(), key.path.clone(), window, cx);
        }
        self.attention.remove(&key);
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
        window.set_window_title(&format!("{} — sik", self.label(&key)));
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

    /// Before quitting (Cmd-Q or closing the window): if any task has unsaved
    /// files, asks and only quits if confirmed. Returns whether it can quit
    /// right away.
    pub fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.unsaved(cx).is_empty() {
            return true;
        }
        if self.quit_confirm.is_none() {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            self.quit_confirm = Some(focus);
            cx.notify();
        }
        false
    }

    fn cancel_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_confirm = None;
        crate::update::cancel_restart(cx);
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
                    crate::update::relaunch_if_restarting(cx);
                    cx.quit();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Click on a file in the dialog: it closes and goes to its tab.
    fn go_to_unsaved(&mut self, key: TaskKey, file: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_confirm = None;
        let path = key.path.join(&file);
        self.activate(key, window, cx);
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.open(path, true, window, cx));
        }
    }

    fn render_quit_confirm(&self, focus: &FocusHandle, cx: &mut Context<Self>) -> impl IntoElement {
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
                                dialog_button("quit-discard", "Quit Without Saving", cx)
                                    .text_color(theme.danger)
                                    .on_click(cx.listener(|_, _, _, cx| cx.quit())),
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
                                    .child(if self.quit_saving { "Saving…" } else { "Save All and Quit" })
                                    .on_click(cx.listener(|this, _, _, cx| this.save_and_quit(cx))),
                            ),
                    ),
            )
    }

    /// Cmd-E: goes back to the previous task; again, to the one before (like Alt-Tab).
    fn previous_task(&mut self, _: &PreviousTask, window: &mut Window, cx: &mut Context<Self>) {
        // Skipped if it no longer exists (deleted, or its server was removed).
        if let Some(key) = self.previous.clone().filter(|key| self.task(key).is_some()) {
            self.activate(key, window, cx);
        }
    }

    fn open_task_picker(&mut self, _: &OpenTaskPicker, window: &mut Window, cx: &mut Context<Self>) {
        if self.task_picker.is_some() {
            return;
        }
        let labels: Vec<String> = self.ordered(cx).into_iter().map(|(key, _)| self.label(&key)).collect();
        let picker = cx.new(|cx| Picker::new(Arc::new(labels), "Go to workspace…", false, window, cx));
        let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
            this.task_picker = None;
            match event {
                PickerEvent::Pick(label) => {
                    let key = this
                        .ordered(cx)
                        .into_iter()
                        .map(|(key, _)| key)
                        .find(|key| &this.label(key) == label);
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
        // The commands run where the focus was, as if their keys were pressed there.
        let previous = window.focused(cx);
        let commands: Vec<_> = SHORTCUTS
            .iter()
            .filter(|shortcut| !matches!(shortcut.id, "OpenCommandPalette" | "ShowShortcuts"))
            .collect();
        let labels: Vec<String> = commands.iter().map(|shortcut| shortcut.label.to_string()).collect();
        let hints: HashMap<String, String> = commands
            .iter()
            .filter_map(|shortcut| Some((shortcut.label.to_string(), Kbd::format(&shortcuts::keys(shortcut, cx)?))))
            .collect();
        let picker = cx.new(|cx| Picker::new(Arc::new(labels), "Run a command…", false, window, cx).with_hints(hints));
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
                        match &previous {
                            // Once this update is over, and outside `Sik`:
                            // dispatching on a focus handle runs now, and the
                            // action may reach `Sik` (Settings), which can't be
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

    /// The tasks' state on the activity bar's icons: the active one's on the
    /// terminals, the most urgent of the others on the workspaces.
    fn task_badges(&self, cx: &App) -> TaskBadges {
        let dot = |(dot, color): (&str, Hsla)| (urgency(dot, color, cx) > 0).then_some(color);
        let active = self.active.as_ref().and_then(|key| Some(self.status(key, self.task(key)?, cx)));
        let others = self
            .ordered(cx)
            .into_iter()
            .filter(|(key, _)| self.active.as_ref() != Some(key))
            .map(|(key, task)| self.status(&key, task, cx))
            .max_by_key(|(dot, color)| urgency(dot, *color, cx));
        TaskBadges { terminals: active.and_then(dot), workspaces: others.and_then(dot) }
    }

    /// Opens `path` on `host`: the task it is, or the folder on its own.
    fn open_path(&mut self, host: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(TaskKey { host, path }, window, cx);
    }

    /// `sik <path>`: `root` as a workspace (the worktree containing it, if
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

    /// Cmd-O: a local folder, with the system's dialog.
    fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
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
            .skip(1)
            .filter(|host| host.client.is_some())
            .map(|host| host.name.to_string())
            .collect();
        match hosts.as_slice() {
            [] => self.open_host_picker(window, cx),
            [host] => self.open_folder_picker(host.clone().into(), window, cx),
            _ => {
                let picker = cx.new(|cx| Picker::new(Arc::new(hosts), "Open a folder on…", false, window, cx));
                let subscription = cx.subscribe_in(&picker, window, |this, _, event: &PickerEvent, window, cx| {
                    this.host_picker = None;
                    match event {
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
            Config::update(cx, |config| config.last = None);
            window.set_window_title("sik");
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
        if !self.tasks_shown(cx) {
            self.show_tasks_column(true, cx);
        }
        // The new row goes under the repo's checkout, with its worktrees.
        let fold_key = TaskKey { host: host.clone(), path: repo.clone() }.config();
        if Config::get(cx).collapsed.contains(&fold_key) {
            self.toggle_fold(&fold_key, cx);
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
                        this.activate(TaskKey { host, path: task.path }, window, cx);
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
        self.error = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    fn remove_task(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&key.host) else {
            return;
        };
        if self.confirm_remove.take().is_some() {
            self.focus_active(window, cx);
        }
        self.error = None;
        self.removing.insert(key.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(Request::TaskRemove { path: key.path.clone() }).await;
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
                            c.sessions.remove(&config);
                        });
                        if this.active.as_ref() == Some(&key) {
                            this.active = None;
                            if let Some(next) = this.ordered(cx).first().map(|(key, _)| key.clone()) {
                                this.activate(next, window, cx);
                            }
                        }
                    }
                    Err(err) => this.error = Some((key, format!("{err:#}").into())),
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
        for (key, task) in self.ordered(cx) {
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
        Config::update(cx, |c| {
            c.hosts.push(HostConfig {
                name: name.to_string(),
                destination: destination.clone(),
            })
        });
        self.hosts.push(Host {
            name: name.clone(),
            destination: Some(destination),
            client: None,
            status: HostStatus::Connecting(CONNECTING),
            tasks: Vec::new(),
            loose: Vec::new(),
            repos: Vec::new(),
            generation: 0,
        });
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
        let Some(client) = self.client(&host) else {
            return;
        };
        let start = self
            .host(&host)
            .and_then(|host| host.repos.first().or(host.tasks.first().map(|task| &task.repo)))
            .and_then(|repo| repo.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("~"));
        let title = format!("OPEN FOLDER ON {}", host.to_uppercase());
        let picker = cx.new(|cx| FolderPicker::new(client.clone(), title, start, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &FolderPickerEvent, window, cx| {
            this.folder_picker = None;
            match event {
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
        self.hosts.retain(|host| host.name != name);
        self.workspaces.retain(|key, _| key.host != name);
        Config::update(cx, |c| c.hosts.retain(|host| host.name != name.as_ref()));
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

    /// The tasks column.
    fn render_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_tasks(cx);
        let theme = cx.theme();
        v_flex()
            .id("task-column")
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }

    /// The activity bar, with only the workspaces' icon; the tasks column,
    /// where its panel's column is; and the welcome.
    fn render_without_workspace(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let badge = self.task_badges(cx).workspaces.map(Badge::Dot);
        let sik = cx.entity().downgrade();
        let click: OnActivity = Rc::new(move |_, window, cx| {
            sik.update(cx, |this, cx| this.toggle_tasks(&ToggleTasks, window, cx)).ok();
        });
        let bar = activity_bar(vec![(Panel::Workspaces, visible, badge)], click, cx);
        let layout = &Config::get(cx).layout;
        let width = layout.find(Panel::Workspaces).and_then(|(column, _)| layout.columns[column].width).unwrap_or(240.);
        let state = self.split.state(window.viewport_size().width - px(ACTIVITY_WIDTH), [visible, true], cx).clone();
        let split = h_resizable("sik-split")
            .with_state(&state)
            .child(
                resizable_panel()
                    .size(config::width(width, 160., 500.))
                    .size_range(px(160.)..px(500.))
                    .visible(visible)
                    .child(self.render_column(cx)),
            )
            .child(resizable_panel().child(match &self.guide {
                Some(guide) => self.render_guide(guide, cx),
                None => self.render_welcome(cx),
            }))
            .on_resize(move |state, _, cx| {
                if visible && let Some(width) = state.read(cx).sizes().first().copied() {
                    Config::update_quietly(cx, |config| {
                        if let Some((column, _)) = config.layout.find(Panel::Workspaces) {
                            config.layout.columns[column].width = Some(f32::from(width));
                        }
                    });
                }
            });
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
        let weak = cx.entity().downgrade();
        let menu_name = name.clone();
        v_flex()
            .px_3()
            .pt_2()
            .pb_1()
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
                    .context_menu(move |menu, _, _| host_menu(menu, &menu_name, connected, &weak)),
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

    fn render_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        let ordered = self.ordered(cx);
        let mut sections: Vec<AnyElement> = Vec::new();
        for host in &self.hosts {
            sections.push(self.render_host_header(host, cx));
            let entries: Vec<(TaskKey, &TaskInfo)> =
                ordered.iter().filter(|(key, _)| key.host == host.name).cloned().collect();
            // Each repo, with its worktrees, apart from the next.
            for (ix, group) in entries.chunk_by(|(_, a), (_, b)| a.repo == b.repo).enumerate() {
                let line = cx.theme().sidebar_border;
                let repo = self.render_repo(group, cx);
                sections.push(div().py_1().when(ix > 0, |el| el.border_t_1().border_color(line)).child(repo).into_any_element());
            }
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
                    "The folders you open stay here. A git repo shows its worktrees, each a workspace with its own terminals and Claude Code session.",
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
        let header_menu = weak.clone();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .id("tasks-header")
                    .h(px(34.))
                    .flex_none()
                    .px_3()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .text_ui_small(cx)
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().child("WORKSPACES"))
                    .child({
                        let add_menu = header_menu.clone();
                        Button::new("tasks-add")
                            .ghost()
                            .xsmall()
                            .icon(Icon::default().path("icons/plus.svg"))
                            .tooltip("Open Folder or Add Server")
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| add_menu_items(menu, &add_menu))
                    })
                    .context_menu(move |menu, _, _| column_menu(menu, &header_menu)),
            )
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
                            .context_menu(move |menu, _, _| column_menu(menu, &weak)),
                    ),
            )
            .into_any_element()
    }

    /// A repo's workspaces: its checkout and, folded under it, its worktrees.
    /// A repo without worktrees is a single row.
    fn render_repo(&self, group: &[(TaskKey, &TaskInfo)], cx: &mut Context<Self>) -> AnyElement {
        let (head, worktrees) = match group {
            [(key, task), rest @ ..] if task.main => (Some((key, *task)), rest),
            _ => (None, group),
        };
        let Some(&(ref first, first_task)) = group.first() else {
            return div().into_any_element();
        };
        let new_task = self
            .new_task
            .as_ref()
            .filter(|form| form.host == first.host && form.repo == first_task.repo)
            .map(|form| render_new_task(form, cx));
        if worktrees.is_empty() && new_task.is_none() {
            return self.render_task(first, first_task, Some(None), &[], cx);
        }
        let fold_key = TaskKey { host: first.host.clone(), path: first_task.repo.clone() }.config();
        let collapsed = Config::get(cx).collapsed.contains(&fold_key);
        let header = match head {
            // Only its new worktree under it: nothing to fold yet.
            Some((key, task)) if worktrees.is_empty() => self.render_task(key, task, Some(None), &[], cx),
            Some((key, task)) => self.render_task(key, task, Some(Some((fold_key, collapsed))), worktrees, cx),
            // Its checkout isn't among them: the repo's name, which only folds.
            None => {
                let theme = cx.theme();
                h_flex()
                    .id(SharedString::from(format!("repo-{fold_key}")))
                    .h(px(26.))
                    .pl(px(ROW_INDENT))
                    .pr_3()
                    .gap_2()
                    .text_ui(cx)
                    .text_color(theme.muted_foreground)
                    .child(svg().path("icons/folder.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                    .child(div().flex_1().child(folder_name(&first_task.repo)))
                    .child(fold_chevron(collapsed, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_fold(&fold_key, cx)))
                    .into_any_element()
            }
        };
        let rows: Vec<AnyElement> = if collapsed {
            Vec::new()
        } else {
            worktrees
                .iter()
                .map(|(key, task)| self.render_task(key, task, None, &[], cx))
                .chain(new_task)
                .collect()
        };
        v_flex().child(header).children(rows).into_any_element()
    }

    fn toggle_fold(&mut self, fold_key: &str, cx: &mut Context<Self>) {
        Config::update(cx, |c| {
            if c.collapsed.iter().any(|key| key == fold_key) {
                c.collapsed.retain(|key| key != fold_key);
            } else {
                c.collapsed.push(fold_key.to_string());
            }
        });
        cx.notify();
    }

    /// The dot before a workspace: deleting, waiting for an answer, working,
    /// finished without being looked at, or nothing.
    fn status(&self, key: &TaskKey, task: &TaskInfo, cx: &App) -> (&'static str, Hsla) {
        let theme = cx.theme();
        if self.removing.contains(key) {
            ("…", theme.muted_foreground)
        } else if self.blocked.contains(key) {
            ("●", theme.danger)
        } else if task.working {
            ("◐", theme.warning)
        } else if self.attention.contains(key) {
            ("●", theme.success)
        } else {
            ("○", theme.muted_foreground)
        }
    }

    /// A workspace's row. `fold` is set on the column's own rows (not a
    /// repo's worktrees): `Some((key, collapsed))` for a checkout with
    /// worktrees; `folded`, those worktrees, whose state shows on it while
    /// they're hidden.
    fn render_task(
        &self,
        key: &TaskKey,
        task: &TaskInfo,
        fold: Option<Option<(String, bool)>>,
        folded: &[(TaskKey, &TaskInfo)],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folder: SharedString = folder_name(&task.path).into();
        // A worktree goes by its branch, what its agent works on (its folder
        // usually repeats the repo's name); a checkout by its folder, with
        // its branch beside it when off the main one.
        let label = match (&task.branch, task.main) {
            (Some(branch), false) => SharedString::from(branch.clone()),
            _ => folder.clone(),
        };
        let branch = task
            .branch
            .clone()
            .filter(|branch| task.main && *branch != *label && !matches!(branch.as_str(), "master" | "main"));
        let active = self.active.as_ref() == Some(key);
        let (mut dot, mut color) = self.status(key, task, cx);
        if let Some(Some((_, true))) = &fold {
            // Folded: the most urgent of its worktrees, if more than its own.
            for (key, task) in folded {
                let (other, other_color) = self.status(key, task, cx);
                if urgency(other, other_color, cx) > urgency(dot, color, cx) {
                    (dot, color) = (other, other_color);
                }
            }
        }
        let theme = cx.theme();
        let known = self
            .host(&key.host)
            .is_some_and(|host| host.tasks.iter().any(|other| other.path == key.path));
        let removable = known && !task.main;
        let local = key.host == LOCAL;
        let weak = cx.entity().downgrade();

        let row = h_flex()
            .id(SharedString::from(format!("task-{}", key.config())))
            .h(px(26.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .when(active, |el| el.bg(theme.sidebar_accent))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            .pl(px(ROW_INDENT))
            // What it is: a folder, or a repo's worktree.
            .child(
                svg()
                    .path(kind_icon(task))
                    .size(px(14.))
                    .flex_none()
                    .text_color(if active { theme.sidebar_foreground } else { theme.muted_foreground }),
            )
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.clone()),
            )
            // Claude's state, only when there's one: working, waiting for an
            // answer or finished unseen.
            .when(dot != "○", |row| row.child(div().flex_none().text_ui_small(cx).text_color(color).child(dot)))
            .child(div().flex_1())
            .children(branch.map(|branch| {
                div()
                    .flex_none()
                    .max_w(px(120.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(branch)
            }))
            .when_some(fold.flatten(), |row, (fold_key, collapsed)| {
                row.child(
                    div()
                        .id(SharedString::from(format!("fold-{fold_key}")))
                        .child(fold_chevron(collapsed, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_fold(&fold_key, cx);
                        })),
                )
            })
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
            .when(!known || label != folder, |row| {
                let path = key.path.display().to_string();
                row.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
            })
            .context_menu({
                let key = key.clone();
                let repo = task.repo.clone();
                let repo_name = folder_name(&task.repo);
                let connected = self.client(&key.host).is_some();
                // Only a repo makes worktrees: one with a branch, or with
                // worktrees of its own.
                let git = known
                    && (task.branch.is_some()
                        || !task.main
                        || self.host(&key.host).is_some_and(|host| {
                            host.tasks.iter().any(|other| other.repo == task.repo && !other.main)
                        }));
                let main = task.main;
                // Closing would lose unsaved changes: save them first.
                let unsaved = self
                    .workspaces
                    .get(&key)
                    .is_some_and(|workspace| !workspace.read(cx).unsaved().is_empty());
                move |menu, _, _| {
                    let (create, copy, finder, init, remove, close, add) =
                        (key.clone(), key.clone(), key.clone(), key.clone(), key.clone(), key.clone(), key.clone());
                    let (repo, folder) = (repo.clone(), repo.clone());
                    menu.when(git, |menu| {
                        menu.item(
                            menu::item(format!("New Worktree in {repo_name}…"), &weak, move |this, window, cx| {
                                this.start_new_task(create.host.clone(), repo.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .when(known && main && !git, |menu| {
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
                                this.add_folder(add.host.clone(), add.path.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path))
                            .disabled(!local),
                    )
                    .separator()
                    // A folder (with its worktrees, if it's a repo) leaves the
                    // list; a worktree is deleted instead.
                    .when(main, |menu| {
                        menu.item(
                            menu::item("Remove from Workspaces", &weak, move |this, window, cx| {
                                if known {
                                    this.remove_folder(&close, folder.clone(), window, cx)
                                } else {
                                    this.close_folder(&close, window, cx)
                                }
                            })
                            .disabled(unsaved),
                        )
                    })
                    .when(removable, |menu| {
                        menu.item(menu::item("Delete Worktree…", &weak, move |this, window, cx| {
                            this.ask_remove(remove.clone(), window, cx)
                        }))
                    })
                }
            });


        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| target == key)
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));

        v_flex().child(row).children(error).into_any_element()
    }
}

/// How much a task's dot (see `Sik::status`) asks to be looked at: waiting
/// for an answer, working, finished unseen, or nothing.
fn urgency(dot: &str, color: Hsla, cx: &App) -> u8 {
    match dot {
        "●" if color == cx.theme().danger => 3,
        "◐" => 2,
        "●" => 1,
        _ => 0,
    }
}

impl Render for Sik {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = self.active_workspace() {
            let width = window.viewport_size().width - px(ACTIVITY_WIDTH);
            let branch = self.active.as_ref().and_then(|key| self.task(key)).and_then(|task| task.branch.clone());
            let (panel, visible) = (self.workspaces_panel.clone(), self.tasks_visible(cx));
            let badges = self.task_badges(cx);
            workspace.update(cx, |workspace, cx| {
                workspace.set_width(width, cx);
                workspace.set_branch(branch, cx);
                workspace.set_workspaces(&panel, visible, cx);
                workspace.set_badges(badges, cx);
            });
        }
        let tasks_visible = self.tasks_shown(cx);
        let title = self.active.as_ref().map(|key| self.label(key)).unwrap_or_else(|| "sik".into());
        v_flex()
            .id("sik")
            .key_context("Sik")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .text_ui(cx)
            .on_action(cx.listener(Self::toggle_tasks))
            .on_action(cx.listener(Self::open_folder))
            .on_action(cx.listener(Self::open_remote_folder))
            .on_action(cx.listener(Self::open_recent))
            .on_action(cx.listener(Self::new_task_action))
            .on_action(cx.listener(Self::open_task_picker))
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| this.open_command_palette(window, cx)))
            .on_action(cx.listener(|this, _: &ShowShortcuts, window, cx| this.open_command_palette(window, cx)))
            .on_action(cx.listener(Self::previous_task))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &About, window, cx| this.open_about(window, cx)))
            .on_action(cx.listener(|this, _: &CheckForUpdates, window, cx| this.check_for_updates(window, cx)))
            .on_action(cx.listener(|this, _: &ShowWelcome, window, cx| this.show_welcome(window, cx)))
            .on_action(cx.listener(|this, _: &OpenShortcutsGuide, window, cx| this.open_guide(window, cx)))
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
                            .text_ui(cx)
                            .text_color(cx.theme().muted_foreground)
                            .child(title),
                    )
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
            .child(div().flex_1().min_h_0().w_full().child(match self.active_workspace() {
                Some(workspace) => workspace.into_any_element(),
                None => self.render_without_workspace(tasks_visible, window, cx),
            }))
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
            .children(
                self.host_picker
                    .as_ref()
                    .map(|(picker, _)| picker.clone().into_any_element())
                    .or_else(|| self.folder_picker.as_ref().map(|(picker, _)| picker.clone().into_any_element()))
                    .map(|picker| div().absolute().top(px(44.)).left_0().right_0().flex().justify_center().child(picker)),
            )
            .children(self.about.as_ref().map(|focus| self.render_about(focus, cx)))
            .children(self.confirm_remove.as_ref().map(|(key, focus)| self.render_confirm_remove(key, focus, cx)))
            .children(self.confirm_restart.as_ref().map(|(name, focus)| self.render_confirm_restart(name, focus, cx)))
            .children(self.confirm_update.as_ref().map(|(version, focus)| self.render_confirm_update(version, focus, cx)))
            .children(self.quit_confirm.as_ref().map(|focus| self.render_quit_confirm(focus, cx)))
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

async fn list_tasks(client: &Client) -> anyhow::Result<Vec<TaskInfo>> {
    match client.request(Request::TaskList).await? {
        Response::Tasks(tasks) => Ok(tasks),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
}

/// The column's + menu: what can be added to it.
fn add_menu_items(menu: PopupMenu, sik: &WeakEntity<Sik>) -> PopupMenu {
    menu.item(menu::item("Open Folder…", sik, |this, window, cx| this.open_folder(&OpenFolder, window, cx)))
        .item(menu::item("Open Folder on Server…", sik, |this, window, cx| {
            this.open_remote_folder(&OpenRemoteFolder, window, cx)
        }))
        .separator()
        .item(menu::item("Add Server…", sik, |this, window, cx| this.open_host_picker(window, cx)))
}

/// Right-click on the tasks column's empty space or its title.
fn column_menu(menu: PopupMenu, sik: &WeakEntity<Sik>) -> PopupMenu {
    add_menu_items(menu, sik)
        .separator()
        .item(menu::item("Hide Workspaces Column", sik, |this, _, cx| this.show_tasks_column(false, cx)))
        .item(menu::reset_layout())
}

/// Right-click on a server's name in the tasks column.
fn host_menu(menu: PopupMenu, name: &SharedString, connected: bool, sik: &WeakEntity<Sik>) -> PopupMenu {
    if name == LOCAL {
        return menu.item(menu::item("Open Folder…", sik, |this, window, cx| this.open_folder(&OpenFolder, window, cx)));
    }
    let (open, reconnect, remove) = (name.clone(), name.clone(), name.clone());
    menu.item(
        menu::item(format!("Open Folder on {name}…"), sik, move |this, window, cx| {
            this.open_folder_picker(open.clone(), window, cx)
        })
        .disabled(!connected),
    )
    .separator()
    .item(menu::item("Reconnect", sik, move |this, window, cx| this.connect(reconnect.clone(), window, cx)))
    .item(menu::item("Remove Server", sik, move |this, window, cx| this.remove_host(remove.clone(), window, cx)))
}

/// In Open Recent: the path (`~/…` locally) and, on a server, its name.
fn recent_label(key: &TaskKey) -> String {
    let path = match std::env::home_dir().and_then(|home| key.path.strip_prefix(&home).ok().map(Path::to_path_buf)) {
        Some(rest) if key.host == LOCAL => format!("~/{}", rest.display()),
        _ => key.path.display().to_string(),
    };
    if key.host == LOCAL { path } else { format!("{}: {path}", key.host) }
}

/// A workspace's icon: a folder, or a branch for a repo's worktree.
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
                .pl(px(ROW_INDENT))
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
                        .child(div().flex_none().text_ui_small(cx).child("…"))
                        .into_any_element()
                } else {
                    div().flex_1().min_w_0().child(Input::new(&form.input).xsmall()).into_any_element()
                }),
        )
        .children(form.error.clone().map(|error| div().px_3().pl(px(ROW_INDENT)).pb_1().child(error_text(error, cx))))
        .into_any_element()
}

/// The arrow that folds a repo's worktrees under its checkout.
fn fold_chevron(collapsed: bool, cx: &App) -> impl IntoElement {
    let icon = if collapsed { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" };
    div()
        .w(px(FOLD_WIDTH))
        .flex_none()
        .child(svg().path(icon).size(px(FOLD_WIDTH)).text_color(cx.theme().muted_foreground))
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

/// The checkout's folder name, or `repo/folder` for a worktree.
fn task_label(task: &TaskInfo) -> String {
    let repo = folder_name(&task.repo);
    if task.path == task.repo { repo } else { format!("{repo}/{}", folder_name(&task.path)) }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use gpui_kit::component::highlighter::SyntaxColors;

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

    use super::Sik;
    use crate::{config::Config, picker::PickerEvent};

    /// Settings from the command palette: the action reaches `Sik` itself,
    /// which must not be in the middle of an update then.
    #[gpui_kit::test]
    fn the_palette_opens_settings(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (sik, cx) = cx.add_window_view(|window, cx| Sik::new(None, None, false, None, window, cx));
        cx.update(|window, cx| {
            sik.update(cx, |sik, cx| {
                sik.focus_handle.focus(window, cx);
                sik.open_command_palette(window, cx);
            })
        });
        cx.run_until_parked();
        let picker = sik.read_with(cx, |sik, _| sik.command_palette.as_ref().map(|(picker, _)| picker.clone()).unwrap());
        picker.update(cx, |_, cx| cx.emit(PickerEvent::Pick("Settings".into())));
        cx.run_until_parked();
        assert!(sik.read_with(cx, |sik, _| sik.settings.is_some()));
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
        let (sik, cx) = cx.add_window_view(|window, cx| Sik::new(None, None, false, None, window, cx));
        cx.update(|window, cx| sik.update(cx, |sik, cx| sik.check_for_updates(window, cx)));
        cx.run_until_parked();
        assert!(sik.read_with(cx, |sik, _| sik.about.is_some()));
        assert!(cx.update(|_, cx| crate::update::status(cx) == crate::update::Status::NotInstalled));
    }

    /// Restart to update asks first; cancelling leaves everything as it was.
    #[gpui_kit::test]
    fn restarting_to_update_asks_first(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (sik, cx) = cx.add_window_view(|window, cx| Sik::new(None, None, false, None, window, cx));
        cx.update(|window, cx| sik.update(cx, |sik, cx| sik.ask_update("9.9.9".into(), window, cx)));
        cx.run_until_parked();
        assert!(sik.read_with(cx, |sik, _| sik.confirm_update.is_some()));
        cx.update(|window, cx| sik.update(cx, |sik, cx| sik.cancel_confirm(window, cx)));
        assert!(sik.read_with(cx, |sik, _| sik.confirm_update.is_none()));
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
