//! Dragging editor tabs within the workspace's two editor groups.
use super::*;

#[derive(Clone)]
pub(super) struct TabDrag {
    // Paths and vector indices aren't stable identities (a file can have two views).
    pub editor: Entity<EditorState>,
    pub label: SharedString,
}

pub(super) struct TabDragPreview(pub SharedString);

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_ui(cx)
            .rounded(cx.theme().radius)
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().tab_active_foreground)
            .shadow_md()
            .child(self.0.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EditorDrop {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl EditorDrop {
    fn at(bounds: Bounds<Pixels>, position: Point<Pixels>, can_split: bool) -> Option<Self> {
        if !bounds.contains(&position) || bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return None;
        }
        if !can_split {
            return Some(Self::Center);
        }
        let x = (position.x - bounds.left()) / bounds.size.width;
        let y = (position.y - bounds.top()) / bounds.size.height;
        // The nearest edge wins at corners; the middle half merges into the group.
        [(x, Self::Left), (1. - x, Self::Right), (y, Self::Top), (1. - y, Self::Bottom)]
            .into_iter()
            .filter(|(distance, _)| *distance < 0.25)
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map(|(_, placement)| placement)
            .or(Some(Self::Center))
    }

    fn split(self) -> Option<(Axis, usize)> {
        match self {
            Self::Center => None,
            Self::Left => Some((Axis::Row, 0)),
            Self::Right => Some((Axis::Row, 1)),
            Self::Top => Some((Axis::Column, 0)),
            Self::Bottom => Some((Axis::Column, 1)),
        }
    }

    pub fn indicator(self, cx: &App) -> Div {
        div()
            .absolute()
            .map(|el| match self {
                Self::Center => el.inset_0(),
                Self::Left => el.left_0().top_0().bottom_0().w(relative(0.5)),
                Self::Right => el.right_0().top_0().bottom_0().w(relative(0.5)),
                Self::Top => el.top_0().left_0().right_0().h(relative(0.5)),
                Self::Bottom => el.bottom_0().left_0().right_0().h(relative(0.5)),
            })
            .bg(cx.theme().primary.opacity(0.16))
            .border_1()
            .border_color(cx.theme().primary.opacity(0.65))
    }
}

impl Workspace {
    pub(super) fn start_tab_drag(&mut self, drag: &TabDrag, window: &mut Window, cx: &mut Context<Self>) {
        self.editor_drop = None;
        // A drag consumes the click, so activate here as well. This also routes
        // Escape here if focus was outside the workspace, e.g. in the task list.
        if let Some(ix) = self.tab_index(&drag.editor) {
            self.activate(ix, window, cx);
        }
    }

    fn can_split_drag(&self, ix: usize) -> bool {
        self.editor_split.is_none()
            && (self.tabs.len() > 1 || (self.tabs[ix].diff.is_none() && matches!(self.tabs[ix].content, Content::Ready)))
    }

    pub(super) fn track_tab_drop(&mut self, group: usize, event: &DragMoveEvent<TabDrag>, cx: &mut Context<Self>) {
        let placement = self
            .tab_index(&event.drag(cx).editor)
            .and_then(|ix| EditorDrop::at(event.bounds, event.event.position, self.can_split_drag(ix)));
        let next = placement.map(|placement| (group, placement));
        // Every group's listener sees the move. Only clear this group's own indicator.
        if (next.is_some() || self.editor_drop.is_some_and(|(target, _)| target == group)) && self.editor_drop != next {
            self.editor_drop = next;
            cx.notify();
        }
    }

    pub(super) fn drop_tab(
        &mut self,
        drag: &TabDrag,
        group: usize,
        mut before: Option<usize>,
        placement: EditorDrop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor_drop = None;
        let Some(mut ix) = self.tab_index(&drag.editor) else {
            return cx.notify();
        };
        cx.stop_propagation();
        if let Some((axis, destination)) = placement.split().filter(|_| self.can_split_drag(ix)) {
            self.tabs[ix].preview = false;
            self.editor_split = Some(axis);
            for tab in &mut self.tabs {
                tab.group = 1 - destination;
            }
            // Splitting the only tab keeps another view behind, sharing unsaved edits.
            let moved = if self.tabs.len() == 1 {
                self.new_view(ix, destination, self.tabs[ix].show_source, window, cx)
            } else {
                self.tabs[ix].group = destination;
                ix
            };
            self.activate(moved, window, cx);
            return;
        }

        if before == Some(ix) {
            return cx.notify();
        }
        // Dropping into the same body activates/pins without reordering its tabs.
        if before.is_none() && self.tabs[ix].group == group {
            self.tabs[ix].preview = false;
            self.activate(ix, window, cx);
            return;
        }
        // Moving a file onto its other view merges them. Close the destination
        // through the usual path so its saved text/dirty state transfers if it
        // owns the file; keep the dragged editor, cursor and undo history.
        if self.tabs[ix].diff.is_none()
            && let Some(duplicate) = self.tabs.iter().enumerate().find_map(|(other, tab)| {
                (other != ix
                    && tab.group == group
                    && tab.shows_file(&self.tabs[ix].path)
                    && (tab.markdown.is_none() || tab.show_source == self.tabs[ix].show_source))
                    .then_some(other)
            })
        {
            self.close(duplicate, window, cx);
            before = before.map(|slot| slot - usize::from(duplicate < slot));
            ix = self.tab_index(&drag.editor).expect("closing the other view keeps the dragged tab");
        }
        let target = before.unwrap_or(self.tabs.len());
        self.tabs[ix].group = group;
        self.tabs[ix].preview = false;
        let moved = move_before(&mut self.tabs, ix, target);
        self.active = None;
        self.normalize_groups();
        self.activate(moved, window, cx);
    }
}

/// `target` is an insertion slot in the original vector, including its end.
fn move_before<T>(items: &mut Vec<T>, from: usize, target: usize) -> usize {
    let item = items.remove(from);
    let target = if from < target { target - 1 } else { target };
    items.insert(target, item);
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn workspace(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            // Never load/save the user's configuration or connect to their agent.
            cx.set_global(Config::default());
        });
        let cx = cx.add_empty_window();
        let workspace = cx.new_window_entity(|window, cx| {
            Workspace::new(PathBuf::from("/tab-drag-test"), None, true, "tab-drag-test".into(), window, cx)
        });
        (workspace, cx)
    }

    fn add_tab(this: &mut Workspace, name: &str, window: &mut Window, cx: &mut Context<Workspace>) -> TabDrag {
        let mut tab = this.new_tab(PathBuf::from(name), false, window, cx);
        tab.content = Content::Ready;
        tab.saved = "original\ntext".into();
        tab.editor.update(cx, |state, cx| state.set_value("original\ntext", window, cx));
        let drag = TabDrag {
            editor: tab.editor.clone(),
            label: name.to_string().into(),
        };
        this.tabs.push(tab);
        drag
    }

    #[gpui_kit::test]
    fn pointer_drag_previews_splits_and_escape_cancels(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        // GPUI's test platform renders offscreen; it never opens a desktop window.
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            let mut workspace = Workspace::new(PathBuf::from("/tab-drag-test"), None, true, "tab-drag-test".into(), window, cx);
            workspace.side_panel_visible = false;
            workspace.terminals_visible = false;
            add_tab(&mut workspace, "first.rs", window, cx);
            add_tab(&mut workspace, "second.rs", window, cx);
            workspace.activate(1, window, cx);
            workspace
        });
        let start = cx.debug_bounds("editor-tab-0").expect("first tab rendered").center();
        let body = cx.debug_bounds("editor-body-0").expect("editor body rendered");
        let end = point(body.right() - px(10.), body.center().y);
        let outside = cx.update(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            focus
        });
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            assert!(cx.has_active_drag());
            assert!(!outside.is_focused(window));
            assert_eq!(workspace.read(cx).editor_drop, Some((0, EditorDrop::Right)));
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| {
            assert!(!cx.has_active_drag());
            assert_eq!(workspace.read(cx).editor_split, None);
            assert_eq!(workspace.read(cx).editor_drop, None);
        });
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| {
            let workspace = workspace.read(cx);
            assert_eq!(workspace.editor_split, Some(Axis::Row));
            assert_eq!(workspace.tabs[0].group, 1);
            assert_eq!(workspace.active, Some(0));
            assert_eq!(workspace.editor_drop, None);
        });
        let start = cx.debug_bounds("editor-tab-0").unwrap().center();
        let end = cx.debug_bounds("editor-body-0").unwrap().center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(start + point(px(12.), px(0.)), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| assert_eq!(workspace.read(cx).editor_drop, Some((0, EditorDrop::Center))));
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.update(|_, cx| {
            let workspace = workspace.read(cx);
            assert_eq!(workspace.editor_split, None);
            assert_eq!(workspace.tabs.len(), 2);
            assert!(workspace.tabs.iter().all(|tab| tab.group == 0));
            assert_eq!(workspace.editor_drop, None);
        });
    }

    #[gpui_kit::test]
    fn splitting_moves_the_same_editor_and_collapsing_keeps_its_state(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                let first = add_tab(this, "first.rs", window, cx);
                let second = add_tab(this, "second.rs", window, cx);
                first
                    .editor
                    .update(cx, |state, cx| state.set_cursor_position(Position::new(1, 2), window, cx));
                this.activate(1, window, cx);
                for placement in [EditorDrop::Left, EditorDrop::Right, EditorDrop::Top, EditorDrop::Bottom] {
                    this.drop_tab(&first, 0, None, placement, window, cx);
                    let (axis, destination) = placement.split().unwrap();
                    assert_eq!(this.editor_split, Some(axis));
                    assert_eq!(this.tabs[this.active.unwrap()].editor, first.editor);
                    assert_eq!(this.group, destination);
                    assert_eq!(this.tabs[this.tab_index(&second.editor).unwrap()].group, 1 - destination);
                    // Move the only tab out of its group; the remaining group fills the area.
                    this.drop_tab(&first, 1 - destination, None, EditorDrop::Center, window, cx);
                    assert_eq!(this.editor_split, None);
                    assert!(this.tabs.iter().all(|tab| tab.group == 0));
                    assert_eq!(this.tabs.len(), 2);
                    assert_eq!(this.tabs[this.active.unwrap()].editor, first.editor);
                    assert_eq!(first.editor.read(cx).cursor_position(), Position::new(1, 2));
                }
            })
        });
    }

    #[gpui_kit::test]
    fn merging_a_dirty_view_preserves_unsaved_text_and_close_confirmation(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        let original = cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                let original = add_tab(this, "first.rs", window, cx);
                this.activate(0, window, cx);
                original
            })
        });
        let view = cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                this.drop_tab(&original, 0, None, EditorDrop::Right, window, cx);
                assert_eq!(this.tabs.len(), 2);
                assert!(this.tabs[1].view);
                let view = TabDrag {
                    editor: this.tabs[1].editor.clone(),
                    label: "first.rs".into(),
                };
                view.editor.update(cx, |state, cx| state.set_value("unsaved edit", window, cx));
                // set_value is programmatic; user edits arrive through this handler.
                this.on_edit(&view.editor, window, cx);
                view
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                assert_eq!(original.editor.read(cx).value(), "unsaved edit");
                assert!(this.is_dirty(0));
                assert!(this.is_dirty(1));
                this.drop_tab(&view, 0, Some(0), EditorDrop::Center, window, cx);
                assert_eq!(this.tabs.len(), 1);
                assert_eq!(this.editor_split, None);
                assert_eq!(this.active, Some(0));
                assert_eq!(this.tabs[0].editor, view.editor);
                assert!(this.tabs[0].is_file());
                assert!(this.tabs[0].dirty);
                assert_eq!(this.tabs[0].saved, "original\ntext");
                assert_eq!(this.tabs[0].editor.read(cx).value(), "unsaved edit");
                this.close(0, window, cx);
                assert_eq!(this.tabs.len(), 1);
                assert!(this.tabs[0].confirm_close);
            })
        });
    }

    #[gpui_kit::test]
    fn drag_focuses_the_tab_and_reordering_pins_it(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                let first = add_tab(this, "first.rs", window, cx);
                let second = add_tab(this, "second.rs", window, cx);
                this.tabs[1].preview = true;
                this.activate(0, window, cx);
                let outside = cx.focus_handle();
                outside.focus(window, cx);
                this.start_tab_drag(&second, window, cx);
                assert_eq!(this.active, Some(1));
                assert!(second.editor.read(cx).focus_handle(cx).is_focused(window));
                this.drop_tab(&second, 0, Some(0), EditorDrop::Center, window, cx);
                assert_eq!(this.active, Some(0));
                assert_eq!(this.tabs[0].editor, second.editor);
                assert!(!this.tabs[0].preview);
                assert_eq!(this.tabs[1].editor, first.editor);
                this.drop_tab(&second, 0, Some(2), EditorDrop::Center, window, cx);
                assert_eq!(this.active, Some(1));
                assert_eq!(this.tabs[0].editor, first.editor);
                assert_eq!(this.tabs[1].editor, second.editor);
            })
        });
    }

    #[gpui_kit::test]
    fn dragging_non_text_content_focuses_the_workspace(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                let drag = add_tab(this, "first.rs", window, cx);
                for content in [Content::Loading, Content::Failed("unavailable".into())] {
                    this.tabs[0].content = content;
                    this.start_tab_drag(&drag, window, cx);
                    assert!(this.focus_handle.is_focused(window));
                }
                this.tabs[0].content = Content::Ready;
                this.tabs[0].image = Some(Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new())));
                this.start_tab_drag(&drag, window, cx);
                assert!(this.focus_handle.is_focused(window));
            })
        });
    }

    #[gpui_kit::test]
    fn markdown_source_and_preview_remain_distinct_when_moved_together(cx: &mut TestAppContext) {
        let (workspace, cx) = workspace(cx);
        cx.update(|window, cx| {
            workspace.update(cx, |this, cx| {
                add_tab(this, "readme.md", window, cx);
                this.activate(0, window, cx);
                this.open_preview_to_side(&OpenPreviewToSide, window, cx);
                assert!(this.tabs[0].show_source);
                assert!(!this.tabs[1].show_source);
                let preview = TabDrag {
                    editor: this.tabs[1].editor.clone(),
                    label: "Preview readme.md".into(),
                };
                this.drop_tab(&preview, 0, None, EditorDrop::Center, window, cx);
                assert_eq!(this.tabs.len(), 2);
                assert_eq!(this.editor_split, None);
                assert!(this.tabs[0].show_source);
                assert!(!this.tabs[1].show_source);
            })
        });
    }

    #[test]
    fn edges_preview_the_destination_half_and_center_merges() {
        let bounds = Bounds::new(point(px(100.), px(50.)), size(px(400.), px(200.)));
        for (x, y, expected) in [
            (110., 150., EditorDrop::Left),
            (490., 150., EditorDrop::Right),
            (300., 60., EditorDrop::Top),
            (300., 240., EditorDrop::Bottom),
            (300., 150., EditorDrop::Center),
            (180., 55., EditorDrop::Top),
        ] {
            assert_eq!(EditorDrop::at(bounds, point(px(x), px(y)), true), Some(expected));
            assert_eq!(EditorDrop::at(bounds, point(px(x), px(y)), false), Some(EditorDrop::Center));
        }
        assert_eq!(EditorDrop::at(bounds, point(px(99.), px(150.)), true), None);
        assert_eq!(EditorDrop::at(bounds, point(px(300.), px(49.)), false), None);
        assert_eq!(EditorDrop::Left.split(), Some((Axis::Row, 0)));
        assert_eq!(EditorDrop::Right.split(), Some((Axis::Row, 1)));
        assert_eq!(EditorDrop::Top.split(), Some((Axis::Column, 0)));
        assert_eq!(EditorDrop::Bottom.split(), Some((Axis::Column, 1)));
    }

    #[test]
    fn reordering_preserves_tabs_and_returns_the_moved_index() {
        let mut tabs = vec!["a", "b", "c", "d"];
        assert_eq!(move_before(&mut tabs, 0, 3), 2);
        assert_eq!(tabs, ["b", "c", "a", "d"]);
        assert_eq!(move_before(&mut tabs, 3, 0), 0);
        assert_eq!(tabs, ["d", "b", "c", "a"]);
        assert_eq!(move_before(&mut tabs, 1, 4), 3);
        assert_eq!(tabs, ["d", "c", "a", "b"]);
        assert_eq!(move_before(&mut tabs, 2, 2), 2);
        assert_eq!(tabs, ["d", "c", "a", "b"]);
    }
}
