//! The Outline panel: the classes, functions, methods, constants, enums and
//! globals of the file in front, from its language server (in Markdown, its
//! headings), each under what it's a member of. Nothing inside a function or
//! a variable: no locals, closures or fields of an object literal. The
//! workspace keeps it on the file in front (see `Workspace::sync_outline`);
//! the symbol the cursor is in is highlighted, and a click goes to one. Those
//! with members (a class, an `impl`…) collapse with their chevron.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::LspSymbol;

use crate::{config::UiText, symbol_picker::kind_icon};

pub enum OutlineEvent {
    Pick(LspSymbol),
}

enum State {
    /// No file in front, or one that isn't text.
    NoFile,
    Loading,
    Ready(Rc<[LspSymbol]>),
    /// Why there are none: no server, or it failed.
    Failed(SharedString),
}

/// A symbol shown: not inside a collapsed one.
struct Row {
    /// In the symbols.
    symbol: usize,
    /// It has members, and shows a chevron.
    parent: bool,
    collapsed: bool,
    /// Its name after those of the symbols it's in: what stays collapsed.
    key: Rc<str>,
}

pub struct OutlinePanel {
    /// The file the symbols are of.
    path: Option<PathBuf>,
    state: State,
    rows: Rc<[Row]>,
    /// The symbols collapsed in each file, by `Row::key`: they stay so while
    /// the file is edited, and when it's in front again.
    collapsed: HashMap<PathBuf, HashSet<Rc<str>>>,
    /// The line of the cursor, and the row of the symbol it's in: the last
    /// one that starts before it, or the collapsed one that holds it.
    cursor: Option<u32>,
    current: Option<usize>,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<OutlineEvent> for OutlinePanel {}

impl OutlinePanel {
    pub fn new() -> Self {
        Self {
            path: None,
            state: State::NoFile,
            rows: Rc::new([]),
            collapsed: HashMap::new(),
            cursor: None,
            current: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// No file in front.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.path.take().is_some() || !matches!(self.state, State::NoFile) {
            self.state = State::NoFile;
            self.rebuild(cx);
        }
    }

    /// Its symbols are being asked for `path`: those of another file go
    /// meanwhile, those of the same one stay until they come.
    pub fn loading(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.path.as_deref() != Some(path) {
            self.path = Some(path.to_path_buf());
            self.state = State::Loading;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            self.rebuild(cx);
        }
    }

    /// The symbols of `path`, or why there are none; ignored if another file
    /// is in front by now. Failing again keeps the ones it had (a server that
    /// can't answer while the file is half typed).
    pub fn set_symbols(&mut self, path: &Path, symbols: Result<Vec<LspSymbol>, SharedString>, cx: &mut Context<Self>) {
        if self.path.as_deref() != Some(path) {
            return;
        }
        self.state = match (symbols, &self.state) {
            (Ok(symbols), _) => State::Ready(symbols.into_iter().filter(|symbol| !symbol.local).collect()),
            (Err(_), State::Ready(symbols)) => State::Ready(symbols.clone()),
            (Err(error), _) => State::Failed(error),
        };
        self.rebuild(cx);
    }

    pub fn set_cursor(&mut self, line: Option<u32>, cx: &mut Context<Self>) {
        if self.cursor != line {
            self.cursor = line;
            self.set_current(cx);
        }
    }

    /// Collapses the symbol of `key`, or expands it.
    fn toggle(&mut self, key: Rc<str>, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let collapsed = self.collapsed.entry(path).or_default();
        if !collapsed.remove(&key) {
            collapsed.insert(key);
        }
        self.rebuild(cx);
    }

    /// Collapses every symbol with members, or expands them all.
    fn collapse_all(&mut self, collapse: bool, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let State::Ready(symbols) = &self.state else {
            return;
        };
        let keys = collapse.then(|| rows(symbols, &HashSet::new()).into_iter().filter(|row| row.parent).map(|row| row.key));
        self.collapsed.insert(path, keys.into_iter().flatten().collect());
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rows = match &self.state {
            State::Ready(symbols) => {
                let collapsed = self.path.as_ref().and_then(|path| self.collapsed.get(path)).cloned().unwrap_or_default();
                rows(symbols, &collapsed).into()
            }
            _ => Rc::new([]),
        };
        self.current = None;
        self.set_current(cx);
    }

    /// The symbol the cursor is in, kept in sight.
    fn set_current(&mut self, cx: &mut Context<Self>) {
        let current = match (&self.state, self.cursor) {
            (State::Ready(symbols), Some(line)) => symbols.iter().rposition(|symbol| symbol.line <= line),
            _ => None,
        };
        // Inside a collapsed symbol, that one: the last row before it.
        let current = current.and_then(|symbol| self.rows.iter().rposition(|row| row.symbol <= symbol));
        if current != self.current
            && let Some(ix) = current
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.current = current;
        cx.notify();
    }
}

/// The rows of `symbols` (in the order of the file, each after the one it's
/// in) but those inside the symbols whose key is in `collapsed`.
fn rows(symbols: &[LspSymbol], collapsed: &HashSet<Rc<str>>) -> Vec<Row> {
    let mut rows = Vec::new();
    // The names of the symbols the next one may be in, outermost first.
    let mut path: Vec<&str> = Vec::new();
    // Inside a collapsed symbol of this depth: hidden.
    let mut hidden_below: Option<u32> = None;
    for (ix, symbol) in symbols.iter().enumerate() {
        path.truncate(symbol.depth as usize);
        path.push(&symbol.name);
        if hidden_below.is_some_and(|depth| symbol.depth > depth) {
            continue;
        }
        let key: Rc<str> = path.join("\u{1f}").into();
        let parent = symbols.get(ix + 1).is_some_and(|next| next.depth > symbol.depth);
        let is_collapsed = parent && collapsed.contains(&key);
        hidden_below = is_collapsed.then_some(symbol.depth);
        rows.push(Row { symbol: ix, parent, collapsed: is_collapsed, key });
    }
    rows
}

impl Render for OutlinePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let status = match &self.state {
            State::NoFile => Some(SharedString::from("No file open")),
            State::Loading => Some("Loading symbols…".into()),
            State::Failed(error) => Some(error.clone()),
            State::Ready(symbols) if symbols.is_empty() => Some("No symbols in this file".into()),
            State::Ready(_) => None,
        };
        if let Some(status) = status {
            return div()
                .size_full()
                .px_3()
                .py_2()
                .text_ui(cx)
                .text_color(theme.muted_foreground)
                .child(status)
                .into_any_element();
        }
        let State::Ready(symbols) = &self.state else {
            unreachable!("a status otherwise");
        };
        let (symbols, rows) = (symbols.clone(), self.rows.clone());
        let current = self.current;
        let view = cx.entity();
        let list = uniform_list("outline", rows.len(), move |range, _, cx| {
            let theme = cx.theme();
            range
                .map(|ix| {
                    let row = &rows[ix];
                    let symbol = &symbols[row.symbol];
                    let (icon, color) = kind_icon(symbol.kind, cx);
                    let chevron = row.parent.then(|| {
                        let (view, key) = (view.clone(), row.key.clone());
                        let icon = if row.collapsed { "icons/tree-chevron-right.svg" } else { "icons/tree-chevron-down.svg" };
                        div()
                            .id("chevron")
                            .child(svg().path(icon).size(px(14.)).text_color(theme.muted_foreground))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                view.update(cx, |outline, cx| outline.toggle(key.clone(), cx));
                            })
                    });
                    let picked = symbol.clone();
                    let view = view.clone();
                    h_flex()
                        .id(ix)
                        .h(px(24.))
                        .w_full()
                        .gap_1()
                        .pl(px(4. + symbol.depth as f32 * 14.))
                        .pr_2()
                        .text_ui(cx)
                        .text_color(theme.sidebar_foreground)
                        .when(current == Some(ix), |el| el.bg(theme.sidebar_accent))
                        .when(current != Some(ix), |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                        .child(div().w(px(14.)).flex_none().children(chevron))
                        .child(svg().path(icon).size(px(14.)).flex_none().text_color(color))
                        .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(symbol.name.clone()))
                        .on_click(move |_, _, cx| view.update(cx, |_, cx| cx.emit(OutlineEvent::Pick(picked.clone()))))
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();
        div()
            .id("outline")
            .size_full()
            .child(list)
            .context_menu({
            let outline = cx.entity().downgrade();
            move |menu, _, _| {
                let item = |label: &'static str, collapse: bool| {
                    let outline = outline.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        outline.update(cx, |outline, cx| outline.collapse_all(collapse, cx)).ok();
                    })
                };
                menu.item(item("Collapse All", true)).item(item("Expand All", false)).separator().item(crate::menu::hide_panel())
            }
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, rc::Rc};

    use proto::LspSymbol;

    use super::rows;

    #[test]
    fn a_collapsed_symbol_hides_what_is_inside() {
        let symbol = |name: &str, depth: u32| LspSymbol {
            path: "main.ts".into(),
            name: name.into(),
            kind: 5,
            container: None,
            line: 0,
            column: 0,
            depth,
            local: false,
        };
        let symbols =
            [symbol("Sale", 0), symbol("cancel", 1), symbol("Line", 1), symbol("total", 2), symbol("Sale", 0), symbol("main", 0)];
        let shown = |collapsed: &[&str]| {
            let collapsed: HashSet<Rc<str>> = collapsed.iter().map(|key| Rc::from(key.replace('/', "\u{1f}"))).collect();
            rows(&symbols, &collapsed)
                .into_iter()
                .map(|row| format!("{}{}", if row.collapsed { "+" } else { "" }, &*row.key).replace('\u{1f}', "/"))
                .collect::<Vec<_>>()
        };
        assert_eq!(shown(&[]), ["Sale", "Sale/cancel", "Sale/Line", "Sale/Line/total", "Sale", "main"]);
        assert_eq!(shown(&["Sale/Line"]), ["Sale", "Sale/cancel", "+Sale/Line", "Sale", "main"]);
        assert_eq!(shown(&["Sale"]), ["+Sale", "Sale", "main"]);
        // Without members, there's nothing to collapse.
        assert_eq!(shown(&["main"]), shown(&[]));
    }
}
