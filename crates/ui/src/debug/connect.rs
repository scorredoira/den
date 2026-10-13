//! Starting the program, or reaching one already running, and the
//! conversation with it: what it's asked and what it says.

use super::*;

impl Debugger {
    // ---- launching ----

    /// F5: continues the focused VM, or starts a session.
    pub fn start_or_continue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_stopped() {
            self.resume("continue", cx);
        } else if self.status == Status::Idle {
            self.start(window, cx);
        }
    }

    pub fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status != Status::Idle {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.info("No agent: can't debug".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        };
        self.started = None;
        self.status = Status::Connecting("Reading the launch file…".into());
        self.generation += 1;
        let generation = self.generation;
        let path = self.root.join(LAUNCH_FILE);
        cx.notify();
        // after the notify: the workspace is debugging by then, and the tab
        // shows in the debugging layout, not in the editing one it goes back to
        cx.emit(DebugEvent::Reveal);
        cx.spawn_in(window, async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                let file = match read {
                    Ok(Response::Bytes(bytes)) => parse_launch_file(&String::from_utf8_lossy(&bytes)),
                    Ok(other) => Err(format!("unexpected response {other:?}")),
                    Err(_) => Err(format!("There is no {LAUNCH_FILE}: create it to say how to start the program.")),
                };
                match file {
                    Ok(mut file) => {
                        this.tests = file.tests;
                        this.targets = file.targets;
                        this.launch_error = None;
                        let target = this.target().map(str::to_string);
                        if let Some(command) = &mut file.launch.command {
                            *command = with_target(command, target.as_deref());
                        }
                        this.begin(file.launch, true, window, cx);
                    }
                    Err(error) => {
                        this.launch_error = Some(error.clone());
                        this.fail(error, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Debugs `command`, which listens on `port`: the test `test` of `path`, from the code.
    pub fn launch_command(
        &mut self,
        command: String,
        port: u16,
        path: PathBuf,
        test: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.status != Status::Idle {
            self.info("A program is being debugged: stop it first (Shift-F5)".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        }
        if self.client.is_none() {
            self.info("No agent: can't debug".into(), cx);
            cx.emit(DebugEvent::Reveal);
            return;
        }
        self.generation += 1;
        self.launching_test = Some((path, test));
        self.begin(Launch { command: Some(command), port }, false, window, cx);
        // after begin's notify, as in start
        cx.emit(DebugEvent::Reveal);
    }

    /// Reads the launch file again: its problems show in the panel, its
    /// tests in the code.
    pub fn refresh_launches(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let path = self.root.join(LAUNCH_FILE);
        cx.spawn(async move |this, cx| {
            let read = client.request(Request::ReadFile { path }).await;
            this.update(cx, |this, cx| {
                match read.ok().and_then(|response| match response {
                    Response::Bytes(bytes) => Some(parse_launch_file(&String::from_utf8_lossy(&bytes))),
                    _ => None,
                }) {
                    Some(Ok(file)) => {
                        this.tests = file.tests;
                        this.targets = file.targets;
                        this.launch_error = None;
                    }
                    Some(Err(error)) => {
                        this.tests = None;
                        this.targets = Vec::new();
                        this.launch_error = Some(error);
                    }
                    None => {
                        this.tests = None;
                        this.targets = Vec::new();
                        this.launch_error = Some(format!("There is no {LAUNCH_FILE}."));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn create_launch_file(&mut self, cx: &mut Context<Self>) -> PathBuf {
        let path = self.root.join(LAUNCH_FILE);
        if let Some(client) = self.client.clone() {
            let dir = self.root.join(".den");
            let file = path.clone();
            cx.spawn(async move |this, cx| {
                let _ = client.request(Request::CreateDir { path: dir }).await;
                let _ = client
                    .request(Request::WriteFile { path: file, data: LAUNCH_TEMPLATE.as_bytes().to_vec() })
                    .await;
                this.update(cx, |this, cx| this.refresh_launches(cx)).ok();
            })
            .detach();
        }
        path
    }

    /// With `open_page`, the program's page is opened once it listens.
    pub(super) fn begin(&mut self, launch: Launch, open_page: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.console.clear();
        self.console_dropped = self.console_written;
        self.ran = None;
        self.page = None;
        self.revealed = None;
        self.launched = false;
        self.open_page = open_page;
        self.started = Some(Started { launch: launch.clone(), test: self.launching_test.clone(), file: None });
        self.status = Status::Connecting(format!("Connecting to port {}…", launch.port));
        self.connect(launch.port, launch.command, window, cx);
        cx.notify();
    }

    /// A session connected, for tests.
    #[cfg(test)]
    pub fn pretend_connected(&mut self, cx: &mut Context<Self>) {
        // attached to a program on a port nothing listens on: a restart tries it again
        let launch = parse_launch_file(r#"{"port":1}"#).map(|file| file.launch).ok();
        self.started = launch.map(|launch| Started { launch, test: None, file: None });
        self.status = Status::Connected;
        cx.notify();
    }

    /// The program never listened, for tests: the session fails.
    #[cfg(test)]
    pub fn pretend_failed(&mut self, cx: &mut Context<Self>) {
        self.fail("the program ended before it listened".into(), cx);
    }

    /// A line from the program, for tests.
    #[cfg(test)]
    pub fn receive(&mut self, line: &str, cx: &mut Context<Self>) {
        self.on_line(line, cx);
    }

    /// The launch's line as its terminal runs it (`ran`), and the file its
    /// `${file}` was, which a restart of the session debugs again.
    pub fn set_ran(&mut self, session: u64, line: String, file: Option<String>) {
        if session != self.generation {
            return;
        }
        self.ran = Some(line);
        if let Some(started) = &mut self.started
            && started.file.is_none()
        {
            started.file = Some(file);
        }
    }

    /// The terminal the command of `session` went to, once it is known.
    pub fn set_terminal(&mut self, session: u64, term: Option<TermId>, cx: &mut Context<Self>) {
        if let Some((_, interrupt)) = self.term_unknown.take_if(|(unknown, _)| *unknown == session)
            && interrupt
            && let Some(term) = term
        {
            cx.emit(DebugEvent::Interrupt { term });
        }
        if term.is_some() && term != self.term {
            self.term = term;
            self.save(cx);
        }
    }

    /// Tries to reach the program until it listens, the time is up or the
    /// command started ends. A program that already listens is attached to;
    /// otherwise `command` starts it. A command with `${port}` listens on a
    /// free port of its own: it is always started, never attached to
    /// another program, a debugger of another window included.
    pub(super) fn connect(
        &mut self,
        port: u16,
        mut command: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let generation = self.generation;
        let started = Instant::now();
        cx.spawn_in(window, async move |this, cx| {
            let mut port = port;
            if let Some(line) = command.as_mut().filter(|line| line.contains("${port}")) {
                let free = match client.request(Request::FreePort).await {
                    Ok(Response::Port(free)) => free,
                    Ok(other) => {
                        this.update(cx, |this, cx| this.fail_session(generation, format!("unexpected response {other:?}"), cx)).ok();
                        return;
                    }
                    Err(error) => {
                        this.update(cx, |this, cx| this.fail_session(generation, format!("No free port: {error:#}"), cx)).ok();
                        return;
                    }
                };
                port = free;
                *line = line.replace("${port}", &free.to_string());
            }
            // Seen running in its terminal: once the terminal is idle again,
            // the program ended without listening. Until a new terminal is
            // known, `term` is the one before.
            let mut seen_running = false;
            let mut waited: Option<Instant> = None;
            let mut watched = None;
            let mut last_read = Instant::now();
            loop {
                let (tx, rx) = smol::channel::unbounded::<RelayUpdate>();
                match client.connect_relay(port, move |update| {
                    let _ = tx.try_send(update);
                }).await {
                    Ok(relay) => {
                        this.update(cx, |this, cx| this.connected(generation, client.clone(), relay, rx, cx)).ok();
                        return;
                    }
                    Err(error) => {
                        let Ok((gone, term)) = this.update(cx, |this, _| (this.generation != generation, this.term)) else {
                            return;
                        };
                        if gone {
                            return;
                        }
                        let busy = match term {
                            Some(term) => matches!(client.request(Request::TermBusy { term }).await, Ok(Response::Busy(true))),
                            None => false,
                        };
                        // What still runs in its terminal (the program before,
                        // ending) gets a moment; then the command takes a new
                        // terminal, and that one is closed.
                        if command.is_some() && busy && waited.get_or_insert_with(Instant::now).elapsed() < RESTART_TIMEOUT {
                            this.update(cx, |this, cx| {
                                if this.generation == generation {
                                    this.status = Status::Connecting("Waiting for the program before to end…".into());
                                    cx.notify();
                                }
                            })
                            .ok();
                            cx.background_executor().timer(CONNECT_RETRY).await;
                            continue;
                        }
                        // every await above may have outlived the session
                        if let Some(command) = command.take() {
                            let ran = this.update(cx, |this, cx| {
                                if this.generation != generation {
                                    return false;
                                }
                                this.launched = true;
                                this.info(format!("$ {command}"), cx);
                                this.status = Status::Connecting("Starting the program…".into());
                                // what still runs in its terminal would read the line as its input
                                let term = if busy { None } else { this.term };
                                this.term_unknown = Some((generation, false));
                                cx.emit(DebugEvent::Run { session: generation, term, line: command });
                                cx.notify();
                                true
                            });
                            if !matches!(ran, Ok(true)) {
                                return;
                            }
                            continue;
                        }
                        if term != watched {
                            watched = term;
                            seen_running = false;
                        }
                        if busy {
                            seen_running = true;
                            // what the command says it's doing (building an app takes a while)
                            if let Some(term) = term
                                && last_read.elapsed() >= PROGRESS_POLL
                            {
                                last_read = Instant::now();
                                if let Ok(Response::Text(text)) = client.request(Request::TermRead { term, lines: 8 }).await
                                    && let Some(line) = text.lines().rev().map(str::trim).find(|line| !line.is_empty())
                                {
                                    let line = line.to_string();
                                    this.update(cx, |this, cx| {
                                        if this.generation == generation {
                                            this.status = Status::Connecting(format!("Starting: {line}"));
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                                }
                            }
                        } else if seen_running {
                            this.update(cx, |this, cx| {
                                let error = format!("The program ended without listening on port {port}: see its terminal");
                                this.fail_session(generation, error, cx)
                            })
                            .ok();
                            return;
                        }
                        // a command still at work (a build) is waited for; Stop ends it
                        if started.elapsed() > CONNECT_TIMEOUT && !busy {
                            this.update(cx, |this, cx| {
                                this.fail_session(generation, format!("Nothing answered on port {port}: {error:#}"), cx)
                            })
                            .ok();
                            return;
                        }
                        cx.background_executor().timer(CONNECT_RETRY).await;
                    }
                }
            }
        })
        .detach();
    }

    /// Opens `url` in the browser once a terminal of the workspace listens on
    /// its port, forwarded over SSH from a server; never if the session ends
    /// first (a script the command ran instead of the server).
    pub(super) fn open_when_listening(&mut self, url: String, cx: &mut Context<Self>) {
        let (Some(client), Some(port)) = (self.client.clone(), Client::loopback_port(&url)) else {
            return;
        };
        let generation = self.generation;
        let group = self.root.to_string_lossy().into_owned();
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(gone) = this.update(cx, |this, _| this.generation != generation) else {
                    return;
                };
                if gone {
                    return;
                }
                if started.elapsed() > OPEN_TIMEOUT {
                    let message = format!("The page wasn't opened: nothing listened on port {port}");
                    this.update(cx, |this, cx| this.info(message, cx)).ok();
                    return;
                }
                if let Ok(Response::Ports(ports)) = client.request(Request::Ports).await
                    && ports.iter().any(|info| info.port == port && info.group == group)
                {
                    break;
                }
                cx.background_executor().timer(OPEN_POLL).await;
            }
            // a tab already showing the program comes to the front as it is
            let result = cx
                .background_spawn(async move {
                    let url = client.local_url(&url)?;
                    let focused = crate::browser::focus_tab(&url);
                    anyhow::Ok((url, focused))
                })
                .await;
            this.update(cx, |this, cx| match result {
                Ok((_, Ok(true))) => {}
                Ok((url, Ok(false))) => cx.open_url(&url),
                Ok((url, Err(err))) => {
                    this.info(format!("Can't look for its tab in Chrome: {err:#}"), cx);
                    cx.open_url(&url);
                }
                Err(err) => this.info(format!("Can't open the page: {err:#}"), cx),
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn connected(
        &mut self,
        generation: u64,
        client: Arc<Client>,
        relay: u64,
        rx: smol::channel::Receiver<RelayUpdate>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            client.close_relay(relay);
            return;
        }
        self.conn = Some(Conn { client, relay, next_id: 0, pending: HashMap::new(), counting: false, cwd: self.root.clone() });
        cx.spawn(async move |this, cx| {
            while let Ok(update) = rx.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        match update {
                            RelayUpdate::Line(line) => this.on_line(&line, cx),
                            RelayUpdate::Closed => this.end("The program ended", cx),
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        // `running` in `hello` is only how many ran then
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(THREADS_POLL).await;
                let alive = this
                    .update(cx, |this, _| {
                        if this.generation != generation {
                            return false;
                        }
                        let counting = this.conn.as_ref().is_some_and(|conn| conn.counting);
                        if this.status == Status::Connected && !counting {
                            if let Some(conn) = &mut this.conn {
                                conn.counting = true;
                            }
                            this.send("threads", json!({}), |this, result, cx| {
                                if let Some(conn) = &mut this.conn {
                                    conn.counting = false;
                                }
                                let running = result.ok().and_then(|body| body.get("running").and_then(Value::as_u64));
                                if let Some(running) = running.filter(|running| *running != this.running) {
                                    this.running = running;
                                    cx.notify();
                                }
                            });
                        }
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
        })
        .detach();
        self.send("hello", json!({ "version": protocol::VERSION }), |this, result, cx| match result {
            Ok(body) => this.on_hello(body, cx),
            Err(error) => this.fail(format!("The program refused the debugger: {error}"), cx),
        });
    }

    pub(super) fn on_hello(&mut self, body: Map<String, Value>, cx: &mut Context<Self>) {
        if let Some(conn) = &mut self.conn
            && let Some(cwd) = body.get("cwd").and_then(Value::as_str)
        {
            conn.cwd = PathBuf::from(cwd);
        }
        self.status = Status::Connected;
        self.launching_test = None;
        self.running = body.get("running").and_then(Value::as_u64).unwrap_or(0);

        let files: Vec<PathBuf> = self.breakpoints.files().map(|(path, _)| path.to_path_buf()).collect();
        for path in files {
            self.send_breakpoints(&path, cx);
        }
        self.send_exceptions();
        // As Visual Studio does, a program that starts stops at its entry,
        // wherever the program says that is; a server, which has a page,
        // just runs.
        let page = page_of(&body);
        self.page = page.clone();
        if body.get("waiting").and_then(Value::as_bool).unwrap_or(false) {
            self.send("run", json!({ "entry": page.is_none() }), |_, _, _| {});
        }
        // the page of a program this launch started, which an attached one
        // or a restart doesn't open again
        if let Some(url) = page
            && self.launched
            && std::mem::take(&mut self.open_page)
        {
            self.open_when_listening(url, cx);
        }
        let stops: Vec<Stop> = protocol::field(&body, "stopped").unwrap_or_default();
        for stop in stops {
            self.on_stop(stop, cx);
        }
        let connected = if self.launched { "Connected" } else { "Attached to the program already running" };
        self.info(connected.into(), cx);
        cx.notify();
    }

    /// Stops the session: the program launched is interrupted, one attached
    /// to goes on running.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Idle {
            if std::mem::take(&mut self.ended) {
                cx.notify();
            }
            return;
        }
        if self.launched {
            match &mut self.term_unknown {
                // the command went to a terminal not known yet: that one, once it is
                Some((session, interrupt)) if *session == self.generation => *interrupt = true,
                _ => {
                    if let Some(term) = self.term {
                        cx.emit(DebugEvent::Interrupt { term });
                    }
                }
            }
        }
        self.end("Stopped", cx);
        self.ended = false;
    }

    /// Stops the session and starts it again as it started: the same
    /// command, test and file, not the one open now (a stop opens others).
    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let started = self.started.clone().filter(|_| self.status != Status::Idle);
        let launched = self.launched;
        self.stop(cx);
        // nothing to repeat, or not past reading the launch file: F5's start
        let Some(started) = started else {
            self.start(window, cx);
            return;
        };
        self.launched = false;
        self.status = Status::Connecting("Restarting…".into());
        let generation = self.generation;
        // A program it started frees its terminal first, and its port when
        // the command has no `${port}`: what still answers there is the
        // program before, which the new session would attach to.
        let port = started
            .launch
            .command
            .as_ref()
            .filter(|command| launched && !command.contains("${port}"))
            .map(|_| started.launch.port);
        let client = self.client.clone();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let deadline = Instant::now() + RESTART_TIMEOUT;
            loop {
                let Ok(Some(term)) = this.update(cx, |this, _| (this.generation == generation).then_some(this.term)) else {
                    return;
                };
                let busy = match (launched, term, &client) {
                    (true, Some(term), Some(client)) => {
                        matches!(client.request(Request::TermBusy { term }).await, Ok(Response::Busy(true)))
                    }
                    _ => false,
                };
                let held = match (port, &client) {
                    (Some(port), Some(client)) => match client.connect_relay(port, |_: RelayUpdate| {}).await {
                        Ok(relay) => {
                            client.close_relay(relay);
                            true
                        }
                        Err(_) => false,
                    },
                    _ => false,
                };
                if !busy && !held {
                    break;
                }
                if Instant::now() >= deadline {
                    if let Some(port) = port.filter(|_| held) {
                        let error = format!("The program before still listens on port {port}: see its terminal");
                        this.update(cx, |this, cx| this.fail_session(generation, error, cx)).ok();
                        return;
                    }
                    // a terminal still busy: the command goes to a new one
                    break;
                }
                cx.background_executor().timer(CONNECT_RETRY).await;
            }
            this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                let mut launch = started.launch;
                if let (Some(command), Some(file)) = (&mut launch.command, &started.file) {
                    *command = command_line(command, file.as_deref());
                }
                this.launching_test = started.test;
                // the page is open already
                this.begin(launch, false, window, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn end(&mut self, why: &str, cx: &mut Context<Self>) {
        self.changed.clear();
        let was = self.status != Status::Idle;
        self.generation += 1;
        if let Some(conn) = self.conn.take() {
            conn.client.close_relay(conn.relay);
        }
        self.status = Status::Idle;
        self.launching_test = None;
        self.stops.clear();
        self.focus = None;
        self.locals.clear();
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        self.forget_console_refs(None);
        self.running = 0;
        for watch in &mut self.watches {
            watch.result = None;
        }
        if was {
            self.ended = true;
            self.info(why.into(), cx);
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    pub fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.end("", cx);
        self.push_console(ConsoleLine::Error(error));
        cx.notify();
    }

    /// `fail`, if `generation` is still the session: an older one's work
    /// that comes back late ends nothing.
    pub(super) fn fail_session(&mut self, generation: u64, error: String, cx: &mut Context<Self>) {
        if self.generation == generation {
            self.fail(error, cx);
        }
    }

    /// Asks the program to let the person pick a widget on its screen (its
    /// `inspect` command): the line that made it comes back as a reveal.
    /// False when nothing is being debugged.
    pub fn inspect(&mut self) -> bool {
        if self.conn.is_none() {
            return false;
        }
        self.send("inspect", json!({ "on": true }), |this, result, cx| {
            if let Err(error) = result {
                this.info(format!("The program can't inspect: {error}"), cx);
            }
        });
        true
    }

    // ---- the protocol ----

    pub(super) fn send(
        &mut self,
        cmd: &str,
        args: Value,
        reply: impl FnOnce(&mut Debugger, Result<Map<String, Value>, String>, &mut Context<Debugger>) + 'static,
    ) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        conn.next_id += 1;
        let id = conn.next_id;
        conn.pending.insert(id, Box::new(reply));
        conn.client.relay_send(conn.relay, protocol::request(id, cmd, args));
    }

    pub(super) fn on_line(&mut self, line: &str, cx: &mut Context<Self>) {
        match protocol::parse(line) {
            Ok(Message::Response { id, result }) => {
                let reply = self.conn.as_mut().and_then(|conn| conn.pending.remove(&id));
                if let Some(reply) = reply {
                    reply(self, result, cx);
                }
            }
            Ok(Message::Event(Event::Stopped(stop))) => self.on_stop(*stop, cx),
            Ok(Message::Event(Event::Resumed { vm })) => self.on_resumed(vm, cx),
            Ok(Message::Event(Event::Reveal { file, line })) => {
                let path = self.local_path(&file);
                self.revealed = Some((file, line));
                cx.emit(DebugEvent::Show { path, line: line.saturating_sub(1), focus: true });
                cx.emit(DebugEvent::Raise);
            }
            Ok(Message::Event(Event::Output { text, file, line, error })) => {
                let path = (!file.is_empty()).then(|| self.local_path(&file));
                self.push_console(ConsoleLine::Output { text: text.trim_end().to_string(), path, line, error });
                cx.notify();
            }
            Ok(Message::Unknown) => {}
            Err(error) => self.info(format!("debugger: {error}"), cx),
        }
    }

    pub(super) fn on_stop(&mut self, stop: Stop, cx: &mut Context<Self>) {
        self.serial += 1;
        let vm = stop.vm;
        if let Some(exc) = &stop.exception {
            self.push_console(ConsoleLine::Error(format!("Exception: {}", exc.message)));
        }
        self.forget_console_refs(Some(vm));
        self.changed = changed_locals(self.stops.get(&vm).map(|previous| &previous.stop), &stop);
        self.locals = stop.locals.clone();
        self.globals = stop.globals;
        let show = stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
        self.stops.insert(vm, VmStop { stop, serial: self.serial, resumed: false, going: false });
        self.focus = Some(vm);
        self.frame = 0;
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        self.value_edit = None;
        cx.emit(DebugEvent::Reveal);
        self.refetch();
        self.evaluate_watches(cx);
        if let Some((path, line)) = show {
            cx.emit(DebugEvent::Show { path, line, focus: true });
        }
        cx.emit(DebugEvent::Marks);
        cx.notify();
    }

    pub(super) fn on_resumed(&mut self, vm: u64, cx: &mut Context<Self>) {
        let Some(stop) = self.stops.get_mut(&vm) else {
            return;
        };
        stop.resumed = true;
        let serial = stop.serial;
        if self.focus == Some(vm) {
            self.hover = None;
        }
        self.forget_console_refs(Some(vm));
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESUME_GRACE).await;
            this.update(cx, |this, cx| {
                if this.stops.get(&vm).is_some_and(|stop| stop.serial == serial && stop.resumed) {
                    this.stops.remove(&vm);
                    if this.focus == Some(vm) {
                        this.focus_next(cx);
                    }
                    cx.emit(DebugEvent::Marks);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The console's values of `vm` (of every VM, without one) can't be
    /// opened any more: a ref is valid only while its VM stays stopped.
    pub(super) fn forget_console_refs(&mut self, vm: Option<u64>) {
        for line in &mut self.console {
            if let ConsoleLine::Result(var, from) = line
                && vm.is_none_or(|vm| vm == *from)
            {
                var.reference = 0;
            }
        }
    }

    /// After the focused VM went on, shows another one still stopped.
    pub(super) fn focus_next(&mut self, cx: &mut Context<Self>) {
        self.changed.clear();
        self.focus = self.stops.iter().find(|(_, stop)| !stop.resumed).map(|(vm, _)| *vm);
        self.frame = 0;
        self.children.clear();
        self.hover = None;
        self.loading.clear();
        match self.focus.and_then(|vm| self.stops.get(&vm)) {
            Some(stop) => {
                self.locals = stop.stop.locals.clone();
                self.globals = stop.stop.globals;
                let show = stop.stop.frames.first().map(|frame| (self.local_path(&frame.file), frame.line.saturating_sub(1)));
                self.refetch();
                self.evaluate_watches(cx);
                if let Some((path, line)) = show {
                    cx.emit(DebugEvent::Show { path, line, focus: false });
                }
            }
            None => {
                self.locals.clear();
                for watch in &mut self.watches {
                    watch.result = None;
                }
            }
        }
    }
}
