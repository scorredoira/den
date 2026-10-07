//! The debugger in the code: breakpoints in the gutter, values inline,
//! and Run and Debug on each test.

use super::*;

impl Workspace {
    pub(super) fn on_debug_event(&mut self, debugger: &Entity<Debugger>, event: &DebugEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DebugEvent::Show { path, line, focus } => {
                let goto = Position::new(*line, 0);
                let open = self.tabs.iter().position(|tab| tab.path == *path && tab.is_file());
                self.open_at_with(path.clone(), goto, true, *focus, window, cx);
                // a file already open only scrolls if the line isn't well in view
                if let Some(ix) = open {
                    let editor = self.tabs[ix].editor.clone();
                    reveal_in_center(&editor, *line, Some(STOP_MARGIN), cx);
                }
                if *focus && debugger.read(cx).is_stopped() {
                    window.activate_window();
                }
                self.refresh_debug_marks(cx);
            }
            DebugEvent::Marks => self.refresh_debug_marks(cx),
            DebugEvent::Run { session, term, line } => {
                let session = *session;
                let file = self.active_file().map(|ix| {
                    let path = &self.tabs[ix].path;
                    path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
                });
                let line = debug::command_line(line, file.as_deref());
                debugger.update(cx, |debugger, _| debugger.set_ran(session, line.clone(), file));
                self.show_panel(Panel::Console, cx);
                let run = self.terminals.update(cx, |terminals, cx| terminals.run_debug(*term, line, window, cx));
                let debugger = debugger.downgrade();
                cx.spawn(async move |_, cx| {
                    let term = run.await;
                    debugger.update(cx, |debugger, cx| debugger.set_terminal(session, term, cx)).ok();
                })
                .detach();
                cx.notify();
            }
            DebugEvent::Interrupt { term } => {
                self.terminals.update(cx, |terminals, cx| terminals.interrupt(*term, cx));
            }
            DebugEvent::Refocus => self.focus_active(window, cx),
            DebugEvent::Reveal => self.reveal_debugger(cx),
            DebugEvent::Hide => self.hide_panel(Panel::Console, cx),
            // the person picked something in the app or the page: Den comes
            // to the front, with that line
            DebugEvent::Raise => {
                cx.activate(true);
                window.activate_window();
            }
        }
    }

    /// Redraws the breakpoints and the line stopped at in every editor.
    pub(super) fn refresh_debug_marks(&mut self, cx: &mut Context<Self>) {
        let debugger = self.debugger.read(cx);
        let execution = debugger.execution();
        let theme = cx.theme();
        let (stop_line, stop_arrow) = (theme.warning.opacity(0.22), theme.warning);
        let (frame_line, frame_arrow) = (theme.info.opacity(0.14), theme.info);
        let (red, orange, gray) = (debug::panel::breakpoint_color(cx), theme.warning, theme.muted_foreground);
        let mut updates = Vec::new();
        for tab in &self.tabs {
            if tab.diff.is_some() || tab.doc {
                continue;
            }
            let marks: Vec<GutterMark> = debugger
                .breakpoints
                .of(&tab.path)
                .iter()
                .map(|bp| GutterMark {
                    line: bp.line as usize,
                    color: if !bp.enabled {
                        gray
                    } else if bp.error.is_some() || bp.is_special() {
                        orange
                    } else {
                        red
                    },
                    hollow: !bp.enabled || !bp.log.is_empty(),
                })
                .collect();
            let line = execution.as_ref().filter(|(path, _, _)| *path == tab.path).map(|(_, line, top)| ExecutionLine {
                line: *line as usize,
                background: if *top { stop_line } else { frame_line },
                arrow: if *top { stop_arrow } else { frame_arrow },
            });
            updates.push((tab.editor.clone(), marks, line));
        }
        for (editor, marks, line) in updates {
            editor.update(cx, |state, cx| {
                state.set_gutter_marks(marks, cx);
                state.set_execution_line(line, cx);
            });
        }
    }

    /// A click in an editor's gutter: left toggles a breakpoint, right edits
    /// its condition.
    pub(super) fn gutter_clicked(&mut self, editor: &Entity<EditorState>, line: u32, button: MouseButton, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tab_index(editor) else {
            return;
        };
        let tab = &self.tabs[ix];
        if tab.diff.is_some() || tab.doc || tab.image.is_some() {
            return;
        }
        let path = tab.path.clone();
        match button {
            MouseButton::Left => self.debugger.update(cx, |debugger, cx| debugger.toggle_breakpoint(&path, line, cx)),
            MouseButton::Right => {
                self.debugger.update(cx, |debugger, cx| debugger.edit_breakpoint(path, line, EditKind::Condition, window, cx))
            }
            _ => {}
        }
    }

    /// The file and line (0-based) of the active editor's cursor.
    pub(super) fn cursor_place(&self, cx: &App) -> Option<(PathBuf, u32)> {
        let ix = self.active?;
        let tab = &self.tabs[ix];
        if tab.diff.is_some() || tab.doc {
            return None;
        }
        Some((tab.path.clone(), tab.editor.read(cx).cursor_position().line))
    }

    pub(super) fn toggle_breakpoint(&mut self, _: &ToggleBreakpoint, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.toggle_breakpoint(&path, line, cx));
        }
    }

    /// The breakpoint editor at the cursor's line, with its condition or message.
    pub(super) fn edit_breakpoint_at_cursor(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.edit_breakpoint(path, line, kind, window, cx));
        }
    }

    /// The editor's selection, else the name or member chain (`a.b.c`) at the cursor.
    pub(super) fn expression_at_cursor(&self, cx: &App) -> Option<String> {
        let state = self.tabs[self.active?].editor.read(cx);
        let selected = state.selected_text().to_string().trim().to_string();
        if !selected.is_empty() {
            return (!selected.contains('\n')).then_some(selected);
        }
        let cursor = state.cursor_position();
        let line = state.text().to_string().lines().nth(cursor.line as usize)?.to_string();
        let offset = line.char_indices().nth(cursor.character as usize).map_or(line.len(), |(byte, _)| byte);
        // A cursor just past the name counts too.
        let span = debug::expression_span(&line, offset)
            .or_else(|| offset.checked_sub(1).and_then(|before| debug::expression_span(&line, before)))?;
        Some(line[span].to_string())
    }

    pub(super) fn add_to_watch(&mut self, _: &AddToWatch, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(expr) = self.expression_at_cursor(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.add_watch(expr, cx));
            self.show_panel(Panel::Console, cx);
        }
    }

    pub(super) fn evaluate_in_console(&mut self, _: &EvaluateInConsole, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(expr) = self.expression_at_cursor(cx) {
            self.debugger.update(cx, |debugger, cx| debugger.evaluate_in_console(expr, cx));
            self.show_panel(Panel::Console, cx);
        }
    }

    pub(super) fn run_to_cursor(&mut self, _: &RunToCursor, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, line)) = self.cursor_place(cx) {
            self.debugger.update(cx, |debugger, _| debugger.run_to(&path, line));
        }
    }

    pub(super) fn set_next_statement(&mut self, _: &SetNextStatement, _: &mut Window, cx: &mut Context<Self>) {
        let Some((path, line)) = self.cursor_place(cx) else {
            return;
        };
        let here = self.debugger.read(cx).execution().is_some_and(|(at, _, top)| at == path && top);
        if here {
            self.debugger.update(cx, |debugger, _| debugger.jump(line));
        } else {
            self.message = Some("The next statement must be in the function stopped at".into());
            cx.notify();
        }
    }

    pub(super) fn toggle_debug_panel(&mut self, _: &ToggleDebugPanel, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_panel(Panel::Console, cx);
    }

    /// Run and Debug at the end of each test's line, as the launch file's
    /// `tests` finds them.
    pub(super) fn test_lenses(&self, editor: &Entity<EditorState>, path: &Path, this: WeakEntity<Self>, cx: &App) -> Vec<AnyElement> {
        let Some(tests) = self.debugger.read(cx).tests.clone() else {
            return Vec::new();
        };
        let state = editor.read(cx);
        let Some(visible) = state.visible_row_range() else {
            return Vec::new();
        };
        let text = state.text();
        let area = state.input_bounds();
        let theme = cx.theme();
        let debugger = self.debugger.read(cx);
        let mut lenses = Vec::new();
        for row in visible.start..visible.end.min(text.lines_len()) {
            let line = text.slice_line(row).to_string();
            let Some(test) = tests.name_in(&line) else {
                continue;
            };
            let end = text.line_start_offset(row) + line.trim_end_matches(['\n', '\r']).len();
            let Some(bounds) = state.range_to_bounds(&(end..end)) else {
                continue;
            };
            let origin = point(bounds.origin.x + px(24.), bounds.origin.y);
            if bounds.origin.y < area.top() || bounds.bottom() > area.bottom() || area.right() - origin.x < px(120.) {
                continue;
            }
            // Debugging it: the bug turns until its program connects.
            let launching = debugger.is_launching(path, &test);
            let lens = |icon: &'static str, tip: &'static str, debug: bool| {
                let test = test.clone();
                let path = path.to_path_buf();
                let this = this.clone();
                div()
                    .id(SharedString::from(format!("test-{debug}-{row}")))
                    .when(cfg!(test), |el| el.debug_selector(move || format!("test-lens-{row}-{debug}")))
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .map(|el| {
                        if debug && launching {
                            el.child(crate::app::spinner(theme.muted_foreground))
                        } else {
                            el.child(svg().path(icon).size(px(13.)).text_color(theme.muted_foreground))
                        }
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
                    .on_click(move |_, window, cx| {
                        this.update(cx, |this, cx| this.run_test(&path, &test, debug, window, cx)).ok();
                    })
            };
            lenses.push(
                anchored()
                    .position(origin)
                    .child(
                        h_flex()
                            .h(bounds.size.height)
                            .gap_1()
                            .items_center()
                            .occlude()
                            .child(lens("icons/play.svg", "Run Test", false))
                            .child(lens("icons/bug.svg", "Debug Test", true)),
                    )
                    .into_any_element(),
            );
        }
        lenses
    }

    /// Runs the test `test` of `path` in a terminal, or debugs it.
    pub(super) fn run_test(&mut self, path: &Path, test: &str, debug: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tests) = self.debugger.read(cx).tests.clone() else {
            return;
        };
        let file = path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/");
        let command = debug::with_target(&tests.command(debug, test), self.debugger.read(cx).target());
        let line = debug::command_line(&command, Some(&file));
        if debug {
            self.debugger.update(cx, |debugger, cx| {
                debugger.launch_command(line, tests.port, path.to_path_buf(), test.to_string(), window, cx)
            });
            return;
        }
        self.show_panel(Panel::Terminals, cx);
        let run = self.terminals.update(cx, |terminals, cx| terminals.run_line(self.test_term, line, window, cx));
        cx.spawn(async move |this, cx| {
            let term = run.await;
            this.update(cx, |this, _| this.test_term = term).ok();
        })
        .detach();
        cx.notify();
    }
}
