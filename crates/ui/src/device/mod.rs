//! The Device panel: the screen of a phone, a simulator or an emulator, that a
//! program of the workspace serves, with the mouse as a finger and the
//! keyboard as the phone's. The device file (`.den/device.json`) names the
//! programs; without it the panel isn't there, not even its icon. Den knows
//! nothing of iOS or Android: each program speaks `protocol`, and its frames
//! are IOSurfaces of this Mac (`surface`), so the panel is only for local
//! workspaces on macOS.

pub mod protocol;
#[cfg(target_os = "macos")]
mod surface;

use std::{
    cell::Cell,
    collections::HashMap,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    sync::{Arc, Mutex},
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex,
    button::{Button, ButtonVariants as _},
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::*;
use crate::menu::PanelItems as _;
use protocol::{Event, Listed, Modifiers, Screen, Touch};
use serde::Deserialize;

use crate::{
    DebugContinue, DebugStop,
    config::UiText as _,
    debug::panel::tool,
    shortcuts,
};

/// Where the device programs are named, relative to the workspace.
pub const DEVICE_FILE: &str = ".den/device.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceFile {
    /// Paths relative to the workspace.
    programs: Vec<String>,
}

/// What the device file says.
#[derive(Clone, Debug, PartialEq)]
enum Programs {
    /// No file, a workspace on a server, or not macOS: no panel.
    None,
    Listed(Vec<String>),
    /// The file is there but can't be used: the panel says why.
    Broken(String),
}

/// How much of what a program wrote to stderr is kept, to say why it ended.
const STDERR_TAIL: usize = 2000;

/// What the panel asks of the workspace.
pub enum DeviceEvent {
    /// Pick a widget on the phone: the program being debugged shows the
    /// line that made it.
    Inspect,
    /// Its play: debug, as F5 does.
    Debug,
    /// Its stop: stop debugging, as Shift-F5 does.
    Stop,
}

impl EventEmitter<DeviceEvent> for Device {}

/// A device, and the program that serves it (as the device file names it).
#[derive(Clone, Debug, PartialEq)]
struct Choice {
    program: String,
    device: Listed,
}

#[derive(Clone, Debug, PartialEq)]
enum Status {
    Idle,
    Starting,
    Running,
    /// The program ended, or couldn't start or be understood.
    Failed(String),
}

/// The program serving the device on screen.
struct Serving {
    /// Commands for its stdin; dropping it closes the stdin, and the program
    /// ends.
    commands: smol::channel::Sender<String>,
}

pub struct Device {
    root: PathBuf,
    local: bool,
    programs: Programs,
    focus: FocusHandle,
    /// What `list` gave, program by program; unset until it's been asked.
    devices: Option<Vec<Choice>>,
    /// Programs whose `list` failed, and why.
    list_errors: Vec<String>,
    chosen: Option<Choice>,
    status: Status,
    serving: Option<Serving>,
    /// The latest error a program reported while serving; it keeps going.
    warning: Option<String>,
    /// Bumped by each start and stop: what a program that ended says
    /// afterwards is ignored.
    generation: u64,
    screen: Option<Screen>,
    /// The program's surfaces, opened by number; a new size brings new ones.
    #[cfg(target_os = "macos")]
    surfaces: HashMap<u32, core_video::pixel_buffer::CVPixelBuffer>,
    #[cfg(target_os = "macos")]
    frame: Option<core_video::pixel_buffer::CVPixelBuffer>,
    /// Where the screen was last painted, to turn the mouse into a point of it.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// A finger is down; with true, two (a pinch).
    touching: Option<bool>,
    /// The device the program being debugged runs on, to show once the
    /// programs have listed it.
    wanted: Option<String>,
    /// `den device show` without a device: serve the chosen one once the
    /// programs have listed theirs.
    serve_chosen: bool,
    /// A debugging session is on: the play is its stop.
    debugging: bool,
    /// What the session does while it starts the program.
    starting: Option<String>,
}

impl Device {
    pub fn new(root: PathBuf, local: bool, cx: &mut Context<Self>) -> Self {
        let mut device = Self {
            root,
            local,
            programs: Programs::None,
            focus: cx.focus_handle(),
            devices: None,
            list_errors: Vec::new(),
            chosen: None,
            status: Status::Idle,
            serving: None,
            warning: None,
            generation: 0,
            screen: None,
            #[cfg(target_os = "macos")]
            surfaces: HashMap::new(),
            #[cfg(target_os = "macos")]
            frame: None,
            bounds: Rc::new(Cell::new(Bounds::default())),
            touching: None,
            wanted: None,
            serve_chosen: false,
            debugging: false,
            starting: None,
        };
        device.load(cx);
        device
    }

    /// Whether the workspace has the panel: its icon shows.
    pub fn available(&self) -> bool {
        self.programs != Programs::None
    }

    /// Reads the device file, again whenever `.den` changes. Other programs
    /// are asked for their devices again.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        if !self.local || !cfg!(target_os = "macos") {
            return;
        }
        let path = self.root.join(DEVICE_FILE);
        cx.spawn(async move |this, cx| {
            let programs = cx.background_spawn(async move { read_device_file(&path) }).await;
            this.update(cx, |this, cx| {
                if this.programs != programs {
                    this.programs = programs;
                    this.devices = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `den where`: the programs, the devices they listed (when the panel
    /// last asked: `booted` may be old), the one on screen and how it goes.
    pub fn state(&self) -> serde_json::Value {
        let device = |choice: &Choice| {
            serde_json::json!({
                "program": choice.program,
                "id": choice.device.id,
                "name": choice.device.name,
                "booted": choice.device.booted,
            })
        };
        let programs = match &self.programs {
            Programs::None => return serde_json::json!({ "available": false }),
            Programs::Listed(programs) => serde_json::json!(programs),
            Programs::Broken(error) => serde_json::json!({ "error": error }),
        };
        let status = match &self.status {
            Status::Idle => "idle".to_string(),
            Status::Starting => "starting".to_string(),
            Status::Running => "running".to_string(),
            Status::Failed(why) => format!("failed: {why}"),
        };
        let mut out = serde_json::json!({
            "available": true,
            "programs": programs,
            "status": status,
            "debugging": self.debugging,
        });
        if let Some(devices) = &self.devices {
            out["devices"] = devices.iter().map(device).collect();
        }
        if let Some(chosen) = &self.chosen {
            out["chosen"] = device(chosen);
        }
        if !self.list_errors.is_empty() {
            out["listErrors"] = serde_json::json!(self.list_errors);
        }
        if let Some(warning) = &self.warning {
            out["warning"] = serde_json::json!(warning);
        }
        out
    }

    /// The panel came to the front: the devices are asked for the first time.
    pub fn shown(&mut self, cx: &mut Context<Self>) {
        if self.devices.is_none() {
            self.refresh(cx);
        }
    }

    /// Asks each program for its devices.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Programs::Listed(programs) = self.programs.clone() else {
            return;
        };
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let lists = cx
                .background_spawn(async move {
                    programs.into_iter().map(|program| (program.clone(), list(&root, &program))).collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                let mut devices = Vec::new();
                this.list_errors.clear();
                for (program, result) in lists {
                    match result {
                        Ok(listed) => devices.extend(listed.into_iter().map(|device| Choice { program: program.clone(), device })),
                        Err(err) => this.list_errors.push(format!("{program}: {err:#}")),
                    }
                }
                let wanted = this.wanted.take().and_then(|id| devices.iter().find(|choice| choice.device.id == id).cloned());
                if this.chosen.as_ref().is_none_or(|chosen| !devices.contains(chosen)) && this.serving.is_none() {
                    // The booted first: a program lists them so.
                    this.chosen = devices.first().cloned();
                }
                this.devices = Some(devices);
                let chosen = this.chosen.clone().filter(|_| std::mem::take(&mut this.serve_chosen));
                match wanted.or(chosen) {
                    Some(choice) => this.choose(choice, cx),
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Shows the device the program being debugged runs on, by its id: at
    /// once if it's listed, else once the programs list it again (a device
    /// booted since).
    pub fn show_device(&mut self, id: String, cx: &mut Context<Self>) {
        let listed = self.devices.as_ref().and_then(|devices| devices.iter().find(|choice| choice.device.id == id).cloned());
        match listed {
            Some(choice) => self.choose(choice, cx),
            None => {
                self.wanted = Some(id);
                self.refresh(cx);
            }
        }
    }

    /// `den device show`: serves the device `id`, or the one chosen (the
    /// first booted) once the programs have listed theirs.
    pub fn serve(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = id {
            self.show_device(id, cx);
            return;
        }
        match self.chosen.clone().filter(|_| self.devices.is_some()) {
            Some(choice) => self.choose(choice, cx),
            None => {
                self.serve_chosen = true;
                self.refresh(cx);
            }
        }
    }

    /// The debugging session: on or not, and what it does while it starts.
    pub fn set_session(&mut self, on: bool, starting: Option<String>, cx: &mut Context<Self>) {
        if self.debugging != on || self.starting != starting {
            self.debugging = on;
            self.starting = starting;
            cx.notify();
        }
    }

    /// Serves `choice`, unless it is the device on screen already (by its
    /// program and id: what `list` says of it, booted or not, may differ).
    fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let same = self.chosen.as_ref().is_some_and(|chosen| chosen.program == choice.program && chosen.device.id == choice.device.id);
        if same && self.serving.is_some() {
            return;
        }
        self.chosen = Some(choice);
        self.start(cx);
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let Some(choice) = self.chosen.clone() else {
            return;
        };
        self.stop(cx);
        if !cfg!(target_os = "macos") {
            self.status = Status::Failed("The Device panel shows the screen only on macOS.".into());
            cx.notify();
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.status = Status::Starting;
        let spawned = Command::new(self.root.join(&choice.program))
            .current_dir(&self.root)
            .args(["serve", &choice.device.id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                self.status = Status::Failed(format!("Can't run {}: {err}", choice.program));
                cx.notify();
                return;
            }
        };
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            self.status = Status::Failed(format!("{}: no pipes to talk to it", choice.program));
            cx.notify();
            return;
        };

        let (commands, pending) = smol::channel::unbounded::<String>();
        std::thread::spawn(move || {
            while let Ok(command) = pending.recv_blocking() {
                if writeln!(stdin, "{command}").and_then(|_| stdin.flush()).is_err() {
                    // It ended: its stdout says so.
                    return;
                }
            }
        });

        let tail = Arc::new(Mutex::new(String::new()));
        let kept = tail.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    return;
                }
                let mut tail = kept.lock().expect("never held across a panic");
                tail.push_str(&String::from_utf8_lossy(&buf[..n]));
                if tail.len() > STDERR_TAIL {
                    let cut = tail.ceil_char_boundary(tail.len() - STDERR_TAIL);
                    tail.drain(..cut);
                }
            }
        });

        let (lines, events) = smol::channel::unbounded::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if lines.send_blocking(line).is_err() {
                    break;
                }
            }
        });

        let name = choice.device.name.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(line) = events.recv().await {
                let alive = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        this.on_line(&line, cx);
                        true
                    })
                    .unwrap_or(false);
                if !alive {
                    break;
                }
            }
            let status = cx.background_spawn(async move { child.wait() }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                let stderr = tail.lock().expect("never held across a panic").trim().to_string();
                let why = match status {
                    Ok(status) if status.success() => format!("{name}: the program ended"),
                    Ok(status) => format!("{name}: the program ended ({status})"),
                    Err(err) => format!("{name}: the program ended ({err})"),
                };
                let why = if stderr.is_empty() { why } else { format!("{why}: {stderr}") };
                this.serving = None;
                this.status = Status::Failed(why);
                cx.notify();
            })
            .ok();
        })
        .detach();

        self.serving = Some(Serving { commands });
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.serving = None;
        self.status = Status::Idle;
        self.warning = None;
        self.screen = None;
        self.touching = None;
        #[cfg(target_os = "macos")]
        {
            self.surfaces.clear();
            self.frame = None;
        }
        cx.notify();
    }

    fn on_line(&mut self, line: &str, cx: &mut Context<Self>) {
        match protocol::parse_event(line) {
            Ok(Event::Size(screen)) => {
                self.screen = Some(screen);
                #[cfg(target_os = "macos")]
                self.surfaces.clear();
            }
            Ok(Event::Frame(id)) => self.on_frame(id),
            Ok(Event::Error(error)) => self.warning = Some(error),
            Err(err) => self.warning = Some(format!("{err:#}")),
        }
        cx.notify();
    }

    #[cfg(target_os = "macos")]
    fn on_frame(&mut self, id: u32) {
        let buffer = match self.surfaces.get(&id) {
            Some(buffer) => buffer.clone(),
            None => match surface::open(id) {
                Ok(buffer) => {
                    self.surfaces.insert(id, buffer.clone());
                    buffer
                }
                Err(err) => {
                    self.warning = Some(format!("{err:#}"));
                    return;
                }
            },
        };
        self.frame = Some(buffer);
        self.status = Status::Running;
    }

    #[cfg(not(target_os = "macos"))]
    fn on_frame(&mut self, _: u32) {}

    /// Says why the inspect asked for can't happen, until the next one.
    pub fn warn(&mut self, text: &str, cx: &mut Context<Self>) {
        self.warning = Some(text.to_string());
        cx.notify();
    }

    /// `den device …`: a command of the protocol, as the panel's mouse and
    /// keyboard send them; an error when no device is on screen.
    pub fn input(&self, command: String) -> Result<(), String> {
        if self.serving.is_none() {
            return Err("no device is on screen: den device show, or start an app (F5)".to_string());
        }
        self.send(command);
        Ok(())
    }

    fn send(&self, command: String) {
        if let Some(serving) = &self.serving {
            // Unbounded: it fails only once the program is gone, which its
            // stdout reports.
            let _ = serving.commands.try_send(command);
        }
    }

    fn point(&self, position: Point<Pixels>) -> Option<(f32, f32)> {
        fit(self.bounds.get(), self.screen?, position)
    }

    /// A finger, or with Option held two, the second mirrored through the
    /// screen's centre, as Simulator.app pinches.
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let Some((x, y)) = self.point(event.position) else {
            return;
        };
        if !(0. ..=1.).contains(&x) || !(0. ..=1.).contains(&y) {
            return;
        }
        self.touching = Some(event.modifiers.alt);
        self.send(protocol::touch(Touch::Down, (x, y), mirror(x, y, event.modifiers.alt)));
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some(pinch) = self.touching else {
            return;
        };
        if let Some((x, y)) = self.point(event.position) {
            let (x, y) = (x.clamp(0., 1.), y.clamp(0., 1.));
            self.send(protocol::touch(Touch::Move, (x, y), mirror(x, y, pinch)));
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        let Some(pinch) = self.touching.take() else {
            return;
        };
        if let Some((x, y)) = self.point(event.position) {
            let (x, y) = (x.clamp(0., 1.), y.clamp(0., 1.));
            self.send(protocol::touch(Touch::Up, (x, y), mirror(x, y, pinch)));
        }
    }

    /// Den's shortcuts (F5, F10…) and Cmd stay Den's, so a stop in the
    /// debugger steps from here; Cmd-V pastes this Mac's text. The rest goes to
    /// the device: characters as text, other keys by name.
    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.serving.is_none() {
            return;
        }
        let keystroke = &event.keystroke;
        if shortcuts::owner(keystroke, cx).is_some() {
            return;
        }
        let m = &keystroke.modifiers;
        if m.platform {
            if keystroke.key == "v" && !m.shift && !m.alt && !m.control {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.send(protocol::text(&text));
                }
                cx.stop_propagation();
            }
            return;
        }
        if let Some(command) = key_command(keystroke) {
            self.send(command);
            cx.stop_propagation();
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let running = self.serving.is_some();
        let label: SharedString = match &self.chosen {
            Some(choice) => choice.device.name.clone().into(),
            None => "No device".into(),
        };
        let this = cx.entity().downgrade();
        let devices = self.devices.clone().unwrap_or_default();
        let status: Option<SharedString> = match (&self.starting, &self.status) {
            (Some(starting), _) => Some(starting.clone().into()),
            (None, Status::Starting) => Some("Starting…".into()),
            _ => self.warning.clone().map(Into::into),
        };
        h_flex()
            .h(px(34.))
            .px_2()
            .gap_1()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("device-pick")
                    .ghost()
                    .xsmall()
                    .label(label)
                    .icon(Icon::default().path("icons/chevron-down.svg"))
                    .dropdown_menu(move |menu, _, _| pick_menu(menu, &devices, &this)),
            )
            .child(if self.debugging {
                tool("device-stop", "icons/square.svg", "Stop Debugging (Shift-F5)", true, theme.danger, cx)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DeviceEvent::Stop)))
                    .into_any_element()
            } else {
                tool("device-start", "icons/play.svg", "Debug (F5)", true, theme.success, cx)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DeviceEvent::Debug)))
                    .into_any_element()
            })
            .child(
                tool("device-home", "icons/house.svg", "Home", running, theme.foreground, cx)
                    .on_click(cx.listener(|this, _, _, _| this.send(protocol::home()))),
            )
            .child(
                tool("device-inspect", "icons/crosshair.svg", "Inspect: tap a widget to see the line that made it", running, theme.foreground, cx)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DeviceEvent::Inspect))),
            )
            .child(
                tool("device-rotate-left", "icons/rotate-ccw.svg", "Rotate Left", running, theme.foreground, cx)
                    .on_click(cx.listener(|this, _, _, _| this.send(protocol::rotate(false)))),
            )
            .child(
                tool("device-rotate-right", "icons/rotate-cw.svg", "Rotate Right", running, theme.foreground, cx)
                    .on_click(cx.listener(|this, _, _, _| this.send(protocol::rotate(true)))),
            )
            .child(
                div()
                    .id("device-status")
                    .ml_2()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .children(status),
            )
            .into_any_element()
    }

    /// The panel's right-click menu: its toolbar's buttons, the devices to
    /// pick, and Hide Panel.
    fn panel_menu(&self, cx: &Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let this = cx.entity().downgrade();
        let (running, debugging) = (self.serving.is_some(), self.debugging);
        let devices = self.devices.clone().unwrap_or_default();
        move |menu, window, cx| {
            let menu = if debugging {
                menu.item(crate::menu::item("Stop Debugging", &this, |_, _, cx| cx.emit(DeviceEvent::Stop)).action(Box::new(DebugStop)))
            } else {
                menu.item(crate::menu::item("Debug", &this, |_, _, cx| cx.emit(DeviceEvent::Debug)).action(Box::new(DebugContinue)))
            };
            let menu = menu
                .item(crate::menu::item("Home", &this, |this, _, _| this.send(protocol::home())).disabled(!running))
                .item(crate::menu::item("Inspect", &this, |_, _, cx| cx.emit(DeviceEvent::Inspect)).disabled(!running))
                .item(crate::menu::item("Rotate Left", &this, |this, _, _| this.send(protocol::rotate(false))).disabled(!running))
                .item(crate::menu::item("Rotate Right", &this, |this, _, _| this.send(protocol::rotate(true))).disabled(!running))
                .separator();
            let devices = devices.clone();
            let this = this.clone();
            menu.submenu("Device", window, cx, move |menu, _, _| pick_menu(menu, &devices, &this))
                .separator()
                .panel_items(crate::menu::hide_panel(), window, cx)
        }
    }

    /// What there is instead of a screen.
    fn render_message(&self, cx: &App) -> Option<AnyElement> {
        let text = match (&self.status, &self.devices) {
            (Status::Idle, _) if let Some(starting) = &self.starting => starting.clone(),
            (Status::Failed(why), _) => why.clone(),
            (Status::Running, _) => return None,
            (Status::Starting, _) => return None,
            (Status::Idle, _) if self.programs == Programs::None => format!("This workspace has no {DEVICE_FILE}."),
            (Status::Idle, _) if let Programs::Broken(why) = &self.programs => why.clone(),
            (Status::Idle, None) => return None,
            (Status::Idle, Some(_)) if self.programs == Programs::Listed(Vec::new()) => {
                format!("{DEVICE_FILE} names no programs.")
            }
            (Status::Idle, Some(devices)) if devices.is_empty() => {
                let mut text = "No devices.".to_string();
                for error in &self.list_errors {
                    text.push_str("\n");
                    text.push_str(error);
                }
                text
            }
            (Status::Idle, Some(_)) => "Debug the app (F5), or pick a device to see its screen.".into(),
        };
        Some(
            div()
                .p_3()
                .text_ui_small(cx)
                .text_color(cx.theme().muted_foreground)
                .child(text)
                .into_any_element(),
        )
    }

    /// The phone: its screen, the edge around it with its rounded corners
    /// (over the screen's square ones), and a line around it, the focus's
    /// color when the keys go to it. A rounded edge covers only what is
    /// inside it: the screen's corners that reach past its curve are covered
    /// by a band of the panel's background around it, wide enough (more than
    /// 0.414 of the radius) to reach them.
    #[cfg(target_os = "macos")]
    fn render_phone(&self, focused: bool, cx: &App) -> Option<AnyElement> {
        let frame = self.frame.clone()?;
        let screen = self.screen?;
        let bounds = self.bounds.get();
        let placed = place(bounds, screen)?;
        let at = |rect: Bounds<Pixels>| {
            div()
                .absolute()
                .left(rect.origin.x - bounds.origin.x)
                .top(rect.origin.y - bounds.origin.y)
                .w(rect.size.width)
                .h(rect.size.height)
        };
        let radius = screen.radius as f32 * placed.scale;
        let line = px(1.);
        let around = Bounds::new(placed.phone.origin - point(line, line), placed.phone.size + size(line * 2., line * 2.));
        let band = px(radius / 2.);
        let covered = Bounds::new(placed.phone.origin - point(band, band), placed.phone.size + size(band * 2., band * 2.));
        let theme = cx.theme();
        Some(
            div()
                .size_full()
                .overflow_hidden()
                .child(at(placed.screen).child(surface(frame).size_full()))
                .child(bordered(at(covered).rounded(px(radius) + band).border_color(theme.background), band))
                .child(bordered(at(placed.phone).rounded(px(radius)).border_color(black()), px(screen.bezel as f32 * placed.scale)))
                .child(
                    bordered(at(around).rounded(px(radius) + line), line)
                        .border_color(if focused { theme.ring } else { theme.border }),
                )
                .into_any_element(),
        )
    }

    #[cfg(not(target_os = "macos"))]
    fn render_phone(&self, _: bool, _: &App) -> Option<AnyElement> {
        None
    }
}

impl Render for Device {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let toolbar = self.render_toolbar(cx);
        let theme = cx.theme();
        let focused = self.focus.is_focused(window);
        let bounds = self.bounds.clone();
        let phone = self.render_phone(focused, cx);
        let message = self.render_message(cx);
        let menu = self.panel_menu(cx);
        v_flex()
            .id("device")
            .size_full()
            .bg(theme.background)
            .context_menu(menu)
            .child(toolbar)
            .child(
                div()
                    .id("device-screen")
                    .track_focus(&self.focus)
                    .key_context("Device")
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .on_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
                    .children(message)
                    .child(
                        div()
                            .size_full()
                            .relative()
                            .child(
                                canvas(
                                    move |b, window, _| {
                                        // The phone is placed by these: a new size draws it again.
                                        if bounds.replace(b) != b {
                                            window.refresh();
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .children(phone),
                    ),
            )
    }
}

impl Focusable for Device {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Space left around the phone.
const MARGIN: f32 = 12.;

/// Where the phone goes in `bounds`, fitted and centred, and its screen
/// inside its edge; `scale`, the panel's pixels per pixel of the screen.
#[derive(Debug, PartialEq)]
struct Placed {
    phone: Bounds<Pixels>,
    screen: Bounds<Pixels>,
    scale: f32,
}

fn place(bounds: Bounds<Pixels>, screen: Screen) -> Option<Placed> {
    let bezel = screen.bezel as f32;
    let (w, h) = (screen.width as f32 + 2. * bezel, screen.height as f32 + 2. * bezel);
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let scale = ((bw - 2. * MARGIN) / w).min((bh - 2. * MARGIN) / h);
    if !(scale > 0.) {
        return None;
    }
    let left = f32::from(bounds.origin.x) + (bw - w * scale) / 2.;
    let top = f32::from(bounds.origin.y) + (bh - h * scale) / 2.;
    Some(Placed {
        phone: Bounds::new(point(px(left), px(top)), size(px(w * scale), px(h * scale))),
        screen: Bounds::new(
            point(px(left + bezel * scale), px(top + bezel * scale)),
            size(px(screen.width as f32 * scale), px(screen.height as f32 * scale)),
        ),
        scale,
    })
}

/// The point of the screen under `position`, in fractions from its top
/// left. Outside it, below 0 or above 1.
fn fit(bounds: Bounds<Pixels>, screen: Screen, position: Point<Pixels>) -> Option<(f32, f32)> {
    let at = place(bounds, screen)?.screen;
    Some((
        (f32::from(position.x) - f32::from(at.origin.x)) / f32::from(at.size.width),
        (f32::from(position.y) - f32::from(at.origin.y)) / f32::from(at.size.height),
    ))
}

/// The style's border, `width` on every side.
fn bordered(mut el: Div, width: Pixels) -> Div {
    let width = AbsoluteLength::from(width);
    el.style().border_widths = EdgesRefinement { top: Some(width), right: Some(width), bottom: Some(width), left: Some(width) };
    el
}

/// The second finger of a pinch: the first mirrored through the centre.
fn mirror(x: f32, y: f32, pinch: bool) -> Option<(f32, f32)> {
    pinch.then(|| (1. - x, 1. - y))
}

/// What a key without Cmd sends: a character as text, unless Ctrl is held; a
/// named key, or a character with Ctrl, as a key. Nothing for the rest (F3).
fn key_command(keystroke: &Keystroke) -> Option<String> {
    let m = &keystroke.modifiers;
    let named = protocol::NAMED_KEYS.contains(&keystroke.key.as_str());
    if !named
        && !m.control
        && let Some(text) = &keystroke.key_char
    {
        return Some(protocol::text(text));
    }
    if !named && keystroke.key.chars().count() != 1 {
        return None;
    }
    let modifiers = Modifiers { shift: m.shift, alt: m.alt, ctrl: m.control, cmd: false };
    Some(protocol::key(&keystroke.key, modifiers))
}

fn pick_menu(mut menu: PopupMenu, devices: &[Choice], this: &WeakEntity<Device>) -> PopupMenu {
    for choice in devices {
        let label = if choice.device.booted { format!("{} · booted", choice.device.name) } else { choice.device.name.clone() };
        let picked = choice.clone();
        let this = this.clone();
        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
            let picked = picked.clone();
            this.update(cx, |this, cx| this.choose(picked, cx)).ok();
        }));
    }
    if !devices.is_empty() {
        menu = menu.separator();
    }
    menu.item(crate::menu::item("Refresh", this, |this, _, cx| this.refresh(cx)))
}

/// The device file at `path`.
fn read_device_file(path: &Path) -> Programs {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Programs::None,
        Err(err) => return Programs::Broken(format!("Can't read {DEVICE_FILE}: {err}")),
    };
    match serde_json::from_str::<DeviceFile>(&text) {
        Ok(file) => Programs::Listed(file.programs),
        Err(err) => Programs::Broken(format!("{DEVICE_FILE}: {err}")),
    }
}

/// `<program> list`, run in the workspace.
fn list(root: &Path, program: &str) -> anyhow::Result<Vec<Listed>> {
    let out = Command::new(root.join(program)).current_dir(root).arg("list").stdin(Stdio::null()).output()?;
    if !out.status.success() {
        anyhow::bail!("{}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    protocol::parse_list(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::{Bounds, Keystroke, Pixels, point, px, size};

    use super::{DEVICE_FILE, Programs, Screen, fit, key_command, place, protocol, read_device_file};

    #[test]
    fn the_phone_fitted_in_the_panel() {
        // A 1024×2048 screen with no edge in a 600×536 panel at (100, 50):
        // 576×512 inside the margin, so a quarter, 256×512, centred.
        let bounds = Bounds::new(point(px(100.), px(50.)), size(px(600.), px(536.)));
        let plain = Screen { width: 1024, height: 2048, bezel: 0, radius: 0 };
        let at = |x: f32, y: f32| fit(bounds, plain, point(px(x), px(y))).unwrap();
        assert_eq!(at(272., 62.), (0., 0.));
        assert_eq!(at(400., 318.), (0.5, 0.5));
        assert_eq!(at(528., 574.), (1., 1.));
        // Beside it, outside.
        assert!(at(150., 300.).0 < 0.);
        // With an edge of 64 the phone is 1152×2176: the height fits it,
        // and the screen is inside the edge.
        let phone = Screen { bezel: 64, radius: 256, ..plain };
        let placed = place(bounds, phone).unwrap();
        let scale = 512. / 2176.;
        assert!((placed.scale - scale).abs() < 1e-6);
        assert_eq!(placed.phone.origin.y, px(62.));
        let near = |a: Pixels, b: f32| (f32::from(a) - b).abs() < 1e-3;
        assert!(near(placed.screen.origin.y, 62. + 64. * scale));
        assert!(near(placed.screen.size.width, 1024. * scale));
        let (x, y) = fit(bounds, phone, placed.screen.center()).unwrap();
        assert!((x - 0.5).abs() < 1e-5 && (y - 0.5).abs() < 1e-5);
        // No room, no phone.
        assert_eq!(place(Bounds::default(), plain), None);
    }

    #[test]
    fn the_device_file() {
        let dir = std::env::temp_dir().join(format!("den-device-file-{}", std::process::id()));
        let path = dir.join(DEVICE_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let read = |text: Option<&str>| {
            match text {
                Some(text) => std::fs::write(&path, text).unwrap(),
                None => std::fs::remove_file(&path).unwrap(),
            }
            read_device_file(&path)
        };
        assert_eq!(read(Some(r#"{"programs": ["tools/simview"]}"#)), Programs::Listed(vec!["tools/simview".into()]));
        assert!(matches!(read(Some(r#"{"programs": "tools/simview"}"#)), Programs::Broken(_)));
        assert!(matches!(read(Some(r#"{"programs": [], "build": "make"}"#)), Programs::Broken(_)));
        assert!(matches!(read(Some("{")), Programs::Broken(_)));
        // Without it, no panel.
        assert_eq!(read(None), Programs::None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys() {
        let send = |keys: &str| key_command(&Keystroke::parse(keys).unwrap());
        let with_char = |keys: &str, c: &str| {
            let mut keystroke = Keystroke::parse(keys).unwrap();
            keystroke.key_char = Some(c.into());
            key_command(&keystroke)
        };
        assert_eq!(with_char("a", "a"), Some(protocol::text("a")));
        assert_eq!(with_char("shift-a", "A"), Some(protocol::text("A")));
        assert_eq!(with_char("alt-n", "ñ"), Some(protocol::text("ñ")));
        let ctrl = protocol::Modifiers { ctrl: true, ..Default::default() };
        assert_eq!(with_char("ctrl-a", "a"), Some(protocol::key("a", ctrl)));
        let shift = protocol::Modifiers { shift: true, ..Default::default() };
        assert_eq!(send("shift-left"), Some(protocol::key("left", shift)));
        assert_eq!(with_char("enter", "\n"), Some(protocol::key("enter", Default::default())));
        assert_eq!(send("f3"), None);
    }
}
