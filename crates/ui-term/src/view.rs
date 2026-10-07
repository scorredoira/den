use std::{
    cell::Cell as StdCell,
    ops::{Range, RangeInclusive},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use alacritty_terminal::{
    grid::Dimensions as _,
    index::{Line, Point as AlacPoint},
    selection::SelectionType,
    term::{
        TermMode,
        search::{Match, RegexSearch},
    },
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{FindBarMemory, Input, InputEvent, InputState},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    Copy, Find, Paste, SendBackTab, SendInterrupt, SendTab,
    element::{GridLayout, TerminalElement, grid_point},
    find,
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
pub(crate) const PADDING_X: f32 = 8.;
pub(crate) const PADDING_Y: f32 = 4.;

/// How often a selection dragged past the top or bottom edge scrolls.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(40);

/// Wait after new output before finding the matches again.
const FIND_AGAIN_AFTER: Duration = Duration::from_millis(150);

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

/// The find bar, while it's open.
struct FindBar {
    input: Entity<InputState>,
    /// `None` with nothing to find or an invalid expression.
    regex: Option<RegexSearch>,
    invalid: bool,
    matches: Vec<Match>,
    active: Option<usize>,
    /// The active match's start, counted from the top of the history.
    anchor: Option<(i32, usize)>,
    /// Finding again after new output, once it calms down.
    again: Option<Task<()>>,
    _subscription: Subscription,
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
    find: Option<FindBar>,
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
            cx.observe(&terminal, |this, _, cx| {
                this.find_again_later(cx);
                cx.notify();
            }),
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
            find: None,
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
        if self.selecting {
            self.end_selection(cx);
        } else if self.reports_mouse(&event.modifiers, cx) {
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

    /// Opens the find bar, or focuses it, with the selection as the query.
    fn open_find(&mut self, _: &Find, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .terminal
            .read(cx)
            .selection_text()
            .filter(|text| !text.contains('\n'));
        if self.find.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
            let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.find_from_scratch(cx);
                }
            });
            self.find = Some(FindBar {
                input,
                regex: None,
                invalid: false,
                matches: Vec::new(),
                active: None,
                anchor: None,
                again: None,
                _subscription: subscription,
            });
        }
        let Some(bar) = &self.find else { return };
        let input = bar.input.clone();
        input.update(cx, |input, cx| {
            if let Some(text) = selected {
                input.set_value(text, window, cx);
            }
            input.select_all(window, cx);
            input.focus(window, cx);
        });
        self.find_from_scratch(cx);
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// The query or the toggles changed: the matches, and the active one
    /// the closest to what was printed last.
    fn find_from_scratch(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else { return };
        let query = bar.input.read(cx).value().to_string();
        let options = cx.try_global::<FindBarMemory>().map(|memory| memory.options).unwrap_or_default();
        (bar.regex, bar.invalid) = match find::regex(&query, &options) {
            Ok(regex) => (regex, false),
            Err(()) => (None, true),
        };
        let terminal = self.terminal.read(cx);
        let term = terminal.term();
        bar.matches = match &mut bar.regex {
            Some(regex) => find::find_all(term, regex),
            None => Vec::new(),
        };
        bar.active = find::nearest(term, &bar.matches);
        bar.anchor = bar.active.map(|ix| find::anchor(term, *bar.matches[ix].start()));
        self.reveal_active(cx);
        cx.notify();
    }

    /// New output: the matches again, keeping the active one, once the
    /// output pauses (a flood would otherwise search on every batch).
    fn find_again_later(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else { return };
        if bar.regex.is_none() || bar.again.is_some() {
            return;
        }
        bar.again = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIND_AGAIN_AFTER).await;
            this.update(cx, |this, cx| this.find_again(cx)).ok();
        }));
    }

    fn find_again(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else { return };
        bar.again = None;
        let Some(regex) = &mut bar.regex else { return };
        let terminal = self.terminal.read(cx);
        let term = terminal.term();
        bar.matches = find::find_all(term, regex);
        bar.active = match bar.anchor {
            Some(anchor) => find::reanchor(term, &bar.matches, anchor),
            None => find::nearest(term, &bar.matches),
        };
        cx.notify();
    }

    /// The next match up (`older`, Enter) or down (Shift-Enter), going
    /// round at the ends.
    fn find_step(&mut self, older: bool, cx: &mut Context<Self>) {
        let Some(bar) = &mut self.find else { return };
        let count = bar.matches.len();
        if count == 0 {
            return;
        }
        let ix = match (bar.active, older) {
            (None, _) => count - 1,
            (Some(ix), true) => (ix + count - 1) % count,
            (Some(ix), false) => (ix + 1) % count,
        };
        bar.active = Some(ix);
        let term = self.terminal.read(cx).term();
        bar.anchor = Some(find::anchor(term, *bar.matches[ix].start()));
        self.reveal_active(cx);
        cx.notify();
    }

    fn reveal_active(&mut self, cx: &mut Context<Self>) {
        let Some(point) = self
            .find
            .as_ref()
            .and_then(|bar| Some(*bar.matches.get(bar.active?)?.start()))
        else {
            return;
        };
        self.terminal.update(cx, |terminal, cx| terminal.scroll_to(point, cx));
    }

    /// The matches in view, the active one marked, for the element to paint.
    fn visible_matches(&self, cx: &App) -> Vec<(RangeInclusive<AlacPoint>, bool)> {
        let Some(bar) = &self.find else { return Vec::new() };
        let term = self.terminal.read(cx).term();
        let top = -(term.grid().display_offset() as i32);
        let bottom = top + term.screen_lines() as i32 - 1;
        let first = bar.matches.partition_point(|found| found.end().line.0 < top);
        bar.matches[first..]
            .iter()
            .enumerate()
            .take_while(|(_, found)| found.start().line.0 <= bottom)
            .map(|(ix, found)| (found.clone(), bar.active == Some(first + ix)))
            .collect()
    }

    fn toggle_find_option(&mut self, change: fn(&mut gpui_kit::component::input::SearchOptions), cx: &mut Context<Self>) {
        cx.update_default_global::<FindBarMemory, _>(|memory, _| change(&mut memory.options));
        self.find_from_scratch(cx);
    }

    fn render_find(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let bar = self.find.as_ref()?;
        let options = cx.try_global::<FindBarMemory>().map(|memory| memory.options).unwrap_or_default();
        let theme = cx.theme();
        let has_matches = !bar.matches.is_empty();
        let status: SharedString = if bar.invalid {
            "Invalid regex".into()
        } else {
            match bar.active {
                Some(ix) if bar.matches.len() >= find::MAX_MATCHES => format!("{} of {}+", ix + 1, find::MAX_MATCHES).into(),
                Some(ix) => format!("{} of {}", ix + 1, bar.matches.len()).into(),
                None => "No results".into(),
            }
        };
        let toggle = |id: &'static str, icon: Icon, tooltip: &'static str, on: bool| {
            Button::new(id).xsmall().compact().ghost().icon(icon).tooltip(tooltip).selected(on)
        };
        let own_icon = |name: &str| Icon::empty().path(format!("icons/{name}.svg"));
        Some(
            h_flex()
                .id("terminal-find")
                .key_context("TerminalFind")
                .occlude()
                .absolute()
                .top_1()
                .right_3()
                .w(px(400.))
                // Wide enough for the input's focus ring, drawn outside it.
                .gap_2()
                .p_1()
                .bg(theme.tokens.popover)
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius)
                .shadow_md()
                .font_family(theme.font_family.clone())
                .on_action(cx.listener(Self::open_find))
                .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let modifiers = &event.keystroke.modifiers;
                    match event.keystroke.key.as_str() {
                        "escape" if !modifiers.modified() => this.close_find(window, cx),
                        "enter" if !modifiers.platform && !modifiers.control && !modifiers.alt => {
                            this.find_step(!modifiers.shift, cx)
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                .child(
                    div().flex_1().min_w_0().child(
                        Input::new(&bar.input)
                            .small()
                            .w_full()
                            .shadow_none()
                            .focus_bordered(true)
                            .when(bar.invalid, |input| input.border_color(theme.danger))
                            .suffix(
                                h_flex()
                                    .gap_0p5()
                                    .child(
                                        toggle("case-sensitive", IconName::CaseSensitive.into(), "Match Case", !options.case_insensitive)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_find_option(|options| options.case_insensitive = !options.case_insensitive, cx)
                                            })),
                                    )
                                    .child(
                                        toggle("whole-word", own_icon("whole-word"), "Match Whole Word", options.whole_word)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_find_option(|options| options.whole_word = !options.whole_word, cx)
                                            })),
                                    )
                                    .child(
                                        toggle("regex", own_icon("regex"), "Use Regular Expression", options.regex)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.toggle_find_option(|options| options.regex = !options.regex, cx)
                                            })),
                                    ),
                            ),
                    ),
                )
                .child(
                    div()
                        .text_sm()
                        .whitespace_nowrap()
                        .min_w(px(72.))
                        .when(bar.invalid, |label| label.text_color(theme.danger))
                        .when(!has_matches && !bar.invalid, |label| label.text_color(theme.muted_foreground))
                        .child(status),
                )
                .child(
                    Button::new("find-older")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowUp)
                        .tooltip("Previous Match (Enter)")
                        .disabled(!has_matches)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
                )
                .child(
                    Button::new("find-newer")
                        .xsmall()
                        .ghost()
                        .icon(IconName::ArrowDown)
                        .tooltip("Next Match (⇧Enter)")
                        .disabled(!has_matches)
                        .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
                )
                .child(
                    Button::new("find-close")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Close)
                        .tooltip("Close (Escape)")
                        .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
                ),
        )
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
        let find = self.render_find(cx);
        let terminal = div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .size_full()
            .px(px(PADDING_X))
            .py(px(PADDING_Y))
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::open_find))
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
                self.visible_matches(cx),
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
            });
        // The find bar is beside the terminal, not in it: its keys aren't the shell's.
        div().relative().size_full().child(terminal).children(find)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;
    use std::{cell::RefCell, future::Future, pin::Pin};

    use super::*;
    use crate::backend::TerminalBackend;

    /// A process that keeps what it's sent.
    #[derive(Default)]
    struct Backend(RefCell<Vec<u8>>);

    impl crate::TerminalBackend for Rc<Backend> {
        fn write(&self, bytes: Vec<u8>) {
            self.0.borrow_mut().extend(bytes);
        }
        fn resize(&self, _: u16, _: u16) {}
        fn kill(&self) {}
        fn cwd(&self) -> Option<PathBuf> {
            None
        }
        fn save_image(&self, _: &str, _: Vec<u8>) -> Pin<Box<dyn Future<Output = anyhow::Result<PathBuf>>>> {
            Box::pin(async { anyhow::bail!("unused") })
        }
    }

    fn terminal(cx: &mut TestAppContext, text: &[u8]) -> (Entity<Terminal>, Rc<Backend>, smol::channel::Sender<crate::PtyEvent>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::init(cx);
        });
        let backend = Rc::new(Backend::default());
        // Kept: a closed channel is a lost connection, which takes no keys.
        let (sender, output) = smol::channel::unbounded();
        let process: Rc<dyn TerminalBackend> = Rc::new(backend.clone());
        let text = text.to_vec();
        let terminal = cx.new(|cx| Terminal::new(process, output, 40, 5, &text, cx));
        (terminal, backend, sender)
    }

    fn status(view: &TerminalView) -> (usize, Option<usize>) {
        let bar = view.find.as_ref().expect("the find bar is open");
        (bar.matches.len(), bar.active)
    }

    /// Cmd-F opens the bar; what's typed there finds and never reaches the
    /// shell; Enter goes up; Escape closes it and gives the keys back.
    #[gpui_kit::test]
    fn find_bar_keys_stay_out_of_the_shell(cx: &mut TestAppContext) {
        let (terminal, backend, _sender) = terminal(cx, b"error 1\r\nok\r\nerror 2\r\nok\r\nok\r\nok\r\n$ ");
        let (view, cx) = cx.add_window_view(|window, cx| TerminalView::new(terminal, true, window, cx));
        cx.update(|window, cx| view.read(cx).focus_handle.clone().focus(window, cx));
        cx.run_until_parked();

        let find = if cfg!(target_os = "macos") { "cmd-f" } else { "ctrl-alt-f" };
        cx.simulate_keystrokes(find);
        cx.simulate_input("error");
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(status(view), (2, Some(1))));
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert_eq!(status(view), (2, Some(0))));
        cx.simulate_keystrokes("shift-enter");
        view.read_with(cx, |view, _| assert_eq!(status(view), (2, Some(1))));
        assert!(backend.0.borrow().is_empty());

        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.find.is_none()));
        cx.run_until_parked();
        cx.simulate_input("x");
        assert_eq!(backend.0.borrow().as_slice(), b"x");
    }

    /// The terminal beside something else that waits for the mouse.
    struct Beside {
        terminal: Entity<TerminalView>,
        releases: Rc<StdCell<usize>>,
    }

    impl Render for Beside {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let releases = self.releases.clone();
            div()
                .flex()
                .size_full()
                .child(div().w(px(400.)).h(px(200.)).child(self.terminal.clone()))
                .child(div().id("other").flex_1().h_full().on_mouse_up(MouseButton::Left, move |_, _, _| {
                    releases.set(releases.get() + 1)
                }))
        }
    }

    fn beside(cx: &mut TestAppContext) -> (Entity<TerminalView>, Rc<StdCell<usize>>, &mut VisualTestContext) {
        let (terminal, _, sender) = terminal(cx, b"one two three\r\nfour five\r\nsix");
        std::mem::forget(sender);
        let releases = Rc::new(StdCell::new(0));
        let shared = releases.clone();
        let (beside, cx) = cx.add_window_view(move |window, cx| Beside {
            terminal: cx.new(|cx| TerminalView::new(terminal, true, window, cx)),
            releases: shared,
        });
        cx.run_until_parked();
        let view = beside.read_with(cx, |beside, _| beside.terminal.clone());
        (view, releases, cx)
    }

    /// Where a cell is in the window, in its left half: a selection that
    /// starts there takes it, one that ends there stops before it.
    fn cells(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> impl Fn(usize, usize) -> Point<Pixels> + use<> {
        let layout = view.read_with(cx, |view, _| view.layout.get().expect("drawn"));
        move |row, col| {
            point(
                layout.origin.x + layout.size.cell_width * (col as f32 + 0.3),
                layout.origin.y + layout.size.line_height * (row as f32 + 0.5),
            )
        }
    }

    fn selected(view: &Entity<TerminalView>, cx: &mut VisualTestContext) -> Option<String> {
        view.read_with(cx, |view, cx| view.terminal.read(cx).selection_text())
    }

    /// A selection dragged out of the terminal ends where it's released,
    /// and that release still reaches whatever is there.
    #[gpui_kit::test]
    fn releasing_a_selection_outside_reaches_what_is_there(cx: &mut TestAppContext) {
        let (view, releases, cx) = beside(cx);
        let cell = cells(&view, cx);
        let none = Modifiers::default();
        cx.simulate_mouse_down(cell(0, 0), MouseButton::Left, none);
        cx.simulate_mouse_move(cell(0, 3), MouseButton::Left, none);
        let outside = point(px(600.), cell(0, 3).y);
        cx.simulate_mouse_up(outside, MouseButton::Left, none);
        assert_eq!(releases.get(), 1);
        view.read_with(cx, |view, _| assert!(!view.selecting()));
        assert_eq!(selected(&view, cx).as_deref(), Some("one"));

        // Released inside, it ends too, and nothing else hears it.
        cx.simulate_mouse_down(cell(1, 0), MouseButton::Left, none);
        cx.simulate_mouse_move(cell(1, 4), MouseButton::Left, none);
        cx.simulate_mouse_up(cell(1, 4), MouseButton::Left, none);
        view.read_with(cx, |view, _| assert!(!view.selecting()));
        assert_eq!(releases.get(), 1);
        assert_eq!(selected(&view, cx).as_deref(), Some("four"));
    }

    /// Shift-click extends the selection to where it's clicked.
    #[gpui_kit::test]
    fn shift_click_extends_the_selection(cx: &mut TestAppContext) {
        let (view, _, cx) = beside(cx);
        let cell = cells(&view, cx);
        let none = Modifiers::default();
        cx.simulate_mouse_down(cell(0, 0), MouseButton::Left, none);
        cx.simulate_mouse_move(cell(0, 3), MouseButton::Left, none);
        cx.simulate_mouse_up(cell(0, 3), MouseButton::Left, none);
        cx.simulate_mouse_down(cell(1, 4), MouseButton::Left, Modifiers::shift());
        cx.simulate_mouse_up(cell(1, 4), MouseButton::Left, Modifiers::shift());
        assert_eq!(selected(&view, cx).as_deref(), Some("one two three\nfour"));
    }
}
