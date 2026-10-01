//! Move live sessions between tabs and split panes without reconnecting them.
use super::*;
use crate::drag_drop::{DropPlacement, TabDragPreview};

#[derive(Clone, Copy)]
pub(super) enum Source {
    Tab(usize),
    Pane(TermId),
}

#[derive(Clone)]
pub(super) struct TerminalDrag {
    area: WeakEntity<TerminalArea>,
    source: Source,
    label: SharedString,
}

impl TerminalArea {
    fn source_index(&self, drag: &TerminalDrag) -> Option<usize> {
        if drag.area != self.weak {
            return None;
        }
        self.tabs.iter().position(|tab| match drag.source {
            Source::Tab(id) => tab.id == id,
            Source::Pane(term) => tab.tree.leaves().contains(&term),
        })
    }

    fn can_drop_on(&self, drag: &TerminalDrag, term: TermId) -> bool {
        let Some(source) = self.source_index(drag) else { return false };
        self.tabs.iter().any(|tab| tab.tree.leaves().contains(&term))
            && match drag.source {
                Source::Tab(_) => !self.tabs[source].tree.leaves().contains(&term),
                Source::Pane(from) => from != term,
            }
    }

    pub(super) fn draggable(&self, element: Stateful<Div>, source: Source, label: SharedString) -> Stateful<Div> {
        let area = self.weak.clone();
        element.on_drag(TerminalDrag { area: area.clone(), source, label }, move |drag, _, window, cx| {
            area.update(cx, |this, cx| {
                this.terminal_drop = None;
                this.drag_origin = this.tabs.get(this.active).map(|tab| tab.id);
                // Reveal a destination when dragging the currently visible tab.
                // A pane drag keeps its siblings visible for rearranging in place.
                if let Source::Tab(_) = drag.source
                    && this.source_index(drag) == Some(this.active)
                    && this.tabs.len() > 1
                {
                    this.active = if this.active == 0 { 1 } else { this.active - 1 };
                }
                this.focus(window, cx);
                cx.notify();
            })
            .ok();
            cx.new(|_| TabDragPreview(drag.label.clone()))
        })
    }

    pub(super) fn track_drop(&mut self, term: TermId, event: &DragMoveEvent<TerminalDrag>, cx: &mut Context<Self>) {
        let next = self
            .can_drop_on(event.drag(cx), term)
            .then(|| DropPlacement::at(event.bounds, event.event.position, true))
            .flatten()
            .filter(|placement| placement.split().is_some())
            .map(|placement| (term, placement));
        if (next.is_some() || self.terminal_drop.is_some_and(|(target, _)| target == term)) && self.terminal_drop != next {
            self.terminal_drop = next;
            cx.notify();
        }
    }

    /// Detach layout data while keeping each terminal view and backend alive.
    fn take_source(&mut self, drag: &TerminalDrag) -> Option<TerminalTab> {
        let ix = self.source_index(drag)?;
        match drag.source {
            Source::Tab(_) => Some(self.tabs.remove(ix)),
            Source::Pane(term) => {
                let tab = &mut self.tabs[ix];
                match tab.tree.clone().remove(term) {
                    None => Some(self.tabs.remove(ix)),
                    Some(tree) => {
                        if tab.active == term {
                            tab.active = tree.leaves()[0];
                        }
                        tab.tree = tree;
                        Some(TerminalTab { id: self.next_tab_id(), tree: Tree::Leaf(term), active: term })
                    }
                }
            }
        }
    }

    fn move_to_pane(&mut self, drag: &TerminalDrag, target: TermId, placement: DropPlacement) -> bool {
        let Some((axis, side)) = placement.split() else { return false };
        if !self.can_drop_on(drag, target) {
            return false;
        }
        let source = self.take_source(drag).expect("validated source");
        let ix = self.tabs.iter().position(|tab| tab.tree.leaves().contains(&target)).expect("destination survives detaching source");
        self.tabs[ix].tree.insert(target, &source.tree, axis, side == 0);
        self.tabs[ix].active = source.active;
        self.active = ix;
        true
    }

    pub(super) fn drop_on_pane(&mut self, drag: &TerminalDrag, target: TermId, window: &mut Window, cx: &mut Context<Self>) {
        let placement = self.terminal_drop.filter(|(term, _)| *term == target).map(|(_, placement)| placement);
        if placement.is_some_and(|placement| self.move_to_pane(drag, target, placement)) {
            self.finish_drop(window, cx);
        } else {
            self.cancel_drag(window, cx);
        }
    }

    fn move_to_bar(&mut self, drag: &TerminalDrag, before: Option<usize>) -> bool {
        let Some(source_ix) = self.source_index(drag) else { return false };
        let source_id = self.tabs[source_ix].id;
        // Dropping a whole tab on itself leaves its position intact.
        if before == Some(source_id) && matches!(drag.source, Source::Tab(_)) {
            self.active = source_ix;
            return true;
        }
        let source = self.take_source(drag).expect("validated source");
        let ix = before.and_then(|id| self.tabs.iter().position(|tab| tab.id == id)).unwrap_or(self.tabs.len());
        self.tabs.insert(ix, source);
        self.active = ix;
        true
    }

    pub(super) fn drop_on_bar(&mut self, drag: &TerminalDrag, before: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if self.move_to_bar(drag, before) {
            self.finish_drop(window, cx);
        }
    }

    fn finish_drop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_drop = None;
        self.drag_origin = None;
        self.focus(window, cx);
        self.save();
        cx.notify();
    }

    pub(super) fn cancel_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal_drop = None;
        if let Some(id) = self.drag_origin.take()
            && let Some(ix) = self.tabs.iter().position(|tab| tab.id == id)
        {
            self.activate_tab(ix, window, cx);
        }
    }

    pub(super) fn hover_tab(&mut self, id: usize, event: &DragMoveEvent<TerminalDrag>, window: &mut Window, cx: &mut Context<Self>) {
        let drag = event.drag(cx);
        if event.bounds.contains(&event.event.position)
            && self.source_index(drag).is_some()
            && !matches!(drag.source, Source::Tab(source) if source == id)
            && let Some(ix) = self.tabs.iter().position(|tab| tab.id == id)
            && ix != self.active
        {
            self.activate_tab(ix, window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use core::prelude::v1::test;
    use ui_term::{PtyEvent, TerminalBackend};

    struct Backend {
        _output: smol::channel::Sender<PtyEvent>,
    }
    impl TerminalBackend for Backend {
        fn write(&self, _: Vec<u8>) {}
        fn resize(&self, _: u16, _: u16) {}
        fn kill(&self) {
            panic!("moving a terminal must not kill its process");
        }
        fn cwd(&self) -> Option<PathBuf> {
            None
        }
        fn save_image(&self, _: &str, _: Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<PathBuf>>>> {
            Box::pin(async { anyhow::bail!("unused") })
        }
    }

    fn init(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
    }

    fn area(window: &mut Window, cx: &mut Context<TerminalArea>) -> TerminalArea {
        let mut area = TerminalArea::new("/terminal-drag-test".into(), None, true, cx);
        for term in 1..=3 {
            let (sender, receiver) = smol::channel::unbounded::<PtyEvent>();
            let terminal = cx.new(|cx| Terminal::new(Rc::new(Backend { _output: sender }), receiver, 80, 24, b"preserved output", cx));
            area.add_view(term, terminal, window, cx);
            let id = area.next_tab_id();
            area.tabs.push(TerminalTab { id, tree: Tree::Leaf(term), active: term });
        }
        area
    }

    fn drag(area: &TerminalArea, source: Source) -> TerminalDrag {
        TerminalDrag { area: area.weak.clone(), source, label: "terminal".into() }
    }

    #[gpui_kit::test]
    fn moving_sessions_preserves_views_and_collapses_empty_groups(cx: &mut TestAppContext) {
        init(cx);
        let cx = cx.add_empty_window();
        let area = cx.new_window_entity(area);
        cx.update(|_, cx| {
            area.update(cx, |this, cx| {
                let first = this.views[&1].clone();
                let terminal = first.read(cx).terminal().clone();
                for placement in [DropPlacement::Left, DropPlacement::Right, DropPlacement::Top, DropPlacement::Bottom] {
                    let source = drag(this, Source::Pane(1));
                    assert!(this.move_to_pane(&source, 2, placement));
                    assert_eq!(this.tabs.len(), 2);
                    assert_eq!(this.tabs[this.active].active, 1);
                    let (axis, side) = placement.split().unwrap();
                    let leaves = if side == 0 { vec![1, 2] } else { vec![2, 1] };
                    assert_eq!(this.tabs[this.active].tree, Tree::Split { axis, children: leaves.into_iter().map(Tree::Leaf).collect() });
                    assert!(this.move_to_bar(&source, None));
                    assert_eq!(this.tabs.len(), 3);
                    assert!(this.tabs.iter().all(|tab| matches!(tab.tree, Tree::Leaf(_))));
                    assert_eq!(this.tabs[this.active].tree, Tree::Leaf(1));
                    assert_eq!(this.views[&1], first);
                    assert_eq!(first.read(cx).terminal(), &terminal);
                }
                // A whole tab carries its nested layout with it.
                let source = drag(this, Source::Pane(1));
                this.move_to_pane(&source, 2, DropPlacement::Right);
                let nested = this.tabs[this.active].tree.clone();
                let source = drag(this, Source::Tab(this.tabs[this.active].id));
                assert!(this.move_to_pane(&source, 3, DropPlacement::Bottom));
                assert_eq!(this.tabs.len(), 1);
                assert_eq!(this.tabs[0].tree, Tree::Split { axis: Axis::Column, children: vec![Tree::Leaf(3), nested] });
                let encoded = serde_json::to_vec(&SavedLayout { tabs: vec![SavedTab::Tree(this.tabs[0].tree.clone())] }).unwrap();
                let restored: SavedLayout = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(restored.tabs.into_iter().next().unwrap().into_tree(), Some(this.tabs[0].tree.clone()));
                // Rearranging a pane in the same tab removes its old empty branch.
                let source = drag(this, Source::Pane(1));
                assert!(this.move_to_pane(&source, 3, DropPlacement::Left));
                assert_eq!(this.tabs[0].tree.leaves(), vec![1, 3, 2]);
                let mut terms = this.tabs[0].tree.leaves();
                terms.sort();
                assert_eq!(terms, vec![1, 2, 3]);
            })
        });
    }

    #[gpui_kit::test]
    fn rejects_stale_self_and_foreign_drops_and_reorders_tabs(cx: &mut TestAppContext) {
        init(cx);
        let cx = cx.add_empty_window();
        let area = cx.new_window_entity(area);
        let other = cx.new_window_entity(|_, cx| TerminalArea::new("/other".into(), None, true, cx));
        cx.update(|_, cx| {
            area.update(cx, |this, _| {
                let source = drag(this, Source::Tab(1));
                let original: Vec<_> = this.tabs.iter().map(|tab| tab.tree.clone()).collect();
                assert!(!this.move_to_pane(&source, 1, DropPlacement::Right));
                assert!(!this.move_to_pane(&source, 99, DropPlacement::Right));
                assert!(!this.move_to_pane(&source, 2, DropPlacement::Center));
                let foreign = TerminalDrag { area: other.downgrade(), ..source.clone() };
                assert!(!this.move_to_pane(&foreign, 2, DropPlacement::Right));
                assert!(!this.move_to_bar(&foreign, None));
                let stale = drag(this, Source::Tab(99));
                assert!(!this.move_to_bar(&stale, None));
                assert_eq!(this.tabs.iter().map(|tab| tab.tree.clone()).collect::<Vec<_>>(), original);
                assert!(this.move_to_bar(&source, None));
                assert_eq!(this.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![2, 3, 1]);
                assert_eq!(this.active, 2);
                assert!(this.move_to_bar(&source, Some(2)));
                assert_eq!(this.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(), vec![1, 2, 3]);
                assert_eq!(this.active, 0);
            })
        });
    }

    // Workspace handles Escape during capture, before the terminal area sees it.
    struct Parent {
        area: Entity<TerminalArea>,
    }

    impl Render for Parent {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("parent")
                .size_full()
                .capture_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" && cx.stop_active_drag(window) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(self.area.clone())
        }
    }

    #[gpui_kit::test]
    fn cancellation_from_parent_and_invalid_drop_restore_the_original_tab(cx: &mut TestAppContext) {
        init(cx);
        let (parent, cx) = cx.add_window_view(|window, cx| Parent { area: cx.new(|cx| area(window, cx)) });
        let area = cx.update(|_, cx| parent.read(cx).area.clone());
        let start = cx.debug_bounds("terminal-tab-1").unwrap().center();
        let body = cx.debug_bounds("terminal-pane-1").unwrap();
        let edge = point(body.right() - px(10.), body.center().y);
        for cancel_with_escape in [true, false] {
            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(edge, MouseButton::Left, Modifiers::default());
            cx.update(|_, cx| assert_eq!(area.read(cx).terminal_drop, Some((2, DropPlacement::Right))));
            if cancel_with_escape {
                cx.simulate_keystrokes("escape");
            } else {
                // The middle of a terminal is not a split destination.
                cx.simulate_mouse_move(body.center(), MouseButton::Left, Modifiers::default());
                cx.simulate_mouse_up(body.center(), MouseButton::Left, Modifiers::default());
            }
            cx.update(|_, cx| {
                assert!(!cx.has_active_drag());
                let area = area.read(cx);
                assert_eq!(area.terminal_drop, None);
                assert_eq!(area.active, 0);
                assert_eq!(area.tabs.len(), 3);
            });
            cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::default());
        }
    }

    #[gpui_kit::test]
    fn pointer_drag_previews_cancels_splits_and_detaches(cx: &mut TestAppContext) {
        init(cx);
        let (area, cx) = cx.add_window_view(area);
        let start = cx.debug_bounds("terminal-tab-1").unwrap().center();
        let body = cx.debug_bounds("terminal-pane-1").unwrap();
        let end = point(body.right() - px(10.), body.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| {
            assert!(cx.has_active_drag());
            assert_eq!(area.read(cx).terminal_drop, Some((2, DropPlacement::Right)));
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| {
            assert!(!cx.has_active_drag());
            assert_eq!(area.read(cx).terminal_drop, None);
            assert_eq!(area.read(cx).active, 0);
            assert_eq!(area.read(cx).tabs.len(), 3);
        });
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            let area = area.read(cx);
            assert_eq!(area.tabs.len(), 2);
            assert_eq!(area.tabs[area.active].tree.leaves(), vec![2, 1]);
            assert_eq!(area.terminal_drop, None);
            assert!(area.views[&1].read(cx).focus_handle(cx).is_focused(window));
        });
        let start = cx.debug_bounds("terminal-pane-handle-1").unwrap().center();
        let end = cx.debug_bounds("terminal-tab-end").unwrap().center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            let area = area.read(cx);
            assert_eq!(area.tabs.len(), 3);
            assert_eq!(area.tabs[area.active].tree, Tree::Leaf(1));
            assert!(area.tabs.iter().all(|tab| matches!(tab.tree, Tree::Leaf(_))));
            assert!(area.views[&1].read(cx).focus_handle(cx).is_focused(window));
        });
    }
}
