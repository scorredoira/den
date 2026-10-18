use std::{cell::RefCell, path::PathBuf, rc::Rc};

use alacritty_terminal::{
    Term,
    event::{Event as AlacEvent, EventListener, WindowSize},
    grid::{Dimensions as _, Scroll},
    index::{Column, Line, Point as AlacPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags, test::TermSize},
    vte::ansi::{ClearMode, Handler as _, Processor},
};
use gpui_kit::*;

use crate::{
    backend::{PtyEvent, TerminalBackend},
    colors::Palette,
    unwrap::{Row, unwrap},
};

/// Maximum bytes processed in one go before repainting.
const MAX_BATCH: usize = 256 * 1024;

pub enum TerminalEvent {
    TitleChanged,
    Bell,
    Exited,
    /// The connection to the process was lost (it's still alive in its agent).
    Disconnected,
}

/// Collects the emulator's events to handle them after processing bytes.
#[derive(Clone, Default)]
pub(crate) struct Listener(Rc<RefCell<Vec<AlacEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: AlacEvent) {
        self.0.borrow_mut().push(event);
    }
}

/// Size of the grid and of each cell in pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: Pixels,
    pub line_height: Pixels,
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    backend: Rc<dyn TerminalBackend>,
    size: GridSize,
    title: Option<String>,
    exited: bool,
    disconnected: bool,
    /// Colors used for the last paint, to answer OSC 4/10/11.
    pub(crate) palette: Option<Rc<Palette>>,
    _reader: Task<()>,
}

impl EventEmitter<TerminalEvent> for Terminal {}

impl Terminal {
    /// A terminal whose process lives in `backend`. `snapshot` is the snapshot it
    /// starts from (of `cols` × `rows`); then `output` arrives.
    pub fn new(
        backend: Rc<dyn TerminalBackend>,
        output: smol::channel::Receiver<PtyEvent>,
        cols: u16,
        rows: u16,
        snapshot: &[u8],
        cx: &mut Context<Self>,
    ) -> Self {
        let size = GridSize {
            cols: cols.max(2),
            rows: rows.max(1),
            cell_width: px(8.),
            line_height: px(16.),
        };
        let events = Listener::default();
        let term = Term::new(
            Config::default(),
            &TermSize::new(size.cols as usize, size.rows as usize),
            events.clone(),
        );
        let mut this = Self {
            term,
            parser: Processor::new(),
            events,
            backend,
            size,
            title: None,
            exited: false,
            disconnected: false,
            palette: None,
            _reader: Self::read(output, cx),
        };
        this.process(snapshot, cx);
        this
    }

    /// Reattaches the terminal to its process after losing the connection: the
    /// emulator is rebuilt from the new snapshot and continues with the new output.
    pub fn reconnect(
        &mut self,
        backend: Rc<dyn TerminalBackend>,
        output: smol::channel::Receiver<PtyEvent>,
        cols: u16,
        rows: u16,
        snapshot: &[u8],
        cx: &mut Context<Self>,
    ) {
        self.size.cols = cols.max(2);
        self.size.rows = rows.max(1);
        self.term = Term::new(
            Config::default(),
            &TermSize::new(self.size.cols as usize, self.size.rows as usize),
            self.events.clone(),
        );
        self.parser = Processor::new();
        self.backend = backend;
        self.disconnected = false;
        self._reader = Self::read(output, cx);
        self.process(snapshot, cx);
    }

    /// Processes the process's output as it arrives.
    fn read(output: smol::channel::Receiver<PtyEvent>, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            // Only an explicit `Exit` means the process ended: a channel that
            // just closes lost its sender, and the process may well be alive.
            let mut disconnected = true;
            while let Ok(event) = output.recv().await {
                let mut bytes = match event {
                    PtyEvent::Output(bytes) => bytes,
                    PtyEvent::Exit => {
                        disconnected = false;
                        break;
                    }
                    PtyEvent::Disconnected => break,
                };
                // Gather whatever has already arrived to process it in a single repaint.
                let mut end = None;
                while bytes.len() < MAX_BATCH {
                    match output.try_recv() {
                        Ok(PtyEvent::Output(more)) => bytes.extend_from_slice(&more),
                        Ok(PtyEvent::Exit) => {
                            end = Some(false);
                            break;
                        }
                        Ok(PtyEvent::Disconnected) => {
                            end = Some(true);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                if this.update(cx, |this, cx| this.process(&bytes, cx)).is_err() {
                    return;
                }
                if let Some(lost) = end {
                    disconnected = lost;
                    break;
                }
                // A flood of output doesn't keep the window from drawing
                // or from handling keys: they go between batches.
                smol::future::yield_now().await;
            }
            this.update(cx, |this, cx| {
                if disconnected {
                    this.disconnected = true;
                    cx.emit(TerminalEvent::Disconnected);
                } else {
                    this.exited = true;
                    cx.emit(TerminalEvent::Exited);
                }
                cx.notify();
            })
            .ok();
        })
    }

    fn process(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.parser.advance(&mut self.term, bytes);
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        for event in events {
            self.handle_event(event, cx);
        }
        cx.notify();
    }

    fn handle_event(&mut self, event: AlacEvent, cx: &mut Context<Self>) {
        match event {
            // Query replies come from the agent's emulator; if this one replied
            // too, the application would receive them twice.
            AlacEvent::PtyWrite(_) => {}
            AlacEvent::Title(title) => {
                self.title = Some(title);
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::ResetTitle => {
                self.title = None;
                cx.emit(TerminalEvent::TitleChanged);
            }
            AlacEvent::ClipboardStore(_, text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            AlacEvent::ClipboardLoad(_, format) => {
                let text = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                self.backend.write(format(&text).into_bytes());
            }
            AlacEvent::ColorRequest(ix, format) => {
                if let Some(palette) = &self.palette {
                    let rgb = palette.rgb_at(ix, self.term.colors());
                    self.backend.write(format(rgb).into_bytes());
                }
            }
            AlacEvent::TextAreaSizeRequest(format) => {
                let size = self.window_size();
                self.backend.write(format(size).into_bytes());
            }
            AlacEvent::Bell => cx.emit(TerminalEvent::Bell),
            _ => {}
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.size.rows,
            num_cols: self.size.cols,
            cell_width: f32::from(self.size.cell_width) as u16,
            cell_height: f32::from(self.size.line_height) as u16,
        }
    }

    pub(crate) fn term(&self) -> &Term<Listener> {
        &self.term
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    /// Disconnected from its process (waiting to reconnect).
    pub fn disconnected(&self) -> bool {
        self.disconnected
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Current directory of the foreground process, if already known.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.backend.cwd()
    }

    /// Saves an image on the process's machine and pastes its path, which
    /// Claude Code attaches as an image. Works the same locally and over SSH.
    pub fn paste_image(&self, extension: &str, data: Vec<u8>, cx: &mut Context<Self>) {
        let save = self.backend.save_image(extension, data);
        cx.spawn(async move |this, cx| {
            let path = save.await;
            this.update(cx, |this, cx| match path {
                Ok(path) => this.paste(&path.to_string_lossy(), cx),
                Err(err) => eprintln!("could not paste the image: {err:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// Kills the terminal's process.
    pub fn kill(&self) {
        self.backend.kill();
    }

    /// Sends what the user types. Scrolls back to the bottom and clears the selection.
    pub fn input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if self.exited || self.disconnected {
            return;
        }
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
            cx.notify();
        }
        if self.term.selection.take().is_some() {
            cx.notify();
        }
        self.backend.write(bytes);
    }

    /// Sends bytes without touching the scroll or the selection (mouse reports).
    pub fn write_raw(&mut self, bytes: Vec<u8>) {
        if !self.exited {
            self.backend.write(bytes);
        }
    }

    pub fn paste(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bytes = if self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
        } else {
            text
        };
        self.input(bytes.into_bytes(), cx);
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        let grid_changed = size.cols != self.size.cols || size.rows != self.size.rows;
        self.size = size;
        if grid_changed {
            self.term
                .resize(TermSize::new(size.cols as usize, size.rows as usize));
            self.backend.resize(size.cols, size.rows);
        }
    }

    pub fn scroll(&mut self, lines: i32, cx: &mut Context<Self>) {
        self.term.scroll_display(Scroll::Delta(lines));
        cx.notify();
    }

    /// Scrolls the least needed for `point` to be in view.
    pub(crate) fn scroll_to(&mut self, point: AlacPoint, cx: &mut Context<Self>) {
        self.term.scroll_to_point(point);
        cx.notify();
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    pub fn start_selection(&mut self, point: AlacPoint, side: Side, ty: SelectionType, cx: &mut Context<Self>) {
        self.term.selection = Some(Selection::new(ty, point, side));
        cx.notify();
    }

    pub fn update_selection(&mut self, point: AlacPoint, side: Side, cx: &mut Context<Self>) {
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
            cx.notify();
        }
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.term.selection.take().is_some() {
            cx.notify();
        }
    }

    /// Selects everything, history included, down to the last line with text.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut last = grid.bottommost_line();
        while last > grid.topmost_line() && (0..cols).all(|col| grid[last][Column(col)].c == ' ') {
            last -= 1;
        }
        let start = AlacPoint::new(grid.topmost_line(), Column(0));
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(AlacPoint::new(last, Column(cols - 1)), Side::Right);
        self.term.selection = Some(selection);
        cx.notify();
    }

    /// Clears the history and the screen like Cmd-K in Terminal.app, keeping
    /// the cursor's line at the top. The agent clears its emulator too and
    /// sends the clear to every view, or reattaching would bring it all
    /// back; if it can't (an older one), only this view clears.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let clear = self.backend.clear();
        cx.spawn(async move |this, cx| {
            if clear.await.is_err() {
                this.update(cx, |this, cx| this.clear_here(cx)).ok();
            }
        })
        .detach();
    }

    /// What the agent's clear does (see its `snapshot::clear`), here only.
    fn clear_here(&mut self, cx: &mut Context<Self>) {
        let line = self.term.grid().cursor.point.line.0 as usize;
        if line > 0 && !self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.term.scroll_up(line);
            self.term.move_up(line);
        }
        self.term.clear_screen(ClearMode::Saved);
        cx.notify();
    }

    /// Whether there's text selected, without copying it.
    pub fn has_selection(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_empty() && selection.to_range(&self.term).is_some())
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection
            .as_ref()
            .filter(|selection| !selection.is_empty())?;
        self.term.selection_to_string()
    }

    /// The selection without the indentation and the line breaks a program
    /// put to fit the width, as [`unwrap`](crate::unwrap::unwrap) says.
    pub fn selection_text_unwrapped(&self) -> Option<String> {
        let selection = self.term.selection.as_ref().filter(|selection| !selection.is_empty())?;
        let range = selection.to_range(&self.term)?;
        if range.is_block {
            return self.selection_text();
        }
        let grid = self.term.grid();
        let cols = grid.columns();
        let rows: Vec<Row> = (range.start.line.0..=range.end.line.0)
            .map(|line| {
                let row = &grid[Line(line)];
                let end = (0..cols).rev().find(|&col| !row[Column(col)].c.is_whitespace()).map_or(0, |col| col + 1);
                let from = if line == range.start.line.0 { range.start.column.0 } else { 0 };
                let to = if line == range.end.line.0 { (range.end.column.0 + 1).min(end) } else { end };
                let text = (from..to.max(from))
                    .map(|col| &row[Column(col)])
                    .filter(|cell| !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER))
                    .map(|cell| cell.c)
                    .collect();
                let wraps = line < range.end.line.0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
                Row { text, end, wraps }
            })
            .collect();
        Some(unwrap(&rows, cols))
    }

    /// Text of the logical line containing `line`, and the cell each char
    /// comes from: rows the terminal wrapped because they didn't fit are
    /// joined.
    ///
    /// With `at_edge`, a row whose text reaches the last column also goes on
    /// into the next one, after its indentation: an application that breaks
    /// a long word itself (Claude Code with a path or a URL) leaves it so,
    /// without the wrap mark.
    pub fn logical_line(&self, line: Line, at_edge: bool) -> (String, Vec<AlacPoint>) {
        let grid = self.term.grid();
        let cols = grid.columns();
        let blank = |line: Line, col: usize| grid[line][Column(col)].c.is_whitespace();
        // The column the row after `line` goes on from, if `line` goes on into it.
        let goes_on = |line: Line| {
            if line >= grid.bottommost_line() {
                None
            } else if grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
                Some(0)
            } else if at_edge && !blank(line, cols - 1) {
                (0..cols).find(|&col| !blank(line + 1i32, col))
            } else {
                None
            }
        };
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            return (String::new(), Vec::new());
        }
        let mut start = line;
        while start > grid.topmost_line() && goes_on(start - 1).is_some() {
            start -= 1;
        }
        let (mut text, mut cells) = (String::new(), Vec::new());
        let (mut current, mut from) = (start, 0);
        loop {
            for col in from..cols {
                text.push(grid[current][Column(col)].c);
                cells.push(AlacPoint::new(current, Column(col)));
            }
            match goes_on(current) {
                Some(next) => (current, from) = (current + 1i32, next),
                None => break,
            }
        }
        (text, cells)
    }

    pub fn columns(&self) -> usize {
        self.term.grid().columns()
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;
    use std::{future::Future, pin::Pin};

    use super::*;

    /// A process that takes nothing and, like an older agent, can't clear.
    struct Backend;

    impl TerminalBackend for Backend {
        fn write(&self, _: Vec<u8>) {}
        fn resize(&self, _: u16, _: u16) {}
        fn kill(&self) {}
        fn cwd(&self) -> Option<PathBuf> {
            None
        }
        fn save_image(&self, _: &str, _: Vec<u8>) -> Pin<Box<dyn Future<Output = anyhow::Result<PathBuf>>>> {
            Box::pin(async { anyhow::bail!("unused") })
        }
    }

    fn terminal(cx: &mut TestAppContext) -> Entity<Terminal> {
        let (_, output) = smol::channel::unbounded();
        cx.new(|cx| Terminal::new(Rc::new(Backend), output, 20, 3, b"one\r\ntwo\r\nthree\r\nfour\r\n$ ls", cx))
    }

    #[gpui_kit::test]
    fn select_all_takes_the_history_and_stops_at_the_last_text(cx: &mut TestAppContext) {
        let terminal = terminal(cx);
        terminal.update(cx, |terminal, cx| {
            assert!(!terminal.has_selection());
            terminal.select_all(cx);
            assert!(terminal.has_selection());
            assert_eq!(terminal.selection_text().as_deref(), Some("one\ntwo\nthree\nfour\n$ ls"));
        });
    }

    /// A word an application broke at the edge goes on in the next row,
    /// after its indentation; a row that doesn't reach the edge ends its line.
    #[gpui_kit::test]
    fn rows_broken_at_the_edge_join_when_asked(cx: &mut TestAppContext) {
        let (_, output) = smol::channel::unbounded();
        let text = b"see /Users/me/proj/s\r\n  rc/main.rs (2KB)\r\nshort\r\nnext";
        let terminal = cx.new(|cx| Terminal::new(Rc::new(Backend), output, 20, 4, text, cx));
        terminal.update(cx, |terminal, _| {
            let line = |line, at_edge| {
                let (text, cells) = terminal.logical_line(Line(line), at_edge);
                (text.trim_end().to_string(), cells[0].line.0)
            };
            assert_eq!(line(1, true), ("see /Users/me/proj/src/main.rs (2KB)".into(), 0));
            assert_eq!(line(1, false), ("  rc/main.rs (2KB)".into(), 1));
            assert_eq!(line(2, true), ("short".into(), 2));
            let (text, cells) = terminal.logical_line(Line(0), true);
            let r = text.find("rc/").unwrap();
            assert_eq!(cells[r], AlacPoint::new(Line(1), Column(2)));
        });
    }

    #[gpui_kit::test]
    fn clear_without_the_agent_clears_here(cx: &mut TestAppContext) {
        let terminal = terminal(cx);
        terminal.update(cx, |terminal, cx| terminal.clear(cx));
        cx.run_until_parked();
        terminal.update(cx, |terminal, cx| {
            assert_eq!(terminal.term.grid().history_size(), 0);
            assert_eq!(terminal.term.grid().cursor.point, AlacPoint::new(Line(0), Column(4)));
            terminal.select_all(cx);
            assert_eq!(terminal.selection_text().as_deref(), Some("$ ls"));
        });
    }
}

