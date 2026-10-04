//! The debugger panel under the code: the toolbar, and the call stack,
//! breakpoints, variables, watches and console.

use std::path::Path;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, h_flex, h_resizable, v_resizable,
    input::Input,
    menu::{ContextMenuExt as _, PopupMenu},
    resizable_panel,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{ConsoleLine, DebugEvent, Debugger, EditKind, Status, Var, child_path};
use crate::{
    DebugContinue, DebugPause, DebugRestart, DebugStop, StepInto, StepOut, StepOver,
    config::UiText,
    menu,
};

const ROW: f32 = 22.;
const INDENT: f32 = 14.;

/// A line of a tree of values.
struct TreeRow {
    key: String,
    depth: usize,
    var: Var,
    expanded: bool,
    /// The expression that names it, to edit it or watch it.
    expr: Option<String>,
    kind: RowKind,
}

enum RowKind {
    Value,
    Loading,
    /// More children to load (of the value with that ref).
    More(u64, u64),
}

impl Debugger {
    fn tree(&self, rows: &mut Vec<TreeRow>, key: String, depth: usize, var: &Var, expr: Option<String>) {
        let expanded = var.reference != 0 && self.expanded.contains(&key);
        rows.push(TreeRow { key: key.clone(), depth, var: var.clone(), expanded, expr: expr.clone(), kind: RowKind::Value });
        if !expanded {
            return;
        }
        match self.children.get(&var.reference) {
            Some(children) => {
                for child in &children.vars {
                    let child_expr = expr.as_deref().map(|parent| child_path(parent, &child.name));
                    self.tree(rows, format!("{key}/{}", child.name), depth + 1, child, child_expr);
                }
                let total = var.count.max(children.total);
                if (children.vars.len() as u64) < total {
                    rows.push(TreeRow {
                        key: format!("{key}/…"),
                        depth: depth + 1,
                        var: Var::default(),
                        expanded: false,
                        expr: None,
                        kind: RowKind::More(var.reference, total - children.vars.len() as u64),
                    });
                }
            }
            None => rows.push(TreeRow {
                key: format!("{key}/…"),
                depth: depth + 1,
                var: Var::default(),
                expanded: false,
                expr: None,
                kind: RowKind::Loading,
            }),
        }
    }

    fn variable_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for var in &self.locals {
            self.tree(&mut rows, format!("l/{}", var.name), 0, var, Some(var.name.clone()));
        }
        if self.globals != 0 {
            let globals = Var {
                name: "Module".into(),
                value: String::new(),
                kind: String::new(),
                reference: self.globals,
                count: 0,
            };
            // its children are named by themselves, not as members
            let expanded = self.expanded.contains("g");
            rows.push(TreeRow { key: "g".into(), depth: 0, var: globals, expanded, expr: None, kind: RowKind::Value });
            if expanded {
                match self.children.get(&self.globals) {
                    Some(children) => {
                        for var in &children.vars {
                            self.tree(&mut rows, format!("g/{}", var.name), 1, var, Some(var.name.clone()));
                        }
                    }
                    None => rows.push(TreeRow {
                        key: "g/…".into(),
                        depth: 1,
                        var: Var::default(),
                        expanded: false,
                        expr: None,
                        kind: RowKind::Loading,
                    }),
                }
            }
        }
        rows
    }

    /// The panel's right-click menu: its toolbar's buttons, and Hide Panel.
    /// The rows that have menus of their own end with it.
    fn panel_menu(&self, cx: &Context<Self>) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let debugger = cx.entity().downgrade();
        let stopped = self.current().is_some_and(|stop| !stop.resumed);
        let active = self.status != Status::Idle;
        let connected = self.status == Status::Connected;
        move |menu, _, _| {
            let menu = if stopped {
                menu.item(menu::item("Continue", &debugger, |this, _, cx| this.continue_(cx)).action(Box::new(DebugContinue)))
            } else if active {
                menu.item(menu::item("Pause", &debugger, |this, _, cx| this.pause(cx)).action(Box::new(DebugPause)).disabled(!connected))
            } else {
                menu.item(
                    menu::item("Start Debugging", &debugger, |this, window, cx| this.start(window, cx))
                        .action(Box::new(DebugContinue)),
                )
            };
            menu.item(menu::item("Step Over", &debugger, |this, _, cx| this.step_over(cx)).action(Box::new(StepOver)).disabled(!stopped))
                .item(menu::item("Step Into", &debugger, |this, _, cx| this.step_in(cx)).action(Box::new(StepInto)).disabled(!stopped))
                .item(menu::item("Step Out", &debugger, |this, _, cx| this.step_out(cx)).action(Box::new(StepOut)).disabled(!stopped))
                .separator()
                .item(menu::item("Restart", &debugger, |this, window, cx| this.restart(window, cx)).action(Box::new(DebugRestart)))
                .item(menu::item("Stop", &debugger, |this, _, cx| this.stop(cx)).action(Box::new(DebugStop)).disabled(!active))
                .separator()
                .item(hide_item(&debugger))
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let stopped = self.current().is_some_and(|stop| !stop.resumed);
        let active = self.status != Status::Idle;

        let status: SharedString = match &self.status {
            Status::Idle => "Not running".into(),
            Status::Connecting(what) => what.clone().into(),
            Status::Connected => match self.current() {
                Some(stop) if !stop.resumed => {
                    let frame = stop.stop.frames.first();
                    let place = frame
                        .map(|frame| format!(" in {} ({}:{})", frame.function, short_file(&frame.file), frame.line))
                        .unwrap_or_default();
                    let reason = match stop.stop.reason.as_str() {
                        "breakpoint" => "Stopped at a breakpoint",
                        "exception" => "Stopped at an exception",
                        "pause" => "Paused",
                        "entry" => "Stopped on entry",
                        _ => "Stopped",
                    };
                    let others = self.stops.len().saturating_sub(1);
                    let others = if others > 0 { format!(" · {others} more stopped") } else { String::new() };
                    format!("{reason}{place}{others}").into()
                }
                _ => "Running".into(),
            },
        };

        h_flex()
            .h(px(34.))
            .px_2()
            .gap_1()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .child(if stopped {
                tool("debug-continue", "icons/play.svg", "Continue (F5)", true, theme.success, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.continue_(cx)))
                    .into_any_element()
            } else if active {
                tool("debug-pause", "icons/pause.svg", "Pause (F6)", self.status == Status::Connected, theme.foreground, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.pause(cx)))
                    .into_any_element()
            } else {
                tool("debug-start", "icons/play.svg", "Start Debugging (F5)", true, theme.success, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.start(window, cx)))
                    .into_any_element()
            })
            .child(
                tool("debug-over", "icons/redo-dot.svg", "Step Over (F10)", stopped, theme.info, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.step_over(cx))),
            )
            .child(
                tool("debug-in", "icons/arrow-down-to-dot.svg", "Step Into (F11)", stopped, theme.info, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.step_in(cx))),
            )
            .child(
                tool("debug-out", "icons/arrow-up-from-dot.svg", "Step Out (Shift-F11)", stopped, theme.info, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.step_out(cx))),
            )
            .child(
                tool("debug-restart", "icons/rotate-ccw.svg", "Restart (Cmd-Shift-F5)", true, theme.success, cx)
                    .on_click(cx.listener(|this, _, window, cx| this.restart(window, cx))),
            )
            .child(
                tool("debug-stop", "icons/square.svg", "Stop (Shift-F5)", active, theme.danger, cx)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            )
            .child(
                div()
                    .id("debug-status")
                    .ml_2()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_ui_small(cx)
                    .text_color(if stopped { theme.warning } else { theme.muted_foreground })
                    .child(status)
                    .context_menu(self.panel_menu(cx)),
            )
            .when(!self.tab, |el| el.child(
                div()
                    .id("debug-close")
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.secondary))
                    .child(svg().path("icons/tab-close.svg").size(px(14.)).text_color(theme.muted_foreground))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DebugEvent::Hide)))
                    .tooltip(|window, cx| Tooltip::new("Hide (Cmd-Shift-D)").build(window, cx)),
            ))
            .into_any_element()
    }

    fn render_stack(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let mut list = v_flex().w_full();
        let several = self.stops.len() > 1;
        if self.stops.is_empty() {
            let text = match self.status {
                Status::Connected => {
                    if self.running == 0 { "Running".to_string() } else { format!("Running · {} VMs", self.running) }
                }
                _ => String::new(),
            };
            return placeholder(text, cx);
        }
        for (vm, stop) in &self.stops {
            let vm = *vm;
            let focused = self.focus == Some(vm);
            if several {
                let top = stop.stop.frames.first().map(|frame| frame.function.clone()).unwrap_or_default();
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("vm-{vm}")))
                        .h(px(ROW))
                        .px_2()
                        .gap_1()
                        .when(focused, |el| el.bg(theme.secondary))
                        .hover(|style| style.bg(theme.secondary))
                        .child(
                            svg()
                                .path(if focused { "icons/tree-chevron-down.svg" } else { "icons/tree-chevron-right.svg" })
                                .size(px(12.))
                                .text_color(theme.muted_foreground),
                        )
                        .child(div().text_color(theme.foreground).child(format!("VM {vm}")))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .text_ui_small(cx)
                                .child(format!("{} · {top}", stop.stop.reason)),
                        )
                        .when(stop.resumed, |el| el.opacity(0.5))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_vm(vm, cx))),
                );
            }
            if !focused {
                continue;
            }
            for (ix, frame) in stop.stop.frames.iter().enumerate() {
                let selected = ix == self.frame;
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("frame-{vm}-{ix}")))
                        .h(px(ROW))
                        .pl(px(if several { 26. } else { 8. }))
                        .pr_2()
                        .gap_2()
                        .when(selected, |el| el.bg(theme.selection))
                        .hover(|style| style.bg(theme.secondary))
                        .when(stop.resumed, |el| el.opacity(0.5))
                        .child(
                            div()
                                .flex_none()
                                .text_color(if ix == 0 { theme.warning } else { theme.foreground })
                                .child(frame.function.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_ui_small(cx)
                                .text_color(theme.muted_foreground)
                                .child(format!("{}:{}", frame.file, frame.line)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.select_frame(ix, cx))),
                );
            }
        }
        list.into_any_element()
    }

    fn render_breakpoints(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let root = self.root.clone();
        let (uncaught, all) = (self.uncaught, self.all);
        let mut list = v_flex().w_full().child(
            h_flex()
                .h(px(ROW))
                .px_2()
                .gap_3()
                .text_ui_small(cx)
                .child(
                    check("exc-uncaught", "Uncaught exceptions", uncaught, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_exceptions(!uncaught, all, cx))),
                )
                .child(
                    check("exc-all", "All exceptions", all, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_exceptions(uncaught, !all, cx))),
                )
                .child(div().flex_1())
                .when(self.breakpoints.files().next().is_some(), |el| {
                    let any_enabled = self.breakpoints.files().any(|(_, bps)| bps.iter().any(|bp| bp.enabled));
                    el.child(
                        div()
                            .id("bp-enable-all")
                            .text_color(theme.muted_foreground)
                            .hover(|style| style.text_color(theme.foreground))
                            .child(if any_enabled { "Disable all" } else { "Enable all" })
                            .on_click(cx.listener(move |this, _, _, cx| this.enable_all_breakpoints(!any_enabled, cx))),
                    )
                    .child(
                        div()
                            .id("bp-remove-all")
                            .text_color(theme.muted_foreground)
                            .hover(|style| style.text_color(theme.foreground))
                            .child("Remove all")
                            .on_click(cx.listener(|this, _, _, cx| this.remove_all_breakpoints(cx))),
                    )
                }),
        );
        for (path, bps) in self.breakpoints.files() {
            let file = path.strip_prefix(&root).unwrap_or(path).to_string_lossy().to_string();
            for bp in bps {
                let (path, line, enabled) = (path.to_path_buf(), bp.line, bp.enabled);
                let detail = if let Some(error) = &bp.error {
                    error.clone()
                } else if !bp.log.is_empty() {
                    format!("log: {}", bp.log)
                } else if !bp.condition.is_empty() {
                    format!("when {}", bp.condition)
                } else if !bp.hit.is_empty() {
                    format!("hit {}", bp.hit)
                } else {
                    String::new()
                };
                let color = if bp.error.is_some() { theme.warning } else { breakpoint_color(cx) };
                let (show, enable, remove) = (path.clone(), path.clone(), path.clone());
                let menu_path = path.clone();
                let debugger = cx.entity().downgrade();
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("bp-{file}-{line}")))
                        .h(px(ROW))
                        .px_2()
                        .gap_2()
                        .hover(|style| style.bg(theme.secondary))
                        .child(
                            div()
                                .id(SharedString::from(format!("bp-dot-{file}-{line}")))
                                .size(px(10.))
                                .flex_none()
                                .rounded_full()
                                .border_1()
                                .border_color(color)
                                .when(enabled, |el| el.bg(color))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.set_breakpoint_enabled(&enable, line, !enabled, cx)
                                }))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(if enabled { "Disable" } else { "Enable" }).build(window, cx)
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .when(!enabled, |el| el.text_color(theme.muted_foreground))
                                .child(format!("{}:{}", short_file(&file), line + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_ui_small(cx)
                                .text_color(theme.muted_foreground)
                                .child(detail),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("bp-x-{file}-{line}")))
                                .flex_none()
                                .child(svg().path("icons/tab-close.svg").size(px(12.)).text_color(theme.muted_foreground))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_breakpoint(&remove, line, cx)
                                })),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(super::DebugEvent::Show { path: show.clone(), line, focus: true })
                        }))
                        .context_menu(move |menu, _, _| {
                            let (go, toggle, remove) = (menu_path.clone(), menu_path.clone(), menu_path.clone());
                            menu.item(menu::item("Go to Breakpoint", &debugger, move |_, _, cx| {
                                cx.emit(super::DebugEvent::Show { path: go.clone(), line, focus: true })
                            }))
                            .item(menu::item(if enabled { "Disable Breakpoint" } else { "Enable Breakpoint" }, &debugger, move |this, _, cx| {
                                this.set_breakpoint_enabled(&toggle, line, !enabled, cx)
                            }))
                            .item(menu::item("Remove Breakpoint", &debugger, move |this, _, cx| this.remove_breakpoint(&remove, line, cx)))
                            .separator()
                            .item(menu::item("Enable All Breakpoints", &debugger, |this, _, cx| this.enable_all_breakpoints(true, cx)))
                            .item(menu::item("Disable All Breakpoints", &debugger, |this, _, cx| this.enable_all_breakpoints(false, cx)))
                            .item(menu::item("Remove All Breakpoints", &debugger, |this, _, cx| this.remove_all_breakpoints(cx)))
                            .separator()
                            .item(hide_item(&debugger))
                        }),
                );
            }
        }
        list.into_any_element()
    }

    fn render_rows(&self, id: &'static str, rows: Vec<TreeRow>, watch: Option<usize>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let entity = cx.entity().downgrade();
        let mut list = v_flex().w_full();
        for row in rows {
            let pad = px(6. + row.depth as f32 * INDENT);
            match row.kind {
                RowKind::Loading => {
                    list = list.child(
                        div().h(px(ROW)).pl(pad + px(16.)).text_color(theme.muted_foreground).child("…"),
                    );
                    continue;
                }
                RowKind::More(reference, left) => {
                    list = list.child(
                        div()
                            .id(SharedString::from(format!("{id}-{}", row.key)))
                            .h(px(ROW))
                            .pl(pad + px(16.))
                            .text_color(theme.link)
                            .hover(|style| style.underline())
                            .child(format!("Show {} more…", left.min(super::PAGE)))
                            .on_click(cx.listener(move |this, _, _, _| this.fetch_more(reference))),
                    );
                    continue;
                }
                RowKind::Value => {}
            }
            let TreeRow { key, var, expanded, expr, depth, .. } = row;
            let expandable = var.reference != 0;
            let editing = self.value_edit.as_ref().filter(|edit| edit.key == key).map(|edit| edit.input.clone());
            let toggle_key = key.clone();
            let reference = var.reference;
            let changed = depth == 0 && key.strip_prefix("l/").is_some_and(|name| self.changed.contains(name));
            let value_color = if changed { theme.warning } else { value_color(&var, cx) };
            let count = (var.count > 0 && var.kind == "array").then(|| format!("({})", var.count));
            let (menu_expr, edit_expr, menu_value) = (expr.clone(), expr.clone(), var.value.clone());
            let edit_key = key.clone();
            let edit_value = var.value.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("{id}-{key}")))
                    .h(px(ROW))
                    .pl(pad)
                    .pr_2()
                    .gap_1()
                    .hover(|style| style.bg(theme.secondary))
                    .child(
                        div()
                            .size(px(14.))
                            .flex_none()
                            .when(expandable, |el| {
                                el.child(
                                    svg()
                                        .path(if expanded { "icons/tree-chevron-down.svg" } else { "icons/tree-chevron-right.svg" })
                                        .size(px(12.))
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(if depth == 0 && key.starts_with('g') && key.len() == 1 {
                                theme.muted_foreground
                            } else {
                                theme.foreground
                            })
                            .child(var.name.clone()),
                    )
                    .when(!var.name.is_empty() && (!var.value.is_empty() || editing.is_some()), |el| {
                        el.child(div().flex_none().text_color(theme.muted_foreground).child("="))
                    })
                    .children(count.map(|count| div().flex_none().text_color(theme.muted_foreground).child(count)))
                    .child(match editing {
                        Some(input) => div().flex_1().min_w_0().child(Input::new(&input).xsmall()).into_any_element(),
                        None => div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(value_color)
                            .child(var.value.clone())
                            .into_any_element(),
                    })
                    .children(watch.map(|ix| {
                        div()
                            .id(SharedString::from(format!("watch-x-{ix}")))
                            .flex_none()
                            .child(svg().path("icons/tab-close.svg").size(px(12.)).text_color(theme.muted_foreground))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_watch(ix, cx)
                            }))
                    }).filter(|_| depth == 0))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            if let Some(target) = &edit_expr {
                                this.start_value_edit(edit_key.clone(), target.clone(), edit_value.clone(), window, cx);
                            }
                        } else if expandable {
                            this.toggle_expanded(toggle_key.clone(), reference, cx);
                        }
                    }))
                    .context_menu({
                        let entity = entity.clone();
                        move |menu, _, _| {
                        let value = menu_value.clone();
                        let mut menu = menu.item(menu::item("Copy Value", &entity, move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()))
                        }));
                        if let Some(expr) = menu_expr.clone() {
                            let watched = expr.clone();
                            menu = menu
                                .item(menu::item("Add to Watch", &entity, move |this, _, cx| this.add_watch(watched.clone(), cx)));
                            let copied = expr.clone();
                            menu = menu.item(menu::item("Copy Expression", &entity, move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                            }));
                        }
                        if let Some(ix) = watch {
                            menu = menu.item(menu::item("Remove Watch", &entity, move |this, _, cx| this.remove_watch(ix, cx)));
                        }
                        menu.separator().item(hide_item(&entity))
                    }}),
            );
        }
        list.into_any_element()
    }

    /// The card of a hovered value, under its name, opened. It goes away a
    /// moment after the pointer leaves both, or when the code under it
    /// scrolls; the wheel over it scrolls only the card.
    pub fn render_hover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let hover = self.hover.as_ref()?;
        let anchor = hover.anchor;
        let mut rows = Vec::new();
        self.tree(&mut rows, "h".into(), 0, &hover.var, Some(paren(&hover.var.name)));
        let list = self.render_rows("hover", rows, None, cx);
        let theme = cx.theme();
        let entity = cx.entity().downgrade();
        let watcher = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let keep = bounds.union(&anchor);
                let moved = entity.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase.bubble() {
                        moved.update(cx, |this, cx| this.track_hover(keep.contains(&event.position), cx)).ok();
                    }
                });
                let scrolled = entity.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                    if phase.bubble() && !bounds.contains(&event.position) {
                        scrolled.update(cx, |this, cx| this.clear_hover(cx)).ok();
                    }
                });
            },
        )
        // Over the whole card: without a place, it would sit after the list.
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        Some(
            deferred(
                anchored().position(point(anchor.left(), anchor.bottom())).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .when(cfg!(test), |el| el.debug_selector(|| "debug-hover-card".into()))
                        .relative()
                        .occlude()
                        // the card scrolls first; what it doesn't use stops here
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .min_w(px(260.))
                        .max_w(px(640.))
                        .bg(theme.popover)
                        .border_1()
                        .border_color(theme.border)
                        .rounded(theme.radius)
                        .shadow_md()
                        .text_ui_small(cx)
                        .child(div().id("debug-hover").max_h(px(360.)).overflow_y_scroll().py_1().child(list))
                        .child(watcher),
                ),
            )
            .into_any_element(),
        )
    }

    fn render_variables(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self.is_stopped() {
            return placeholder(String::new(), cx);
        }
        let rows = self.variable_rows();
        self.render_rows("var", rows, None, cx)
    }

    fn render_watches(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let mut list = v_flex().w_full();
        for (ix, watch) in self.watches.iter().enumerate() {
            let var = match &watch.result {
                Some(Ok(var)) => Var { name: watch.expr.clone(), ..var.clone() },
                Some(Err(error)) => Var { name: watch.expr.clone(), value: error.clone(), kind: "error".into(), reference: 0, count: 0 },
                None => Var { name: watch.expr.clone(), value: String::new(), kind: String::new(), reference: 0, count: 0 },
            };
            let mut rows = Vec::new();
            self.tree(&mut rows, format!("w{ix}"), 0, &var, Some(paren(&watch.expr)));
            list = list.child(self.render_rows("watch", rows, Some(ix), cx));
        }
        v_flex()
            .w_full()
            .child(list)
            .child(div().px_2().py_1().child(Input::new(&self.watch_input).xsmall()))
            .when(self.watches.is_empty() && !self.is_stopped(), |el| {
                el.child(div().px_2().text_ui_small(cx).text_color(muted).child("Expressions evaluated at every stop"))
            })
            .into_any_element()
    }

    fn render_console(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut list = v_flex().w_full().font_family(theme.mono_font_family.clone()).text_ui_small(cx);
        for (ix, line) in self.console.iter().enumerate() {
            list = list.child(match line {
                ConsoleLine::Info(text) => div().px_2().text_color(theme.muted_foreground).child(text.clone()).into_any_element(),
                ConsoleLine::Input(text) => div().px_2().text_color(theme.foreground).child(format!("› {text}")).into_any_element(),
                ConsoleLine::Error(text) => {
                    div().px_2().text_color(theme.danger).whitespace_normal().child(text.clone()).into_any_element()
                }
                ConsoleLine::Output { text, path, line } => {
                    let place = path.clone().map(|path| (path, *line));
                    h_flex()
                        .id(("console-out", ix))
                        .px_2()
                        .gap_2()
                        .child(div().flex_1().min_w_0().whitespace_normal().text_color(theme.foreground).child(text.clone()))
                        .children(place.clone().map(|(path, line)| {
                            div().flex_none().text_color(theme.muted_foreground).child(format!(
                                "{}:{}",
                                short_file(&path.to_string_lossy()),
                                line
                            ))
                        }))
                        .when_some(place, |el, (path, line)| {
                            el.hover(|style| style.bg(theme.secondary)).on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(super::DebugEvent::Show { path: path.clone(), line: line.saturating_sub(1), focus: true })
                            }))
                        })
                        .into_any_element()
                }
                ConsoleLine::Result(var) => {
                    let mut rows = Vec::new();
                    self.tree(&mut rows, format!("c{ix}"), 0, &Var { name: String::new(), ..var.clone() }, None);
                    self.render_rows("console", rows, None, cx)
                }
            });
        }
        v_flex()
            .size_full()
            .child(
                div()
                    .id("debug-console-lines")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.console_scroll)
                    .child(list),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .flex_none()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
                        "up" => {
                            this.console_history(true, window, cx);
                            cx.stop_propagation();
                        }
                        "down" => {
                            this.console_history(false, window, cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }))
                    .child(Input::new(&self.console_input).xsmall()),
            )
            .into_any_element()
    }

    fn render_launch_problem(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.launch_error.clone()?;
        let theme = cx.theme();
        Some(
            h_flex()
                .px_3()
                .py_1()
                .gap_3()
                .flex_none()
                .border_b_1()
                .border_color(theme.border)
                .text_ui_small(cx)
                .child(div().flex_1().text_color(theme.warning).whitespace_normal().child(error))
                .child(
                    div()
                        .id("debug-create-launch")
                        .px_2()
                        .py_0p5()
                        .rounded(theme.radius)
                        .bg(theme.secondary)
                        .hover(|style| style.bg(theme.secondary_hover))
                        .child(format!("Open {}", super::LAUNCH_FILE))
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path = this.create_launch_file(cx);
                            cx.emit(super::DebugEvent::Show { path, line: 0, focus: true });
                        })),
                )
                .into_any_element(),
        )
    }
}

impl Render for Debugger {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.console.len() != self.console_seen {
            self.console_seen = self.console.len();
            self.console_scroll.scroll_to_bottom();
        }
        let section = |title: &'static str, id: &'static str, body: AnyElement, cx: &App| {
            let theme = cx.theme();
            v_flex()
                .size_full()
                .min_h_0()
                .child(
                    div()
                        .h(px(22.))
                        .px_2()
                        .flex()
                        .items_center()
                        .flex_none()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child(title),
                )
                .child(div().id(id).flex_1().min_h_0().overflow_y_scroll().child(body))
        };
        let stack = self.render_stack(cx);
        let breakpoints = self.render_breakpoints(cx);
        let variables = self.render_variables(cx);
        let watches = self.render_watches(cx);
        let console = self.render_console(cx);
        let problem = self.render_launch_problem(cx);
        let toolbar = self.render_toolbar(cx);
        let theme = cx.theme();
        let stack_and_breakpoints = v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(section("CALL STACK", "debug-stack", stack, cx)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(section("BREAKPOINTS", "debug-breakpoints", breakpoints, cx)),
            );
        let variables = section("VARIABLES", "debug-variables", variables, cx);
        let watch_and_console = v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(section("WATCH", "debug-watch", watches, cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .h(px(22.))
                            .px_2()
                            .flex()
                            .items_center()
                            .flex_none()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child("CONSOLE"),
                    )
                    .child(div().flex_1().min_h_0().child(console)),
            );
        // Under the code its parts are columns; as a column, they are rows.
        let parts = if self.tall {
            v_resizable("debug-rows")
                .child(resizable_panel().size(px(240.)).child(stack_and_breakpoints))
                .child(resizable_panel().child(variables))
                .child(resizable_panel().size(px(320.)).child(watch_and_console))
                .into_any_element()
        } else {
            h_resizable("debug-columns")
                .child(resizable_panel().size(px(360.)).child(stack_and_breakpoints))
                .child(resizable_panel().child(variables))
                .child(resizable_panel().size(px(560.)).child(watch_and_console))
                .into_any_element()
        };
        v_flex()
            .id("debugger")
            .size_full()
            .when(cfg!(test), |el| el.debug_selector(|| "debugger".into()))
            .bg(theme.background)
            .text_ui(cx)
            .child(toolbar)
            .children(problem)
            .child(div().flex_1().min_h_0().child(parts))
            .context_menu(self.panel_menu(cx))
    }
}

/// Hide Panel: the debugger's own, wherever it is (a tab of the terminals'
/// or a place of its own).
fn hide_item(debugger: &WeakEntity<Debugger>) -> menu::PopupMenuItem {
    menu::item("Hide Panel", debugger, |_, _, cx| cx.emit(DebugEvent::Hide))
}

/// The box that edits a breakpoint's condition, hit count or log message,
/// shown by the workspace under the breakpoint's line.
pub fn breakpoint_editor(debugger: &Entity<Debugger>, cx: &App) -> Option<AnyElement> {
    let edit = debugger.read(cx).edit.as_ref()?;
    let theme = cx.theme();
    let kind = edit.kind;
    let tab = |id: &'static str, this: EditKind| {
        let debugger = debugger.downgrade();
        div()
            .id(id)
            .px_2()
            .py_0p5()
            .rounded(theme.radius)
            .text_ui_small(cx)
            .when(kind == this, |el| el.bg(theme.secondary).text_color(theme.foreground))
            .when(kind != this, |el| el.text_color(theme.muted_foreground))
            .hover(|style| style.text_color(theme.foreground))
            .child(this.label())
            .on_click(move |_, window, cx| {
                debugger.update(cx, |debugger, cx| debugger.switch_edit_kind(this, window, cx)).ok();
            })
    };
    let close = debugger.downgrade();
    Some(
        v_flex()
            .w(px(460.))
            .p_2()
            .gap_1()
            .bg(theme.popover)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .shadow_lg()
            .on_mouse_down_out(move |_, _, cx| {
                close.update(cx, |debugger, cx| debugger.finish_breakpoint_edit(false, cx)).ok();
            })
            .capture_key_down({
                let debugger = debugger.downgrade();
                move |event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        debugger.update(cx, |debugger, cx| debugger.finish_breakpoint_edit(false, cx)).ok();
                        cx.stop_propagation();
                    }
                }
            })
            .child(
                h_flex()
                    .gap_1()
                    .child(tab("bp-edit-condition", EditKind::Condition))
                    .child(tab("bp-edit-hit", EditKind::Hit))
                    .child(tab("bp-edit-log", EditKind::Log))
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child("Enter to save · Esc to cancel"),
                    ),
            )
            .child(Input::new(&edit.input).small())
            .into_any_element(),
    )
}

/// A toolbar button.
pub(crate) fn tool(
    id: &'static str,
    icon: &'static str,
    tip: &'static str,
    enabled: bool,
    color: Hsla,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .when(enabled, |el| el.hover(|style| style.bg(theme.secondary)))
        .when(!enabled, |el| el.opacity(0.35))
        .child(svg().path(icon).size(px(16.)).text_color(color))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
}

fn check(id: &'static str, label: &'static str, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .gap_1()
        .child(
            div()
                .size(px(12.))
                .rounded(px(2.))
                .border_1()
                .border_color(theme.muted_foreground)
                .flex()
                .items_center()
                .justify_center()
                .when(on, |el| el.bg(theme.primary).border_color(theme.primary)),
        )
        .child(div().text_color(if on { theme.foreground } else { theme.muted_foreground }).child(label))
}

fn placeholder(text: String, cx: &App) -> AnyElement {
    div()
        .px_2()
        .text_ui_small(cx)
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// Colors values by what they are, like the code does.
fn value_color(var: &Var, cx: &App) -> Hsla {
    let theme = cx.theme();
    match var.kind.as_str() {
        "string" | "rune" => theme.success,
        "int" | "float" | "bool" => theme.info,
        "null" | "undefined" => theme.muted_foreground,
        "error" => theme.danger,
        _ => theme.foreground,
    }
}

pub fn breakpoint_color(cx: &App) -> Hsla {
    cx.theme().danger
}

/// The file name with its folder: `server/main.ts`.
fn short_file(file: &str) -> String {
    let path = Path::new(file);
    let mut parts: Vec<String> = path.iter().rev().take(2).map(|part| part.to_string_lossy().to_string()).collect();
    parts.reverse();
    parts.join("/")
}

/// An expression ready to have members added: `(a + b)`, `a.b` as it is.
fn paren(expr: &str) -> String {
    if expr.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | '[' | ']' | '"')) {
        expr.to_string()
    } else {
        format!("({expr})")
    }
}
