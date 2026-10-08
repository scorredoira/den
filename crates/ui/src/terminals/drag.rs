//! Move live sessions between tabs and split panes without reconnecting them.
use super::*;
use crate::drag_drop::{DropPlacement, TabDragPreview};

#[derive(Clone, Copy)]
pub(super) enum Source {
    Tab(usize),
    /// A pane by its title, or the notes by their tab.
    Pane(Pane),
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
            Source::Pane(pane) => tab.contains(pane),
        })
    }

    /// The notes dragged by their tab, out of any split.
    fn notes_from_tab(&self, drag: &TerminalDrag) -> bool {
        drag.area == self.weak && matches!(drag.source, Source::Pane(Pane::Notes)) && self.notes_split().is_none()
    }

    fn can_drop_on(&self, drag: &TerminalDrag, target: Pane) -> bool {
        if !self.tabs.iter().any(|tab| tab.contains(target)) {
            return false;
        }
        if self.notes_from_tab(drag) {
            return true;
        }
        let Some(source) = self.source_index(drag) else { return false };
        match drag.source {
            Source::Tab(_) => !self.tabs[source].contains(target),
            Source::Pane(from) => from != target,
        }
    }

    pub(super) fn draggable(&self, element: Stateful<Div>, source: Source, label: SharedString) -> Stateful<Div> {
        let area = self.weak.clone();
        element.on_drag(TerminalDrag { area: area.clone(), source, label }, move |drag, _, window, cx| {
            area.update(cx, |this, cx| {
                this.terminal_drop = None;
                this.drag_origin = this.tabs.get(this.active).map(|tab| tab.id);
                // The notes leave the front for the terminals they go beside.
                if this.notes_from_tab(drag) && this.panel_showing().is_some() {
                    cx.emit(TerminalAreaEvent::ShowPanel(None));
                }
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

    pub(super) fn track_drop(&mut self, pane: Pane, event: &DragMoveEvent<TerminalDrag>, cx: &mut Context<Self>) {
        let next = self
            .can_drop_on(event.drag(cx), pane)
            .then(|| DropPlacement::at(event.bounds, event.event.position, true))
            .flatten()
            .filter(|placement| placement.split().is_some())
            .map(|placement| (pane, placement));
        if (next.is_some() || self.terminal_drop.is_some_and(|(target, _)| target == pane)) && self.terminal_drop != next {
            self.terminal_drop = next;
            cx.notify();
        }
    }

    /// Detach layout data while keeping each terminal view and backend alive.
    /// A tab left with only the notes goes: they're their tab again.
    fn take_source(&mut self, drag: &TerminalDrag) -> Option<TerminalTab> {
        if self.notes_from_tab(drag) {
            return Some(TerminalTab { id: self.next_tab_id(), tree: Tree::Leaf(Pane::Notes), active: Pane::Notes, name: None });
        }
        let ix = self.source_index(drag)?;
        match drag.source {
            Source::Tab(_) => Some(self.tabs.remove(ix)),
            Source::Pane(pane) => {
                let tab = &mut self.tabs[ix];
                match tab.tree.clone().remove(pane) {
                    None => Some(self.tabs.remove(ix)),
                    Some(tree) => {
                        if tab.active == pane {
                            tab.active = tree.leaves()[0];
                        }
                        tab.tree = tree;
                        if tab.notes_only() {
                            self.tabs.remove(ix);
                        }
                        Some(TerminalTab { id: self.next_tab_id(), tree: Tree::Leaf(pane), active: pane, name: None })
                    }
                }
            }
        }
    }

    fn move_to_pane(&mut self, drag: &TerminalDrag, target: Pane, placement: DropPlacement) -> bool {
        let Some((axis, side)) = placement.split() else { return false };
        if !self.can_drop_on(drag, target) {
            return false;
        }
        let source = self.take_source(drag).expect("validated source");
        let ix = self.tabs.iter().position(|tab| tab.contains(target)).expect("destination survives detaching source");
        self.tabs[ix].tree.insert(target, &source.tree, axis, side == 0);
        self.tabs[ix].active = source.active;
        self.active = ix;
        true
    }

    pub(super) fn drop_on_pane(&mut self, drag: &TerminalDrag, target: Pane, window: &mut Window, cx: &mut Context<Self>) {
        let placement = self.terminal_drop.filter(|(pane, _)| *pane == target).map(|(_, placement)| placement);
        if placement.is_some_and(|placement| self.move_to_pane(drag, target, placement)) {
            self.finish_drop(window, cx);
        } else {
            self.cancel_drag(window, cx);
        }
    }

    /// The notes dropped on the bar leave their split: they're their tab again.
    fn move_to_bar(&mut self, drag: &TerminalDrag, before: Option<usize>) -> bool {
        let Some(source_ix) = self.source_index(drag) else { return false };
        if matches!(drag.source, Source::Pane(Pane::Notes)) {
            let active = self.tabs[source_ix].id;
            self.take_source(drag);
            self.active = self.tabs.iter().position(|tab| tab.id == active).unwrap_or(self.active.min(self.tabs.len().saturating_sub(1)));
            return true;
        }
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
        cx.emit(TerminalAreaEvent::NotesMoved);
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
            area.tabs.push(TerminalTab { id, tree: Tree::Leaf(Pane::Term(term)), active: Pane::Term(term), name: None });
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
                    let source = drag(this, Source::Pane(Pane::Term(1)));
                    assert!(this.move_to_pane(&source, Pane::Term(2), placement));
                    assert_eq!(this.tabs.len(), 2);
                    assert_eq!(this.tabs[this.active].active, Pane::Term(1));
                    let (axis, side) = placement.split().unwrap();
                    let leaves: Vec<TermId> = if side == 0 { vec![1, 2] } else { vec![2, 1] };
                    assert_eq!(this.tabs[this.active].tree, Tree::Split { axis, children: leaves.into_iter().map(|term| Tree::Leaf(Pane::Term(term))).collect() });
                    assert!(this.move_to_bar(&source, None));
                    assert_eq!(this.tabs.len(), 3);
                    assert!(this.tabs.iter().all(|tab| matches!(tab.tree, Tree::Leaf(_))));
                    assert_eq!(this.tabs[this.active].tree, Tree::Leaf(Pane::Term(1)));
                    assert_eq!(this.views[&1], first);
                    assert_eq!(first.read(cx).terminal(), &terminal);
                }
                // A whole tab carries its nested layout with it.
                let source = drag(this, Source::Pane(Pane::Term(1)));
                this.move_to_pane(&source, Pane::Term(2), DropPlacement::Right);
                let nested = this.tabs[this.active].tree.clone();
                let source = drag(this, Source::Tab(this.tabs[this.active].id));
                assert!(this.move_to_pane(&source, Pane::Term(3), DropPlacement::Bottom));
                assert_eq!(this.tabs.len(), 1);
                assert_eq!(this.tabs[0].tree, Tree::Split { axis: Axis::Column, children: vec![Tree::Leaf(Pane::Term(3)), nested] });
                let encoded = serde_json::to_vec(&SavedLayout { tabs: vec![SavedTab::Tree(this.tabs[0].tree.clone())] }).unwrap();
                let restored: SavedLayout = serde_json::from_slice(&encoded).unwrap();
                assert_eq!(restored.tabs.into_iter().next().unwrap().into_tab(), Some((this.tabs[0].tree.clone(), None)));
                // Rearranging a pane in the same tab removes its old empty branch.
                let source = drag(this, Source::Pane(Pane::Term(1)));
                assert!(this.move_to_pane(&source, Pane::Term(3), DropPlacement::Left));
                assert_eq!(this.tabs[0].terms(), vec![1, 3, 2]);
                let mut terms = this.tabs[0].terms();
                terms.sort();
                assert_eq!(terms, vec![1, 2, 3]);
            })
        });
    }

    /// The notes go beside a terminal from their tab, and back to it from
    /// the bar or when the terminals beside them close.
    #[gpui_kit::test]
    fn the_notes_split_like_a_terminal(cx: &mut TestAppContext) {
        init(cx);
        let cx = cx.add_empty_window();
        let area = cx.new_window_entity(area);
        cx.update(|window, cx| {
            area.update(cx, |this, cx| {
                let notes = drag(this, Source::Pane(Pane::Notes));
                assert!(!this.move_to_bar(&notes, None), "from their tab, the bar is where they are");
                assert!(this.move_to_pane(&notes, Pane::Term(2), DropPlacement::Bottom));
                assert_eq!(this.notes_split(), Some(1));
                assert_eq!(this.tabs[1].tree, Tree::Split { axis: Axis::Column, children: vec![Tree::Leaf(Pane::Term(2)), Tree::Leaf(Pane::Notes)] });
                assert_eq!(this.tabs[1].active, Pane::Notes);
                let encoded = serde_json::to_string(&SavedLayout { tabs: vec![SavedTab::Tree(this.tabs[1].tree.clone())] }).unwrap();
                let restored: SavedLayout = serde_json::from_str(&encoded).unwrap();
                assert_eq!(restored.tabs.into_iter().next().unwrap().into_tab(), Some((this.tabs[1].tree.clone(), None)));
                // In a split, onto the bar: their tab again.
                assert!(this.move_to_bar(&notes, None));
                assert_eq!(this.notes_split(), None);
                assert_eq!(this.tabs.len(), 3);
                assert_eq!(this.tabs[this.active].tree, Tree::Leaf(Pane::Term(2)));
                // A terminal leaving them alone, moved or exited: the tab goes.
                assert!(this.move_to_pane(&notes, Pane::Term(2), DropPlacement::Right));
                assert!(this.move_to_pane(&drag(this, Source::Pane(Pane::Term(2))), Pane::Term(1), DropPlacement::Right));
                assert_eq!(this.notes_split(), None);
                assert_eq!(this.tabs.len(), 2);
                assert!(this.move_to_pane(&notes, Pane::Term(3), DropPlacement::Left));
                this.remove(Pane::Term(3), window, cx);
                assert_eq!(this.notes_split(), None);
                assert_eq!(this.tabs.len(), 1);
            })
        });
    }

    /// Dragged by their tab onto a terminal's edge, the notes show beside it.
    #[gpui_kit::test]
    fn the_notes_tab_drags_onto_a_terminal(cx: &mut TestAppContext) {
        init(cx);
        let (area, cx) = cx.add_window_view(|window, cx| {
            let mut area = area(window, cx);
            let notes = cx.new(|cx| NotesPanel::new("notes-drag-test".into(), window, cx));
            area.set_notes(notes.clone(), window, cx);
            area.panel_tabs = vec![PanelTab {
                panel: Panel::Notes,
                view: notes.into(),
                icon: "icons/sticky-note.svg",
                title: "Notes",
                showing: false,
                closable: false,
                dot: None,
            }];
            area
        });
        let start = cx.debug_bounds("notes-tab").unwrap().center();
        let body = cx.debug_bounds("terminal-pane-1").unwrap();
        let end = point(body.right() - px(10.), body.center().y);
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start - point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| assert_eq!(area.read(cx).terminal_drop, Some((Pane::Term(1), DropPlacement::Right))));
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| {
            let area = area.read(cx);
            assert_eq!(area.tabs[area.active].tree.leaves(), vec![Pane::Term(1), Pane::Notes]);
        });
        let notes = cx.debug_bounds("terminal-pane-notes").unwrap();
        assert!(notes.left() >= cx.debug_bounds("terminal-pane-1").unwrap().right() - px(1.), "{notes:?}");
        cx.debug_bounds("terminal-pane-handle-notes").unwrap();
    }

    /// A double click types the tab's name in its place: Enter gives it,
    /// Escape leaves it, and none brings back its terminal's title. The
    /// name is saved with the layout.
    #[gpui_kit::test]
    fn a_tab_is_renamed_in_place(cx: &mut TestAppContext) {
        init(cx);
        let (area, cx) = cx.add_window_view(area);
        let tab = cx.debug_bounds("terminal-tab-1").unwrap().center();
        let name = |cx: &mut VisualTestContext| cx.update(|_, cx| area.read(cx).tabs[0].name.clone());
        cx.simulate_event(MouseDownEvent { position: tab, button: MouseButton::Left, modifiers: Modifiers::default(), click_count: 2, first_mouse: false });
        cx.simulate_event(MouseUpEvent { position: tab, button: MouseButton::Left, modifiers: Modifiers::default(), click_count: 2 });
        cx.update(|_, cx| assert!(area.read(cx).renaming.is_some()));
        cx.simulate_input("server");
        cx.simulate_keystrokes("enter");
        assert_eq!(name(cx), Some("server".into()));
        cx.update(|_, cx| assert!(area.read(cx).renaming.is_none()));
        let encoded = cx.update(|_, cx| serde_json::to_string(&SavedTab::of(&area.read(cx).tabs[0])).unwrap());
        let restored: SavedTab = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.into_tab().unwrap().1, Some("server".into()));

        area.update_in(cx, |area, window, cx| area.start_rename(1, window, cx));
        cx.simulate_input("other");
        cx.simulate_keystrokes("escape");
        assert_eq!(name(cx), Some("server".into()));
        cx.update(|_, cx| assert!(area.read(cx).renaming.is_none()));

        area.update_in(cx, |area, window, cx| area.start_rename(1, window, cx));
        cx.simulate_keystrokes("backspace enter");
        assert_eq!(name(cx), None);
    }

    /// Layouts saved before the notes could split still read.
    #[test]
    fn old_layouts_read() {
        let saved: SavedLayout = serde_json::from_str(r#"{"tabs":[{"Split":{"axis":"Row","children":[{"Leaf":1},{"Leaf":2}]}},[3,4]]}"#).unwrap();
        let trees: Vec<_> = saved.tabs.into_iter().filter_map(SavedTab::into_tab).map(|(tree, _)| tree).collect();
        assert_eq!(trees[0].leaves(), vec![Pane::Term(1), Pane::Term(2)]);
        assert_eq!(trees[1].leaves(), vec![Pane::Term(3), Pane::Term(4)]);
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
                assert!(!this.move_to_pane(&source, Pane::Term(1), DropPlacement::Right));
                assert!(!this.move_to_pane(&source, Pane::Term(99), DropPlacement::Right));
                assert!(!this.move_to_pane(&source, Pane::Term(2), DropPlacement::Center));
                let foreign = TerminalDrag { area: other.downgrade(), ..source.clone() };
                assert!(!this.move_to_pane(&foreign, Pane::Term(2), DropPlacement::Right));
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
            cx.update(|_, cx| assert_eq!(area.read(cx).terminal_drop, Some((Pane::Term(2), DropPlacement::Right))));
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
            assert_eq!(area.read(cx).terminal_drop, Some((Pane::Term(2), DropPlacement::Right)));
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
            assert_eq!(area.tabs[area.active].terms(), vec![2, 1]);
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
            assert_eq!(area.tabs[area.active].tree, Tree::Leaf(Pane::Term(1)));
            assert!(area.tabs.iter().all(|tab| matches!(tab.tree, Tree::Leaf(_))));
            assert!(area.views[&1].read(cx).focus_handle(cx).is_focused(window));
        });
    }
}
