//! The sik window: the tasks column, grouped by server, and the active task's
//! workspace. Each task (a worktree) has its own workspace, which is kept
//! when switching from one to another.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Theme, ThemeMode, TitleBar, h_flex, h_resizable,
    highlighter::SyntaxColors,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    resizable_panel, v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{Event, Request, Response, TaskInfo};

use crate::{
    ActivateTask1, ActivateTask2, ActivateTask3, ActivateTask4, ActivateTask5, ActivateTask6,
    ActivateTask7, ActivateTask8, ActivateTask9, NewTask, OpenSettings, OpenTaskPicker, PreviousTask, ToggleTasks,
    config::{self, Config, HostConfig, SavedTask, SavedWindow, ThemeChoice},
    menu,
    folder_picker::{FolderPicker, FolderPickerEvent},
    picker::{Picker, PickerEvent},
    workspace::Workspace,
};

mod settings;

/// How often the task list is re-read (worktrees created elsewhere).
const REFRESH: Duration = Duration::from_secs(5);

/// Name of this machine in the tasks column.
const LOCAL: &str = "local";

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
    Connecting,
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
    repos: Vec<PathBuf>,
    /// This server's "Add repo" input, in settings.
    repo_input: Option<Entity<InputState>>,
    /// Goes up with each new connection attempt: an earlier one still
    /// retrying notices and gives up.
    generation: u64,
}

/// Inline "New task", in the column.
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
            .text_sm()
            .rounded(cx.theme().radius)
            .bg(cx.theme().sidebar_accent)
            .text_color(cx.theme().sidebar_foreground)
            .child(self.0.clone())
    }
}


pub struct Sik {
    hosts: Vec<Host>,
    /// Open local folders that don't belong to any known repo.
    loose: Vec<TaskInfo>,
    active: Option<TaskKey>,
    workspaces: HashMap<TaskKey, Entity<Workspace>>,
    /// Tasks that finished working without being looked at.
    attention: HashSet<TaskKey>,
    /// Tasks waiting for an answer (Claude is asking something): in red.
    blocked: HashSet<TaskKey>,
    panel_visible: bool,
    new_task: Option<NewTaskInput>,
    /// Task whose deletion is being confirmed.
    confirm_remove: Option<TaskKey>,
    /// Server whose agent is about to be restarted (confirming).
    confirm_restart: Option<SharedString>,
    removing: HashSet<TaskKey>,
    /// Last error from a column action, with the task it affects.
    error: Option<(Option<TaskKey>, SharedString)>,
    /// "Add server", in settings.
    host_input: Option<Entity<InputState>>,
    /// File to open in the first workspace (`sik file`).
    open_file: Option<PathBuf>,
    /// Last task from the previous session, on a server not yet connected:
    /// it's entered on connecting (unless another was opened first).
    pending_last: Option<TaskKey>,
    /// The task before the active one, for Cmd-E.
    previous: Option<TaskKey>,
    /// Cmd-K: jump to a task by name.
    task_picker: Option<(Entity<Picker>, Subscription)>,
    /// Settings: pick a server from `~/.ssh/config`.
    host_picker: Option<(Entity<Picker>, Subscription)>,
    /// Settings: pick a repo's folder on a server.
    folder_picker: Option<(Entity<FolderPicker>, Subscription)>,
    /// Quit dialog with unsaved files (its focus, for Esc and Enter).
    quit_confirm: Option<FocusHandle>,
    /// Saving everything before quitting.
    quit_saving: bool,
    /// Tasks column and workspace.
    split: config::Split,
    /// Settings, if open.
    settings: Option<settings::Settings>,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Sik {
    pub fn new(
        root: PathBuf,
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
            repos: Vec::new(),
            repo_input: None,
            generation: 0,
        };
        let remotes: Vec<Host> = Config::get(cx)
            .hosts
            .iter()
            .map(|host| Host {
                name: host.name.clone().into(),
                destination: Some(host.destination.clone()),
                client: None,
                status: HostStatus::Connecting,
                tasks: Vec::new(),
                repos: Vec::new(),
                repo_input: None,
                generation: 0,
            })
            .collect();
        let mut this = Self {
            hosts: std::iter::once(local).chain(remotes).collect(),
            loose: Vec::new(),
            active: None,
            workspaces: HashMap::new(),
            attention: HashSet::new(),
            blocked: HashSet::new(),
            panel_visible: true,
            new_task: None,
            confirm_remove: None,
            confirm_restart: None,
            removing: HashSet::new(),
            error: None,
            host_input: None,
            open_file,
            pending_last: None,
            previous: None,
            task_picker: None,
            host_picker: None,
            folder_picker: None,
            quit_confirm: None,
            quit_saving: false,
            split: config::Split::new(cx),
            settings: None,
            focus_handle: cx.focus_handle(),
            _tasks: Vec::new(),
            _subscriptions: vec![appearance, bounds],
        };
        Self::install_code_colors(cx);
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
            this.loose.push(loose_task(&root));
            this.activate(TaskKey { host: LOCAL.into(), path: root }, window, cx);
            return this;
        };
        this.watch_host(LOCAL.into(), client.clone(), window, cx);
        this.track(LOCAL.into(), &client, window, cx);

        // Registers the open folder's repo (if it is one), reads the tasks and
        // enters the one containing the folder.
        let startup = cx.spawn_in(window, async move |this, cx| {
            let _ = client.request(Request::RepoAdd { path: root.clone() }).await;
            let tasks = list_tasks(&client).await.unwrap_or_default();
            this.update_in(cx, |this, window, cx| {
                this.hosts[0].tasks = tasks;
                let task = this.hosts[0]
                    .tasks
                    .iter()
                    .filter(|task| root.starts_with(&task.path))
                    .max_by_key(|task| task.path.components().count())
                    .map(|task| task.path.clone());
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
                        if !this.hosts[0].tasks.iter().any(|task| task.path == last.path) {
                            this.loose.push(loose_task(&last.path));
                        }
                        TaskKey { host: LOCAL.into(), path: last.path }
                    }
                    (None, _) => {
                        this.loose.push(loose_task(&root));
                        TaskKey { host: LOCAL.into(), path: root.clone() }
                    }
                };
                let config = key.config();
                Config::update(cx, |c| c.hidden.retain(|hidden| hidden != &config));
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
        host.status = HostStatus::Connecting;
        cx.notify();
        let agents = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
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
                let (destination, agents) = (destination.clone(), agents.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        match destination {
                            Some(destination) => client::connect_ssh(&destination, &agents),
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
    fn restart_agent(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.confirm_restart = None;
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
        let (tx, rx) = smol::channel::unbounded::<Event>();
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

    /// Code colors like VS Code's 2026 theme (light and dark): only the
    /// highlighting changes, the rest of the app's theme stays.
    fn install_code_colors(cx: &mut App) {
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
            *config = Rc::new(updated);
        }
    }

    fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match Config::get(cx).theme {
            ThemeChoice::System => Theme::sync_system_appearance(Some(window), cx),
            ThemeChoice::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            ThemeChoice::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
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

    /// Visible tasks in list order (the one for Cmd-1…9): by server and,
    /// within each, by the dragged order.
    fn ordered<'a>(&'a self, cx: &App) -> Vec<(TaskKey, &'a TaskInfo)> {
        let config = Config::get(cx);
        let mut entries: Vec<(usize, TaskKey, &TaskInfo)> = Vec::new();
        for (ix, host) in self.hosts.iter().enumerate() {
            let loose = if ix == 0 { self.loose.as_slice() } else { &[] };
            for task in host.tasks.iter().chain(loose) {
                let key = TaskKey {
                    host: host.name.clone(),
                    path: task.path.clone(),
                };
                if !config.hidden.contains(&key.config()) {
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
        entries.sort_by(|(ha, ka, a), (hb, kb, b)| {
            (ha, position(ka), &a.repo, !a.main, &a.branch).cmp(&(hb, position(kb), &b.repo, !b.main, &b.branch))
        });
        entries.into_iter().map(|(_, key, task)| (key, task)).collect()
    }

    fn task(&self, key: &TaskKey) -> Option<&TaskInfo> {
        let host = self.host(&key.host)?;
        let loose = if key.host == LOCAL { self.loose.as_slice() } else { &[] };
        host.tasks.iter().chain(loose).find(|task| task.path == key.path)
    }

    fn label(&self, key: &TaskKey) -> String {
        let label = self
            .task(key)
            .map(task_label)
            .unwrap_or_else(|| key.path.display().to_string());
        if key.host == LOCAL { label } else { format!("{}: {label}", key.host) }
    }

    /// Enters task `key`.
    fn activate(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
        self.attention.remove(&key);
        let workspace = match self.workspaces.get(&key) {
            Some(workspace) => workspace.clone(),
            None => {
                let client = self.client(&key.host);
                let root = key.path.clone();
                let local = key.host == LOCAL;
                let workspace = cx.new(|cx| Workspace::new(root, client, local, key.config(), window, cx));
                workspace.update(cx, |workspace, cx| workspace.restore(window, cx));
                if local && let Some(file) = self.open_file.take() {
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
        Config::update(cx, |config| config.last = Some(last));
        self.pending_last = None;
        if let Some(old) = self.active.take().filter(|old| *old != key) {
            self.previous = Some(old);
        }
        self.active = Some(key);
        cx.notify();
    }

    fn activate_nth(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.ordered(cx).get(ix).map(|(key, _)| key.clone()) {
            self.activate(key, window, cx);
        }
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
        let button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.bg(theme.secondary_hover))
                .child(label)
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
                    .text_sm()
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
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Click a file to go to it."),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(button("quit-cancel", "Cancel").on_click(cx.listener(|this, _, window, cx| this.cancel_quit(window, cx))))
                            .child(
                                button("quit-discard", "Quit Without Saving")
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
        let picker = cx.new(|cx| Picker::new(Arc::new(labels), "Go to task…", false, window, cx));
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

    fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(workspace) = self.active_workspace() {
            workspace.update(cx, |workspace, cx| workspace.focus(window, cx));
        }
    }

    fn toggle_tasks(&mut self, _: &ToggleTasks, _: &mut Window, cx: &mut Context<Self>) {
        self.panel_visible = !self.panel_visible;
        cx.notify();
    }

    /// Cmd-Shift-N: new task in the active task's repo.
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
        self.panel_visible = true;
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("branch name"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.create_task(window, cx);
            }
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
                    Ok(other) => this.new_task_error(format!("Unexpected response: {other:?}"), cx),
                    Err(err) => this.new_task_error(format!("{err:#}"), cx),
                }
            })
            .ok();
        })
        .detach();
    }

    fn new_task_error(&mut self, error: String, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.new_task {
            form.busy = false;
            form.error = Some(error.into());
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
        self.confirm_remove = None;
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
                    Err(err) => this.error = Some((Some(key), format!("{err:#}").into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn hide_task(&mut self, key: &TaskKey, cx: &mut Context<Self>) {
        let config = key.config();
        Config::update(cx, |c| {
            if !c.hidden.contains(&config) {
                c.hidden.push(config);
            }
        });
        cx.notify();
    }

    fn show_task(&mut self, config: &str, cx: &mut Context<Self>) {
        Config::update(cx, |c| c.hidden.retain(|hidden| hidden != config));
        cx.notify();
    }

    /// Puts `dragged` right before `target` and saves the order.
    fn move_task(&mut self, dragged: &TaskKey, target: &TaskKey, cx: &mut Context<Self>) {
        if dragged == target || dragged.host != target.host {
            return;
        }
        let mut order: Vec<String> = self.ordered(cx).into_iter().map(|(key, _)| key.config()).collect();
        let Some(from) = order.iter().position(|key| *key == dragged.config()) else {
            return;
        };
        let moved = order.remove(from);
        let to = order.iter().position(|key| *key == target.config()).unwrap_or(order.len());
        order.insert(to, moved);
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

    fn add_repo(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.host(&host).and_then(|host| host.repo_input.clone()) else {
            return;
        };
        let Some(client) = self.client(&host) else {
            return;
        };
        let raw = input.read(cx).value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        // `~/` is expanded here only locally: on a server, home is somewhere else.
        let path = if host == LOCAL { expand_home(&raw) } else { PathBuf::from(raw) };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(Request::RepoAdd { path }).await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        this.error = None;
                    }
                    Err(err) => this.error = Some((None, format!("{err:#}").into())),
                }
                this.refresh_repos(window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn remove_repo(&mut self, host: SharedString, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&host) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let _ = client.request(Request::RepoRemove { path: repo }).await;
            this.update_in(cx, |this, window, cx| this.refresh_repos(window, cx)).ok();
        })
        .detach();
    }

    fn add_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.host_input.clone() else {
            return;
        };
        let destination = input.read(cx).value().trim().to_string();
        if self.add_host_named(destination, window, cx) {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
    }

    /// Registers the server `destination` (from `~/.ssh/config` or
    /// `user@host`) and connects; returns whether it was added.
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
            status: HostStatus::Connecting,
            tasks: Vec::new(),
            repos: Vec::new(),
            repo_input: None,
            generation: 0,
        });
        self.connect(name, window, cx);
        self.open_settings(window, cx);
        true
    }

    /// Button next to "add server": those in `~/.ssh/config` not added yet.
    fn open_host_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hosts: Vec<String> = ssh_hosts()
            .into_iter()
            .filter(|host| self.host(host).is_none())
            .collect();
        let picker = cx.new(|cx| Picker::new(Arc::new(hosts), "Server from ~/.ssh/config…", false, window, cx));
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

    /// Button next to "add repo": browse the server's folders, starting next
    /// to its repos (or in its home folder).
    fn open_folder_picker(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client(&host) else {
            return;
        };
        let start = self
            .host(&host)
            .and_then(|host| host.repos.first())
            .and_then(|repo| repo.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("~"));
        let title = format!("ADD REPO ON {}", host.to_uppercase());
        let picker = cx.new(|cx| FolderPicker::new(client.clone(), title, start, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, picker, event: &FolderPickerEvent, window, cx| {
            match event {
                FolderPickerEvent::Dismiss => this.folder_picker = None,
                FolderPickerEvent::Pick(path) => {
                    let (client, path, picker) = (client.clone(), path.clone(), picker.clone());
                    cx.spawn_in(window, async move |this, cx| {
                        let result = client.request(Request::RepoAdd { path }).await;
                        this.update_in(cx, |this, window, cx| match result {
                            Ok(_) => {
                                this.folder_picker = None;
                                this.refresh_repos(window, cx);
                            }
                            Err(err) => picker.update(cx, |picker, cx| picker.set_error(format!("{err:#}"), cx)),
                        })
                        .ok();
                    })
                    .detach();
                }
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

    fn active_workspace(&self) -> Option<Entity<Workspace>> {
        self.active.as_ref().and_then(|key| self.workspaces.get(key).cloned())
    }

    fn render_column(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_tasks(cx);
        let theme = cx.theme();
        v_flex()
            .id("task-column")
            .size_full()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .text_color(theme.sidebar_foreground)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }

    fn render_host_header(&self, host: &Host, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (dot, color, detail): (&str, Hsla, Option<SharedString>) = match &host.status {
            HostStatus::Connected => ("●", theme.success, None),
            HostStatus::Connecting => ("○", theme.muted_foreground, Some("connecting…".into())),
            HostStatus::Failed(err) => ("✕", theme.danger, Some(err.clone())),
        };
        let name = host.name.clone();
        let retry = matches!(host.status, HostStatus::Failed(_));
        let outdated = host.client.as_ref().is_some_and(|client| client.outdated());
        let confirming = self.confirm_restart.as_ref() == Some(&name);
        let (restart, cancel) = (name.clone(), name.clone());
        v_flex()
            .px_3()
            .pt_2()
            .pb_1()
            .child(
                h_flex()
                    .id(SharedString::from(format!("host-{name}")))
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().text_color(color).child(dot))
                    .child(name.clone())
                    .when(retry, |el| {
                        el.child(div().flex_1())
                            .child(div().hover(|style| style.underline()).child("retry"))
                            .on_click(cx.listener(move |this, _, window, cx| this.connect(name.clone(), window, cx)))
                    })
                    .when(outdated && !confirming, |el| {
                        el.child(div().flex_1())
                            .child(
                                div()
                                    .text_color(theme.warning)
                                    .hover(|style| style.underline())
                                    .child("outdated agent · restart"),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.confirm_restart = Some(restart.clone());
                                cx.notify();
                            }))
                    }),
            )
            .when(confirming, |el| {
                el.child(
                    v_flex()
                        .mt_1()
                        .p_2()
                        .gap_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.warning)
                        .bg(theme.background)
                        .text_xs()
                        .child(
                            div()
                                .whitespace_normal()
                                .child("A new version of the agent is available. Restarting it restarts its terminals: they reopen in place, without their scrollback, and Claude Code resumes its conversation."),
                        )
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .id(SharedString::from(format!("restart-{cancel}")))
                                        .text_color(theme.warning)
                                        .hover(|style| style.underline())
                                        .child("Restart")
                                        .on_click(cx.listener({
                                            let name = cancel.clone();
                                            move |this, _, _, cx| this.restart_agent(name.clone(), cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("restart-cancel-{cancel}")))
                                        .text_color(theme.muted_foreground)
                                        .hover(|style| style.underline())
                                        .child("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.confirm_restart = None;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .children(detail.map(|detail| {
                div()
                    .pl_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(detail)
            }))
            .into_any_element()
    }

    fn render_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        let new_task = self.new_task.as_ref().map(|form| {
            let theme = cx.theme();
            let place = if form.host == LOCAL {
                folder_name(&form.repo)
            } else {
                format!("{}: {}", form.host, folder_name(&form.repo))
            };
            v_flex()
                .mx_2()
                .mb_2()
                .p_2()
                .gap_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_sm()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("New task in {place}")),
                )
                .child(Input::new(&form.input))
                .child(div().text_xs().text_color(theme.muted_foreground).child(if form.busy {
                    "Creating…"
                } else {
                    "Enter to create · Esc to cancel"
                }))
                .children(form.error.clone().map(|error| error_text(error, cx)))
        });
        let ordered = self.ordered(cx);
        let mut sections: Vec<AnyElement> = Vec::new();
        let mut number = 0;
        for host in &self.hosts {
            sections.push(self.render_host_header(host, cx));
            for (key, task) in ordered.iter().filter(|(key, _)| key.host == host.name) {
                sections.push(self.render_task(number, key, task, cx));
                number += 1;
            }
        }
        let theme = cx.theme();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(px(34.))
                    .flex_none()
                    .px_3()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("TASKS"),
            )
            .children(new_task)
            .child(
                v_flex()
                    .id("task-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(sections),
            )
            .child(
                h_flex()
                    .id("open-settings")
                    .h(px(32.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .hover(|style| style.text_color(theme.sidebar_foreground))
                    .child(svg().path("icons/settings.svg").size(px(14.)).text_color(theme.muted_foreground))
                    .child("Settings")
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .into_any_element()
    }

    fn render_task(&self, ix: usize, key: &TaskKey, task: &TaskInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let label: SharedString = task_label(task).into();
        let active = self.active.as_ref() == Some(key);
        let (dot, color) = if self.removing.contains(key) {
            ("…", theme.muted_foreground)
        } else if self.blocked.contains(key) {
            ("●", theme.danger)
        } else if task.working {
            ("●", theme.success)
        } else if self.attention.contains(key) {
            ("◐", theme.warning)
        } else {
            ("○", theme.muted_foreground)
        };
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
            .text_sm()
            .when(active, |el| el.bg(theme.sidebar_accent))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            .child(div().text_color(color).child(dot))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label.clone()),
            )
            .when(ix < 9, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("⌘{}", ix + 1)),
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
            .context_menu({
                let key = key.clone();
                let repo = task.repo.clone();
                let repo_name = folder_name(&task.repo);
                let can_create = known && self.client(&key.host).is_some();
                move |menu, _, _| {
                    let (create, copy, finder, hide, remove) =
                        (key.clone(), key.clone(), key.clone(), key.clone(), key.clone());
                    let repo = repo.clone();
                    menu.item(
                        menu::item(format!("New Task in {repo_name}…"), &weak, move |this, window, cx| {
                            this.start_new_task(create.host.clone(), repo.clone(), window, cx)
                        })
                        .disabled(!can_create),
                    )
                    .separator()
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path))
                            .disabled(!local),
                    )
                    .item(menu::item("Hide", &weak, move |this, _, cx| this.hide_task(&hide, cx)))
                    .separator()
                    .item(
                        menu::item("Delete Task…", &weak, move |this, _, cx| {
                            this.confirm_remove = Some(remove.clone());
                            this.error = None;
                            cx.notify();
                        })
                        .disabled(!removable),
                    )
                }
            });

        let confirm = (self.confirm_remove.as_ref() == Some(key)).then(|| {
            let detail = if local && task.repo.join(".task/remove").is_file() {
                "The repo's .task/remove deletes it; depending on the repo, along with its uncommitted changes."
            } else {
                "With the repo's .task/remove if it has one; otherwise git worktree remove, which won't delete with uncommitted changes."
            };
            v_flex()
                .mx_2()
                .my_1()
                .p_2()
                .gap_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.danger)
                .bg(theme.background)
                .text_xs()
                .child(div().text_sm().child(format!("Delete {label}?")))
                .child(div().text_color(theme.muted_foreground).whitespace_normal().child(detail))
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .id("confirm-remove")
                                .text_color(theme.danger)
                                .hover(|style| style.underline())
                                .child("Delete")
                                .on_click(cx.listener({
                                    let key = key.clone();
                                    move |this, _, window, cx| this.remove_task(key.clone(), window, cx)
                                })),
                        )
                        .child(
                            div()
                                .id("cancel-remove")
                                .text_color(theme.muted_foreground)
                                .hover(|style| style.underline())
                                .child("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                        ),
                )
        });
        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| target.as_ref() == Some(key))
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));

        v_flex().child(row).children(confirm).children(error).into_any_element()
    }
}

impl Render for Sik {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // While the tasks column's edge is being dragged, its width is already
        // in the state before painting.
        let tasks = match self.split.state(window.viewport_size().width, cx).read(cx).sizes().first() {
            Some(width) if self.panel_visible => *width,
            _ => px(0.),
        };
        if let Some(workspace) = self.active_workspace() {
            let width = window.viewport_size().width - tasks;
            workspace.update(cx, |workspace, cx| workspace.set_width(width, cx));
        }
        let title = self.active.as_ref().map(|key| self.label(key)).unwrap_or_else(|| "sik".into());
        v_flex()
            .id("sik")
            .key_context("Sik")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .on_action(cx.listener(Self::toggle_tasks))
            .on_action(cx.listener(Self::new_task_action))
            .on_action(cx.listener(Self::open_task_picker))
            .on_action(cx.listener(Self::previous_task))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .relative()
            .on_action(cx.listener(|this, _: &ActivateTask1, window, cx| this.activate_nth(0, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask2, window, cx| this.activate_nth(1, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask3, window, cx| this.activate_nth(2, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask4, window, cx| this.activate_nth(3, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask5, window, cx| this.activate_nth(4, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask6, window, cx| this.activate_nth(5, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask7, window, cx| this.activate_nth(6, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask8, window, cx| this.activate_nth(7, window, cx)))
            .on_action(cx.listener(|this, _: &ActivateTask9, window, cx| this.activate_nth(8, window, cx)))
            // Our own bar, in the theme's color (macOS's is gray): the traffic
            // lights on the left, the active task in the middle, and it drags
            // and zooms like the system one.
            .child(
                TitleBar::new().child(
                    div()
                        .flex_1()
                        .flex()
                        .justify_center()
                        .pr(px(72.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(title),
                ),
            )
            .child({
                let visible = self.panel_visible;
                div().flex_1().min_h_0().w_full().child(h_resizable("sik-split")
                    .with_state(self.split.state(window.viewport_size().width, cx))
                    .child(
                        resizable_panel()
                            .size(config::width(Config::get(cx).layout.tasks, 160., 500.))
                            .size_range(px(160.)..px(500.))
                            .visible(visible)
                            .child(self.render_column(cx)),
                    )
                    .child(resizable_panel().child(div().size_full().children(self.active_workspace())))
                    .on_resize(move |state, _, cx| {
                        if visible && let Some(width) = state.read(cx).sizes().first() {
                            let width = f32::from(*width);
                            Config::update_quietly(cx, |config| config.layout.tasks = width);
                        }
                    }))
            })
            .children(self.task_picker.as_ref().map(|(picker, _)| {
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
            .children(self.quit_confirm.as_ref().map(|focus| self.render_quit_confirm(focus, cx)))
    }
}

fn error_text(error: SharedString, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
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

/// `~/something` → the home folder plus `something`.
/// A small icon button, next to a field.
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

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `repo/branch`, or the folder name if it has no branch.
fn task_label(task: &TaskInfo) -> String {
    let repo = folder_name(&task.repo);
    match &task.branch {
        Some(branch) => format!("{repo}/{branch}"),
        None if task.path == task.repo => repo,
        None => format!("{repo}/{}", folder_name(&task.path)),
    }
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
