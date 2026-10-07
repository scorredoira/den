//! The servers: connecting to each one's agent, following what it reports,
//! reconnecting when the connection drops, and adding or removing them.

use super::*;

impl Den {
    /// Connects to a server (over SSH, installing the agent if needed; or to
    /// the local agent), retrying with increasing backoff until it succeeds.
    pub(super) fn connect(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn connected(&mut self, name: SharedString, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn track(&mut self, name: SharedString, client: &Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn restart_agent(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn lost(&mut self, name: SharedString, client: &Weak<Client>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn is_current(&self, name: &str, client: &Arc<Client>) -> bool {
        self.client(name).is_some_and(|current| Arc::ptr_eq(&current, client))
    }

    /// Receives the agents running on the server and the tasks created with
    /// `den task` on it.
    pub(super) fn watch_host(&mut self, name: SharedString, client: Arc<Client>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn refresh_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn forget_gone(&mut self, key: TaskKey, window: &mut Window, cx: &mut Context<Self>) {
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

    /// Registers the server `destination` (from `~/.ssh/config`,
    /// `user@host` or, on Windows, `wsl:<distro>`) and connects; returns whether it was added.
    pub(super) fn add_host_named(&mut self, destination: String, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
    pub(super) fn open_host_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn show_host_picker(&mut self, mut hosts: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn open_folder_picker(&mut self, host: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let start = self
            .host(&host)
            .and_then(|host| host.repos.first().or(host.tasks.first().map(|task| &task.repo)))
            .and_then(|repo| repo.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("~"));
        self.open_folder_picker_at(host, start, window, cx);
    }

    pub(super) fn open_folder_picker_at(&mut self, host: SharedString, start: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn remove_host(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(super) fn forget_host(&mut self, name: SharedString, window: &mut Window, cx: &mut Context<Self>) {
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
}
