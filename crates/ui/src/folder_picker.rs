//! Folder browser for a server, through its agent (works the same locally
//! and over SSH): for choosing a folder to open, or making a new one.
//!
//! As VS Code's: the box is the path. Below it, the folders in the part up
//! to its last `/`, filtered by what follows it, after `..`. Enter (or Tab)
//! goes into the one selected; Cmd-Enter or Open opens what the box says.
//! A name that isn't there is offered as a new folder.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{Request, Response};

use crate::config::UiText;

/// The first row while nothing is typed after the last `/`: the folder above.
const UP: &str = "..";

pub enum FolderPickerEvent {
    Pick(PathBuf),
    Dismiss,
}

pub struct FolderPicker {
    client: Arc<Client>,
    title: SharedString,
    /// What the box says: the path.
    path: Entity<InputState>,
    /// The folder whose subfolders are listed, asked or shown.
    listed: Option<PathBuf>,
    /// Its subfolders.
    dirs: Vec<String>,
    selected: usize,
    loading: bool,
    error: Option<SharedString>,
    /// The path to put in the box on the next render (it needs the window).
    set_path: Option<String>,
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
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/folder"));
        let subscription = cx.subscribe(&path, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                this.selected = 0;
                this.sync(cx);
            }
            InputEvent::PressEnter { secondary: true, .. } => this.pick(cx),
            InputEvent::PressEnter { .. } => this.enter_selected(cx),
            _ => {}
        });
        path.update(cx, |path, cx| path.focus(window, cx));
        let mut picker = Self {
            client,
            title: title.into(),
            path,
            listed: None,
            dirs: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            set_path: None,
            load: None,
            _subscription: subscription,
        };
        picker.start(start, cx);
        picker
    }

    /// The box starts at `start` as the server names it: `~` is `/home/me`,
    /// so every folder above shows. An older agent doesn't resolve it.
    fn start(&mut self, start: PathBuf, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.load = Some(cx.spawn(async move |this, cx| {
            let start = match client.request(Request::Resolve { path: start.clone() }).await {
                Ok(Response::Path(Some(resolved))) => resolved,
                _ => start,
            };
            this.update(cx, |this, cx| this.go(&start, cx)).ok();
        }));
    }

    /// Puts folder `dir` in the box, ready to list or filter its subfolders.
    fn go(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let mut text = dir.to_string_lossy().into_owned();
        if !text.ends_with(['/', '\\']) {
            text.push(separator(&text));
        }
        self.set_path = Some(text);
        cx.notify();
    }

    fn text(&self, cx: &App) -> String {
        self.path.read(cx).value().to_string()
    }

    /// The box's folder (up to its last separator) and what follows it.
    fn parts(&self, cx: &App) -> (Option<PathBuf>, String) {
        let text = self.text(cx);
        match text.rfind(['/', '\\']) {
            Some(at) => (Some(PathBuf::from(&text[..=at])), text[at + 1..].to_string()),
            None => (None, text),
        }
    }

    /// Lists the box's folder if it isn't listed already.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (dir, _) = self.parts(cx);
        if dir == self.listed {
            cx.notify();
            return;
        }
        self.listed = dir.clone();
        self.dirs.clear();
        self.error = None;
        let Some(dir) = dir else {
            self.loading = false;
            cx.notify();
            return;
        };
        let client = self.client.clone();
        self.loading = true;
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = client.request(Request::ListDir { path: dir.clone() }).await;
            this.update(cx, |this, cx| {
                // Typed on meanwhile: another folder is being listed.
                if this.listed.as_ref() != Some(&dir) {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(Response::Dir(entries)) => {
                        this.dirs = entries.into_iter().filter(|entry| entry.is_dir).map(|entry| entry.name).collect();
                        this.dirs.sort_by_key(|name| name.to_lowercase());
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

    /// `..` while nothing follows the last separator (but at the root), and
    /// the subfolders with what does in their name, those starting with it
    /// first.
    fn visible(&self, cx: &App) -> Vec<String> {
        let (dir, typed) = self.parts(cx);
        let Some(dir) = dir else {
            return Vec::new();
        };
        let typed = typed.to_lowercase();
        let up = (typed.is_empty() && dir.parent().is_some()).then(|| UP.to_string());
        let mut dirs: Vec<String> = self.dirs.iter().filter(|name| name.to_lowercase().contains(&typed)).cloned().collect();
        dirs.sort_by_key(|name| !name.to_lowercase().starts_with(&typed));
        up.into_iter().chain(dirs).collect()
    }

    /// What follows the last separator, if no subfolder has that name:
    /// offered as a new folder, after those that match.
    fn new_name(&self, cx: &App) -> Option<String> {
        let (dir, name) = self.parts(cx);
        let name = name.trim().to_string();
        let valid = dir.is_some() && !name.is_empty() && name != "." && name != "..";
        (valid && !self.dirs.contains(&name) && !self.loading).then_some(name)
    }

    /// Into the folder selected (Enter, Tab), up for `..`, or makes the new one.
    fn enter_selected(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible(cx);
        let Some(dir) = self.parts(cx).0 else {
            return;
        };
        match visible.get(self.selected) {
            Some(name) if name == UP => {
                if let Some(parent) = dir.parent() {
                    self.go(parent, cx);
                }
            }
            Some(name) => self.go(&dir.join(name), cx),
            None if self.selected == visible.len() => self.create(cx),
            None => {}
        }
    }

    /// Makes the folder named after the last separator and goes into it.
    fn create(&mut self, cx: &mut Context<Self>) {
        let (Some(dir), Some(name)) = (self.parts(cx).0, self.new_name(cx)) else {
            return;
        };
        let (client, dir) = (self.client.clone(), dir.join(name));
        self.loading = true;
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = client.request(Request::CreateDir { path: dir.clone() }).await;
            this.update(cx, |this, cx| match result {
                Ok(_) => this.go(&dir, cx),
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

    /// What the box says, as the agent's machine names it: `~/x` would be a
    /// different workspace from `/home/me/x`, and only some requests expand it.
    fn pick(&mut self, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let trimmed = text.trim_end_matches(['/', '\\']);
        let path = PathBuf::from(if trimmed.is_empty() { text.as_str() } else { trimmed });
        if path.as_os_str().is_empty() {
            return;
        }
        let client = self.client.clone();
        self.load = Some(cx.spawn(async move |this, cx| {
            // An older agent doesn't know the request: the folder goes as it is.
            let path = match client.request(Request::Resolve { path: path.clone() }).await {
                Ok(Response::Path(Some(resolved))) => resolved,
                _ => path,
            };
            this.update(cx, |_, cx| cx.emit(FolderPickerEvent::Pick(path))).ok();
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

/// The separator a path uses: `\` for a Windows one without `/`.
fn separator(path: &str) -> char {
    if path.contains('\\') && !path.contains('/') { '\\' } else { '/' }
}

impl Render for FolderPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(text) = self.set_path.take() {
            self.path.update(cx, |path, cx| path.set_value(text, window, cx));
            self.selected = 0;
            self.sync(cx);
        }
        let visible = self.visible(cx);
        let new_name = self.new_name(cx);
        let nothing = visible.is_empty() && !self.loading && new_name.is_none() && self.error.is_none();
        let new_selected = self.selected == visible.len();
        let theme = cx.theme();
        let button = |id: &'static str, label: SharedString| {
            div()
                .id(id)
                .flex_none()
                .px_3()
                .py_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.bg(theme.secondary_hover))
                .child(label)
        };
        let row = |id: ElementId, selected: bool| {
            h_flex()
                .id(id)
                .h(px(26.))
                .flex_none()
                .px_2()
                .gap_2()
                .rounded(theme.radius)
                .when(selected, |el| el.bg(theme.accent))
                .hover(|style| style.bg(theme.accent.opacity(0.6)))
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
                match event.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, cx),
                    "down" => this.move_selection(1, cx),
                    "tab" => this.enter_selected(cx),
                    "escape" => cx.emit(FolderPickerEvent::Dismiss),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(div().px_1().text_ui_small(cx).font_semibold().text_color(theme.muted_foreground).child(self.title.clone()))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.path)))
                    .child(button("folder-cancel", "Cancel".into()).on_click(cx.listener(|_, _, _, cx| cx.emit(FolderPickerEvent::Dismiss))))
                    .child(
                        div()
                            .id("folder-pick")
                            .flex_none()
                            .px_3()
                            .py_1()
                            .rounded(theme.radius)
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .hover(|style| style.bg(theme.primary_hover))
                            .child("Open")
                            .on_click(cx.listener(|this, _, _, cx| this.pick(cx))),
                    ),
            )
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
                        let up = name == UP;
                        row(("folder", ix).into(), ix == self.selected)
                            .when(!up, |el| {
                                el.child(svg().path("icons/tree-folder.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                            })
                            .child(name.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = ix;
                                this.enter_selected(cx);
                            }))
                    }))
                    .children(new_name.map(|name| {
                        row("folder-new".into(), new_selected)
                            .text_color(theme.muted_foreground)
                            .child(svg().path("icons/plus.svg").size(px(14.)).flex_none().text_color(theme.muted_foreground))
                            .child(format!("New Folder “{name}”"))
                            .on_click(cx.listener(|this, _, _, cx| this.create(cx)))
                    }))
                    .when(self.loading, |el| {
                        el.child(div().px_2().py_1().text_ui_small(cx).text_color(theme.muted_foreground).child("…"))
                    })
                    .when(nothing, |el| {
                        el.child(div().px_2().py_1().text_ui_small(cx).text_color(theme.muted_foreground).child("No folders here"))
                    }),
            )
            .child(
                div()
                    .px_1()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child("Enter or Tab goes in · Cmd-Enter opens what the box says"),
            )
    }
}
