//! The Outline panel: the constants, interfaces, classes with their methods
//! and functions of the file in front, from its language server (in
//! Markdown, its headings), each under what it's a member of. Never a field,
//! nor anything inside a function or a variable: no locals or closures. The
//! icons at the top show or hide each of the four groups. They're listed by
//! group, in the icons' order, and each group in the file's; or, chosen in
//! its right-click, all in the file's order. The
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
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use crate::menu::PanelItems as _;
use proto::LspSymbol;

use crate::{
    config::{Config, OutlineGroup as Group, UiText},
    symbol_picker::kind_icon,
};

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

impl Group {
    const ALL: [Self; 4] = [Group::Constants, Group::Interfaces, Group::Classes, Group::Functions];

    /// The kind its icon is drawn as, and its tooltip.
    fn icon(self) -> (u32, &'static str) {
        match self {
            Group::Constants => (14, "Constants"),
            Group::Interfaces => (11, "Interfaces and Types"),
            Group::Classes => (5, "Classes and Methods"),
            Group::Functions => (12, "Functions"),
        }
    }

    /// The group of a symbol of LSP `kind`: none for those always shown (a
    /// module, a heading…).
    fn of(kind: u32) -> Option<Self> {
        match kind {
            // Constant, Variable, Array.
            13 | 14 | 18 => Some(Group::Constants),
            // Interface, TypeParameter, Enum.
            10 | 11 | 26 => Some(Group::Interfaces),
            // Class, Struct, Object (a Rust `impl`), Method, Constructor.
            5 | 23 | 19 | 6 | 9 => Some(Group::Classes),
            12 => Some(Group::Functions),
            _ => None,
        }
    }
}

/// Property, Field, Key, EnumMember: never in the outline.
fn is_field(kind: u32) -> bool {
    matches!(kind, 7 | 8 | 20 | 22)
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
    /// The last symbols of the files seen lately: going back to one shows
    /// them at once, while its server is asked again.
    seen: HashMap<PathBuf, Rc<[LspSymbol]>>,
}

/// Files whose symbols `seen` keeps.
const SEEN: usize = 32;

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
            seen: HashMap::new(),
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
            self.state = match self.seen.get(path) {
                Some(symbols) => State::Ready(symbols.clone()),
                None => State::Loading,
            };
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
            (Ok(symbols), _) => {
                let symbols: Rc<[LspSymbol]> = symbols.into_iter().filter(|symbol| !symbol.local).collect();
                if self.seen.len() >= SEEN && !self.seen.contains_key(path) {
                    self.seen.clear();
                }
                self.seen.insert(path.to_path_buf(), symbols.clone());
                State::Ready(symbols)
            }
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

    /// Lists the symbols in the file's order, or by group.
    fn set_file_order(&mut self, file_order: bool, cx: &mut Context<Self>) {
        Config::update(cx, |config| config.outline_file_order = file_order);
        self.rebuild(cx);
    }

    /// Shows the symbols of `group`, or hides them.
    fn toggle_group(&mut self, group: Group, cx: &mut Context<Self>) {
        Config::update(cx, |config| {
            let hidden = &mut config.outline_hidden;
            match hidden.iter().position(|hidden| *hidden == group) {
                Some(ix) => _ = hidden.remove(ix),
                None => hidden.push(group),
            }
        });
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
        let config = Config::get(cx);
        let keys = collapse.then(|| {
            rows(symbols, &HashSet::new(), &config.outline_hidden, config.outline_file_order).into_iter().filter(|row| row.parent).map(|row| row.key)
        });
        self.collapsed.insert(path, keys.into_iter().flatten().collect());
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.rows = match &self.state {
            State::Ready(symbols) => {
                let collapsed = self.path.as_ref().and_then(|path| self.collapsed.get(path)).cloned().unwrap_or_default();
                let config = Config::get(cx);
                rows(symbols, &collapsed, &config.outline_hidden, config.outline_file_order).into()
            }
            _ => Rc::new([]),
        };
        self.current = None;
        self.set_current(cx);
    }

    /// The symbol the cursor is in, kept in sight.
    fn set_current(&mut self, cx: &mut Context<Self>) {
        let current = match (&self.state, self.cursor) {
            (State::Ready(symbols), Some(line)) => symbols.iter().rposition(|symbol| symbol.line <= line).and_then(|mut symbol| {
                // Not a row (a field, or inside a collapsed symbol): the one
                // it's in.
                loop {
                    if let Some(row) = self.rows.iter().position(|row| row.symbol == symbol) {
                        return Some(row);
                    }
                    let depth = symbols[symbol].depth;
                    symbol = symbols[..symbol].iter().rposition(|outer| outer.depth < depth)?;
                }
            }),
            _ => None,
        };
        if current != self.current
            && let Some(ix) = current
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        self.current = current;
        cx.notify();
    }
}

/// The order of `symbols` (in the file's, each after the one it's in) by
/// group: at each level the constants, then the interfaces, the classes, the
/// functions and the rest, each in the file's order and with what's in it.
fn by_group(symbols: &[LspSymbol]) -> Vec<usize> {
    fn level(symbols: &[LspSymbol], range: std::ops::Range<usize>, order: &mut Vec<usize>) {
        let mut subtrees = Vec::new();
        let mut start = range.start;
        while start < range.end {
            let depth = symbols[start].depth;
            let end = (start + 1..range.end).find(|ix| symbols[*ix].depth <= depth).unwrap_or(range.end);
            subtrees.push(start..end);
            start = end;
        }
        let rank = |kind| Group::of(kind).map_or(Group::ALL.len(), |group| Group::ALL.iter().position(|other| *other == group).unwrap_or(0));
        subtrees.sort_by_key(|subtree| rank(symbols[subtree.start].kind));
        for subtree in subtrees {
            order.push(subtree.start);
            level(symbols, subtree.start + 1..subtree.end, order);
        }
    }
    let mut order = Vec::with_capacity(symbols.len());
    level(symbols, 0..symbols.len(), &mut order);
    order
}

/// The rows of `symbols` (by group, or in the order of the file with
/// `file_order`; each after the one it's in): no fields, nor those of a group
/// in `hidden`, nor those inside the symbols whose key is in `collapsed`. A
/// symbol inside one of a group is of that group: a class's methods come and
/// go with it.
fn rows(symbols: &[LspSymbol], collapsed: &HashSet<Rc<str>>, hidden: &[Group], file_order: bool) -> Vec<Row> {
    let order = match file_order {
        true => (0..symbols.len()).collect(),
        false => by_group(symbols),
    };
    // The symbols of the outline, with their keys.
    let mut shown: Vec<(usize, Rc<str>)> = Vec::new();
    // The names and groups of the symbols the next one may be in, outermost
    // first.
    let mut path: Vec<(&str, Option<Group>)> = Vec::new();
    // Inside a hidden symbol of this depth: hidden.
    let mut hidden_below: Option<u32> = None;
    for ix in order {
        let symbol = &symbols[ix];
        path.truncate(symbol.depth as usize);
        let group = path.iter().rev().find_map(|(_, group)| *group).or(Group::of(symbol.kind));
        path.push((&symbol.name, group));
        if hidden_below.is_some_and(|depth| symbol.depth > depth) {
            continue;
        }
        hidden_below = None;
        if is_field(symbol.kind) || group.is_some_and(|group| hidden.contains(&group)) {
            hidden_below = Some(symbol.depth);
            continue;
        }
        let names: Vec<&str> = path.iter().map(|(name, _)| *name).collect();
        shown.push((ix, names.join("\u{1f}").into()));
    }
    let mut rows = Vec::new();
    // Inside a collapsed symbol of this depth: hidden.
    let mut collapsed_below: Option<u32> = None;
    for (ix, (symbol, key)) in shown.iter().enumerate() {
        let depth = symbols[*symbol].depth;
        if collapsed_below.is_some_and(|below| depth > below) {
            continue;
        }
        let parent = shown.get(ix + 1).is_some_and(|(next, _)| symbols[*next].depth > depth);
        let is_collapsed = parent && collapsed.contains(key);
        collapsed_below = is_collapsed.then_some(depth);
        rows.push(Row { symbol: *symbol, parent, collapsed: is_collapsed, key: key.clone() });
    }
    rows
}

impl OutlinePanel {
    /// The icons that show or hide each group.
    fn render_groups(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = Group::ALL.map(|group| {
            let (kind, tip) = group.icon();
            let (icon, color) = kind_icon(kind, cx);
            let theme = cx.theme();
            let on = !Config::get(cx).outline_hidden.contains(&group);
            div()
                .id(tip)
                .p_1()
                .rounded(theme.radius)
                .when(on, |el| el.bg(theme.sidebar_accent))
                .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
                .child(svg().path(icon).size(px(14.)).text_color(if on { color } else { theme.muted_foreground }))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                .on_click(cx.listener(move |outline, _, _, cx| outline.toggle_group(group, cx)))
        });
        h_flex().px_2().py_1().gap_0p5().children(groups)
    }
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
                        .when(current == Some(ix), |el| el.bg(crate::app::selected_row(cx)))
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
        let groups = self.render_groups(cx);
        v_flex()
            .id("outline")
            .size_full()
            .child(groups)
            .child(div().flex_1().min_h_0().child(list))
            .context_menu({
            let outline = cx.entity().downgrade();
            move |menu, window, cx| {
                let item = |label: &'static str, collapse: bool| {
                    let outline = outline.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        outline.update(cx, |outline, cx| outline.collapse_all(collapse, cx)).ok();
                    })
                };
                let file_order = Config::get(cx).outline_file_order;
                let order = |label: &'static str, file: bool| {
                    let outline = outline.clone();
                    PopupMenuItem::new(label).checked(file_order == file).on_click(move |_, _, cx| {
                        outline.update(cx, |outline, cx| outline.set_file_order(file, cx)).ok();
                    })
                };
                menu.item(order("Sort by Kind", false))
                    .item(order("Sort by Position", true))
                    .separator()
                    .item(item("Collapse All", true))
                    .item(item("Expand All", false))
                    .separator()
                    .panel_items(crate::menu::hide_panel(), window, cx)
            }
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, rc::Rc};

    use proto::LspSymbol;

    use super::{Group, rows};

    fn symbol(name: &str, kind: u32, depth: u32) -> LspSymbol {
        LspSymbol { path: "main.ts".into(), name: name.into(), kind, container: None, line: 0, column: 0, depth, local: false }
    }

    fn keys(symbols: &[LspSymbol], hidden: &[Group]) -> Vec<String> {
        rows(symbols, &HashSet::new(), hidden, true).into_iter().map(|row| row.key.replace('\u{1f}', "/")).collect()
    }

    #[test]
    fn fields_never_show_and_each_group_hides() {
        let symbols = [
            symbol("LIMIT", 14, 0),
            symbol("TimedWindow", 11, 0),
            symbol("start", 7, 1),
            symbol("end", 7, 1),
            symbol("Sale", 5, 0),
            symbol("total", 8, 1),
            symbol("cancel", 6, 1),
            symbol("check", 12, 0),
        ];
        assert_eq!(keys(&symbols, &[]), ["LIMIT", "TimedWindow", "Sale", "Sale/cancel", "check"]);
        assert_eq!(keys(&symbols, &[Group::Constants]), ["TimedWindow", "Sale", "Sale/cancel", "check"]);
        assert_eq!(keys(&symbols, &[Group::Interfaces]), ["LIMIT", "Sale", "Sale/cancel", "check"]);
        assert_eq!(keys(&symbols, &[Group::Classes]), ["LIMIT", "TimedWindow", "check"]);
        // A method goes with its class, not with the functions.
        assert_eq!(keys(&symbols, &[Group::Functions]), ["LIMIT", "TimedWindow", "Sale", "Sale/cancel"]);
        // An interface with only fields has nothing to collapse.
        let rows = rows(&symbols, &HashSet::new(), &[], true);
        assert!(!rows[1].parent);
    }

    #[test]
    fn a_collapsed_symbol_hides_what_is_inside() {
        let symbols = [
            symbol("Sale", 5, 0),
            symbol("cancel", 5, 1),
            symbol("Line", 5, 1),
            symbol("total", 5, 2),
            symbol("Sale", 5, 0),
            symbol("main", 5, 0),
        ];
        let shown = |collapsed: &[&str]| {
            let collapsed: HashSet<Rc<str>> = collapsed.iter().map(|key| Rc::from(key.replace('/', "\u{1f}"))).collect();
            rows(&symbols, &collapsed, &[], true)
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

    #[test]
    fn by_kind_each_group_in_the_file_s_order() {
        let symbols = [
            symbol("scl", 13, 0),
            symbol("DialogType", 11, 0),
            symbol("open", 12, 0),
            symbol("AUTO", 14, 0),
            symbol("Sale", 5, 0),
            symbol("total", 6, 1),
            symbol("Line", 5, 1),
            symbol("cancel", 6, 1),
            symbol("Refs", 11, 0),
        ];
        let by_kind: Vec<String> = rows(&symbols, &HashSet::new(), &[], false).into_iter().map(|row| row.key.replace('\u{1f}', "/")).collect();
        assert_eq!(by_kind, ["scl", "AUTO", "DialogType", "Refs", "Sale", "Sale/total", "Sale/Line", "Sale/cancel", "open"]);
        assert_eq!(keys(&symbols, &[])[..3], ["scl", "DialogType", "open"]);
    }
}
