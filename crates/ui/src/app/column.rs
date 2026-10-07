//! Drawing the workspaces column: each server, its repos and their worktrees.

use super::*;

impl Den {
    /// The workspaces: with `header`, its title above them, as without a
    /// workspace open (with one, its panel's header is the side column's).
    pub(super) fn render_column(&self, header: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.render_tasks(cx);
        let header = header.then(|| self.render_tasks_header(cx));
        let theme = cx.theme();
        v_flex()
            .id("task-column")
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .children(header)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel(window, cx);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }

    /// The workspaces' title, with what adds to them.
    pub(super) fn render_tasks_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let weak = cx.entity().downgrade();
        h_flex()
            .id("tasks-header")
            .h(px(34.))
            .flex_none()
            .px_3()
            .border_b_1()
            .border_color(theme.sidebar_border)
            .child(div().flex_1().text_ui_small(cx).text_color(theme.muted_foreground).child("PROJECTS"))
            .child(tasks_add_button(&weak))
            .context_menu(move |menu, window, cx| column_menu(menu, &weak, window, cx))
            .into_any_element()
    }

    /// The activity bar, with only the explorer's icon; the workspaces, as
    /// wide as the side column; and the welcome.
    pub(super) fn render_without_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let badge = self.task_badges(cx).workspaces.map(Badge::Dot);
        let click: OnActivity = Rc::new(|_, _, _| {});
        let bar = activity_bar(vec![(Item::Place(Place(Panel::Workspaces)), true, badge)], Vec::new(), click, cx);
        let width = Config::get(cx).layout.side_width;
        let state = self.split.state(window.viewport_size().width - px(ACTIVITY_WIDTH), [true, true], cx).clone();
        let split = h_resizable("den-split")
            .with_state(&state)
            .child(
                resizable_panel()
                    .size(config::width(width, 160., 800.))
                    .size_range(px(160.)..px(800.))
                    .child(self.render_column(true, cx)),
            )
            .child(resizable_panel().child(match &self.guide {
                Some(guide) => self.render_guide(guide, cx),
                None => self.render_welcome(cx),
            }))
;
        h_flex().size_full().child(bar).child(div().flex_1().min_w_0().h_full().child(split)).into_any_element()
    }

    pub(super) fn render_host_header(&self, host: &Host, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        // The connection's state is the icon's color.
        let (color, detail): (Hsla, Option<SharedString>) = match &host.status {
            HostStatus::Connected => (theme.success, None),
            HostStatus::Connecting(step) => (theme.muted_foreground, Some((*step).into())),
            HostStatus::Failed(err) => (theme.danger, Some(err.clone())),
        };
        let name = host.name.clone();
        let retry = matches!(host.status, HostStatus::Failed(_));
        let outdated = host.client.as_ref().is_some_and(|client| client.outdated());
        let restart = name.clone();
        let connected = host.client.is_some();
        let keep = self.unkept(cx);
        let weak = cx.entity().downgrade();
        let menu_name = name.clone();
        let hidden = is_hidden_host(&name, cx);
        v_flex()
            .px_3()
            .pt_2()
            .pb_1()
            .when(hidden, |el| el.opacity(0.5))
            .child(
                h_flex()
                    .id(SharedString::from(format!("host-{name}")))
                    .group("host")
                    .h(px(20.))
                    .gap_1()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(
                        svg()
                            .path(if host.destination.is_none() { "icons/monitor.svg" } else { "icons/server.svg" })
                            .size(px(12.))
                            .text_color(color),
                    )
                    .child(name.clone())
                    .child(div().flex_1())
                    .when(connected, |el| {
                        let open = name.clone();
                        el.child(
                            div()
                                .id(SharedString::from(format!("host-open-{name}")))
                                .invisible()
                                .group_hover("host", |style| style.visible())
                                .rounded(theme.radius)
                                .hover(|style| style.bg(theme.sidebar_accent))
                                .child(svg().path("icons/plus.svg").size(px(12.)).text_color(theme.muted_foreground))
                                .tooltip({
                                    let tip = if open == LOCAL { "Open Folder…".to_string() } else { format!("Open Folder on {open}…") };
                                    move |window, cx| Tooltip::new(tip.clone()).build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_folder_on(open.clone(), window, cx)
                                })),
                        )
                    })
                    .when(retry, |el| {
                        el.child(div().hover(|style| style.underline()).child("retry"))
                        .on_click(cx.listener(move |this, _, window, cx| this.connect(name.clone(), window, cx)))
                    })
                    .when(outdated, |el| {
                        el.child(
                            div()
                                .text_color(theme.warning)
                                .hover(|style| style.underline())
                                .child("outdated agent · restart"),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| this.ask_restart(restart.clone(), window, cx)))
                    })
                    .context_menu(move |menu, window, cx| {
                        let host = menu_name.clone();
                        host_menu(menu, &menu_name, connected, keep, &weak)
                            .item(menu::item(if hidden { "Show Server" } else { "Hide Server" }, &weak, move |this, _, cx| {
                                this.set_host_hidden(&host, !hidden, cx)
                            }))
                            .item(show_hidden_item(&weak, cx))
                            .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                    }),
            )
            .children(detail.map(|detail| {
                div()
                    .pl_3()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(detail)
            }))
            .into_any_element()
    }

    /// The projects, by server: a row each. Hidden ones only with Show
    /// Hidden Projects, or while in front.
    pub(super) fn render_tasks(&self, cx: &mut Context<Self>) -> AnyElement {
        let ordered = self.ordered_all(cx);
        let show_hidden = Config::get(cx).show_hidden_projects;
        let active = self.active_project();
        let mut sections: Vec<AnyElement> = Vec::new();
        for host in &self.hosts {
            let entries: Vec<(TaskKey, &TaskInfo)> =
                ordered.iter().filter(|(key, _)| key.host == host.name).cloned().collect();
            let mut projects = Vec::new();
            for group in entries.chunk_by(|(_, a), (_, b)| a.repo == b.repo) {
                let Some((key, task)) = group.first() else {
                    continue;
                };
                let project = project_of(key, task);
                let hidden = self.is_hidden_project(&project, cx);
                if hidden && !show_hidden && active.as_ref() != Some(&project) {
                    continue;
                }
                projects.push(self.render_project(project, hidden, group, cx));
            }
            // A hidden server shows only for its project in front.
            if is_hidden_host(&host.name, cx) && !show_hidden && projects.is_empty() {
                continue;
            }
            sections.push(self.render_host_header(host, cx));
            sections.extend(projects);
        }
        let theme = cx.theme();
        // Nothing in it yet: say what it's for.
        let hint = self.hosts.iter().all(|host| host.tasks.is_empty() && host.loose.is_empty()).then(|| {
            v_flex()
                .px_3()
                .py_2()
                .gap_1()
                .text_ui_small(cx)
                .text_color(theme.muted_foreground)
                .child(div().whitespace_normal().child(
                    "The folders you open stay here. A git repo shows its worktrees under it, each a workspace with its own terminals and Claude Code session.",
                ))
                .child(
                    div()
                        .id("hint-open-folder")
                        .text_color(theme.sidebar_foreground)
                        .hover(|style| style.underline())
                        .child("Open Folder…")
                        .on_click(cx.listener(|this, _, window, cx| this.open_folder(&OpenFolder, window, cx))),
                )
        });
        let weak = cx.entity().downgrade();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .id("task-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(sections)
                    .children(hint)
                    // The empty space below: right-click to add things.
                    .child(
                        div()
                            .id("task-list-space")
                            .flex_1()
                            .min_h(px(32.))
                            .context_menu(move |menu, window, cx| column_menu(menu, &weak, window, cx)),
                    ),
            )
            .into_any_element()
    }

    /// A project's row, dimmed while hidden: a folder, or a repo's checkout
    /// with its worktrees under it, which its chevron folds away. A click
    /// enters the checkout; folded, the worktree of it used last, and its
    /// dot is the most urgent of all of them.
    pub(super) fn render_project(&self, project: TaskKey, hidden: bool, group: &[(TaskKey, &TaskInfo)], cx: &mut Context<Self>) -> AnyElement {
        let new_task = self
            .new_task
            .as_ref()
            .filter(|form| form.host == project.host && form.repo == project.path)
            .map(|form| render_new_task(form, cx));
        let checkout = group.iter().find(|(key, _)| *key == project);
        let worktrees: Vec<&(TaskKey, &TaskInfo)> = group.iter().filter(|(key, _)| *key != project).collect();
        let tree = !worktrees.is_empty() || new_task.is_some();
        let folded = tree && Config::get(cx).folded_projects.contains(&project.config());
        let active = match folded {
            true => self.active_project().as_ref() == Some(&project),
            false => checkout.is_some() && self.active.as_ref() == Some(&project),
        };
        let own: Vec<&(TaskKey, &TaskInfo)> = match folded {
            true => group.iter().collect(),
            false => checkout.into_iter().collect(),
        };
        let (dot, color) = own
            .iter()
            .map(|(key, task)| self.status(key, task, cx))
            .filter(|(dot, _)| *dot != "…")
            .max_by_key(|(dot, color)| urgency(dot, *color, cx))
            .unwrap_or(("○", cx.theme().muted_foreground));
        let theme = cx.theme();
        let known = self
            .host(&project.host)
            .is_some_and(|host| host.tasks.iter().any(|other| other.path == project.path));
        let git = self.is_repo(&project);
        let local = project.host == LOCAL;
        let connected = self.client(&project.host).is_some();
        let name = folder_name(&project.path);
        let label: SharedString = name.clone().into();
        let weak = cx.entity().downgrade();
        // Removing it would close its checkout, unsaved changes and all.
        let unsaved = self
            .workspaces
            .get(&project)
            .is_some_and(|workspace| !workspace.read(cx).unsaved().is_empty());

        // On hover, as in its menu: a repo makes a worktree.
        let action = (git && connected).then(|| {
            let (host, repo) = (project.host.clone(), project.path.clone());
            row_action(
                format!("task-new-{}", project.config()),
                "icons/plus.svg",
                format!("New Worktree in {name}…"),
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.start_new_task(host.clone(), repo.clone(), window, cx)
                }),
                cx,
            )
        });
        let chevron = tree.then(|| {
            let path = match folded {
                true => "icons/tree-chevron-right.svg",
                false => "icons/tree-chevron-down.svg",
            };
            let project = project.clone();
            div()
                .id(SharedString::from(format!("project-fold-{}", project.config())))
                .flex_none()
                .child(svg().path(path).size(px(14.)).text_color(theme.muted_foreground))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_project_folded(&project, !folded, cx);
                }))
        });

        let row = h_flex()
            .id(SharedString::from(format!("project-{}", project.config())))
            .group("task")
            .h(px(24.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .when(hidden, |row| row.opacity(0.5))
            .when(active, |el| el.bg(selected_row(cx)))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            // Its agents' state, and only the dot.
            .child(div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot))
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
            .child(div().flex_1())
            .children(action)
            .children(chevron)
            .on_drag(TaskDrag { key: project.clone(), label }, |drag, _, _, cx| cx.new(|_| DragPreview(drag.label.clone())))
            .drag_over::<TaskDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
            .on_drop(cx.listener({
                let project = project.clone();
                move |this, drag: &TaskDrag, _, cx| this.move_task(&drag.key, &project, cx)
            }))
            .on_click(cx.listener({
                let project = project.clone();
                let enters = checkout.is_some() && !folded;
                move |this, _, window, cx| match enters {
                    true => this.activate(project.clone(), window, cx),
                    false => this.enter_project(project.clone(), window, cx),
                }
            }))
            .tooltip({
                let path = project.path.display().to_string();
                move |window, cx| Tooltip::new(path.clone()).build(window, cx)
            })
            .context_menu({
                let project = project.clone();
                move |menu, window, cx| {
                    let (create, init, add, window_key, copy, finder, hide, close) = (
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                        project.clone(),
                    );
                    menu.when(git, |menu| {
                        menu.item(
                            menu::item(format!("New Worktree in {name}…"), &weak, move |this, window, cx| {
                                this.start_new_task(create.host.clone(), create.path.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .when(known && !git, |menu| {
                        menu.item(
                            menu::item("Initialize Git Repository", &weak, move |this, window, cx| {
                                this.init_git(init.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .when(!known, |menu| {
                        menu.item(
                            menu::item("Add to Workspaces", &weak, move |this, window, cx| {
                                if this.remembers() {
                                    this.add_folder(add.host.clone(), add.path.clone(), window, cx)
                                } else {
                                    this.keep(Some(add.clone()), window, cx)
                                }
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    .item(menu::item("Open in New Window", &weak, move |this, _, cx| {
                        let key = window_key.clone();
                        let destination = this.host(&key.host).and_then(|host| host.destination.clone());
                        let except = cx.entity_id();
                        cx.defer(move |cx| open_new_window_except(key.host, destination, key.path, None, Some(except), cx));
                    }))
                    .separator()
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path)).disabled(!local),
                    )
                    .separator()
                    // Its own hiding: one hidden with its server shows with it.
                    .item({
                        let hidden = Config::get(cx).hidden_projects.contains(&hide.config());
                        menu::item(if hidden { "Show Project" } else { "Hide Project" }, &weak, move |this, _, cx| {
                            this.set_project_hidden(&hide, !hidden, cx)
                        })
                    })
                    .item(
                        menu::item("Remove from Workspaces", &weak, move |this, window, cx| {
                            this.set_project_hidden(&close, false, cx);
                            if known {
                                this.remove_folder(&close, close.path.clone(), window, cx)
                            } else {
                                this.close_folder(&close, window, cx)
                            }
                        })
                        .disabled(unsaved),
                    )
                    .separator()
                    .item(show_hidden_item(&weak, cx))
                    .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                }
            });

        // What went wrong with it (its worktrees' under them).
        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| *target == project)
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));
        let rows: Vec<AnyElement> = match tree && !folded {
            true => worktrees.iter().map(|(key, task)| self.render_task(key, task, cx)).chain(new_task).collect(),
            false => Vec::new(),
        };
        let line = cx.theme().sidebar_border;
        v_flex()
            .child(row)
            .children(error)
            .when(!rows.is_empty(), |el| el.child(v_flex().ml(px(18.)).border_l_1().border_color(line).children(rows)))
            .into_any_element()
    }

    /// A workspace's state: being deleted, or the most urgent of its
    /// agents' (waiting for an answer, working, done unseen, idle).
    pub(super) fn status(&self, key: &TaskKey, _: &TaskInfo, cx: &App) -> (&'static str, Hsla) {
        if self.removing.contains(key) {
            return ("…", cx.theme().muted_foreground);
        }
        let (dot, color, _) = self.workspace_state(key, cx);
        (dot, color)
    }

    /// A workspace's row under its project: its state's dot and its branch;
    /// on hover, Delete Worktree on a worktree.
    pub(super) fn render_task(&self, key: &TaskKey, task: &TaskInfo, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active.as_ref() == Some(key);
        let (dot, color) = self.status(key, task, cx);
        let theme = cx.theme();
        let known = self
            .host(&key.host)
            .is_some_and(|host| host.tasks.iter().any(|other| other.path == key.path));
        let removable = known && !task.main;
        let local = key.host == LOCAL;
        let weak = cx.entity().downgrade();
        let connected = self.client(&key.host).is_some();
        let repo_name = folder_name(&task.repo);
        // Only a repo makes worktrees.
        let git = known && self.is_repo(&project_of(key, task));
        let label = worktree_label(task);
        // On hover, as in its menu: a worktree is deleted (its project's
        // row makes one).
        let action = if removable && !self.removing.contains(key) {
            let remove = key.clone();
            Some(row_action(
                format!("task-delete-{}", key.config()),
                "icons/trash.svg",
                "Delete Worktree…".into(),
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.ask_remove(remove.clone(), window, cx)
                }),
                cx,
            ))
        } else {
            None
        };

        let row = h_flex()
            .id(SharedString::from(format!("task-{}", key.config())))
            .group("task")
            // On its way out.
            .when(self.removing.contains(key), |row| row.opacity(0.5))
            .h(px(24.))
            .px_3()
            .gap_2()
            .text_ui(cx)
            .when(active, |el| el.bg(selected_row(cx)))
            .when(!active, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
            // Its agents' state, the same dot as in the Agents panel, and
            // only the dot: no word for it ("working", "done"…), ever.
            .child(match dot {
                "…" => spinner(color).into_any_element(),
                _ => div().flex_none().w(px(12.)).text_ui_small(cx).text_color(color).child(dot).into_any_element(),
            })
            .child(div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(label.clone()))
            .child(div().flex_1())
            .children(action)
            .on_drag(
                TaskDrag {
                    key: key.clone(),
                    label: label.clone(),
                },
                |drag, _, _, cx| cx.new(|_| DragPreview(drag.label.clone())),
            )
            .drag_over::<TaskDrag>(|style, _, _, cx| style.border_t_2().border_color(cx.theme().primary))
            .on_drop(cx.listener({
                let key = key.clone();
                move |this, drag: &TaskDrag, _, cx| this.move_task(&drag.key, &key, cx)
            }))
            .on_click(cx.listener({
                let key = key.clone();
                move |this, _, window, cx| this.activate(key.clone(), window, cx)
            }))
            .tooltip({
                let path = key.path.display().to_string();
                move |window, cx| Tooltip::new(path.clone()).build(window, cx)
            })
            .context_menu({
                let key = key.clone();
                let repo = task.repo.clone();
                let repo_name = repo_name.clone();
                let branch = task.branch.clone();
                move |menu, window, cx| {
                    let (create, copy, finder, remove) = (key.clone(), key.clone(), key.clone(), key.clone());
                    let repo = repo.clone();
                    let (window_key, branch) = (key.clone(), branch.clone());
                    menu.when(git, |menu| {
                        menu.item(
                            menu::item(format!("New Worktree in {repo_name}…"), &weak, move |this, window, cx| {
                                this.start_new_task(create.host.clone(), repo.clone(), window, cx)
                            })
                            .disabled(!connected),
                        )
                        .separator()
                    })
                    // What `den -n <path>` does: a window with only it (and
                    // its server).
                    .item(menu::item("Open in New Window", &weak, move |this, _, cx| {
                        let key = window_key.clone();
                        let destination = this.host(&key.host).and_then(|host| host.destination.clone());
                        let except = cx.entity_id();
                        cx.defer(move |cx| open_new_window_except(key.host, destination, key.path, None, Some(except), cx));
                    }))
                    .separator()
                    .item(menu::item("Copy Path", &weak, move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.path.to_string_lossy().into_owned()))
                    }))
                    .when_some(branch, |menu, branch| {
                        menu.item(menu::item("Copy Branch Name", &weak, move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()))
                        }))
                    })
                    .item(
                        menu::item("Reveal in Finder", &weak, move |_, _, cx| cx.reveal_path(&finder.path))
                            .disabled(!local),
                    )
                    .when(removable, |menu| {
                        menu.separator().item(menu::item("Delete Worktree…", &weak, move |this, window, cx| {
                            this.ask_remove(remove.clone(), window, cx)
                        }))
                    })
                    .separator()
                    .panel_items(hide_panel(&weak, Panel::Workspaces), window, cx)
                }
            });


        // Under its row.
        let error = self
            .error
            .as_ref()
            .filter(|(target, _)| target == key)
            .map(|(_, error)| div().mx_3().mb_1().child(error_text(error.clone(), cx)));

        v_flex().child(row).children(error).into_any_element()
    }
}
