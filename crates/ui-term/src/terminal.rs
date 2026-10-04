use std::{cell::RefCell, path::PathBuf, rc::Rc};

use alacritty_terminal::{
    Term,
    event::{Event as AlacEvent, EventListener, WindowSize},
    grid::{Dimensions as _, Scroll},
    index::{Column, Line, Point as AlacPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags, test::TermSize},
    vte::ansi::Processor,
};
use gpui_kit::*;

use crate::{
    backend::{PtyEvent, TerminalBackend},
    colors::Palette,
};

/// Maximum bytes processed in one go before repainting.
const MAX_BATCH: usize = 1024 * 1024;

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

    pub fn selection_text(&self) -> Option<String> {
        self.term
            .selection
            .as_ref()
            .filter(|selection| !selection.is_empty())?;
        self.term.selection_to_string()
    }

    /// Text of the logical line containing `line`: rows the terminal wrapped
    /// because they didn't fit are joined. Returns the text (one char per
    /// column) and the first row.
    pub fn logical_line(&self, line: Line) -> (String, Line) {
        let grid = self.term.grid();
        let cols = grid.columns();
        let wraps = |line: Line| grid[line][Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            return (String::new(), line);
        }
        let mut start = line;
        while start > grid.topmost_line() && wraps(start - 1) {
            start -= 1;
        }
        let mut text = String::new();
        let mut current = start;
        loop {
            let row = &grid[current];
            text.extend((0..cols).map(|col| row[Column(col)].c));
            if current >= grid.bottommost_line() || !wraps(current) {
                break;
            }
            current += 1;
        }
        (text, start)
    }

    pub fn columns(&self) -> usize {
        self.term.grid().columns()
    }
}
