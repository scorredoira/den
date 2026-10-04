//! Snapshot of a terminal as escape sequences, like tmux does on reconnect:
//! a fresh emulator of the same size that processes them ends up with the
//! same screen, history, cursor and modes.
//!
//! Limitation: with the alternate screen active (vim, htop) only that screen
//! is reproduced; the main screen's history isn't accessible in
//! `alacritty_terminal` and is lost in the view (not in the agent).

use std::fmt::Write as _;

use alacritty_terminal::{
    Term,
    event::EventListener,
    grid::Dimensions as _,
    index::{Column, Line},
    term::{
        TermMode,
        cell::{Cell, Flags},
    },
    vte::ansi::{Color, CursorShape, NamedColor},
};

/// What clears a terminal like Cmd-K in Terminal.app: the history and the
/// screen go, the cursor's line moves to the top. The agent's emulator
/// processes it and the UIs receive it as output, so every view clears alike.
/// A CAN first ends an escape sequence the output may have left halfway. In
/// the alternate screen (vim, htop) the screen is the application's: only
/// the history goes.
pub fn clear<T: EventListener>(term: &Term<T>) -> Vec<u8> {
    let line = term.grid().cursor.point.line.0;
    let mut out = String::from("\x18");
    if line > 0 && !term.mode().contains(TermMode::ALT_SCREEN) {
        // Scrolling the lines above into the history, then dropping it.
        let _ = write!(out, "\x1b[{line}S\x1b[{line}A");
    }
    out.push_str("\x1b[3J");
    out.into_bytes()
}

/// Attributes that change how a cell looks.
const STYLE_FLAGS: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

pub fn snapshot<T: EventListener>(term: &Term<T>, title: Option<&str>) -> Vec<u8> {
    let grid = term.grid();
    let cols = grid.columns();
    let mode = *term.mode();
    let alt = mode.contains(TermMode::ALT_SCREEN);

    let mut out = String::new();
    if alt {
        out.push_str("\x1b[?1049h");
    }

    let top = if alt { Line(0) } else { grid.topmost_line() };
    let bottom = grid.bottommost_line();
    let mut style: Option<(Color, Color, Flags)> = None;
    let mut line = top;
    while line <= bottom {
        let row = &grid[line];
        let wraps = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
        let end = if wraps {
            cols
        } else {
            (0..cols).rev().find(|&col| !is_blank(&row[Column(col)])).map_or(0, |col| col + 1)
        };
        for col in 0..end {
            let cell = &row[Column(col)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let cell_style = (cell.fg, cell.bg, cell.flags & STYLE_FLAGS);
            if style != Some(cell_style) {
                push_sgr(&mut out, cell);
                style = Some(cell_style);
            }
            out.push(cell.c);
            if let Some(zerowidth) = cell.zerowidth() {
                out.extend(zerowidth);
            }
        }
        if line < bottom && !wraps {
            out.push_str("\x1b[0m\r\n");
            style = None;
        }
        line += 1;
    }
    out.push_str("\x1b[0m");

    let cursor = grid.cursor.point;
    let _ = write!(out, "\x1b[{};{}H", cursor.line.0 + 1, cursor.column.0 + 1);
    push_modes(&mut out, mode);
    push_cursor_style(&mut out, term);
    if let Some(title) = title {
        let _ = write!(out, "\x1b]2;{title}\x07");
    }
    out.into_bytes()
}

fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell.flags.intersects(STYLE_FLAGS)
}

fn push_sgr(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    let flags = cell.flags;
    for (flag, code) in [
        (Flags::BOLD, "1"),
        (Flags::DIM, "2"),
        (Flags::ITALIC, "3"),
        (Flags::INVERSE, "7"),
        (Flags::HIDDEN, "8"),
        (Flags::STRIKEOUT, "9"),
    ] {
        if flags.contains(flag) {
            out.push(';');
            out.push_str(code);
        }
    }
    if flags.contains(Flags::UNDERCURL) {
        out.push_str(";4:3");
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        out.push_str(";21");
    } else if flags.intersects(Flags::ALL_UNDERLINES) {
        out.push_str(";4");
    }
    push_color(out, cell.fg, false);
    push_color(out, cell.bg, true);
    out.push('m');
}

fn push_color(out: &mut String, color: Color, background: bool) {
    let base = if background { 40 } else { 30 };
    let extended = if background { 48 } else { 38 };
    match color {
        Color::Spec(rgb) => {
            let _ = write!(out, ";{extended};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(ix) => {
            let _ = write!(out, ";{extended};5;{ix}");
        }
        Color::Named(named) => {
            let ix = named as usize;
            // Dimmed shades are reproduced through the cell's DIM attribute.
            let ix = if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&ix) {
                ix - NamedColor::DimBlack as usize
            } else {
                ix
            };
            match ix {
                0..=7 => {
                    let _ = write!(out, ";{}", base + ix);
                }
                8..=15 => {
                    let _ = write!(out, ";{}", base + 60 + ix - 8);
                }
                // Default foreground and background colors.
                _ => {}
            }
        }
    }
}

fn push_modes(out: &mut String, mode: TermMode) {
    let private = [
        (TermMode::APP_CURSOR, 1, false),
        (TermMode::LINE_WRAP, 7, true),
        (TermMode::SHOW_CURSOR, 25, true),
        (TermMode::MOUSE_REPORT_CLICK, 1000, false),
        (TermMode::MOUSE_DRAG, 1002, false),
        (TermMode::MOUSE_MOTION, 1003, false),
        (TermMode::FOCUS_IN_OUT, 1004, false),
        (TermMode::UTF8_MOUSE, 1005, false),
        (TermMode::SGR_MOUSE, 1006, false),
        (TermMode::ALTERNATE_SCROLL, 1007, true),
        (TermMode::BRACKETED_PASTE, 2004, false),
    ];
    for (flag, code, default) in private {
        let on = mode.contains(flag);
        if on != default {
            let _ = write!(out, "\x1b[?{code}{}", if on { 'h' } else { 'l' });
        }
    }
    if mode.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    if mode.contains(TermMode::INSERT) {
        out.push_str("\x1b[4h");
    }
}

fn push_cursor_style<T: EventListener>(out: &mut String, term: &Term<T>) {
    let style = term.cursor_style();
    let code = match style.shape {
        CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 2,
        CursorShape::Underline => 4,
        CursorShape::Beam => 6,
    } - style.blinking as u8;
    let _ = write!(out, "\x1b[{code} q");
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::{
        event::VoidListener,
        term::{Config, test::TermSize},
        vte::ansi::Processor,
    };

    use super::*;

    fn term(cols: usize, rows: usize) -> Term<VoidListener> {
        Term::new(Config::default(), &TermSize::new(cols, rows), VoidListener)
    }

    fn screen<T: EventListener>(term: &Term<T>) -> Vec<String> {
        let grid = term.grid();
        (grid.topmost_line().0..=grid.bottommost_line().0)
            .map(|line| {
                let row = &grid[Line(line)];
                let text: String = (0..grid.columns()).map(|col| row[Column(col)].c).collect();
                text.trim_end().to_string()
            })
            .collect()
    }

    /// Processing the snapshot in a fresh emulator yields the same screen, history and cursor.
    #[test]
    fn replays_screen_history_and_cursor() {
        let mut original = term(20, 5);
        let mut parser: Processor = Processor::new();
        let input = "one\r\ntwo\r\n\x1b[1;31mred\x1b[0m and normal\r\na long line that gets split\r\n\r\nsix\r\nseven\r\nend";
        parser.advance(&mut original, input.as_bytes());

        let data = snapshot(&original, Some("title"));
        let mut copy = term(20, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut copy, &data);

        assert_eq!(screen(&copy), screen(&original));
        assert_eq!(copy.grid().cursor.point, original.grid().cursor.point);
        let red = &copy.grid()[Line(-2)][Column(0)];
        assert_eq!(red.c, 'r');
        assert!(red.flags.contains(Flags::BOLD));
        assert_eq!(red.fg, Color::Named(NamedColor::Red));
    }

    #[test]
    fn replays_modes_and_alt_screen() {
        let mut original = term(20, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut original, b"\x1b[?1049h\x1b[?2004h\x1b[?1hhello\x1b[3;4H");

        let data = snapshot(&original, None);
        let mut copy = term(20, 5);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut copy, &data);

        assert!(copy.mode().contains(TermMode::ALT_SCREEN | TermMode::BRACKETED_PASTE | TermMode::APP_CURSOR));
        assert_eq!(screen(&copy), screen(&original));
        assert_eq!(copy.grid().cursor.point, original.grid().cursor.point);
    }

    #[test]
    fn clear_keeps_the_cursor_line_on_top() {
        let mut term = term(20, 3);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"one\r\ntwo\r\nthree\r\nfour\r\n$ ls\x1b[3");
        assert_eq!(term.grid().history_size(), 2);

        // Halfway through an escape sequence: the clear ends it.
        let data = clear(&term);
        parser.advance(&mut term, &data);

        assert_eq!(screen(&term), vec!["$ ls", "", ""]);
        assert_eq!(term.grid().history_size(), 0);
        assert_eq!(term.grid().cursor.point, alacritty_terminal::index::Point::new(Line(0), Column(4)));
    }
}
