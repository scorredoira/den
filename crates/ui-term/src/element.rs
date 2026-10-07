//! Paints the terminal grid. Follows the approach of Zed's `terminal_element.rs`
//! (GPL-3.0-or-later): cells grouped into runs of the same style, and each run
//! with the cell width forced so everything lines up in columns.

use std::{cell::Cell as StdCell, ops::Range, rc::Rc};

use alacritty_terminal::{
    index::{Line, Point as AlacPoint},
    term::{
        TermMode,
        cell::{Cell, Flags},
    },
    vte::ansi::CursorShape,
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::{
    colors::Palette,
    terminal::{GridSize, Terminal},
    view::{PADDING_X, PADDING_Y, TerminalView},
};

/// Line height relative to the font size.
const LINE_HEIGHT: f32 = 1.3;

pub struct TerminalElement {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    focus: FocusHandle,
    focused: bool,
    font_size: Pixels,
    /// Where the grid ended up, so the view can translate the mouse to cells.
    layout: Rc<StdCell<Option<GridLayout>>>,
    /// Cells of the link under the mouse (with Cmd), which get underlined.
    link_cells: Vec<(Line, Range<usize>)>,
}

#[derive(Clone, Copy)]
pub struct GridLayout {
    pub origin: Point<Pixels>,
    pub size: GridSize,
}

impl TerminalElement {
    pub fn new(
        terminal: Entity<Terminal>,
        view: Entity<TerminalView>,
        focus: FocusHandle,
        focused: bool,
        font_size: Pixels,
        layout: Rc<StdCell<Option<GridLayout>>>,
        link_cells: Vec<(Line, Range<usize>)>,
    ) -> Self {
        Self {
            terminal,
            view,
            focus,
            focused,
            font_size,
            layout,
            link_cells,
        }
    }
}

struct TextRun_ {
    line: usize,
    col: usize,
    text: String,
    cells: usize,
    run: TextRun,
    /// A wide character's (two cells): shaped alone, as each glyph of a run
    /// takes one cell.
    wide: bool,
}

struct Background {
    line: usize,
    cols: Range<usize>,
    color: Hsla,
}

struct CursorLayout {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    text: Option<ShapedLine>,
}

struct Block {
    line: usize,
    col: usize,
    /// Part of the cell that's filled, as fractions: x0, y0, x1, y1.
    rect: [f32; 4],
    color: Hsla,
}

pub struct Prepaint {
    size: GridSize,
    blocks: Vec<Block>,
    link_underlines: Vec<Bounds<Pixels>>,
    palette: Rc<Palette>,
    backgrounds: Vec<Background>,
    lines: Vec<(usize, usize, ShapedLine)>,
    cursor: Option<CursorLayout>,
    marked: Option<ShapedLine>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(relative(1.).into(), relative(1.).into()),
            ..Default::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let theme = cx.theme();
        let font = Font {
            family: theme.mono_font_family.clone(),
            ..Font::default()
        };
        let palette = Rc::new(Palette::new(
            theme.is_dark(),
            theme.foreground,
            theme.background,
            theme.selection,
        ));
        let font_size = self.font_size;
        let grid = grid_size(bounds.size, &font, font_size, window);
        let GridSize {
            cell_width,
            line_height,
            ..
        } = grid;
        self.layout.set(Some(GridLayout {
            origin: bounds.origin,
            size: grid,
        }));
        self.terminal.update(cx, |terminal, _| {
            terminal.resize(grid);
            terminal.palette = Some(palette.clone());
        });

        let terminal = self.terminal.read(cx);
        let term = terminal.term();
        let content = term.renderable_content();
        let display_offset = content.display_offset as i32;
        let overrides = content.colors;

        let mut runs: Vec<TextRun_> = Vec::new();
        let mut backgrounds: Vec<Background> = Vec::new();
        let mut blocks: Vec<Block> = Vec::new();
        let mut cursor_cell: Option<(Cell, Hsla, Hsla)> = None;
        let cursor_point = content.cursor.point;

        for indexed in content.display_iter {
            let cell = &*indexed;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let line = (indexed.point.line.0 + display_offset) as usize;
            let col = indexed.point.column.0;

            let mut fg = palette.resolve(&cell.fg, overrides);
            let mut bg = palette.resolve(&cell.bg, overrides);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.intersects(Flags::DIM) {
                fg = fg.opacity(0.7);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let selected = content
                .selection
                .is_some_and(|selection| selection.contains(indexed.point));
            if selected {
                bg = palette.selection;
            }
            if indexed.point == cursor_point {
                cursor_cell = Some((Cell::clone(cell), fg, bg));
            }

            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let width = if wide { 2 } else { 1 };
            if bg != palette.background {
                match backgrounds.last_mut() {
                    Some(last) if last.line == line && last.cols.end == col && last.color == bg => {
                        last.cols.end = col + width;
                    }
                    _ => backgrounds.push(Background {
                        line,
                        cols: col..col + width,
                        color: bg,
                    }),
                }
            }

            // Block characters are painted as rectangles that fill the cell,
            // with no gaps between rows.
            if let Some(parts) = block_parts(cell.c) {
                for (rect, alpha) in parts {
                    blocks.push(Block {
                        line,
                        col,
                        rect,
                        color: fg.opacity(alpha),
                    });
                }
                continue;
            }

            let run = cell_run(cell, fg, &font);
            let plain_space = cell.c == ' ' && run.underline.is_none() && run.strikethrough.is_none();
            let appended = match runs.last_mut() {
                Some(last)
                    if !wide
                        && !last.wide
                        && last.line == line
                        && last.col + last.cells == col
                        && ((plain_space && !decorated(&last.run)) || same_style(&last.run, &run)) =>
                {
                    last.text.push(cell.c);
                    last.run.len += cell.c.len_utf8();
                    last.cells += 1;
                    true
                }
                _ => false,
            };
            if !appended {
                if plain_space {
                    continue;
                }
                runs.push(TextRun_ {
                    line,
                    col,
                    text: cell.c.to_string(),
                    cells: width,
                    run,
                    wide,
                });
            }
            if let Some(zerowidth) = cell.zerowidth() {
                let last = runs.last_mut().expect("just added");
                for c in zerowidth {
                    last.text.push(*c);
                    last.run.len += c.len_utf8();
                }
            }
        }

        let lines = runs
            .into_iter()
            .map(|run| {
                let shaped = window.text_system().shape_line(
                    run.text.into(),
                    font_size,
                    &[run.run],
                    Some(cell_width),
                );
                (run.line, run.col, shaped)
            })
            .collect();

        let cursor_visible = content.cursor.shape != CursorShape::Hidden
            && term.mode().contains(TermMode::SHOW_CURSOR);
        let cursor_line = cursor_point.line.0 + display_offset;
        let cursor = (cursor_visible && cursor_line >= 0 && cursor_line < grid.rows as i32).then(|| {
            let (cell, _, bg) = cursor_cell.unwrap_or((Cell::default(), palette.foreground, palette.background));
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2. } else { 1. };
            let origin = point(
                bounds.origin.x + cell_width * cursor_point.column.0 as f32,
                bounds.origin.y + line_height * cursor_line as f32,
            );
            let shape = if self.focused {
                content.cursor.shape
            } else {
                CursorShape::HollowBlock
            };
            // Under a block cursor, the glyph is repainted with the background color.
            let text = (shape == CursorShape::Block && cell.c != ' ').then(|| {
                let run = cell_run(&cell, bg, &font);
                window.text_system().shape_line(
                    cell.c.to_string().into(),
                    font_size,
                    &[run],
                    Some(cell_width),
                )
            });
            CursorLayout {
                bounds: Bounds::new(origin, size(cell_width * width, line_height)),
                shape,
                text,
            }
        });

        let marked = self.view.read(cx).marked_text().map(|text| {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: palette.foreground,
                background_color: Some(palette.background),
                underline: Some(UnderlineStyle {
                    color: Some(palette.foreground),
                    thickness: px(1.),
                    wavy: false,
                }),
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(text.to_string().into(), font_size, &[run], Some(cell_width))
        });

        let link_underlines = self
            .link_cells
            .iter()
            .filter_map(|(line, cols)| {
                let row = line.0 + display_offset;
                (row >= 0 && row < grid.rows as i32).then(|| {
                    Bounds::new(
                        point(
                            bounds.origin.x + cell_width * cols.start as f32,
                            bounds.origin.y + line_height * (row + 1) as f32 - px(1.),
                        ),
                        size(cell_width * cols.len() as f32, px(1.)),
                    )
                })
            })
            .collect();

        Prepaint {
            size: grid,
            blocks,
            link_underlines,
            palette,
            backgrounds,
            lines,
            cursor,
            marked,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let GridSize {
            cell_width,
            line_height,
            ..
        } = prepaint.size;
        let origin = bounds.origin;
        let cell_origin =
            |line: usize, col: usize| point(origin.x + cell_width * col as f32, origin.y + line_height * line as f32);

        window.paint_quad(fill(bounds, prepaint.palette.background));
        for background in &prepaint.backgrounds {
            let bounds = Bounds::new(
                cell_origin(background.line, background.cols.start),
                size(cell_width * background.cols.len() as f32, line_height),
            );
            window.paint_quad(fill(bounds, background.color));
        }
        for block in &prepaint.blocks {
            let [x0, y0, x1, y1] = block.rect;
            let cell = cell_origin(block.line, block.col);
            let bounds = Bounds::from_corners(
                point(cell.x + cell_width * x0, cell.y + line_height * y0),
                point(cell.x + cell_width * x1, cell.y + line_height * y1),
            );
            window.paint_quad(fill(bounds, block.color));
        }
        for (line, col, shaped) in &prepaint.lines {
            let _ = shaped.paint(cell_origin(*line, *col), line_height, TextAlign::Left, None, window, cx);
        }

        for underline in &prepaint.link_underlines {
            window.paint_quad(fill(*underline, prepaint.palette.foreground));
        }

        let mut cursor_bounds = None;
        if let Some(cursor) = &prepaint.cursor {
            let b = cursor.bounds;
            cursor_bounds = Some(b);
            let color = prepaint.palette.cursor;
            match cursor.shape {
                CursorShape::Block => {
                    window.paint_quad(fill(b, color));
                    if let Some(text) = &cursor.text {
                        let _ = text.paint(b.origin, line_height, TextAlign::Left, None, window, cx);
                    }
                }
                CursorShape::Beam => {
                    window.paint_quad(fill(Bounds::new(b.origin, size(px(2.), b.size.height)), color));
                }
                CursorShape::Underline => {
                    let y = b.origin.y + b.size.height - px(2.);
                    window.paint_quad(fill(
                        Bounds::new(point(b.origin.x, y), size(b.size.width, px(2.))),
                        color,
                    ));
                }
                CursorShape::HollowBlock => {
                    window.paint_quad(outline(b, color, BorderStyle::Solid));
                }
                CursorShape::Hidden => {}
            }
        }
        if let (Some(marked), Some(b)) = (&prepaint.marked, cursor_bounds) {
            let _ = marked.paint(b.origin, line_height, TextAlign::Left, None, window, cx);
        }

        // While selecting, the drag goes on outside the view (it scrolls past
        // the edges) and a release out there ends it too; inside, the view's
        // own mouse up does. The release is never swallowed: whoever else
        // waits for it (a drag, a click) has to see it.
        if self.view.read(cx).selecting() {
            let view_bounds = Bounds::new(
                point(bounds.origin.x - px(PADDING_X), bounds.origin.y - px(PADDING_Y)),
                size(bounds.size.width + px(2. * PADDING_X), bounds.size.height + px(2. * PADDING_Y)),
            );
            let view = self.view.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Capture {
                    view.update(cx, |view, cx| match event.pressed_button {
                        Some(MouseButton::Left) => view.drag_selection(event.position, cx),
                        _ => view.end_selection(cx),
                    });
                }
            });
            let view = self.view.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && !view_bounds.contains(&event.position)
                {
                    view.update(cx, |view, cx| view.end_selection(cx));
                }
            });
        }

        window.handle_input(
            &self.focus,
            TerminalInputHandler {
                view: self.view.clone(),
                cursor_bounds,
            },
            cx,
        );
    }
}

/// Parts of the cell covered by a block character (U+2580–U+259F), with their
/// opacity; `None` if it isn't one of them.
fn block_parts(c: char) -> Option<Vec<([f32; 4], f32)>> {
    let full = |rect: [f32; 4]| Some(vec![(rect, 1.)]);
    let eighth = |n: u32| n as f32 / 8.;
    let quadrants = |mask: u8| {
        let quads = [[0., 0., 0.5, 0.5], [0.5, 0., 1., 0.5], [0., 0.5, 0.5, 1.], [0.5, 0.5, 1., 1.]];
        Some(
            quads
                .into_iter()
                .enumerate()
                .filter(|(ix, _)| mask & (1 << ix) != 0)
                .map(|(_, rect)| (rect, 1.))
                .collect(),
        )
    };
    let code = c as u32;
    match code {
        0x2580 => full([0., 0., 1., 0.5]),
        0x2581..=0x2587 => full([0., 1. - eighth(code - 0x2580), 1., 1.]),
        0x2588 => full([0., 0., 1., 1.]),
        0x2589..=0x258F => full([0., 0., eighth(8 - (code - 0x2588)), 1.]),
        0x2590 => full([0.5, 0., 1., 1.]),
        0x2591..=0x2593 => Some(vec![([0., 0., 1., 1.], (code - 0x2590) as f32 * 0.25)]),
        0x2594 => full([0., 0., 1., eighth(1)]),
        0x2595 => full([eighth(7), 0., 1., 1.]),
        // Quadrants: bit 0 top left, 1 top right, 2 bottom left, 3 bottom right.
        0x2596 => quadrants(0b0100),
        0x2597 => quadrants(0b1000),
        0x2598 => quadrants(0b0001),
        0x2599 => quadrants(0b1101),
        0x259A => quadrants(0b1001),
        0x259B => quadrants(0b0111),
        0x259C => quadrants(0b1011),
        0x259D => quadrants(0b0010),
        0x259E => quadrants(0b0110),
        0x259F => quadrants(0b1110),
        _ => None,
    }
}

/// Columns and rows that fit in `size` with the terminal font.
pub fn grid_size(size: Size<Pixels>, font: &Font, font_size: Pixels, window: &Window) -> GridSize {
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(font);
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|advance| advance.width)
        .unwrap_or(font_size * 0.6);
    let line_height = (font_size * LINE_HEIGHT).round();
    GridSize {
        cols: ((size.width / cell_width).floor() as u16).max(2),
        rows: ((size.height / line_height).floor() as u16).max(1),
        cell_width,
        line_height,
    }
}

fn cell_run(cell: &Cell, color: Hsla, font: &Font) -> TextRun {
    let underline = cell
        .flags
        .intersects(Flags::ALL_UNDERLINES)
        .then(|| UnderlineStyle {
            color: Some(color),
            thickness: px(1.),
            wavy: cell.flags.contains(Flags::UNDERCURL),
        });
    let strikethrough = cell
        .flags
        .contains(Flags::STRIKEOUT)
        .then(|| StrikethroughStyle {
            color: Some(color),
            thickness: px(1.),
        });
    TextRun {
        len: cell.c.len_utf8(),
        font: Font {
            weight: if cell.flags.contains(Flags::BOLD) {
                FontWeight::BOLD
            } else {
                font.weight
            },
            style: if cell.flags.contains(Flags::ITALIC) {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..font.clone()
        },
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

/// A space can't join an underlined or struck-through run: it would inherit the line.
fn decorated(run: &TextRun) -> bool {
    run.underline.is_some() || run.strikethrough.is_some()
}

fn same_style(a: &TextRun, b: &TextRun) -> bool {
    a.font == b.font && a.color == b.color && a.underline == b.underline && a.strikethrough == b.strikethrough
}

/// Receives the text the user types, including what the system composes
/// (accents and dead keys).
struct TerminalInputHandler {
    view: Entity<TerminalView>,
    cursor_bounds: Option<Bounds<Pixels>>,
}

impl InputHandler for TerminalInputHandler {
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut App) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        self.view
            .read(cx)
            .marked_text()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.commit_text(text, cx));
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| view.set_marked_text(text, cx));
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.set_marked_text("", cx));
    }

    fn bounds_for_range(&mut self, _: Range<usize>, _: &mut Window, _: &mut App) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut App) -> Option<usize> {
        None
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }
}

/// Alacritty point for a mouse position, and the side of the cell.
pub fn grid_point(
    layout: GridLayout,
    position: Point<Pixels>,
    display_offset: usize,
) -> (AlacPoint, alacritty_terminal::index::Side) {
    use alacritty_terminal::index::{Column, Line, Side};
    let GridSize {
        cols,
        rows,
        cell_width,
        line_height,
    } = layout.size;
    let x = (position.x - layout.origin.x).max(px(0.));
    let y = (position.y - layout.origin.y).max(px(0.));
    let cell = x / cell_width;
    let col = (cell as usize).min(cols as usize - 1);
    let row = ((y / line_height) as usize).min(rows as usize - 1);
    // Past the last column (the right margin) counts as its right half, or
    // dragging there would leave the last column out of the selection.
    let side = if cell >= cols as f32 || cell.fract() > 0.5 { Side::Right } else { Side::Left };
    (
        AlacPoint::new(Line(row as i32 - display_offset as i32), Column(col)),
        side,
    )
}

#[cfg(test)]
mod tests {
    use super::{AlacPoint, GridLayout, GridSize, Line, grid_point};
    use alacritty_terminal::index::{Column, Side};
    use gpui_kit::{point, px};

    #[test]
    fn the_right_margin_selects_the_last_column_whole() {
        let layout = GridLayout {
            origin: point(px(0.), px(0.)),
            size: GridSize { cols: 10, rows: 5, cell_width: px(8.), line_height: px(16.) },
        };
        let at = |x: f32| grid_point(layout, point(px(x), px(1.)), 0);
        assert_eq!(at(81.), (AlacPoint::new(Line(0), Column(9)), Side::Right));
        assert_eq!(at(100.), (AlacPoint::new(Line(0), Column(9)), Side::Right));
        assert_eq!(at(73.), (AlacPoint::new(Line(0), Column(9)), Side::Left));
        assert_eq!(at(78.), (AlacPoint::new(Line(0), Column(9)), Side::Right));
    }
}
