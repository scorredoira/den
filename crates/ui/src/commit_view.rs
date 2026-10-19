//! A whole commit in one tab (VS Code's multi-diff): its message, author and
//! date, and then each file's changes side by side, one after another, with
//! the lines highlighted as the file's language. Read-only rows in one list;
//! a file's name opens that file's diff in its own tab. Text is selected
//! with the mouse in one column at a time, and Cmd-C copies it.

use std::{cell::Cell, ops::Range, rc::Rc, sync::Arc};

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    highlighter::{HighlightTheme, SyntaxHighlighter},
    input::{Copy, Rope},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    config::{Config, TextArea, UiText as _},
    diff::{self, Kind},
    language,
    workspace::measure_width,
};

/// Beyond this many rows a file's changes aren't drawn: its name opens them.
const MAX_FILE_ROWS: usize = 3000;
/// Sides larger than this aren't highlighted.
const MAX_HIGHLIGHTED: usize = 256 * 1024;

pub enum CommitViewEvent {
    OpenFile(String),
}

pub struct CommitView {
    rows: Rc<Vec<Row>>,
    /// The same in one column, for when there's no room for two sides.
    inline: Rc<Vec<Row>>,
    scroll: UniformListScrollHandle,
    width: Rc<Cell<Pixels>>,
    /// Drawn in one column last time: the rows a file's name is among.
    narrow: Cell<bool>,
    selection: Option<Selection>,
    /// The button is down since a press on a line: moving extends the selection.
    selecting: bool,
    focus_handle: FocusHandle,
}

/// The text of a line a selection is in: the old side or the new one (also
/// the one column's).
#[derive(Clone, Copy, PartialEq)]
enum Column {
    Old,
    New,
}

/// Text selected in one column, from where it started to where it ends
/// now, each a row and a byte of its line.
#[derive(Clone, Copy, PartialEq)]
struct Selection {
    column: Column,
    /// In the rows of one column, not of two.
    narrow: bool,
    anchor: (usize, usize),
    head: (usize, usize),
}

impl Selection {
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    /// The bytes of row `ix`'s line in it, `len` long.
    fn bytes(&self, ix: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered();
        if ix < start.0 || ix > end.0 {
            return None;
        }
        let from = if ix == start.0 { start.1.min(len) } else { 0 };
        let to = if ix == end.0 { end.1.min(len) } else { len };
        (from < to).then_some(from..to)
    }
}

/// The text of row `ix` in `column`: none in a gap, nor in a row that isn't
/// a line.
fn line(rows: &[Row], ix: usize, column: Column) -> Option<&Half> {
    match (rows.get(ix)?, column) {
        (Row::Line { old, .. }, Column::Old) => Some(old),
        (Row::Line { new, .. }, Column::New) => Some(new),
        (Row::Single { half, .. }, Column::New) => Some(half),
        _ => None,
    }
    .filter(|half| half.kind != Kind::Gap)
}

/// The text selected, its lines one under the other.
fn selected_text(rows: &[Row], selection: &Selection) -> String {
    let (start, end) = selection.ordered();
    (start.0..=end.0)
        .filter_map(|ix| {
            let half = line(rows, ix, selection.column)?;
            let len = half.text.len();
            let from = if ix == start.0 { start.1.min(len) } else { 0 };
            let to = if ix == end.0 { end.1.min(len) } else { len };
            Some(half.text.get(from..to.max(from)).unwrap_or(""))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The word at `byte` of `text`, or the run of other characters there.
fn word_at(text: &str, byte: usize) -> Range<usize> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let byte = byte.min(text.len());
    let Some(at) = text[byte..].chars().next().or_else(|| text[..byte].chars().next_back()) else {
        return byte..byte;
    };
    let same = |c: char| match (is_word(at), at.is_whitespace()) {
        (true, _) => is_word(c),
        (_, true) => c.is_whitespace(),
        _ => !is_word(c) && !c.is_whitespace(),
    };
    let start = text[..byte].char_indices().rev().take_while(|(_, c)| same(*c)).last().map_or(byte, |(ix, _)| ix);
    let end = text[byte..].char_indices().find(|(_, c)| !same(*c)).map_or(text.len(), |(ix, _)| byte + ix);
    start..end
}

/// The byte of a laid out line under `position`, the nearest between two
/// characters.
fn byte_at(layout: &TextLayout, position: Point<Pixels>) -> usize {
    let Some(line) = layout.line_layout_for_index(0) else {
        return 0;
    };
    let x = (position.x - layout.bounds().left()).max(px(0.));
    line.closest_index_for_position(point(x, px(0.)), layout.line_height()).unwrap_or_else(|ix| ix)
}

impl EventEmitter<CommitViewEvent> for CommitView {}

enum Row {
    Subject(SharedString),
    Body(SharedString),
    Meta(SharedString),
    Blank,
    File { path: SharedString, added: usize, removed: usize },
    Note(SharedString),
    /// Lines skipped between two groups of changes.
    Skip,
    Line { old: Half, new: Half },
    /// A line in one column: both numbers, and `kind` `Changed` with only
    /// `old` (removed) or only `new` (added).
    Single { old: Option<u32>, new: Option<u32>, half: Half },
}

#[derive(Clone)]
struct Half {
    number: Option<u32>,
    kind: Kind,
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
}

/// A commit's rows, in two columns and in one, worked out away from the UI
/// thread: highlighting every file of a big commit takes a while.
#[derive(Default)]
pub struct Prepared {
    rows: Vec<Row>,
    inline: Vec<Row>,
    /// The rows of the message, first in both.
    header: usize,
}

/// A commit's message, for showing it apart from its changes.
#[derive(Clone, Default)]
pub struct CommitMessage {
    pub subject: SharedString,
    pub body: SharedString,
    /// Who wrote it, when, and its short hash.
    pub meta: SharedString,
}

/// A file a commit changed, as its rows head it.
#[derive(Clone)]
pub struct CommitFile {
    pub path: SharedString,
    pub added: usize,
    pub removed: usize,
}

impl Prepared {
    /// The files, in the order they show.
    pub fn files(&self) -> Vec<CommitFile> {
        self.rows
            .iter()
            .filter_map(|row| match row {
                Row::File { path, added, removed } => Some(CommitFile { path: path.clone(), added: *added, removed: *removed }),
                _ => None,
            })
            .collect()
    }

    /// Takes the message out of the rows: they start at the first file.
    pub fn take_message(&mut self) -> CommitMessage {
        let mut message = CommitMessage::default();
        let mut body: Vec<&str> = Vec::new();
        for row in &self.rows[..self.header] {
            match row {
                Row::Subject(text) => message.subject = text.clone(),
                Row::Body(text) => body.push(text),
                Row::Blank => body.push(""),
                Row::Meta(text) => message.meta = text.clone(),
                _ => {}
            }
        }
        message.body = body.join("\n").trim().to_string().into();
        // and the blank line before the first file
        let header = (self.header + 1).min(self.rows.len()).min(self.inline.len());
        self.rows.drain(..header);
        self.inline.drain(..header);
        self.header = 0;
        message
    }
}

/// Prepares `git show --format=fuller --patch` of a commit in the background:
/// only `only`'s changes if given, a file or (`true`) a folder's files.
pub fn prepare(show: String, only: Option<(String, bool)>, cx: &App) -> Task<Prepared> {
    let theme = cx.theme();
    let colors = Colors {
        removed: theme.danger.opacity(0.3),
        added: theme.success.opacity(0.3),
        syntax: theme.highlight_theme.clone(),
    };
    cx.background_spawn(async move {
        let (rows, header) = rows(&show, only.as_ref(), &colors);
        let inline = inline(&rows);
        Prepared { rows, inline, header }
    })
}

/// What `rows` needs of the theme, to run off the UI thread.
struct Colors {
    removed: Hsla,
    added: Hsla,
    syntax: Arc<HighlightTheme>,
}

impl CommitView {
    /// `width`: the room it has, if known, so its first frame already has
    /// one column or two.
    pub fn new(prepared: Prepared, width: Pixels, cx: &mut Context<Self>) -> Self {
        Self {
            rows: Rc::new(prepared.rows),
            inline: Rc::new(prepared.inline),
            scroll: UniformListScrollHandle::new(),
            width: Rc::new(Cell::new(width)),
            narrow: Cell::new(false),
            selection: None,
            selecting: false,
            focus_handle: cx.focus_handle(),
        }
    }

    /// No room for its two sides: unknown until it is painted.
    pub fn cramped(&self, cx: &App) -> bool {
        let width = self.width.get();
        width > px(0.) && !Config::get(cx).room_for_two_sides(width)
    }

    /// Scrolls to `file`'s changes.
    pub fn scroll_to(&self, file: &str) {
        let rows = if self.narrow.get() { &self.inline } else { &self.rows };
        if let Some(ix) = rows.iter().position(|row| matches!(row, Row::File { path, .. } if path == file)) {
            self.scroll.scroll_to_item_strict(ix, ScrollStrategy::Top);
        }
    }

    /// Shows another commit in its place, from the top: no new view, so it
    /// keeps its width and doesn't go blank in between.
    pub fn set(&mut self, prepared: Prepared, cx: &mut Context<Self>) {
        self.rows = Rc::new(prepared.rows);
        self.inline = Rc::new(prepared.inline);
        self.scroll = UniformListScrollHandle::new();
        self.selection = None;
        cx.notify();
    }

    /// The button went down on row `ix`'s line at `byte`: a new selection
    /// there (the word or the line on a second or third click), or with
    /// Shift the one in this column taken there.
    fn press(&mut self, column: Column, ix: usize, byte: usize, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        self.selecting = true;
        let narrow = self.narrow.get();
        if let Some(selection) = self.selection.as_mut().filter(|selection| event.modifiers.shift && selection.column == column && selection.narrow == narrow) {
            selection.head = (ix, byte);
        } else {
            let rows = if narrow { &self.inline } else { &self.rows };
            let text = line(rows, ix, column).map(|half| half.text.as_ref()).unwrap_or("");
            let range = match event.click_count {
                2 => word_at(text, byte),
                3.. => 0..text.len(),
                _ => byte..byte,
            };
            self.selection = Some(Selection { column, narrow, anchor: (ix, range.start), head: (ix, range.end) });
        }
        cx.notify();
    }

    /// The pointer is over row `ix`'s line at `byte` with the button down.
    fn drag(&mut self, column: Column, ix: usize, byte: usize, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.as_mut().filter(|selection| self.selecting && selection.column == column) else {
            return;
        };
        if selection.head != (ix, byte) {
            selection.head = (ix, byte);
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let Some(selection) = self.selection.filter(|selection| selection.anchor != selection.head) else {
            return;
        };
        let rows = if selection.narrow { &self.inline } else { &self.rows };
        cx.write_to_clipboard(ClipboardItem::new_string(selected_text(rows, &selection)));
    }
}

/// The rows in one column, as VS Code's inline diff: in each block of
/// changes the removed lines, then the added ones.
fn inline(rows: &[Row]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut added = Vec::new();
    for row in rows {
        match row {
            Row::Line { old, new } if old.kind != Kind::Same || new.kind != Kind::Same => {
                if old.kind == Kind::Changed {
                    out.push(Row::Single { old: old.number, new: None, half: old.clone() });
                }
                if new.kind == Kind::Changed {
                    added.push(Row::Single { old: None, new: new.number, half: new.clone() });
                }
            }
            _ => {
                out.append(&mut added);
                out.push(match row {
                    Row::Line { old, new } => Row::Single { old: old.number, new: new.number, half: new.clone() },
                    Row::Subject(text) => Row::Subject(text.clone()),
                    Row::Body(text) => Row::Body(text.clone()),
                    Row::Meta(text) => Row::Meta(text.clone()),
                    Row::Blank => Row::Blank,
                    Row::File { path, added, removed } => Row::File { path: path.clone(), added: *added, removed: *removed },
                    Row::Note(text) => Row::Note(text.clone()),
                    Row::Skip => Row::Skip,
                    Row::Single { .. } => unreachable!("only in the inline rows"),
                });
            }
        }
    }
    out.append(&mut added);
    out
}

/// The rows, and how many of them are the message.
fn rows(show: &str, only: Option<&(String, bool)>, colors: &Colors) -> (Vec<Row>, usize) {
    let (header, patch) = match show.find("\ndiff --git ") {
        Some(at) => (&show[..at], &show[at + 1..]),
        None => (show, ""),
    };
    let mut rows = header_rows(header);
    let message = rows.len();
    for section in patch.split("\ndiff --git ").filter(|section| !section.trim().is_empty()) {
        let path = section_path(section);
        if !only.is_none_or(|only| shows(only, &path)) {
            continue;
        }
        rows.push(Row::Blank);
        let Some(sides) = diff::split(section) else {
            rows.push(Row::File { path: path.clone().into(), added: 0, removed: 0 });
            let note = if section.contains("Binary files") { "Binary file" } else { "No changes to show" };
            rows.push(Row::Note(note.into()));
            continue;
        };
        let count = |side: &diff::Side| side.lines.iter().filter(|line| line.kind == Kind::Changed).count();
        rows.push(Row::File { path: path.clone().into(), added: count(&sides.new), removed: count(&sides.old) });
        if sides.old.lines.len() > MAX_FILE_ROWS {
            rows.push(Row::Note(format!("{} lines: open the file to see them", sides.old.lines.len()).into()));
            continue;
        }
        let language = language::for_path(std::path::Path::new(&path));
        let old = halves(&sides.old, language, colors.removed, &colors.syntax);
        let new = halves(&sides.new, language, colors.added, &colors.syntax);
        let mut last: Option<(Option<u32>, Option<u32>)> = None;
        for (old, new) in old.into_iter().zip(new) {
            if let Some((old_last, new_last)) = last {
                let jumped = |last: Option<u32>, now: Option<u32>| matches!((last, now), (Some(last), Some(now)) if now > last + 1);
                if jumped(old_last, old.number) || jumped(new_last, new.number) {
                    rows.push(Row::Skip);
                }
            }
            let (old_last, new_last) = last.unwrap_or_default();
            last = Some((old.number.or(old_last), new.number.or(new_last)));
            rows.push(Row::Line { old, new });
        }
    }
    (rows, message)
}

/// Whether `path` is `only`'s file, or in its folder.
fn shows((only, folder): &(String, bool), path: &str) -> bool {
    if *folder {
        path.strip_prefix(only.trim_end_matches('/')).is_some_and(|rest| rest.starts_with('/'))
    } else {
        path == only
    }
}

/// The message (its first line in bold), who wrote it and when.
fn header_rows(header: &str) -> Vec<Row> {
    let field = |name: &str| {
        header
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_string())
            .unwrap_or_default()
    };
    let hash = header.lines().next().and_then(|line| line.strip_prefix("commit ")).unwrap_or("");
    let short: String = hash.chars().take(10).collect();
    let author = field("Author:");
    let author = author.split(" <").next().unwrap_or(&author).to_string();
    let mut rows = Vec::new();
    let mut message = header.lines().filter_map(|line| line.strip_prefix("    ")).peekable();
    if let Some(subject) = message.next() {
        rows.push(Row::Subject(subject.to_string().into()));
    }
    for line in message {
        rows.push(if line.is_empty() { Row::Blank } else { Row::Body(line.to_string().into()) });
    }
    rows.push(Row::Meta(format!("{author} · {} · {short}", field("AuthorDate:")).into()));
    rows
}

/// The file a section is about: the new name, or the old one if deleted.
fn section_path(section: &str) -> String {
    let line = |prefix: &str| {
        section
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .filter(|path| *path != "/dev/null")
            .map(|path| path.get(2..).unwrap_or(path).to_string())
    };
    line("+++ ").or_else(|| line("--- ")).unwrap_or_else(|| {
        // `a/x b/x` in the section's first line, for binary files.
        let first = section.lines().next().unwrap_or("");
        first.rsplit(" b/").next().unwrap_or(first).to_string()
    })
}

/// Each line of a side, with its syntax colors and what changed within it.
fn halves(side: &diff::Side, language: &str, word: Hsla, theme: &HighlightTheme) -> Vec<Half> {
    let highlighter = (side.text.len() <= MAX_HIGHLIGHTED).then(|| {
        let mut highlighter = SyntaxHighlighter::new(language);
        highlighter.update(None, &Rope::from(side.text.as_str()), None);
        highlighter
    });
    let mut start = 0;
    side.text
        .split('\n')
        .zip(&side.lines)
        .map(|(text, line)| {
            let range = start..start + text.len();
            start = range.end + 1;
            let relative = |absolute: &Range<usize>| absolute.start.max(range.start) - range.start..absolute.end.min(range.end) - range.start;
            let syntax: Vec<(Range<usize>, HighlightStyle)> = highlighter
                .as_ref()
                .map(|highlighter| highlighter.styles(&range, theme))
                .unwrap_or_default()
                .into_iter()
                .map(|(absolute, style)| (relative(&absolute), style))
                .filter(|(range, _)| !range.is_empty())
                .collect();
            let changed: Vec<(Range<usize>, HighlightStyle)> = line
                .changed
                .iter()
                .map(relative)
                .filter(|range| !range.is_empty())
                .map(|range| (range, HighlightStyle { background_color: Some(word), ..Default::default() }))
                .collect();
            Half {
                number: line.number,
                kind: line.kind,
                text: text.to_string().into(),
                highlights: combine_highlights(syntax, changed).collect(),
            }
        })
        .collect()
}

impl Render for CommitView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // No room for two sides (or one column chosen): one column.
        let narrow = !Config::get(cx).diff_side_by_side(self.width.get());
        self.narrow.set(narrow);
        let rows = if narrow { self.inline.clone() } else { self.rows.clone() };
        let view = cx.entity().downgrade();
        let size = Config::get(cx).font_size(TextArea::Editor);
        let height = px((size * 1.6).round());
        let theme = cx.theme();
        let digits = rows
            .iter()
            .filter_map(|row| match row {
                Row::Line { old, new } => old.number.max(new.number),
                Row::Single { old, new, .. } => (*old).max(*new),
                _ => None,
            })
            .max()
            .unwrap_or(1)
            .to_string()
            .len();
        let gutter = px(size * 0.62 * (digits + 2) as f32);
        let (removed, added) = (theme.danger.opacity(0.14), theme.success.opacity(0.14));
        let selection = self.selection.filter(|selection| selection.narrow == narrow);
        let list = uniform_list("commit-view", rows.len(), move |range, _, cx| {
            let theme = cx.theme();
            // A line's text, with what's selected of it, which a press
            // starts selecting and a move with the button down extends.
            let text = |half: &Half, ix: usize, column: Column| {
                let selected = selection
                    .filter(|selection| selection.column == column && half.kind != Kind::Gap)
                    .and_then(|selection| selection.bytes(ix, half.text.len()));
                let highlights: Vec<_> = match selected {
                    Some(selected) => {
                        let style = HighlightStyle { background_color: Some(theme.selection), ..Default::default() };
                        combine_highlights(half.highlights.clone(), [(selected, style)]).collect()
                    }
                    None => half.highlights.clone(),
                };
                let text = StyledText::new(half.text.clone()).with_highlights(highlights);
                let layout = text.layout().clone();
                let (press, drag) = (view.clone(), view.clone());
                let drag_layout = layout.clone();
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        cx.stop_propagation();
                        let byte = byte_at(&layout, event.position);
                        press.update(cx, |this, cx| this.press(column, ix, byte, event, window, cx)).ok();
                    })
                    .on_mouse_move(move |event, _, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            let byte = byte_at(&drag_layout, event.position);
                            drag.update(cx, |this, cx| this.drag(column, ix, byte, cx)).ok();
                        }
                    })
                    .child(text)
            };
            let half = |half: &Half, background: Hsla, marker: &str, ix: usize, column: Column| {
                let changed = half.kind == Kind::Changed;
                h_flex()
                    .w_1_2()
                    .h_full()
                    .overflow_hidden()
                    .when(changed, |el| el.bg(background))
                    .when(half.kind == Kind::Gap, |el| el.bg(theme.muted.opacity(0.5)))
                    .child(
                        div()
                            .flex_none()
                            .w(gutter)
                            .pr_2()
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(half.number.map(|number| format!("{number}{}", if changed { marker } else { " " })).unwrap_or_default()),
                    )
                    .child(text(half, ix, column))
            };
            range
                .map(|ix| {
                    let row = div().id(ix).h(height).w_full().flex().items_center();
                    match &rows[ix] {
                        Row::Subject(text) => row.px_4().text_ui(cx).font_semibold().child(text.clone()),
                        Row::Body(text) => row.px_4().text_ui(cx).child(text.clone()),
                        Row::Meta(text) => row.px_4().text_ui_small(cx).text_color(theme.muted_foreground).child(text.clone()),
                        Row::Blank => row,
                        Row::Note(text) => row.px_4().text_ui_small(cx).text_color(theme.muted_foreground).child(text.clone()),
                        Row::Skip => row
                            .px_4()
                            .bg(theme.muted.opacity(0.3))
                            .text_color(theme.muted_foreground)
                            .font_family(theme.mono_font_family.clone())
                            .text_size(px(size))
                            .child("⋯"),
                        Row::File { path, added, removed } => {
                            let file = path.to_string();
                            let view = view.clone();
                            row.px_4()
                                .gap_3()
                                .border_b_1()
                                .border_color(theme.border)
                                .bg(theme.secondary)
                                .text_ui(cx)
                                .cursor_pointer()
                                .hover(|style| style.bg(theme.secondary_hover))
                                .child(div().font_semibold().child(path.clone()))
                                .child(div().text_color(theme.success).child(format!("+{added}")))
                                .child(div().text_color(theme.danger).child(format!("−{removed}")))
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |_, cx| cx.emit(CommitViewEvent::OpenFile(file.clone()))).ok();
                                })
                        }
                        Row::Line { old, new } => row
                            .font_family(theme.mono_font_family.clone())
                            .text_size(px(size))
                            .child(half(old, removed, "−", ix, Column::Old))
                            .child(div().flex_none().w(px(1.)).h_full().bg(theme.border))
                            .child(half(new, added, "+", ix, Column::New)),
                        Row::Single { old, new, half } => {
                            let (background, marker) = match (old, new) {
                                (Some(_), None) => (Some(removed), "−"),
                                (None, Some(_)) => (Some(added), "+"),
                                _ => (None, " "),
                            };
                            let number = |number: &Option<u32>| number.map(|number| number.to_string()).unwrap_or_default();
                            let column = || div().flex_none().w(gutter).pr_2().text_right().text_color(theme.muted_foreground);
                            row.font_family(theme.mono_font_family.clone())
                                .text_size(px(size))
                                .when_some(background, |el, background| el.bg(background))
                                .child(column().child(number(old)))
                                .child(column().child(format!("{}{marker}", number(new))))
                                .child(text(half, ix, Column::New))
                        }
                    }
                    .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy))
            // a press anywhere but on a line's text leaves nothing selected
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.selection.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.selecting = false))
            .child(measure_width(&self.width))
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::{header_rows, section_path, selected_text, shows, word_at, Column, Half, Kind, Row, Selection};

    #[test]
    fn header_and_paths() {
        let header = "commit 0123456789abcdef (HEAD)\nAuthor:     Ana <a@b.c>\nAuthorDate: Wed Sep 30 23:42:58 2026 +0200\nCommit:     Ana <a@b.c>\n\n    Subject\n    \n    Body line\n";
        let rows = header_rows(header);
        assert!(matches!(&rows[0], Row::Subject(text) if text == "Subject"));
        assert!(matches!(&rows[1], Row::Blank));
        assert!(matches!(&rows[2], Row::Body(text) if text == "Body line"));
        assert!(matches!(&rows[3], Row::Meta(text) if text == "Ana · Wed Sep 30 23:42:58 2026 +0200 · 0123456789"));
        assert_eq!(section_path("a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1 @@\n"), "x.rs");
        assert_eq!(section_path("a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n"), "gone.rs");
        assert_eq!(section_path("a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n"), "i.png");
    }

    #[test]
    fn a_file_s_history_shows_only_its_changes() {
        let file = ("src/main.rs".to_string(), false);
        assert!(shows(&file, "src/main.rs"));
        assert!(!shows(&file, "src/main.rs.orig"));
        let folder = ("src".to_string(), true);
        assert!(shows(&folder, "src/main.rs"));
        assert!(!shows(&folder, "srcs/main.rs"));
    }

    #[test]
    fn copies_what_is_selected_of_one_column() {
        let half = |kind: Kind, text: &str| Half { number: Some(1), kind, text: text.to_string().into(), highlights: Vec::new() };
        let rows = vec![
            Row::Line { old: half(Kind::Same, "let a = 1;"), new: half(Kind::Same, "let a = 1;") },
            Row::Line { old: half(Kind::Changed, "let b = 2;"), new: half(Kind::Changed, "let b = 3;") },
            Row::Line { old: half(Kind::Gap, ""), new: half(Kind::Changed, "let c = 4;") },
            Row::Skip,
            Row::Line { old: half(Kind::Same, "done()"), new: half(Kind::Same, "done()") },
        ];
        let selection = |column, anchor, head| Selection { column, narrow: false, anchor, head };
        // from the middle of a line to the middle of another, backwards too
        assert_eq!(selected_text(&rows, &selection(Column::New, (0, 4), (2, 5))), "a = 1;\nlet b = 3;\nlet c");
        assert_eq!(selected_text(&rows, &selection(Column::New, (2, 5), (0, 4))), "a = 1;\nlet b = 3;\nlet c");
        // the old side's gap and the folded lines are no lines of it
        assert_eq!(selected_text(&rows, &selection(Column::Old, (1, 0), (4, 4))), "let b = 2;\ndone");
        assert_eq!(selection(Column::Old, (1, 4), (4, 4)).bytes(2, 0), None);
        assert_eq!(selection(Column::Old, (1, 4), (4, 4)).bytes(1, 10), Some(4..10));
    }

    #[test]
    fn a_double_click_takes_a_word() {
        assert_eq!(word_at("let some_name = 1;", 6), 4..13);
        assert_eq!(word_at("a  b", 1), 1..3);
        assert_eq!(word_at("x == y", 3), 2..4);
        assert_eq!(word_at("end", 3), 0..3);
        assert_eq!(word_at("", 0), 0..0);
    }
}
