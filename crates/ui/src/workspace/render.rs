//! Drawing the workspace: its tab bars, editor groups and status bar.

use super::*;

impl Workspace {
    pub(super) fn render_tab_bar(&self, group: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let shown = self.shown_in(group);
        let focused = self.editor_split.is_none() || group == self.group;
        let split = self.editor_split.is_some();
        h_flex()
            .id(("tab-bar", group))
            .h(px(34.))
            .flex_none()
            .overflow_x_scroll()
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                this.drop_tab(drag, group, Some(this.tabs.len()), EditorDrop::Center, window, cx);
            }))
            .children(self.tabs.iter().enumerate().filter(|(_, tab)| tab.group == group).map(|(ix, tab)| {
                let active = shown == Some(ix);
                let name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = match &tab.page {
                    Some(page) => page.title(),
                    None => name,
                };
                let name = match &tab.diff {
                    Some(DiffOf { commit: Some((_, short)), file, .. }) if file.is_empty() => format!("Commit {short}"),
                    Some(DiffOf { commit: Some((_, short)), source: true, .. }) => format!("{name} @ {short}"),
                    Some(DiffOf { commit: Some((_, short)), .. }) => format!("{name} ({short})"),
                    Some(_) => format!("{name} (changes)"),
                    None if tab.view && tab.rendered().is_some() => format!("Preview {name}"),
                    None => name,
                };
                let dirty = self.is_dirty(ix);
                let close_icon = if dirty {
                    "icons/tab-dirty.svg"
                } else {
                    "icons/tab-close.svg"
                };
                h_flex()
                    .id(("tab", ix))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("editor-tab-{ix}")))
                    .group("tab")
                    .h_full()
                    .flex_none()
                    .gap_1()
                    .pl_3()
                    .pr_1()
                    .text_ui(cx)
                    .border_r_1()
                    .border_color(theme.border)
                    .when(active, |el| {
                        el.bg(theme.tab_active).text_color(if focused {
                            theme.tab_active_foreground
                        } else {
                            theme.tab_foreground
                        })
                    })
                    .when(!active, |el| el.bg(theme.tab).text_color(theme.tab_foreground))
                    .on_drag(
                        TabDrag { editor: tab.editor.clone(), label: name.clone().into() },
                        {
                            let workspace = cx.entity().downgrade();
                            move |drag, _, window, cx| {
                                workspace.update(cx, |this, cx| {
                                    this.start_tab_drag(drag, window, cx);
                                }).ok();
                                cx.new(|_| TabDragPreview(drag.label.clone()))
                            }
                        },
                    )
                    .drag_over::<TabDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .on_drop(cx.listener({
                        let before = tab.editor.clone();
                        move |this, drag: &TabDrag, window, cx| {
                            let before = this.tab_index(&before);
                            this.drop_tab(drag, group, before, EditorDrop::Center, window, cx);
                        }
                    }))
                    // Only the name: the right-click menu is a child of the tab and would inherit it.
                    .child(div().when(tab.preview, |el| el.italic()).child(name))
                    .child(
                        div()
                            .id(("tab-close", ix))
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(theme.radius)
                            .hover(|style| style.bg(theme.muted))
                            .child(
                                svg()
                                    .path(close_icon)
                                    .size(px(if dirty { 8. } else { 14. }))
                                    .text_color(theme.muted_foreground)
                                    .when(!dirty && !active, |el| {
                                        el.invisible().group_hover("tab", |s| s.visible())
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close(ix, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.tabs[ix].preview = false;
                        }
                        this.activate(ix, window, cx);
                    }))
                    .context_menu(self.tab_menu(tab, split, cx.entity().downgrade()))
            }))
            .child(
                div()
                    .id(("tab-drop-end", group))
                    .h_full()
                    .flex_1()
                    .min_w(px(24.))
                    .drag_over::<TabDrag>(|style, _, _, cx| style.border_l_2().border_color(cx.theme().primary))
                    .context_menu({
                        let workspace = cx.entity().downgrade();
                        move |menu, _, _| {
                            menu.item(
                                menu::item("New File", &workspace, |this, window, cx| this.new_file(&NewFile, window, cx))
                                    .action(Box::new(NewFile)),
                            )
                            .item(
                                menu::item("Close All", &workspace, |this, window, cx| this.close_others(None, window, cx))
                                    .action(Box::new(CloseAllTabs)),
                            )
                        }
                    }),
            )
    }

    /// A tab's right-click menu, also on what it shows where that has
    /// none of its own (an image, a whole commit).
    pub(super) fn tab_menu(
        &self,
        tab: &FileTab,
        split: bool,
        workspace: WeakEntity<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let path = tab.path.clone();
        let editor = tab.editor.clone();
        let diff = tab.diff.is_some();
        let markdown = tab.markdown.is_some() && tab.is_file();
        let whole_commit = tab.diff.as_ref().is_some_and(|diff| diff.file.is_empty());
        let in_preview = tab.preview;
        let notes = matches!(tab.page, Some(pages::Page::Notes));
        // The file's own changes, from its tab or a view of it.
        let changeable = !diff && !tab.doc;
        let changed = self.has_changes(&tab.path);
        let local = self.local;
        let relative = tab
            .path
            .strip_prefix(&self.root)
            .unwrap_or(&tab.path)
            .to_string_lossy()
            .into_owned();
        move |menu, _, cx| {
            let relative = relative.clone();
            let (close, others, moved, right, down, preview) =
                (editor.clone(), editor.clone(), editor.clone(), editor.clone(), editor.clone(), editor.clone());
            let (to_the_right, saved, keep) = (editor.clone(), editor.clone(), editor.clone());
            menu.when(notes, |menu| {
                menu.item(menu::item("Move to Terminals", &workspace, |this, window, cx| this.notes_to_terminals(window, cx)))
                    .separator()
            })
            .item(
                menu::item("Close", &workspace, move |this, window, cx| {
                    if let Some(ix) = this.tab_index(&close) {
                        this.close(ix, window, cx);
                    }
                })
                .action(Box::new(CloseTab)),
            )
            .item(menu::item("Close Others", &workspace, move |this, window, cx| {
                this.close_others(this.tab_index(&others), window, cx)
            }))
            .item(menu::item("Close to the Right", &workspace, move |this, window, cx| {
                if let Some(ix) = this.tab_index(&to_the_right) {
                    this.close_to_the_right(ix, window, cx);
                }
            }))
            .item(menu::item("Close Saved", &workspace, move |this, window, cx| {
                if let Some(ix) = this.tab_index(&saved) {
                    this.close_saved(this.tabs[ix].group, window, cx);
                }
            }))
            .item(
                menu::item("Close All", &workspace, |this, window, cx| this.close_others(None, window, cx))
                    .action(Box::new(CloseAllTabs)),
            )
            .separator()
            .when(in_preview, |menu| {
                menu.item(menu::item("Keep Open", &workspace, move |this, _, cx| {
                    if let Some(ix) = this.tab_index(&keep) {
                        this.keep_open(ix, cx);
                    }
                }))
            })
            .when(!split, |menu| {
                menu.item(
                    menu::item("Split Right", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&right) {
                            this.activate(ix, window, cx);
                            this.split_editor(Axis::Row, window, cx);
                        }
                    })
                    .action(Box::new(SplitEditorRight)),
                )
                .item(
                    menu::item("Split Down", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&down) {
                            this.activate(ix, window, cx);
                            this.split_editor(Axis::Column, window, cx);
                        }
                    })
                    .action(Box::new(SplitEditorDown)),
                )
            })
            .when(split, |menu| {
                menu.item(menu::item("Move to Other Side", &workspace, move |this, window, cx| {
                    if let Some(ix) = this.tab_index(&moved) {
                        this.move_to_other_group(ix, window, cx);
                    }
                }))
            })
            .when(markdown, |menu| {
                menu.item(
                    menu::item("Open Preview to the Side", &workspace, move |this, window, cx| {
                        if let Some(ix) = this.tab_index(&preview) {
                            this.activate(ix, window, cx);
                            this.open_preview_to_side(&OpenPreviewToSide, window, cx);
                        }
                    })
                    .action(Box::new(OpenPreviewToSide)),
                )
            })
            .when(diff && !whole_commit, |menu| {
                let path = path.clone();
                menu.item(menu::item("Open File", &workspace, move |this, window, cx| {
                    this.open(path.clone(), true, window, cx)
                }))
            })
            .when(changeable, |menu| {
                let path = path.clone();
                menu.item(
                    menu::item("Open Changes", &workspace, move |this, window, cx| this.open_changes(&path, window, cx))
                        .disabled(!changed(cx)),
                )
            })
            .when(!whole_commit, |menu| {
                let path = path.clone();
                menu.item(menu::item("Show File History", &workspace, move |this, window, cx| {
                    this.show_history(&path, false, window, cx)
                }))
            })
            .separator()
            .item(menu::item("Copy Path", &workspace, {
                let path = path.clone();
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()))
                }
            }))
            .item(menu::item("Copy Relative Path", &workspace, move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()))
            }))
            .item(menu::item("Reveal in File Tree", &workspace, {
                let path = path.clone();
                move |this, _, cx| this.reveal_in_tree(&path, cx)
            }))
            .when(local, |menu| {
                let path = path.clone();
                menu.item(menu::item("Reveal in Finder", &workspace, move |_, _, cx| {
                    cx.reveal_path(&path)
                }))
            })
        }
    }

    /// A group's tab bar and the tab it shows. A click anywhere in it gives
    /// it the focus.
    pub(super) fn render_group(&self, group: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let shown = self.shown_in(group).map(|ix| &self.tabs[ix]);
        // Still loading: what was there before, if anything.
        let shown = shown.map(|tab| match (&tab.content, &tab.stand_in) {
            (Content::Loading, Some(stand_in)) => &**stand_in,
            _ => tab,
        });
        let body = match shown {
            None => div()
                .id("code-empty")
                .size_full()
                .overflow_hidden()
                .p_6()
                .flex()
                .items_center()
                .justify_center()
                .context_menu({
                    let workspace = cx.entity().downgrade();
                    move |menu, _, _| {
                        menu.item(
                            menu::item("New File", &workspace, |this, window, cx| this.new_file(&NewFile, window, cx))
                                .action(Box::new(NewFile)),
                        )
                    }
                })
                .child(
                    v_flex()
                        .items_center()
                        .gap_10()
                        .max_w_full()
                        .child(
                            svg()
                                .path("icons/den-empty.svg")
                                .size(px(360.))
                                .max_w_full()
                                .flex_none()
                                .text_color(theme.muted_foreground.opacity(EMPTY_LOGO_OPACITY)),
                        )
                        .child(empty_hints(cx)),
                )
                .into_any_element(),
            Some(tab) => match &tab.content {
                Content::Loading => div().size_full().into_any_element(),
                Content::Failed(err) => div()
                    .p_4()
                    .text_ui(cx)
                    .text_color(theme.danger)
                    .child(err.clone())
                    .into_any_element(),
                Content::Ready if let Some(image) = &tab.image => div()
                    .id("image-view")
                    .size_full()
                    .context_menu(self.tab_menu(tab, self.editor_split.is_some(), cx.entity().downgrade()))
                    .p_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(image.clone()).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                Content::Ready if let Some(page) = &tab.page => page.view(self),
                Content::Ready if let Some(commit) = &tab.commit => {
                    let tab_menu = self.tab_menu(tab, self.editor_split.is_some(), cx.entity().downgrade());
                    let hash = tab.diff.as_ref().and_then(|diff| diff.commit.as_ref()).map(|(hash, _)| hash.clone()).unwrap_or_default();
                    div()
                        .id("commit-view")
                        .size_full()
                        .context_menu(move |menu, window, cx| {
                            let hash = hash.clone();
                            let menu = diff_layouts(cx)
                                .into_iter()
                                .fold(menu, |menu, (label, checked, action)| menu.menu_with_check(label, checked, action))
                                .separator()
                                .item(menu::PopupMenuItem::new("Copy Hash").on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))
                                }))
                                .separator();
                            tab_menu(menu, window, cx)
                        })
                        .child(commit.clone())
                        .into_any_element()
                }
                Content::Ready => match tab.rendered() {
                    Some(markdown) => div()
                        .id("markdown-preview")
                        .size_full()
                        .context_menu({
                            let readonly = tab.diff.is_some() || tab.doc;
                            move |menu, _, _| menu.menu_with_disabled("Edit", Box::new(ToggleMarkdownSource), readonly)
                        })
                        .text_size(px(Config::get(cx).font_size(TextArea::Preview)))
                        .child(
                            TextView::new(markdown)
                                .resolve_image_source(markdown_images::resolver(
                                    self.client.clone(), self.root.clone(), tab.path.clone(),
                                ))
                                .on_link_click({
                                    let workspace = cx.entity().downgrade();
                                    let dir = tab.path.parent().map(Path::to_path_buf).unwrap_or_default();
                                    move |url, _, window, cx| {
                                        workspace
                                            .update(cx, |this, cx| this.follow_link(url, &dir, window, cx))
                                            .ok();
                                    }
                                })
                                .selectable(true)
                                .scrollable(true)
                                .size_full()
                                .px_8()
                                .py_6(),
                        )
                        .into_any_element(),
                    None => {
                        let readonly = tab.diff.is_some() || tab.doc;
                        let markdown = tab.markdown.is_some() && !readonly;
                        let file = self.tabs.iter().find(|file| file.path == tab.path && file.is_file());
                        // Stopped here, the ends of the lines show the debugger's values.
                        let execution = self.debugger.read(cx).execution();
                        let stopped_here = execution.as_ref().is_some_and(|(at, _, _)| *at == tab.path);
                        // The debugger stopped: run to a line of any file; set the next
                        // statement only in the function stopped at.
                        let stopped = execution.is_some() && !readonly;
                        let changed = self.has_changes(&tab.path);
                        let jumpable = execution.as_ref().is_some_and(|(at, _, top)| *at == tab.path && *top);
                        let blame = file
                            .filter(|file| !file.dirty && tab.diff.is_none() && !stopped_here)
                            .and_then(|file| file.blame.clone());
                        // A diff (not a file as it was) has its own menu, and
                        // Open File opens the file it's of.
                        let diff = tab.diff.as_ref().filter(|of| !of.source);
                        let open_file = diff.filter(|of| !of.file.is_empty()).map(|_| tab.path.clone());
                        let open = open_file.is_some();
                        let layouts = tab.old.is_some();
                        let diff_selected = diff.map(|_| tab.old.as_ref().map(|old| old.selected.clone()));
                        let editor = Editor::new(&tab.editor)
                            .bordered(false)
                            .readonly(readonly)
                            .h_full()
                            // The right click already put the cursor where clicked. The menu
                            // is built while the editor is mid-update: it can't be
                            // read (GPUI aborts), so Cut and Copy are always
                            // enabled and do nothing without a selection.
                            .context_menu(move |menu, _, cx| {
                                if let Some(selected) = &diff_selected {
                                    return diff_menu(menu, layouts, open, selected.as_ref().map(|selected| &*selected[1]), cx);
                                }
                                let menu = if markdown {
                                    menu.menu("Show Preview", Box::new(ToggleMarkdownSource)).separator()
                                } else {
                                    menu
                                };
                                let menu = if stopped {
                                    menu.menu("Run to Cursor", Box::new(RunToCursor))
                                        .menu_with_disabled("Set Next Statement", !jumpable, Box::new(SetNextStatement))
                                        .menu("Add to Watch", Box::new(AddToWatch))
                                        .menu("Evaluate in Console", Box::new(EvaluateInConsole))
                                        .separator()
                                } else {
                                    menu
                                };
                                let menu = menu
                                    .menu_with_disabled("Go to Definition", readonly, Box::new(GoToDefinition))
                                    .menu_with_disabled("Find References", readonly, Box::new(FindReferences))
                                    .menu_with_disabled("Go to Symbol…", readonly, Box::new(GoToSymbol))
                                    .menu_with_disabled("Format Document", readonly, Box::new(FormatDocument));
                                // The file's changes and history, and its breakpoints:
                                // they go in before a session too, as with F9.
                                let menu = if readonly {
                                    menu
                                } else {
                                    menu.separator()
                                        .menu_with_disabled("Open Changes", !changed(cx), Box::new(OpenChanges))
                                        .menu("Show File History", Box::new(ShowFileHistory))
                                        .separator()
                                        .menu("Toggle Breakpoint", Box::new(ToggleBreakpoint))
                                        .menu("Add Conditional Breakpoint…", Box::new(AddConditionalBreakpoint))
                                        .menu("Add Logpoint…", Box::new(AddLogpoint))
                                };
                                menu.separator()
                                    .menu_with_disabled("Cut", readonly, Box::new(input::Cut))
                                    .menu("Copy", Box::new(input::Copy))
                                    .menu_with_disabled("Paste", readonly, Box::new(input::Paste))
                                    .separator()
                                    .menu("Select All", Box::new(input::SelectAll))
                            });
                        let code = div()
                            .key_context("CodeEditor")
                            .size_full()
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" && this.signature.is_some() {
                                    this.close_signature();
                                    cx.notify();
                                }
                            }))
                            .on_action(cx.listener(Self::select_next_occurrence))
                            .on_action(cx.listener(|this, _: &MoveLineUp, window, cx| this.edit_lines(true, false, window, cx)))
                            .on_action(cx.listener(|this, _: &MoveLineDown, window, cx| this.edit_lines(false, false, window, cx)))
                            .on_action(cx.listener(|this, _: &DuplicateLineUp, window, cx| this.edit_lines(true, true, window, cx)))
                            .on_action(cx.listener(|this, _: &DuplicateLineDown, window, cx| this.edit_lines(false, true, window, cx)))
                            .child(editor)
                            .children(blame.and_then(|blame| inline_blame(&tab.editor, &blame, cx)))
                            .children(debug_inline_values(&tab.editor, &tab.path, &self.debugger, cx))
                            .children(self.test_lenses(&tab.editor, &tab.path, cx.entity().downgrade(), cx))
                            .children(breakpoint_edit_box(&tab.editor, &tab.path, &self.debugger, cx))
                            .children(
                                self.signature
                                    .as_ref()
                                    .filter(|hint| hint.editor == tab.editor)
                                    .and_then(|hint| signature::render(hint, cx)),
                            );
                        let side_menu = |selected: &Rc<Cell<bool>>| {
                            let selected = selected.clone();
                            move |menu, _: &mut Window, cx: &mut App| diff_menu(menu, true, open, Some(&selected), cx)
                        };
                        let body = match &tab.old {
                            // No room for two sides (or one column chosen): VS Code's inline diff.
                            Some(old) if !Config::get(cx).diff_side_by_side(old.width.get()) => div()
                                .size_full()
                                .relative()
                                .child(measure_width(&old.width))
                                .child(
                                    Editor::new(&old.inline)
                                        .bordered(false)
                                        .readonly(true)
                                        .h_full()
                                        .context_menu(side_menu(&old.selected[2])),
                                )
                                .into_any_element(),
                            Some(old) => h_flex()
                                .size_full()
                                .relative()
                                .child(measure_width(&old.width))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .h_full()
                                        .border_r_1()
                                        .border_color(cx.theme().border)
                                        .child(
                                            Editor::new(&old.editor)
                                                .bordered(false)
                                                .readonly(true)
                                                .h_full()
                                                .context_menu(side_menu(&old.selected[0])),
                                        ),
                                )
                                .child(div().flex_1().min_w_0().h_full().child(code))
                                .into_any_element(),
                            None => code.into_any_element(),
                        };
                        match open_file {
                            // From any of the diff's editors, the menu's Open File.
                            Some(path) => div()
                                .size_full()
                                .on_action(cx.listener(move |this, _: &OpenDiffFile, window, cx| this.open(path.clone(), true, window, cx)))
                                .child(body)
                                .into_any_element(),
                            None => body,
                        }
                    }
                },
            },
        };
        v_flex()
            .id(("editor-group", group))
            .size_full()
            .bg(theme.background)
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                if this.group != group
                    && let Some(ix) = this.shown_in(group)
                {
                    this.activate_with(ix, false, window, cx);
                }
            }))
            .when(!self.tabs.is_empty(), |el| el.child(self.render_tab_bar(group, cx)))
            .child(
                div()
                    .id(("editor-drop-area", group))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("editor-body-{group}")))
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| {
                        this.track_tab_drop(group, event, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                        let placement = this.editor_drop.filter(|(target, _)| *target == group)
                            .map_or(EditorDrop::Center, |(_, placement)| placement);
                        this.drop_tab(drag, group, None, placement, window, cx);
                    }))
                    .child(measure_width(&self.group_widths[group]))
                    .child(body)
                    .when_some(self.editor_drop.filter(|(target, _)| *target == group && cx.has_active_drag()), |el, (_, placement)| {
                        el.child(placement.indicator(cx))
                    }),
            )
            .into_any_element()
    }

    /// The code area: one group, or two split side by side or one above the
    /// other.
    pub(super) fn render_editor_area(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let groups = match self.editor_split {
            None => self.render_group(0, cx),
            Some(axis) => {
                let (first, second) = (self.render_group(0, cx), self.render_group(1, cx));
                let split = match axis {
                    Axis::Row => h_resizable("editor-groups-row"),
                    Axis::Column => v_resizable("editor-groups-column"),
                };
                split
                    .child(resizable_panel().child(first))
                    .child(resizable_panel().child(second))
                    .into_any_element()
            }
        };
        v_flex().size_full().bg(cx.theme().background).child(div().flex_1().min_h_0().child(groups))
    }

    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut left = h_flex().gap_3().min_w_0().overflow_hidden();
        let mut right = h_flex().gap_3().flex_none().whitespace_nowrap();
        let branch = self.branch.clone().or_else(|| self.changes.read(cx).branch().map(str::to_string));
        if let Some(branch) = branch {
            left = left.child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .child(svg().path("icons/git-branch.svg").size(px(12.)).text_color(theme.muted_foreground))
                    .child(branch),
            );
        }
        // A page (the history, the notes) has no file to tell of.
        if let Some(tab) = self.active.map(|ix| &self.tabs[ix]).filter(|tab| tab.page.is_none()) {
            let relative = tab.path.strip_prefix(&self.root).unwrap_or(&tab.path);
            left = left.child(relative.display().to_string());
            let state = tab.editor.read(cx);
            if tab.image.is_some() {
                right = right.child("Image");
            } else {
                if tab.rendered().is_none() {
                    let pos = state.cursor_position();
                    right = right.child(format!("Ln {}, Col {}", pos.line + 1, pos.character + 1));
                }
                right = right.child(state.language_name());
            }
            if matches!(tab.content, Content::Ready) && tab.image.is_none() && tab.diff.is_none() {
                let problem = if !self.client.as_ref().is_some_and(|client| client.is_connected()) {
                    Some(SharedString::from("The agent is disconnected. Language features are unavailable."))
                } else {
                    tab.lsp_status.problem.clone()
                };
                if let Some(problem) = problem {
                    right = right.child(
                        div()
                            .id("lsp-status")
                            .text_color(theme.warning)
                            .child("LSP unavailable")
                            .tooltip(move |window, cx| Tooltip::new(problem.clone()).max_w(px(480.)).build(window, cx)),
                    );
                }
            }
            if tab.markdown.is_some() {
                let label = if tab.show_source { "Show Preview" } else { "Show Source" };
                right = right.child(
                    div()
                        .id("toggle-markdown")
                        .text_color(theme.link)
                        .hover(|style| style.underline())
                        .child(label)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_markdown_source(&ToggleMarkdownSource, window, cx)
                        })),
                );
            }
        }
        if let Some(message) = &self.message {
            left = left.child(div().text_color(theme.warning).child(message.clone()));
        }
        for info in &self.ports {
            let port = info.port;
            right = right.child(
                div()
                    .id(("port", port as usize))
                    .text_color(theme.link)
                    .hover(|style| style.underline())
                    .child(format!("{} :{port}", info.process))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_port(port, cx))),
            );
        }
        h_flex()
            .h(px(24.))
            .flex_none()
            .px_3()
            .justify_between()
            .text_ui_small(cx)
            .bg(theme.status_bar)
            .border_t_1()
            .border_color(theme.status_bar_border)
            .text_color(theme.muted_foreground)
            .child(left)
            .child(right.child("local"))
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.files_asked {
            self.read_files(cx);
        }
        // Loaded: what it stood in for goes.
        for tab in &mut self.tabs {
            if !matches!(tab.content, Content::Loading) {
                tab.stand_in = None;
            }
        }
        self.apply_word_wrap(window, cx);
        self.apply_tab(cx);
        self.sync_mode(window, cx);
        // What the panels show follows what's drawn here, told to them right
        // after: told while drawing, the ones drawn from cache wouldn't see
        // it until something else redrew them.
        cx.defer_in(window, |this, _, cx| {
            this.shape_terminals(cx);
            this.sync_debug_layout(cx);
            this.place_shown(cx);
            if this.is_shown(Panel::Outline, cx) {
                this.sync_outline(cx);
            }
        });
        if !cx.has_active_drag() {
            self.editor_drop = None;
        }
        self.report_ide_selection(cx);
        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && cx.stop_active_drag(window) {
                    this.editor_drop = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(Self::type_in_preview))
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(|this, _: &CloseAllTabs, window, cx| this.close_others(None, window, cx)))
            .on_action(cx.listener(|this, _: &CollapseFileTree, _, cx| {
                this.file_tree.update(cx, |tree, cx| tree.collapse_all(cx))
            }))
            .on_action(cx.listener(|this, _: &RefreshFiles, _, cx| {
                this.file_tree.update(cx, |tree, cx| tree.refresh(cx))
            }))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::toggle_side_panel))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.toggle_panel(Panel::Files, cx)))
            .on_action(cx.listener(|this, _: &ShowChanges, _, cx| this.toggle_panel(Panel::Changes, cx)))
            .on_action(cx.listener(|this, _: &ShowHistory, window, cx| this.toggle_history(window, cx)))
            .on_action(cx.listener(Self::show_search))
            .on_action(cx.listener(Self::open_file_finder))
            // F4 steps through the visible panel's results: References or Search.
            .on_action(cx.listener(|this, _: &NextResult, _, cx| this.step_result(1, cx)))
            .on_action(cx.listener(|this, _: &PrevResult, _, cx| this.step_result(-1, cx)))
            .on_action(cx.listener(Self::go_to_definition))
            .on_action(cx.listener(Self::go_to_line))
            .on_action(cx.listener(Self::go_to_symbol))
            .on_action(cx.listener(Self::go_to_workspace_symbol))
            .on_action(cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)))
            .on_action(cx.listener(|this, _: &NavigateForward, window, cx| this.navigate(false, window, cx)))
            .on_action(cx.listener(Self::find_references))
            .on_action(
                cx.listener(|this, _: &ShowReferences, _, cx| this.toggle_panel(Panel::References, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowOutline, _, cx| this.toggle_panel(Panel::Outline, cx)))
            .on_action(cx.listener(Self::toggle_markdown_source))
            .on_action(cx.listener(Self::open_preview_to_side))
            .on_action(cx.listener(Self::toggle_word_wrap))
            .on_action(cx.listener(Self::format_document))
            .on_action(cx.listener(|this, _: &SplitEditorRight, window, cx| this.split_editor(Axis::Row, window, cx)))
            .on_action(cx.listener(|this, _: &SplitEditorDown, window, cx| this.split_editor(Axis::Column, window, cx)))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| this.split(Axis::Row, window, cx)))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| this.split(Axis::Column, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| this.focus_pane(Direction::Left, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| this.focus_pane(Direction::Right, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| this.focus_pane(Direction::Up, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| this.focus_pane(Direction::Down, window, cx)))
            .on_action(cx.listener(Self::toggle_terminals))
            .on_action(cx.listener(Self::maximize_terminals))
            .on_action(cx.listener(|this, _: &MoveTerminals, _, cx| this.move_terminals(cx)))
            .on_action(cx.listener(Self::toggle_breakpoint))
            .on_action(cx.listener(Self::run_to_cursor))
            .on_action(cx.listener(|this, _: &AddConditionalBreakpoint, window, cx| {
                this.edit_breakpoint_at_cursor(EditKind::Condition, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddLogpoint, window, cx| this.edit_breakpoint_at_cursor(EditKind::Log, window, cx)))
            .on_action(cx.listener(|this, _: &OpenChanges, window, cx| {
                if let Some((path, _)) = this.cursor_place(cx) {
                    this.open_changes(&path, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowFileHistory, window, cx| {
                if let Some((path, _)) = this.cursor_place(cx) {
                    this.show_history(&path, false, window, cx);
                }
            }))
            .on_action(cx.listener(Self::add_to_watch))
            .on_action(cx.listener(Self::evaluate_in_console))
            .on_action(cx.listener(Self::set_next_statement))
            .on_action(cx.listener(Self::toggle_debug_panel))
            .on_action(cx.listener(Self::toggle_notes))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(|this, _: &DebugContinue, window, cx| {
                this.debugger.update(cx, |debugger, cx| debugger.start_or_continue(window, cx))
            }))
            .on_action(cx.listener(|this, _: &DebugStop, _, cx| this.debugger.update(cx, |debugger, cx| debugger.stop(cx))))
            .on_action(cx.listener(|this, _: &DebugRestart, window, cx| {
                this.debugger.update(cx, |debugger, cx| debugger.restart(window, cx))
            }))
            .on_action(cx.listener(|this, _: &DebugPause, _, cx| this.debugger.update(cx, |debugger, cx| debugger.pause(cx))))
            .on_action(cx.listener(|this, _: &StepOver, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_over(cx))))
            .on_action(cx.listener(|this, _: &StepInto, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_in(cx))))
            .on_action(cx.listener(|this, _: &StepOut, _, cx| this.debugger.update(cx, |debugger, cx| debugger.step_out(cx))))
            .relative()
            .size_full()
            .font_family(cx.theme().font_family.clone())
            .text_ui(cx)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_activity_bar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            // A right-click forgets the panel the last one was in; the
                            // side panels, inside, set theirs after.
                            .capture_any_mouse_down(|event: &MouseDownEvent, _, cx| {
                                if event.button == MouseButton::Right {
                                    menu::set_panel_under(None, None, cx);
                                }
                            })
                            .child(self.render_layout(window, cx)),
                    ),
            )
            // Across the whole window, as VS Code's.
            .child(self.render_status_bar(cx))
            .children(
                self.finder
                    .as_ref()
                    .map(|(finder, _)| finder.clone().into_any_element())
                    .or_else(|| self.symbols.as_ref().map(|search| search.picker.clone().into_any_element()))
                    .map(|picker| div().absolute().top(px(44.)).left_0().right_0().flex().justify_center().child(picker)),
            )
            .child(self.debug_hover.clone())
    }
}
