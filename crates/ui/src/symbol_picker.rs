//! Cmd-Shift-O: the symbols of the file (functions, classes, methods…), from
//! its language server or, in Markdown, its headings. Filtered as you type,
//! like VS Code; the editor shows the selected one while you choose.
//! Cmd-Shift-T: those of the whole workspace, asked again as you type.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use proto::LspSymbol;

use crate::config::UiText;

const ROW: f32 = 26.;
/// Rows shown without scrolling.
const ROWS: usize = 14;

pub enum SymbolPickerEvent {
    /// The selection moved to this symbol of the file: the editor shows it.
    Preview(LspSymbol),
    /// Of the workspace: what's typed changed, its symbols are wanted.
    Query(String),
    Pick(LspSymbol),
    /// Esc: back to where the cursor was.
    Dismiss,
    /// Click outside: focus stays on what was clicked.
    Close,
}

/// What there is to choose from.
enum Symbols {
    Loading,
    Ready(Rc<[LspSymbol]>),
    /// Why there are none: no server, or it failed.
    Failed(SharedString),
}

pub struct SymbolPicker {
    /// Of the workspace at this folder (else, of the file): their paths are shown from it.
    workspace: Option<PathBuf>,
    input: Entity<InputState>,
    symbols: Symbols,
    /// The symbols that match, best first, and the characters of their name that matched.
    matches: Rc<[(usize, Vec<u32>)]>,
    selected: usize,
    scroll: UniformListScrollHandle,
    _subscription: Subscription,
}

impl EventEmitter<SymbolPickerEvent> for SymbolPicker {}

impl SymbolPicker {
    pub fn new(workspace: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = if workspace.is_some() { "Go to Symbol in Workspace…" } else { "Go to Symbol in File…" };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            // Meanwhile, what was found for the previous text is narrowed.
            InputEvent::Change => {
                this.refilter(cx);
                match this.workspace {
                    Some(_) => {
                        let query = this.query(cx);
                        if this.matches.is_empty() && !query.is_empty() {
                            this.symbols = Symbols::Loading;
                        }
                        cx.emit(SymbolPickerEvent::Query(query));
                    }
                    None => this.preview(cx),
                }
            }
            InputEvent::PressEnter { .. } => {
                if let Some(symbol) = this.selected_symbol() {
                    cx.emit(SymbolPickerEvent::Pick(symbol.clone()));
                }
            }
            _ => {}
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        Self {
            // Of the workspace, nothing to show until something's typed.
            symbols: if workspace.is_some() { Symbols::Ready(Rc::new([])) } else { Symbols::Loading },
            workspace,
            input,
            matches: Rc::new([]),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            _subscription: subscription,
        }
    }

    /// The symbols, or why there are none. Of the workspace, those that
    /// match `query`: ignored if something else is typed by now.
    pub fn set_symbols(&mut self, query: Option<&str>, symbols: Result<Vec<LspSymbol>, SharedString>, cx: &mut Context<Self>) {
        if query.is_some_and(|query| query != self.query(cx)) {
            return;
        }
        self.symbols = match symbols {
            Ok(symbols) => Symbols::Ready(symbols.into()),
            Err(error) => Symbols::Failed(error),
        };
        self.refilter(cx);
        // Something typed while they were coming: show the best match.
        if self.workspace.is_none() && !self.query(cx).is_empty() {
            self.preview(cx);
        }
    }

    /// What's typed, without VS Code's `@` or `#`.
    pub fn query(&self, cx: &App) -> String {
        let value = self.input.read(cx).value();
        value.trim().trim_start_matches(['@', '#']).trim().to_string()
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.matches = match &self.symbols {
            Symbols::Ready(_) if self.workspace.is_some() && query.is_empty() => Rc::new([]),
            Symbols::Ready(symbols) => filter(symbols, &query).into(),
            _ => Rc::new([]),
        };
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn selected_symbol(&self) -> Option<&LspSymbol> {
        let Symbols::Ready(symbols) = &self.symbols else {
            return None;
        };
        self.matches.get(self.selected).map(|(ix, _)| &symbols[*ix])
    }

    fn preview(&self, cx: &mut Context<Self>) {
        if let Some(symbol) = self.selected_symbol() {
            cx.emit(SymbolPickerEvent::Preview(symbol.clone()));
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.scroll.scroll_to_item(self.selected, ScrollStrategy::Nearest);
        if self.workspace.is_none() {
            self.preview(cx);
        }
        cx.notify();
    }
}

/// The symbols whose name matches `query`, best first and, when equal, in
/// the order of the file; with nothing typed, all of them in that order.
fn filter(symbols: &[LspSymbol], query: &str) -> Vec<(usize, Vec<u32>)> {
    if query.is_empty() {
        return (0..symbols.len()).map(|ix| (ix, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, Vec<u32>)> = symbols
        .iter()
        .enumerate()
        .filter_map(|(ix, symbol)| {
            let mut indices = Vec::new();
            let score = pattern.indices(Utf32Str::new(&symbol.name, &mut buf), &mut matcher, &mut indices)?;
            indices.sort_unstable();
            indices.dedup();
            Some((score, ix, indices))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, ix, indices)| (ix, indices)).collect()
}

/// Byte ranges of `text` for the characters at `indices` (sorted), joined
/// when they're next to each other.
fn char_ranges(text: &str, indices: &[u32]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut wanted = indices.iter().peekable();
    for (ix, (byte, ch)) in text.char_indices().enumerate() {
        if wanted.peek().is_none() {
            break;
        }
        if wanted.peek() == Some(&&(ix as u32)) {
            wanted.next();
            let end = byte + ch.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == byte => last.end = end,
                _ => ranges.push(byte..end),
            }
        }
    }
    ranges
}

/// The headings of Markdown file `path`, as symbols: each inside the one above it.
pub fn markdown_symbols(path: &Path, text: &str) -> Vec<LspSymbol> {
    let mut symbols = Vec::new();
    // The headings that contain the next one: their level and name.
    let mut open: Vec<(usize, String)> = Vec::new();
    let mut fence: Option<&str> = None;
    for (line, text) in text.lines().enumerate() {
        let trimmed = text.trim_start();
        if let Some(marker) = ["```", "~~~"].into_iter().find(|marker| trimmed.starts_with(marker)) {
            match fence {
                Some(open) if open == marker => fence = None,
                None => fence = Some(marker),
                _ => {}
            }
            continue;
        }
        if fence.is_some() || text.len() - trimmed.len() > 3 {
            continue;
        }
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        let rest = &trimmed[level..];
        if !(1..=6).contains(&level) || !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
            continue;
        }
        let name = rest.trim().trim_end_matches('#').trim_end().to_string();
        if name.is_empty() {
            continue;
        }
        open.retain(|(open, _)| *open < level);
        symbols.push(LspSymbol {
            path: path.to_path_buf(),
            name: name.clone(),
            // String, as VS Code's.
            kind: 15,
            container: open.last().map(|(_, name)| name.clone()),
            line: line as u32,
            column: (text.chars().count() - trimmed.chars().count()) as u32,
        });
        open.push((level, name));
    }
    symbols
}

/// The icon and color of an LSP `SymbolKind`, after VS Code's.
fn kind_icon(kind: u32, cx: &App) -> (&'static str, Hsla) {
    let theme = cx.theme();
    match kind {
        // Method, Constructor, Function.
        6 | 9 | 12 => ("icons/symbol-function.svg", theme.magenta),
        // Property, Field, Key.
        7 | 8 | 20 => ("icons/symbol-field.svg", theme.blue),
        // Variable, Object, Array.
        13 | 18 | 19 => ("icons/symbol-variable.svg", theme.blue),
        14 => ("icons/symbol-constant.svg", theme.blue),
        // Class, Struct.
        5 | 23 => ("icons/symbol-class.svg", theme.yellow),
        // Interface, TypeParameter.
        11 | 26 => ("icons/symbol-type.svg", theme.cyan),
        // Enum, EnumMember.
        10 | 22 => ("icons/symbol-enum.svg", theme.yellow),
        // Module, Namespace, Package.
        2..=4 => ("icons/symbol-module.svg", theme.muted_foreground),
        1 => ("icons/symbol-file.svg", theme.muted_foreground),
        // String (Markdown headings).
        15 => ("icons/symbol-heading.svg", theme.muted_foreground),
        24 => ("icons/symbol-event.svg", theme.yellow),
        _ => ("icons/symbol-type.svg", theme.muted_foreground),
    }
}

impl Render for SymbolPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let typed = !self.query(cx).is_empty();
        let status = match &self.symbols {
            Symbols::Ready(_) if self.workspace.is_some() && !typed => Some("Type the name of a symbol".into()),
            Symbols::Loading => Some(SharedString::from("Loading symbols…")),
            Symbols::Failed(error) => Some(error.clone()),
            Symbols::Ready(symbols) if symbols.is_empty() && self.workspace.is_none() => Some("No symbols in this file".into()),
            Symbols::Ready(_) if self.matches.is_empty() => Some("No matching symbols".into()),
            Symbols::Ready(_) => None,
        };
        let symbols = match &self.symbols {
            Symbols::Ready(symbols) => symbols.clone(),
            _ => Rc::new([]),
        };
        let matches = self.matches.clone();
        let selected = self.selected;
        let count = matches.len();
        let workspace = self.workspace.clone();
        let view = cx.entity();
        v_flex()
            .id("symbol-picker")
            .w(px(600.))
            .p_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_ui(cx)
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(SymbolPickerEvent::Close)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                "up" => {
                    this.move_selection(-1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    this.move_selection(1, cx);
                    cx.stop_propagation();
                }
                "pageup" => {
                    this.move_selection(-(ROWS as isize), cx);
                    cx.stop_propagation();
                }
                "pagedown" => {
                    this.move_selection(ROWS as isize, cx);
                    cx.stop_propagation();
                }
                "escape" => {
                    cx.emit(SymbolPickerEvent::Dismiss);
                    cx.stop_propagation();
                }
                _ => {}
            }))
            .child(Input::new(&self.input))
            .children(status.map(|status| {
                div().px_2().py_1().text_color(theme.muted_foreground).child(status)
            }))
            .when(count > 0, |el| {
                el.child(
                    uniform_list("symbol-picker-results", count, move |range, _, cx| {
                        let theme = cx.theme();
                        let highlight = HighlightStyle {
                            color: Some(theme.blue),
                            font_weight: Some(FontWeight::BOLD),
                            ..Default::default()
                        };
                        range
                            .map(|ix| {
                                let (symbol, indices) = &matches[ix];
                                let symbol = &symbols[*symbol];
                                let (icon, color) = kind_icon(symbol.kind, cx);
                                let highlights: Vec<_> = char_ranges(&symbol.name, indices)
                                    .into_iter()
                                    .map(|range| (range, highlight))
                                    .collect();
                                let picked = symbol.clone();
                                // Of the workspace, also its file.
                                let file = workspace.as_ref().map(|root| {
                                    symbol.path.strip_prefix(root).unwrap_or(&symbol.path).to_string_lossy().into_owned()
                                });
                                let description: Vec<String> = symbol.container.iter().cloned().chain(file).collect();
                                h_flex()
                                    .id(("symbol-row", ix))
                                    .w_full()
                                    .h(px(ROW))
                                    .px_2()
                                    .gap_2()
                                    .rounded(theme.radius)
                                    .when(ix == selected, |el| el.bg(theme.accent))
                                    .hover(|style| style.bg(theme.accent.opacity(0.6)))
                                    .child(svg().path(icon).size(px(14.)).flex_none().text_color(color))
                                    .child(
                                        div()
                                            .flex_none()
                                            .whitespace_nowrap()
                                            .child(StyledText::new(symbol.name.clone()).with_highlights(highlights)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_ui_small(cx)
                                            .text_color(theme.muted_foreground)
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(description.join("  ·  ")),
                                    )
                                    .when(ix == 0, |el| {
                                        el.child(
                                            div()
                                                .ml_auto()
                                                .flex_none()
                                                .text_ui_small(cx)
                                                .text_color(theme.muted_foreground)
                                                .child(format!("symbols ({count})")),
                                        )
                                    })
                                    .on_click({
                                        let view = view.clone();
                                        move |_, _, cx| {
                                            view.update(cx, |_, cx| cx.emit(SymbolPickerEvent::Pick(picked.clone())))
                                        }
                                    })
                            })
                            .collect()
                    })
                    .track_scroll(&self.scroll)
                    .h(px(ROW * count.min(ROWS) as f32)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use proto::LspSymbol;

    use super::{char_ranges, filter, markdown_symbols};

    fn symbol(name: &str) -> LspSymbol {
        LspSymbol { path: "main.ts".into(), name: name.into(), kind: 12, container: None, line: 0, column: 0 }
    }

    #[test]
    fn fuzzy_best_first_then_file_order() {
        let symbols: Vec<LspSymbol> =
            ["install", "onCancelingSales", "onCronTick", "initRoutes"].into_iter().map(symbol).collect();
        let names = |query| filter(&symbols, query).into_iter().map(|(ix, _)| symbols[ix].name.clone()).collect::<Vec<_>>();
        // Equally good: in the order of the file.
        assert_eq!(names("onc"), ["onCancelingSales", "onCronTick"]);
        assert_eq!(names("tick"), ["onCronTick"]);
        assert_eq!(names("ins")[0], "install");
        assert_eq!(names("zzz"), Vec::<String>::new());
        assert_eq!(names(""), ["install", "onCancelingSales", "onCronTick", "initRoutes"]);
    }

    #[test]
    fn highlights_join_neighbors() {
        assert_eq!(char_ranges("onCronTick", &[0, 1, 2, 6]), [0..3, 6..7]);
        assert_eq!(char_ranges("ñandú", &[1, 4]), [2..3, 5..7]);
    }

    #[test]
    fn markdown_headings_nest_and_skip_code() {
        let text = "# Title\n\nintro\n\n## One ##\n```\n# not a heading\n```\n### Deep\n## Two\n#hashtag\n";
        let found: Vec<_> =
            markdown_symbols(Path::new("README.md"), text).into_iter().map(|s| (s.name, s.container, s.line)).collect();
        assert_eq!(
            found,
            [
                ("Title".to_string(), None, 0),
                ("One".to_string(), Some("Title".to_string()), 4),
                ("Deep".to_string(), Some("One".to_string()), 8),
                ("Two".to_string(), Some("Title".to_string()), 9),
            ],
        );
    }
}
