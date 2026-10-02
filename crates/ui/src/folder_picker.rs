//! Folder browser for a server, through its agent (works the same locally
//! and over SSH): for choosing a folder to open, or making a new one.

use std::{path::PathBuf, sync::Arc};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{Request, Response};

use crate::config::UiText;

pub enum FolderPickerEvent {
    Pick(PathBuf),
    Dismiss,
}

pub struct FolderPicker {
    client: Arc<Client>,
    title: SharedString,
    /// Folder being shown (may start with `~`, which the agent expands).
    dir: PathBuf,
    /// Its subfolders.
    dirs: Vec<String>,
    filter: Entity<InputState>,
    selected: usize,
    loading: bool,
    error: Option<SharedString>,
    /// The folder changed: the filter is cleared on the next render.
    clear_filter: bool,
    load: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<FolderPickerEvent> for FolderPicker {}

impl FolderPicker {
    pub fn new(
        client: Arc<Client>,
        title: impl Into<SharedString>,
        start: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter Folders"));
        let subscription = cx.subscribe(&filter, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                this.selected = 0;
                cx.notify();
            }
            InputEvent::PressEnter { secondary: true, .. } => this.pick(cx),
            InputEvent::PressEnter { .. } => this.enter_selected(cx),
            _ => {}
        });
        filter.update(cx, |filter, cx| filter.focus(window, cx));
        let mut picker = Self {
            client,
            title: title.into(),
            dir: start.clone(),
            dirs: Vec::new(),
            filter,
            selected: 0,
            loading: false,
            error: None,
            clear_filter: false,
            load: None,
            _subscription: subscription,
        };
        picker.open(start, cx);
        picker
    }

    fn open(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.loading = true;
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = client.request(Request::ListDir { path: dir.clone() }).await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(Response::Dir(entries)) => {
                        this.dir = dir;
                        this.dirs = entries.into_iter().filter(|entry| entry.is_dir).map(|entry| entry.name).collect();
                        this.selected = 0;
                        this.error = None;
                        this.clear_filter = true;
                    }
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The subfolders that match the filter (case-insensitive).
    fn visible(&self, cx: &App) -> Vec<String> {
        let filter = self.filter.read(cx).value().to_lowercase();
        self.dirs
            .iter()
            .filter(|name| name.to_lowercase().contains(&filter))
            .cloned()
            .collect()
    }

    /// What's typed in the filter, if no subfolder has that name: offered as
    /// a new folder, after those that match.
    fn new_name(&self, cx: &App) -> Option<String> {
        let name = self.filter.read(cx).value().trim().to_string();
        let valid = !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..";
        (valid && !self.dirs.contains(&name)).then_some(name)
    }

    fn enter_selected(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible(cx);
        match visible.get(self.selected) {
            Some(name) => {
                let dir = self.dir.join(name);
                self.open(dir, cx);
            }
            None if self.selected == visible.len() => self.create(cx),
            None => {}
        }
    }

    /// Makes the folder typed in the filter and goes into it.
    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.new_name(cx) else {
            return;
        };
        let (client, dir) = (self.client.clone(), self.dir.join(name));
        self.loading = true;
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = client.request(Request::CreateDir { path: dir.clone() }).await;
            this.update(cx, |this, cx| match result {
                Ok(_) => this.open(dir, cx),
                Err(err) => {
                    this.loading = false;
                    this.error = Some(format!("{err:#}").into());
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    /// Cmd-↑, like in Finder. From `~`, to the root.
    fn up(&mut self, cx: &mut Context<Self>) {
        let parent = match self.dir.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ if self.dir != PathBuf::from("/") => PathBuf::from("/"),
            _ => return,
        };
        self.open(parent, cx);
    }

    /// The folder shown, as the agent's machine names it: `~/x` would be a
    /// different workspace from `/home/me/x`, and only some requests expand it.
    fn pick(&mut self, cx: &mut Context<Self>) {
        let (client, dir) = (self.client.clone(), self.dir.clone());
        self.load = Some(cx.spawn(async move |this, cx| {
            // An older agent doesn't know the request: the folder goes as it is.
            let dir = match client.request(Request::Resolve { path: dir.clone() }).await {
                Ok(Response::Path(Some(resolved))) => resolved,
                _ => dir,
            };
            this.update(cx, |_, cx| cx.emit(FolderPickerEvent::Pick(dir))).ok();
        }));
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = (self.visible(cx).len() + usize::from(self.new_name(cx).is_some())) as isize;
        if len > 0 {
            self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
            cx.notify();
        }
    }
}

impl Render for FolderPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.clear_filter) {
            self.filter.update(cx, |filter, cx| filter.set_value("", window, cx));
        }
        let visible = self.visible(cx);
        let new_name = self.new_name(cx);
        let no_subfolders = visible.is_empty() && !self.loading && new_name.is_none();
        let new_selected = self.selected == visible.len();
        let theme = cx.theme();
        let name = self
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dir.display().to_string());
        let button = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.bg(theme.secondary_hover))
                .child(label)
        };
        v_flex()
            .id("folder-picker")
            .w(px(560.))
            .max_h(px(480.))
            .p_2()
            .gap_1()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .shadow_lg()
            .text_ui(cx)
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(FolderPickerEvent::Dismiss)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let keystroke = &event.keystroke;
                match keystroke.key.as_str() {
                    "up" if keystroke.modifiers.secondary() => this.up(cx),
                    "up" => this.move_selection(-1, cx),
                    "down" => this.move_selection(1, cx),
                    "escape" => cx.emit(FolderPickerEvent::Dismiss),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(div().px_1().text_ui_small(cx).font_semibold().text_color(theme.muted_foreground).child(self.title.clone()))
            .child(
                h_flex()
                    .px_1()
                    .gap_2()
                    .child(
                        div()
                            .id("folder-up")
                            .px_1()
                            .rounded(theme.radius)
                            .text_color(theme.muted_foreground)
                            .hover(|style| style.bg(theme.accent))
                            .child("↑")
                            .on_click(cx.listener(|this, _, _, cx| this.up(cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(self.dir.display().to_string()),
                    )
                    .when(self.loading, |el| el.child(div().text_color(theme.muted_foreground).child("…"))),
            )
            .child(Input::new(&self.filter))
            .children(self.error.clone().map(|error| {
                div().px_1().text_ui_small(cx).text_color(theme.danger).whitespace_normal().child(error)
            }))
            .child(
                v_flex()
                    .id("folder-picker-dirs")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(visible.iter().enumerate().map(|(ix, name)| {
                        let name = name.clone();
                        h_flex()
                            .id(("folder", ix))
                            .h(px(26.))
                            .px_2()
                            .gap_2()
                            .rounded(theme.radius)
                            .when(ix == self.selected, |el| el.bg(theme.accent))
                            .hover(|style| style.bg(theme.accent.opacity(0.6)))
                            .child(svg().path("icons/tree-folder.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                            .child(name.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let dir = this.dir.join(&name);
                                this.open(dir, cx);
                            }))
                    }))
                    .children(new_name.map(|name| {
                        h_flex()
                            .id("folder-new")
                            .h(px(26.))
                            .px_2()
                            .gap_2()
                            .rounded(theme.radius)
                            .when(new_selected, |el| el.bg(theme.accent))
                            .hover(|style| style.bg(theme.accent.opacity(0.6)))
                            .text_color(theme.muted_foreground)
                            .child(svg().path("icons/plus.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                            .child(format!("New Folder “{name}”"))
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                    }))
                    .when(no_subfolders, |el| {
                        el.child(div().px_2().py_1().text_ui_small(cx).text_color(theme.muted_foreground).child("No subfolders"))
                    }),
            )
            .child(
                h_flex()
                    .pt_1()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child("Enter to go in · Cmd-↑ to go up · Cmd-Enter to open · type a new name to create it"),
                    )
                    .child(button("folder-cancel", "Cancel".into()).on_click(cx.listener(|_, _, _, cx| cx.emit(FolderPickerEvent::Dismiss))))
                    .child(
                        div()
                            .id("folder-pick")
                            .px_3()
                            .py_1()
                            .rounded(theme.radius)
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .hover(|style| style.bg(theme.primary_hover))
                            .child(format!("Open “{name}”"))
                            .on_click(cx.listener(|this, _, _, cx| this.pick(cx))),
                    ),
            )
    }
}
