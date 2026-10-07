use std::{cell::Cell as StdCell, ops::Range, path::PathBuf, rc::Rc, time::Duration};

use alacritty_terminal::{
    index::{Line, Point as AlacPoint},
    selection::SelectionType,
    term::TermMode,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    Copy, Paste, SendBackTab, SendInterrupt, SendTab,
    element::{GridLayout, TerminalElement, grid_point},
    keys::to_esc_str,
    links::{Link, link_at},
    terminal::{Terminal, TerminalEvent},
};

/// Terminal text size, set by the app; 13 unless it says otherwise.
pub struct TerminalFontSize(pub f32);

impl Global for TerminalFontSize {}

fn font_size(cx: &App) -> Pixels {
    px(cx.try_global::<TerminalFontSize>().map_or(13., |size| size.0))
}

/// Inner padding of the view, on each side.
const PADDING_X: f32 = 8.;
const PADDING_Y: f32 = 4.;

/// How often a selection dragged past the top or bottom edge scrolls.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(40);

pub enum TerminalViewEvent {
    TitleChanged,
    Exited,
    Focused,
    /// Cmd-click on a path that exists.
    OpenPath {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// Cmd-click on a URL in a terminal on a server: it may point at the
    /// server itself, so whoever knows the connection opens it.
    OpenUrl(String),
}

/// Link under the mouse with Cmd held: it's underlined and a click opens it.
struct HoveredLink {
    link: Link,
    /// First row of the logical line and the columns the link spans in it.
    start: Line,
    range: Range<usize>,
    cols: usize,
}

pub struct TerminalView {
    terminal: Entity<Terminal>,
    local: bool,
    hovered_link: Option<HoveredLink>,
    last_mouse: Option<Point<Pixels>>,
    focus_handle: FocusHandle,
    marked_text: Option<String>,
    layout: Rc<StdCell<Option<GridLayout>>>,
    selecting: bool,
    /// Where the mouse is while dragging a selection, inside or outside the view.
    drag_position: Point<Pixels>,
    /// Scrolls while a dragged selection is past the top or bottom edge.
    autoscroll: Option<Task<()>>,
    /// Accumulated scroll that doesn't yet add up to a whole line.
    scroll_remainder: f32,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    /// `local`: the terminal is on this machine (its paths can be checked).
    pub fn new(terminal: Entity<Terminal>, local: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.subscribe(&terminal, |_, _, event: &TerminalEvent, cx| match event {
                TerminalEvent::TitleChanged => cx.emit(TerminalViewEvent::TitleChanged),
                TerminalEvent::Exited => cx.emit(TerminalViewEvent::Exited),
                TerminalEvent::Disconnected => cx.notify(),
                TerminalEvent::Bell => {}
            }),
            cx.observe(&terminal, |_, _, cx| cx.notify()),
            cx.on_focus(&focus_handle, window, |_, _, cx| {
                cx.emit(TerminalViewEvent::Focused);
                cx.notify();
            }),
            cx.on_blur(&focus_handle, window, |_, _, cx| cx.notify()),
        ];
        Self {
            terminal,
            local,
            hovered_link: None,
            last_mouse: None,
            focus_handle,
            marked_text: None,
            layout: Rc::default(),
            selecting: false,
            drag_position: Point::default(),
            autoscroll: None,
            scroll_remainder: 0.,
            _subscriptions: subscriptions,
        }
    }

    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    pub fn title(&self, cx: &App) -> String {
        self.terminal
            .read(cx)
            .title()
            .map(str::to_string)
            .unwrap_or_else(|| "terminal".into())
    }

    pub(crate) fn marked_text(&self) -> Option<&str> {
        self.marked_text.as_deref().filter(|text| !text.is_empty())
    }

    pub(crate) fn set_marked_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked_text = Some(text.to_string());
        cx.notify();
    }

    pub(crate) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.marked_text = None;
        if !text.is_empty() {
            let bytes = text.as_bytes().to_vec();
            self.terminal.update(cx, |terminal, cx| terminal.input(bytes, cx));
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        // App shortcuts (Cmd on Mac) don't go to the terminal, except the line
        // editing ones of a Mac terminal: Cmd+Backspace deletes to the start of
        // the line (Ctrl+U) and Cmd+←/→ go to its start and end (Ctrl+A/E).
        if keystroke.modifiers.platform {
            let modifiers = &keystroke.modifiers;
            let only_cmd = !modifiers.alt && !modifiers.control && !modifiers.shift && !modifiers.function;
            let line_edit = match keystroke.key.as_str() {
                "backspace" => Some(b"\x15"),
                "left" => Some(b"\x01"),
                "right" => Some(b"\x05"),
                _ => None,
            };
            if let Some(bytes) = line_edit.filter(|_| only_cmd && cfg!(target_os = "macos")) {
                self.send(bytes, cx);
                cx.stop_propagation();
            }
            return;
        }
        // Regular text arrives through the input handler, with accents already composed.
        if event.prefer_character_input && keystroke.key_char.is_some() {
            return;
        }
        let mode = self.terminal.read(cx).mode();
        if let Some(esc) = to_esc_str(keystroke, mode, false) {
            let bytes = esc.as_bytes().to_vec();
            self.terminal.update(cx, |terminal, cx| terminal.input(bytes, cx));
            cx.stop_propagation();
        }
    }

    fn send(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        let bytes = bytes.to_vec();
        self.terminal.update(cx, |terminal, cx| terminal.input(bytes, cx));
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_selection(cx);
    }

    /// Copies the selection, if any.
    pub fn copy_selection(&self, cx: &mut App) {
        if let Some(text) = self.terminal.read(cx).selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn has_selection(&self, cx: &App) -> bool {
        self.terminal.read(cx).selection_text().is_some()
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        self.paste_clipboard(cx);
    }

    /// Pastes text or, if the clipboard has an image, the path of a copy of it
    /// on the terminal's machine.
    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let image = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::Image(image) => Some(image.clone()),
            _ => None,
        });
        match (image, item.text()) {
            (Some(image), _) => self.terminal.update(cx, |terminal, cx| {
                terminal.paste_image(image.format.extension(), image.bytes, cx)
            }),
            (None, Some(text)) => self.terminal.update(cx, |terminal, cx| terminal.paste(&text, cx)),
            (None, None) => {}
        }
    }

    fn point_at(&self, position: Point<Pixels>, cx: &App) -> Option<(AlacPoint, alacritty_terminal::index::Side)> {
        let layout = self.layout.get()?;
        Some(grid_point(layout, position, self.terminal.read(cx).display_offset()))
    }

    /// The application inside (vim, htop…) wants the clicks. With Shift it
    /// selects anyway.
    fn reports_mouse(&self, modifiers: &Modifiers, cx: &App) -> bool {
        self.terminal.read(cx).mode().intersects(TermMode::MOUSE_MODE) && !modifiers.shift
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        let Some((point, side)) = self.point_at(event.position, cx) else {
            return;
        };
        if event.button == MouseButton::Left && event.modifiers.platform {
            if let Some(hovered) = self.link_at(point, cx) {
                self.open_link(hovered.link, cx);
            }
            return;
        }
        if self.reports_mouse(&event.modifiers, cx) {
            self.report_mouse(event.button, true, false, event.position, event.modifiers, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        self.selecting = true;
        self.drag_position = event.position;
        if event.modifiers.shift && event.click_count == 1 && self.has_selection(cx) {
            self.terminal
                .update(cx, |terminal, cx| terminal.update_selection(point, side, cx));
            return;
        }
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.terminal
            .update(cx, |terminal, cx| terminal.start_selection(point, side, ty, cx));
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.last_mouse = Some(event.position);
        self.update_hovered_link(event.modifiers.platform, cx);
        if !self.selecting
            && let Some(button) = event.pressed_button
            && self.reports_mouse(&event.modifiers, cx)
            && self
                .terminal
                .read(cx)
                .mode()
                .intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
        {
            self.report_mouse(button, true, true, event.position, event.modifiers, cx);
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting && self.reports_mouse(&event.modifiers, cx) {
            self.report_mouse(event.button, false, false, event.position, event.modifiers, cx);
        }
    }

    pub(crate) fn selecting(&self) -> bool {
        self.selecting
    }

    /// The mouse moves while selecting, wherever it is: past the top or bottom
    /// edge the terminal scrolls, and keeps scrolling while it stays there.
    pub(crate) fn drag_selection(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag_position = position;
        self.extend_selection(cx);
        if self.autoscroll.is_none() && self.autoscroll_lines() != 0 {
            self.autoscroll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(AUTOSCROLL_EVERY).await;
                    let Ok(true) = this.update(cx, |this, cx| this.autoscroll_step(cx)) else {
                        break;
                    };
                }
            }));
        }
    }

    pub(crate) fn end_selection(&mut self, cx: &mut Context<Self>) {
        self.selecting = false;
        self.autoscroll = None;
        if !self.has_selection(cx) {
            self.terminal.update(cx, |terminal, cx| terminal.clear_selection(cx));
        }
    }

    fn extend_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((point, side)) = self.point_at(self.drag_position, cx) {
            self.terminal
                .update(cx, |terminal, cx| terminal.update_selection(point, side, cx));
        }
    }

    fn autoscroll_step(&mut self, cx: &mut Context<Self>) -> bool {
        let lines = self.autoscroll_lines();
        if !self.selecting || lines == 0 {
            self.autoscroll = None;
            return false;
        }
        self.terminal.update(cx, |terminal, cx| terminal.scroll(lines, cx));
        self.extend_selection(cx);
        true
    }

    /// Lines to scroll per step for the dragged selection: up (positive) above
    /// the grid, down below it, faster the farther the mouse is.
    fn autoscroll_lines(&self) -> i32 {
        let Some(layout) = self.layout.get() else {
            return 0;
        };
        let line_height = layout.size.line_height;
        let top = layout.origin.y;
        let bottom = top + line_height * layout.size.rows as f32;
        let y = self.drag_position.y;
        if y < top {
            ((top - y) / line_height) as i32 + 1
        } else if y >= bottom {
            -(((y - bottom) / line_height) as i32 + 1)
        } else {
            0
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let line_height = layout.size.line_height;
        self.scroll_remainder += event.delta.pixel_delta(line_height).y / line_height;
        let lines = self.scroll_remainder.trunc() as i32;
        if lines == 0 {
            return;
        }
        self.scroll_remainder -= lines as f32;

        let mode = self.terminal.read(cx).mode();
        if self.reports_mouse(&event.modifiers, cx) {
            let button = if lines > 0 { 64 } else { 65 };
            for _ in 0..lines.abs() {
                self.send_mouse_report(button, true, event.position, event.modifiers, cx);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
            // Full-screen programs without mouse support: the wheel moves with the arrow keys.
            let arrow = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            let bytes = arrow.repeat(lines.unsigned_abs() as usize).into_bytes();
            self.terminal.update(cx, |terminal, _| terminal.write_raw(bytes));
        } else {
            self.terminal.update(cx, |terminal, cx| terminal.scroll(lines, cx));
            if self.selecting {
                self.extend_selection(cx);
            }
        }
    }

    fn report_mouse(
        &mut self,
        button: MouseButton,
        pressed: bool,
        motion: bool,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let code = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        let code = if motion { code + 32 } else { code };
        self.send_mouse_report(code, pressed, position, modifiers, cx);
    }

    /// Mouse report in SGR format or, if the application didn't ask for it, the classic one.
    fn send_mouse_report(
        &mut self,
        mut code: u8,
        pressed: bool,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(layout) = self.layout.get() else {
            return;
        };
        let (point, _) = grid_point(layout, position, 0);
        let (col, row) = (point.column.0 + 1, point.line.0 as usize + 1);
        if modifiers.shift {
            code += 4;
        }
        if modifiers.alt {
            code += 8;
        }
        if modifiers.control {
            code += 16;
        }
        let bytes = if self.terminal.read(cx).mode().contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{code};{col};{row}{}", if pressed { 'M' } else { 'm' }).into_bytes()
        } else {
            if col > 223 || row > 223 {
                return;
            }
            let code = if pressed { code } else { 3 };
            vec![b'\x1b', b'[', b'M', 32 + code, 32 + col as u8, 32 + row as u8]
        };
        self.terminal.update(cx, |terminal, _| terminal.write_raw(bytes));
    }

    /// Link (path or URL) at a point, looking at the whole logical line.
    fn link_at(&self, point: AlacPoint, cx: &App) -> Option<HoveredLink> {
        let terminal = self.terminal.read(cx);
        let (text, start) = terminal.logical_line(point.line);
        let cols = terminal.columns();
        let offset = (point.line.0 - start.0) as usize * cols + point.column.0;
        let cwd = terminal.cwd();
        let (link, range) = link_at(&text, offset, cwd.as_deref(), self.local)?;
        Some(HoveredLink {
            link,
            start,
            range,
            cols,
        })
    }

    fn update_hovered_link(&mut self, cmd: bool, cx: &mut Context<Self>) {
        let hovered = cmd
            .then(|| self.last_mouse)
            .flatten()
            .and_then(|position| self.point_at(position, cx))
            .and_then(|(point, _)| self.link_at(point, cx));
        let changed = match (&self.hovered_link, &hovered) {
            (Some(a), Some(b)) => a.start != b.start || a.range != b.range,
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.hovered_link = hovered;
            cx.notify();
        }
    }

    /// Cells of the link under the mouse, by row (in alacritty coordinates).
    pub(crate) fn link_cells(&self) -> Vec<(Line, Range<usize>)> {
        let Some(hovered) = &self.hovered_link else {
            return Vec::new();
        };
        let mut cells: Vec<(Line, Range<usize>)> = Vec::new();
        for offset in hovered.range.clone() {
            let line = hovered.start + (offset / hovered.cols) as i32;
            let col = offset % hovered.cols;
            match cells.last_mut() {
                Some((last, cols)) if *last == line => cols.end = col + 1,
                _ => cells.push((line, col..col + 1)),
            }
        }
        cells
    }

    fn open_link(&mut self, link: Link, cx: &mut Context<Self>) {
        match link {
            Link::Url(url) if self.local => cx.open_url(&url),
            Link::Url(url) => cx.emit(TerminalViewEvent::OpenUrl(url)),
            Link::Path { path, line, column } => {
                cx.emit(TerminalViewEvent::OpenPath { path, line, column })
            }
        }
    }
}

/// Columns and rows of a terminal occupying `size` (with its padding).
pub fn grid_for(size: Size<Pixels>, window: &Window, cx: &App) -> (u16, u16) {
    let inner = gpui_kit::size(size.width - px(2. * PADDING_X), size.height - px(2. * PADDING_Y));
    let font = Font {
        family: gpui_kit::component::ActiveTheme::theme(cx).mono_font_family.clone(),
        ..Font::default()
    };
    let grid = crate::element::grid_size(inner, &font, font_size(cx), window);
    (grid.cols, grid.rows)
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(|this, _: &SendTab, _, cx| this.send(b"\t", cx)))
            .on_action(cx.listener(|this, _: &SendBackTab, _, cx| this.send(b"\x1b[Z", cx)))
            .on_action(cx.listener(|this, _: &SendInterrupt, _, cx| this.send(b"\x03", cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.update_hovered_link(event.modifiers.platform, cx)
            }))
            .when(self.hovered_link.is_some(), |el| el.cursor_pointer())
            .child(TerminalElement::new(
                self.terminal.clone(),
                cx.entity(),
                self.focus_handle.clone(),
                focused,
                font_size(cx),
                self.layout.clone(),
                self.link_cells(),
            ))
            .when(self.terminal.read(cx).disconnected(), |el| {
                let theme = gpui_kit::component::ActiveTheme::theme(&**cx);
                el.relative().child(
                    div()
                        .absolute()
                        .top_2()
                        .right_3()
                        .px_2()
                        .py_0p5()
                        .rounded(theme.radius)
                        .bg(theme.warning.opacity(0.9))
                        .text_color(theme.background)
                        .text_xs()
                        .child("disconnected — reconnecting…"),
                )
            })
    }
}
