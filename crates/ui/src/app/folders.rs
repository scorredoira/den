//! Opening folders and making worktrees: from the menus, from a terminal's
//! `den <path>`, the recent ones, and removing them.

use super::*;

impl Den {
    /// Opens `path` on `host`: the task it is, or the folder on its own.
    pub(super) fn open_path(&mut self, host: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.activate(TaskKey { host, path }, window, cx);
    }

    /// `den <path>`: `root` as a workspace (the worktree containing it, if
    /// any), with `file` open in it. A server not yet connected enters it on
    /// connecting.
    pub(super) fn open_from_terminal(
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
    pub(super) fn open_start(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn workspace_containing(&self, host: &str, path: &Path) -> PathBuf {
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
    pub(super) fn open_folder(&mut self, _: &OpenFolder, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn open_folder_on(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if host == LOCAL {
            self.open_folder(&OpenFolder, window, cx);
        } else {
            self.open_folder_picker(host, window, cx);
        }
    }

    /// The system's dialog for choosing a folder on this machine.
    pub(super) fn pick_local_folder(
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
    pub(super) fn open_remote_folder(&mut self, _: &OpenRemoteFolder, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn recents(&self, cx: &App) -> Vec<(TaskKey, String)> {
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
    pub(super) fn open_recent(&mut self, _: &OpenRecent, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn remove_folder(&mut self, key: &TaskKey, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn close_folder(&mut self, key: &TaskKey, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn add_folder(&mut self, host: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn new_task_action(&mut self, _: &NewTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.active.clone() else {
            return;
        };
        if let Some(repo) = self.task(&key).map(|task| task.repo.clone()) {
            self.start_new_task(key.host, repo, window, cx);
        }
    }

    pub(super) fn start_new_task(&mut self, host: SharedString, repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn create_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn new_task_error(&mut self, error: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.new_task {
            form.busy = false;
            form.error = Some(error.into());
            form.input.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    /// Esc closes whatever is open in the column.
    pub(super) fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.confirm_remove = None;
        self.confirm_force_remove = None;
        self.error = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// With `force`, along with what git won't delete: uncommitted changes.
    pub(super) fn remove_task(&mut self, key: TaskKey, force: bool, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn init_git(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn move_task(&mut self, dragged: &TaskKey, target: &TaskKey, cx: &mut Context<Self>) {
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

    pub(super) fn refresh_repos(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
}
