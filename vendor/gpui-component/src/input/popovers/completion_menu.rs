use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, AppContext, Context, DismissEvent, Empty, Entity, EventEmitter,
    Half as _, HighlightStyle, Hsla, InteractiveElement as _, IntoElement, ParentElement, Pixels, Point,
    Render, RenderOnce, SharedString, Styled, StyledText, Subscription, WeakEntity, Window,
    deferred, div, prelude::FluentBuilder, px, relative,
};
use gpui_base::input::RopeExt as _;
use lsp_types::{CompletionItem, CompletionItemKind};

const MAX_MENU_HEIGHT: Pixels = px(240.);
const POPOVER_GAP: Pixels = px(4.);

use crate::{
    ActiveTheme, Icon, IndexPath, Selectable, actions, h_flex,
    input::{
        self, EditorState,
        popovers::{editor_popover, render_markdown},
    },
    list::{List, ListDelegate, ListEvent, ListState},
};

struct ContextMenuDelegate {
    query: SharedString,
    /// (sik) Weak: the menu owns this list, and a strong handle back would
    /// keep both alive after the menu goes.
    menu: WeakEntity<CompletionMenu>,
    /// (sik) The menu's editor, kept here so resolving doesn't read the menu.
    editor: WeakEntity<EditorState>,
    items: Vec<Rc<CompletionItem>>,
    selected_ix: usize,
    /// (sik) Items already asked for their detail and documentation.
    resolved: std::collections::HashSet<usize>,
}

impl ContextMenuDelegate {
    fn set_items(&mut self, items: Vec<CompletionItem>) {
        self.items = items.into_iter().map(Rc::new).collect();
        self.selected_ix = 0;
        self.resolved.clear();
    }

    /// (sik) Asks the provider once for the rest of the selected item (the
    /// detail and documentation some servers leave out of the list).
    fn resolve_selected(&mut self, cx: &mut Context<ListState<Self>>) {
        let ix = self.selected_ix;
        let Some(item) = self.items.get(ix).cloned() else {
            return;
        };
        if item.detail.is_some() || item.documentation.is_some() || !self.resolved.insert(ix) {
            return;
        }
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let Some(provider) = editor.read(cx).lsp().completion_provider.clone() else {
            return;
        };
        let task = provider.resolve_completion(&item, cx);
        let menu = self.menu.clone();
        cx.spawn(async move |list, cx| {
            let Ok(resolved) = task.await else {
                return;
            };
            let _ = list.update(cx, |list, cx| {
                let delegate = list.delegate_mut();
                if delegate.items.get(ix).is_some_and(|current| Rc::ptr_eq(current, &item)) {
                    delegate.items[ix] = Rc::new(resolved);
                }
                cx.notify();
            });
            let _ = menu.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    fn selected_item(&self) -> Option<&Rc<CompletionItem>> {
        self.items.get(self.selected_ix)
    }
}

#[derive(IntoElement)]
struct CompletionMenuItem {
    ix: usize,
    item: Rc<CompletionItem>,
    children: Vec<AnyElement>,
    selected: bool,
    highlight_prefix: SharedString,
}

impl CompletionMenuItem {
    fn new(ix: usize, item: Rc<CompletionItem>) -> Self {
        Self {
            ix,
            item,
            children: vec![],
            selected: false,
            highlight_prefix: "".into(),
        }
    }

    fn highlight_prefix(mut self, s: impl Into<SharedString>) -> Self {
        self.highlight_prefix = s.into();
        self
    }
}
impl Selectable for CompletionMenuItem {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl ParentElement for CompletionMenuItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}
impl RenderOnce for CompletionMenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let item = self.item;

        let deprecated = item.deprecated.unwrap_or(false);
        // (sik) The letters of the label that match what's typed, as VS Code.
        let highlight = HighlightStyle {
            color: Some(cx.theme().blue),
            font_weight: Some(gpui::FontWeight::BOLD),
            ..Default::default()
        };
        let highlights = matched(&item.label, &self.highlight_prefix)
            .into_iter()
            .map(|range| (range, highlight))
            .collect::<Vec<_>>();
        let (icon, color) = kind_icon(item.kind, cx);

        h_flex()
            .id(self.ix)
            .gap_2()
            .p_1()
            .text_xs()
            .line_height(relative(1.))
            .rounded(cx.theme().radius.half())
            .when(item.deprecated.unwrap_or(false), |this| this.line_through())
            .hover(|this| this.bg(cx.theme().accent.opacity(0.8)))
            .when(self.selected, |this| {
                this.bg(cx.theme().tokens.accent)
                    .text_color(cx.theme().accent_foreground)
            })
            .child(Icon::default().path(icon).size_3p5().flex_none().text_color(color))
            .child(
                div()
                    .flex_none()
                    .child(StyledText::new(item.label.clone()).with_highlights(highlights)),
            )
            // (sik) The detail only on the selected one, at the right, as VS Code.
            .when_some(item.detail.clone().filter(|_| self.selected), |this, detail| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl_4()
                        .truncate()
                        .text_right()
                        .opacity(0.7)
                        .when(deprecated, |this| this.line_through())
                        .child(detail),
                )
            })
            .children(self.children)
    }
}

/// (sik) Byte ranges of `label` matching `query`: its start if it starts with
/// it, or else each letter of it in order (case ignored).
fn matched(label: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    if label.to_lowercase().starts_with(&query.to_lowercase()) {
        // As many characters as the query has: lowercase may change byte
        // lengths (`İ`), so the query's length in bytes may end mid-character.
        let end = label.char_indices().nth(query.chars().count()).map_or(label.len(), |(ix, _)| ix);
        return vec![0..end];
    }
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut wanted = query.chars().flat_map(char::to_lowercase).peekable();
    for (ix, ch) in label.char_indices() {
        let Some(next) = wanted.peek() else { break };
        if ch.to_lowercase().eq(std::iter::once(*next)) {
            wanted.next();
            let end = ix + ch.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == ix => last.end = end,
                _ => ranges.push(ix..end),
            }
        }
    }
    ranges
}

/// (sik) The icon and color of a `CompletionItemKind`, after VS Code's.
fn kind_icon(kind: Option<CompletionItemKind>, cx: &App) -> (&'static str, Hsla) {
    let theme = cx.theme();
    match kind {
        Some(CompletionItemKind::METHOD | CompletionItemKind::FUNCTION | CompletionItemKind::CONSTRUCTOR) => {
            ("icons/box.svg", theme.magenta)
        }
        Some(CompletionItemKind::FIELD | CompletionItemKind::PROPERTY) => ("icons/tag.svg", theme.blue),
        Some(CompletionItemKind::VARIABLE | CompletionItemKind::VALUE | CompletionItemKind::REFERENCE) => {
            ("icons/variable.svg", theme.blue)
        }
        Some(CompletionItemKind::CONSTANT) => ("icons/pi.svg", theme.blue),
        Some(CompletionItemKind::CLASS | CompletionItemKind::STRUCT) => ("icons/shapes.svg", theme.yellow),
        Some(CompletionItemKind::INTERFACE | CompletionItemKind::TYPE_PARAMETER) => ("icons/type.svg", theme.cyan),
        Some(CompletionItemKind::ENUM | CompletionItemKind::ENUM_MEMBER) => ("icons/list.svg", theme.yellow),
        Some(CompletionItemKind::MODULE) => ("icons/package.svg", theme.muted_foreground),
        Some(CompletionItemKind::KEYWORD | CompletionItemKind::OPERATOR) => ("icons/key.svg", theme.muted_foreground),
        Some(CompletionItemKind::SNIPPET) => ("icons/code.svg", theme.muted_foreground),
        Some(CompletionItemKind::FILE) => ("icons/file.svg", theme.muted_foreground),
        Some(CompletionItemKind::FOLDER) => ("icons/folder.svg", theme.muted_foreground),
        Some(CompletionItemKind::EVENT) => ("icons/zap.svg", theme.yellow),
        _ => ("icons/type.svg", theme.muted_foreground),
    }
}

impl EventEmitter<DismissEvent> for ContextMenuDelegate {}

impl ListDelegate for ContextMenuDelegate {
    type Item = CompletionMenuItem;

    fn items_count(&self, _: usize, _: &gpui::App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: crate::IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let item = self.items.get(ix.row)?;
        Some(CompletionMenuItem::new(ix.row, item.clone()).highlight_prefix(self.query.clone()))
    }

    fn set_selected_index(
        &mut self,
        ix: Option<crate::IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_ix = ix.map(|i| i.row).unwrap_or(0);
        // (sik) Resolved once the current update is over: the selection
        // changes while the editor (arrow keys) or the menu (`show`) is
        // leased, and resolving reads the editor.
        let list = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = list.update(cx, |list, cx| list.delegate_mut().resolve_selected(cx));
        });
        cx.notify();
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(item) = self.selected_item() else {
            return;
        };

        let _ = self.menu.update(cx, |this, cx| {
            this.select_item(&item, window, cx);
        });
    }
}

/// A context menu for code completions and code actions.
pub struct CompletionMenu {
    offset: usize,
    editor: WeakEntity<EditorState>,
    list: Entity<ListState<ContextMenuDelegate>>,
    open: bool,

    /// The offset of the first character that triggered the completion.
    pub(crate) trigger_start_offset: Option<usize>,
    query: SharedString,
    _subscriptions: Vec<Subscription>,
}

impl CompletionMenu {
    /// Creates a new `CompletionMenu` with the given offset and completion items.
    ///
    /// NOTE: This element should not call from EditorState::new, unless that will stack overflow.
    pub(crate) fn new(
        editor: Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let menu = ContextMenuDelegate {
                query: SharedString::default(),
                menu: cx.entity().downgrade(),
                editor: editor.downgrade(),
                items: vec![],
                selected_ix: 0,
                resolved: Default::default(),
            };

            let list = cx.new(|cx| ListState::new(menu, window, cx));

            let _subscriptions =
                vec![
                    cx.subscribe(&list, |this: &mut Self, _, ev: &ListEvent, cx| {
                        match ev {
                            ListEvent::Confirm(_) => {
                                this.hide(cx);
                            }
                            _ => {}
                        }
                        cx.notify();
                    }),
                ];

            Self {
                offset: 0,
                editor: editor.downgrade(),
                list,
                open: false,
                trigger_start_offset: None,
                query: SharedString::default(),
                _subscriptions,
            }
        })
    }

    fn select_item(&mut self, item: &CompletionItem, window: &mut Window, cx: &mut Context<Self>) {
        let item = item.clone();
        let range = self.trigger_start_offset.unwrap_or(self.offset)..self.offset;

        let editor = self.editor.clone();

        cx.spawn_in(window, async move |_, cx| {
            editor.update_in(cx, |editor, window, cx| {
                editor.insert_completion(&item, range, window, cx);
            })
        })
        .detach();

        self.hide(cx);
    }

    pub(crate) fn handle_action(
        &mut self,
        action: Box<dyn Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.open {
            return false;
        }

        cx.propagate();
        if input::Enter::is_primary(&*action) {
            self.on_action_enter(window, cx);
        } else if action.partial_eq(&input::Escape) {
            self.on_action_escape(window, cx);
        } else if action.partial_eq(&input::MoveUp) {
            self.on_action_up(window, cx);
        } else if action.partial_eq(&input::MoveDown) {
            self.on_action_down(window, cx);
        } else {
            return false;
        }

        true
    }

    fn on_action_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.list.read(cx).delegate().selected_item().cloned() else {
            return;
        };
        self.select_item(&item, window, cx);
    }

    fn on_action_escape(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.hide(cx);
    }

    fn on_action_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |this, cx| {
            this.on_action_select_prev(&actions::SelectUp, window, cx)
        });
    }

    fn on_action_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.list.update(cx, |this, cx| {
            this.on_action_select_next(&actions::SelectDown, window, cx)
        });
    }

    /// Hide the completion menu and reset the trigger start offset.
    pub(crate) fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.trigger_start_offset = None;
        let editor = self.editor.clone();
        cx.spawn(async move |_, cx| {
            let _ = editor.update(cx, |editor, cx| editor.dismiss_completion_overlay(cx));
        })
        .detach();
        cx.notify();
    }

    /// Sets the trigger start offset if it is not already set.
    pub(crate) fn update_query(&mut self, start_offset: usize, query: impl Into<SharedString>) {
        if self.trigger_start_offset.is_none() {
            self.trigger_start_offset = Some(start_offset);
        }
        self.query = query.into();
    }

    pub(crate) fn show(
        &mut self,
        offset: usize,
        items: impl Into<Vec<CompletionItem>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = items.into();
        self.offset = offset;
        self.open = true;
        // (sik) What's highlighted is the word typed before the cursor.
        if let Some(editor) = self.editor.upgrade() {
            let text = editor.read(cx).text();
            let position = text.offset_to_position(offset);
            let before: Vec<char> = text
                .slice_line(position.line as usize)
                .chars()
                .take(position.character as usize)
                .collect();
            let word = before
                .iter()
                .rev()
                .take_while(|ch| ch.is_alphanumeric() || **ch == '_' || **ch == '$')
                .count();
            self.query = before[before.len() - word..].iter().collect::<String>().into();
        }
        self.list.update(cx, |this, cx| {
            let longest_ix = items
                .iter()
                .enumerate()
                .max_by_key(|(_, item)| {
                    item.label.len() + item.detail.as_ref().map(|d| d.len()).unwrap_or(0)
                })
                .map(|(ix, _)| ix)
                .unwrap_or(0);

            this.delegate_mut().query = self.query.clone();
            this.delegate_mut().set_items(items);
            this.set_selected_index(Some(IndexPath::new(0)), window, cx);
            this.set_item_to_measure_index(IndexPath::new(longest_ix), window, cx);
        });

        cx.notify();
    }

    fn origin(&self, cx: &App) -> Option<Point<Pixels>> {
        let editor = self.editor.upgrade()?;
        let editor = editor.read(cx);
        let Some((cursor_bounds, line_height)) = editor.cursor_layout() else {
            return None;
        };
        let cursor_origin = cursor_bounds.origin;

        let scroll_origin = editor.scroll_offset();

        Some(
            scroll_origin + cursor_origin - editor.input_bounds().origin
                + Point::new(-px(4.), line_height + px(4.)),
        )
    }
}

impl Render for CompletionMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return Empty.into_any_element();
        }

        if self.list.read(cx).delegate().items.is_empty() {
            self.open = false;
            return Empty.into_any_element();
        }

        let Some(pos) = self.origin(cx) else {
            return Empty.into_any_element();
        };

        let selected_documentation = self
            .list
            .read(cx)
            .delegate()
            .selected_item()
            .and_then(|item| item.documentation.clone());

        let Some(editor) = self.editor.upgrade() else {
            return Empty.into_any_element();
        };
        let configured_max = editor.read(cx).lsp().completion_menu.max_width;
        let max_width = configured_max.min(window.bounds().size.width - pos.x);
        let abs_pos = editor.read(cx).input_bounds().origin + pos;
        let vertical_layout =
            abs_pos.x + configured_max + POPOVER_GAP + configured_max + POPOVER_GAP
                > window.bounds().size.width;

        deferred(
            div()
                .absolute()
                .left(pos.x)
                .top(pos.y)
                .flex()
                .flex_row()
                .gap(POPOVER_GAP)
                .items_start()
                .when(vertical_layout, |this| this.flex_col())
                .child(
                    editor_popover("completion-menu", cx)
                        .max_w(max_width)
                        .min_w(px(120.))
                        .child(List::new(&self.list).max_h(MAX_MENU_HEIGHT)),
                )
                .when_some(selected_documentation, |this, documentation| {
                    let mut doc = match documentation {
                        lsp_types::Documentation::String(s) => s.clone(),
                        lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
                    };
                    if vertical_layout {
                        doc = doc.split("\n").next().unwrap_or_default().to_string();
                    }

                    this.child(
                        div().child(
                            editor_popover("completion-menu", cx)
                                .w(configured_max)
                                .px_2()
                                .child(render_markdown("doc", doc, window, cx)),
                        ),
                    )
                })
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.hide(cx);
                })),
        )
        .into_any_element()
    }
}
