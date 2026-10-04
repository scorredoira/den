//! File tree: lazy loading, keyboard (arrows with preview, Enter renames) and
//! context menu. Everything goes through the agent on the task's machine, so
//! it works the same locally as on a server.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use client::Client;
use proto::{Request, Response};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use crate::menu::PanelItems as _;

use crate::{CollapseFileTree, config::UiText};

actions!(
    file_tree,
    [SelectPrev, SelectNext, Collapse, Expand, Rename, Trash, OpenSelected]
);

/// Tree shortcuts: they only apply when the tree has focus.
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("FileTree");
    vec![
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("enter", Rename, context),
        KeyBinding::new("secondary-backspace", Trash, context),
        KeyBinding::new("secondary-down", OpenSelected, context),
    ]
}

pub enum FileTreeEvent {
    /// Open a file; `pin` is false for the preview.
    Open { path: PathBuf, pin: bool },
    Renamed { from: PathBuf, to: PathBuf },
    Trashed { path: PathBuf },
    /// Show the commits that changed a file or folder.
    ShowHistory { path: PathBuf, dir: bool },
    Error(SharedString),
}

#[derive(Clone, PartialEq)]
struct DirEntry {
    path: PathBuf,
    name: SharedString,
    is_dir: bool,
}

#[derive(Clone)]
enum RowKind {
    Entry(DirEntry),
    /// The row where the name of something new is typed.
    New,
}

#[derive(Clone)]
struct Row {
    kind: RowKind,
    depth: usize,
}

impl Row {
    fn path(&self) -> Option<&Path> {
        match &self.kind {
            RowKind::Entry(entry) => Some(&entry.path),
            RowKind::New => None,
        }
    }
}

#[derive(Clone, PartialEq)]
enum EditKind {
    Rename(PathBuf),
    NewFile { dir: PathBuf },
    NewFolder { dir: PathBuf },
}

struct Edit {
    kind: EditKind,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// File tree with lazy loading: a folder is read when expanded.
pub struct FileTree {
    root: PathBuf,
    client: Option<Arc<Client>>,
    /// On this machine (not on a server): Finder is available.
    local: bool,
    /// Folders requested from the agent that haven't arrived yet.
    loading: HashSet<PathBuf>,
    children: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    edit: Option<Edit>,
    /// Row the right-click menu was opened on; `None` is the empty space (the
    /// task's folder).
    menu_target: Option<PathBuf>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, local: bool, cx: &mut Context<Self>) -> Self {
        let mut tree = Self {
            expanded: HashSet::from([root.clone()]),
            root,
            client,
            local,
            loading: HashSet::new(),
            children: HashMap::new(),
            rows: Vec::new(),
            selected: None,
            edit: None,
            menu_target: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        };
        tree.rebuild(cx);
        tree
    }

    /// Marks the path as selected, expands its folders and scrolls it into view.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path) {
            return;
        }
        self.selected = Some(path.to_path_buf());
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(d.to_path_buf());
            dir = d.parent();
        }
        self.rebuild(cx);
        self.scroll_to_selected();
        cx.notify();
    }

    fn scroll_to_selected(&mut self) {
        let selected = self.selected.as_deref();
        if let Some(ix) = self.rows.iter().position(|row| row.path() == selected) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// Rereads the folders affected by paths that changed on disk.
    pub fn invalidate<'a>(&mut self, paths: impl IntoIterator<Item = &'a PathBuf>, cx: &mut Context<Self>) {
        // Re-request the affected folders already read, without removing what's
        // shown while they arrive (removing it would make the tree flicker).
        let dirs: HashSet<PathBuf> = paths
            .into_iter()
            .flat_map(|path| [Some(path.as_path()), path.parent()])
            .flatten()
            .filter(|dir| self.children.contains_key(*dir))
            .map(Path::to_path_buf)
            .collect();
        for dir in dirs {
            self.load_dir(dir, cx);
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.push_rows(&root, 0, &mut rows, cx);
        self.rows = rows;
    }

    fn push_rows(&mut self, dir: &Path, depth: usize, rows: &mut Vec<Row>, cx: &mut Context<Self>) {
        if !self.children.contains_key(dir) {
            // Ask the agent; rebuild when it arrives.
            self.load_dir(dir.to_path_buf(), cx);
            return;
        }
        // New items are typed at the top of their folder.
        if let Some(Edit {
            kind: EditKind::NewFile { dir: target } | EditKind::NewFolder { dir: target },
            ..
        }) = &self.edit
            && target == dir
        {
            rows.push(Row { kind: RowKind::New, depth });
        }
        let entries = self.children[dir].clone();
        for entry in entries {
            let expand = entry.is_dir && self.expanded.contains(&entry.path);
            let path = entry.path.clone();
            rows.push(Row {
                kind: RowKind::Entry(entry),
                depth,
            });
            if expand {
                self.push_rows(&path, depth + 1, rows, cx);
            }
        }
    }

    /// Switches to a new connection with the agent: everything is reread.
    pub fn set_client(&mut self, client: Arc<Client>, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.children.clear();
        self.loading.clear();
        self.rebuild(cx);
        cx.notify();
    }

    fn load_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.loading.insert(dir.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let response = client.request(Request::ListDir { path: dir.clone() }).await;
            this.update(cx, |this, cx| {
                this.loading.remove(&dir);
                let entries = match response {
                    Ok(Response::Dir(entries)) => entries
                        .into_iter()
                        .map(|entry| DirEntry {
                            path: dir.join(&entry.name),
                            name: entry.name.into(),
                            is_dir: entry.is_dir,
                        })
                        .collect(),
                    // A folder that no longer exists (or can't be read) is left empty.
                    _ => Vec::new(),
                };
                // Only repaint if something actually changed.
                if this.children.get(&dir) == Some(&entries) {
                    return;
                }
                this.children.insert(dir, entries);
                this.rebuild(cx);
                this.scroll_to_selected();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Sends an operation to the agent; if it succeeds, runs `then`.
    fn fs_op(
        &mut self,
        request: Request,
        then: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return cx.emit(FileTreeEvent::Error("No agent".into()));
        };
        cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                match response {
                    Ok(_) => then(this, cx),
                    Err(err) => cx.emit(FileTreeEvent::Error(format!("{err:#}").into())),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn entry(&self, path: &Path) -> Option<&DirEntry> {
        self.rows.iter().find_map(|row| match &row.kind {
            RowKind::Entry(entry) if entry.path == path => Some(entry),
            _ => None,
        })
    }

    fn selected_entry(&self) -> Option<DirEntry> {
        self.entry(self.selected.as_deref()?).cloned()
    }

    fn click(&mut self, path: PathBuf, click_count: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(&path).cloned() else {
            return;
        };
        self.cancel_edit(cx);
        self.selected = Some(path.clone());
        if entry.is_dir {
            self.toggle(&path, cx);
            self.focus_handle.focus(window, cx);
        } else if click_count >= 2 {
            cx.emit(FileTreeEvent::Open { path, pin: true });
        } else {
            // A single click previews and keeps the keyboard in the tree.
            cx.emit(FileTreeEvent::Open { path, pin: false });
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Collapses every folder but the root.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.retain(|dir| *dir == self.root);
        // The selection moves up to the folder that now hides it.
        if let Some(top) = self.selected.as_ref().and_then(|path| {
            let first = path.strip_prefix(&self.root).ok()?.components().next()?;
            Some(self.root.join(first))
        }) {
            self.selected = Some(top);
        }
        self.rebuild(cx);
        self.scroll_to_selected();
        cx.notify();
    }

    fn toggle(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_path_buf());
        }
        self.rebuild(cx);
    }

    fn select_offset(&mut self, offset: isize, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.rows.iter().filter_map(|row| row.path().map(Path::to_path_buf)).collect();
        if paths.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| paths.iter().position(|path| path == selected));
        let next = match current {
            Some(ix) => (ix as isize + offset).clamp(0, paths.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(paths[next].clone());
        self.scroll_to_selected();
        if let Some(entry) = self.selected_entry()
            && !entry.is_dir
        {
            cx.emit(FileTreeEvent::Open {
                path: entry.path,
                pin: false,
            });
        }
        cx.notify();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select_offset(1, cx);
    }

    fn collapse(&mut self, _: &Collapse, _: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.is_dir && self.expanded.contains(&entry.path) {
            self.toggle(&entry.path, cx);
        } else if let Some(parent) = entry.path.parent()
            && parent != self.root
        {
            self.selected = Some(parent.to_path_buf());
            self.scroll_to_selected();
        }
        cx.notify();
    }

    fn expand(&mut self, _: &Expand, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry()
            && entry.is_dir
            && !self.expanded.contains(&entry.path)
        {
            self.toggle(&entry.path, cx);
            cx.notify();
        }
    }

    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.is_dir {
            self.toggle(&entry.path, cx);
            cx.notify();
        } else {
            cx.emit(FileTreeEvent::Open {
                path: entry.path,
                pin: true,
            });
        }
    }

    /// Enter: renames the selection or, while a name is being typed, confirms it.
    /// The single-line `Input` lets Enter through to here, so this is the only
    /// place that confirms (otherwise, after creating, the selection would be renamed).
    fn rename_selected(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            return self.commit_edit(window, cx);
        }
        if let Some(path) = self.selected.clone() {
            self.start_edit(EditKind::Rename(path), window, cx);
        }
    }

    fn trash_selected(&mut self, _: &Trash, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected.clone() {
            self.trash(path, cx);
        }
    }

    /// Folder where something new is created from `path`: itself or its parent.
    fn dir_for(&self, path: &Path) -> PathBuf {
        if path == self.root {
            return self.root.clone();
        }
        match self.entry(path) {
            Some(entry) if entry.is_dir => entry.path.clone(),
            _ => path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
        }
    }

    /// The folder a new file's name is being typed in.
    #[cfg(test)]
    pub fn new_file_dir(&self) -> Option<&Path> {
        match &self.edit {
            Some(Edit { kind: EditKind::NewFile { dir }, .. }) => Some(dir),
            _ => None,
        }
    }

    /// Types a new file's name at the top of `dir`, its folders open.
    pub fn new_file_in(&mut self, dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let mut open = Some(dir.as_path());
        while let Some(d) = open
            && d.starts_with(&self.root)
        {
            self.expanded.insert(d.to_path_buf());
            open = d.parent();
        }
        self.start_edit(EditKind::NewFile { dir }, window, cx);
    }

    fn start_edit(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        let initial = match &kind {
            EditKind::Rename(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            _ => String::new(),
        };
        if let EditKind::NewFile { dir } | EditKind::NewFolder { dir } = &kind {
            self.expanded.insert(dir.clone());
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Blur = event {
                this.cancel_edit(cx);
            }
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.edit = Some(Edit {
            kind,
            input,
            _subscription: subscription,
        });
        self.rebuild(cx);
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.edit.take().is_some() {
            self.rebuild(cx);
            cx.notify();
        }
    }

    fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.take() else {
            return;
        };
        let name = edit.input.read(cx).value().trim().to_string();
        self.rebuild(cx);
        self.focus_handle.focus(window, cx);
        cx.notify();
        if name.is_empty() {
            return;
        }
        if name.contains('/') || name == "." || name == ".." {
            return cx.emit(FileTreeEvent::Error("Invalid name".into()));
        }
        match edit.kind {
            EditKind::Rename(from) => {
                let to = from.with_file_name(&name);
                if to == from {
                    return;
                }
                self.fs_op(
                    Request::Rename { from: from.clone(), to: to.clone() },
                    move |this, cx| {
                        this.after_change(&[&from, &to], Some(to.clone()), cx);
                        cx.emit(FileTreeEvent::Renamed { from, to });
                    },
                    cx,
                );
            }
            EditKind::NewFile { dir } => {
                let path = dir.join(&name);
                self.fs_op(
                    Request::CreateFile { path: path.clone() },
                    move |this, cx| {
                        this.after_change(&[&path], Some(path.clone()), cx);
                        cx.emit(FileTreeEvent::Open { path, pin: true });
                    },
                    cx,
                );
            }
            EditKind::NewFolder { dir } => {
                let path = dir.join(&name);
                self.fs_op(
                    Request::CreateDir { path: path.clone() },
                    move |this, cx| this.after_change(&[&path], Some(path.clone()), cx),
                    cx,
                );
            }
        }
    }

    /// Moves to the Trash (recoverable) and selects the next row.
    fn trash(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path == self.root {
            return;
        }
        let paths: Vec<&Path> = self.rows.iter().filter_map(Row::path).collect();
        let next = paths
            .iter()
            .position(|other| *other == path)
            .and_then(|ix| paths.get(ix + 1).or_else(|| ix.checked_sub(1).and_then(|ix| paths.get(ix))))
            .filter(|next| !next.starts_with(&path))
            .map(|next| next.to_path_buf());
        self.fs_op(
            Request::Trash { path: path.clone() },
            move |this, cx| {
                this.after_change(&[&path], next, cx);
                cx.emit(FileTreeEvent::Trashed { path });
            },
            cx,
        );
    }

    fn after_change(&mut self, paths: &[&Path], select: Option<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            if let Some(parent) = path.parent() {
                self.children.remove(parent);
            }
            self.children.remove(*path);
        }
        if select.is_some() {
            self.selected = select;
        }
        self.rebuild(cx);
        self.scroll_to_selected();
    }

    fn context_menu(&self, path: PathBuf, menu: PopupMenu, tree: WeakEntity<Self>) -> PopupMenu {
        let dir = self.dir_for(&path);
        let relative = path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().into_owned();
        let absolute = path.to_string_lossy().into_owned();
        // Rename and Trash have their shortcuts in the tree's context.
        let menu = menu.action_context(self.focus_handle.clone());
        let item = |label: &'static str, action: Box<dyn Fn(&mut FileTree, &mut Window, &mut Context<FileTree>)>| {
            let tree = tree.clone();
            let action: std::rc::Rc<dyn Fn(&mut FileTree, &mut Window, &mut Context<FileTree>)> = action.into();
            PopupMenuItem::new(label).on_click(move |_, window, cx| {
                let action = action.clone();
                tree.update(cx, |tree, cx| action(tree, window, cx)).ok();
            })
        };
        menu.item(item("New File…", {
            let dir = dir.clone();
            Box::new(move |tree, window, cx| tree.start_edit(EditKind::NewFile { dir: dir.clone() }, window, cx))
        }))
        .item(item("New Folder…", {
            let dir = dir.clone();
            Box::new(move |tree, window, cx| tree.start_edit(EditKind::NewFolder { dir: dir.clone() }, window, cx))
        }))
        .separator()
        // The task's folder (the empty space) can't be renamed or deleted.
        .when(path != self.root, |menu| {
            menu.item(item("Rename", {
                let path = path.clone();
                Box::new(move |tree, window, cx| {
                    tree.selected = Some(path.clone());
                    tree.start_edit(EditKind::Rename(path.clone()), window, cx)
                })
            }).action(Box::new(Rename)))
            .item(item("Move to Trash", {
                let path = path.clone();
                Box::new(move |tree, _, cx| tree.trash(path.clone(), cx))
            }).action(Box::new(Trash)))
            .separator()
        })
        .item(item("Copy Path", Box::new(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(absolute.clone()))
        })))
        .item(item("Copy Relative Path", Box::new(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()))
        })))
        .when(self.local, |menu| {
            menu.item(item("Reveal in Finder", {
                let path = path.clone();
                Box::new(move |_, _, cx| cx.reveal_path(&path))
            }))
        })
        .when(path != self.root, |menu| {
            let dir = dir == path;
            menu.separator().item(item("Show History", {
                let path = path.clone();
                Box::new(move |_, _, cx| cx.emit(FileTreeEvent::ShowHistory { path: path.clone(), dir }))
            }))
        })
        .separator()
        .item(item("Collapse All Folders", Box::new(|tree, _, cx| tree.collapse_all(cx))).action(Box::new(CollapseFileTree)))
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let focused = self.focus_handle.contains_focused(_window, cx);
        let editing = self.edit.as_ref().map(|edit| (edit.kind.clone(), edit.input.clone()));
        let view = cx.entity();
        let list = uniform_list("file-tree", rows.len(), move |range, _window, cx| {
            let theme = cx.theme();
            range
                .map(|ix| {
                    let row = &rows[ix];
                    let (path, name, is_dir) = match &row.kind {
                        RowKind::Entry(entry) => (Some(entry.path.clone()), entry.name.clone(), entry.is_dir),
                        RowKind::New => (None, SharedString::default(), false),
                    };
                    let is_selected = path.is_some() && selected == path;
                    let expanded = is_dir && path.as_ref().is_some_and(|path| view.read(cx).expanded.contains(path));
                    let input = editing.as_ref().and_then(|(kind, input)| match (kind, &path) {
                        (EditKind::Rename(target), Some(path)) if target == path => Some(input.clone()),
                        (EditKind::NewFile { .. } | EditKind::NewFolder { .. }, None) => Some(input.clone()),
                        _ => None,
                    });
                    let new_folder = matches!(editing, Some((EditKind::NewFolder { .. }, _)));
                    let (chevron, icon) = match (is_dir || (path.is_none() && new_folder), expanded) {
                        (true, true) => (Some("icons/tree-chevron-down.svg"), "icons/tree-folder-open.svg"),
                        (true, false) => (Some("icons/tree-chevron-right.svg"), "icons/tree-folder.svg"),
                        _ => (None, "icons/tree-file.svg"),
                    };
                    let row_el = div()
                        .id(ix)
                        .h(px(24.))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl(px(8. + row.depth as f32 * 14.))
                        .pr_2()
                        .text_ui(cx)
                        .text_color(theme.sidebar_foreground)
                        .when(is_selected, |el| {
                            el.bg(if focused { theme.sidebar_accent } else { theme.sidebar_accent.opacity(0.6) })
                        })
                        .when(!is_selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                        .child(div().w(px(14.)).flex_none().children(chevron.map(|path| {
                            svg().path(path).size(px(14.)).text_color(theme.muted_foreground)
                        })))
                        .child(svg().path(icon).size(px(14.)).flex_none().text_color(theme.muted_foreground))
                        .child(match input {
                            Some(input) => div().flex_1().child(Input::new(&input).xsmall()).into_any_element(),
                            None => div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(name)
                                .into_any_element(),
                        });
                    match path {
                        None => row_el.into_any_element(),
                        Some(path) => {
                            let click_view = view.clone();
                            let menu_view = view.clone();
                            let click_path = path.clone();
                            row_el
                                .on_click(move |event, window, cx| {
                                    let count = event.click_count();
                                    let path = click_path.clone();
                                    click_view.update(cx, |tree, cx| tree.click(path, count, window, cx));
                                })
                                // The menu belongs to the container; the row only says what it opens on.
                                .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                                    menu_view.update(cx, |tree, _| tree.menu_target = Some(path.clone()));
                                })
                                .into_any_element()
                        }
                    }
                })
                .collect()
        })
        .track_scroll(&self.scroll)
        .size_full();

        div()
            .id("file-tree-container")
            .key_context("FileTree")
            .track_focus(&self.focus_handle)
            .size_full()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.edit.is_some() {
                    this.cancel_edit(cx);
                    this.focus_handle.focus(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::rename_selected))
            .on_action(cx.listener(Self::trash_selected))
            .on_action(cx.listener(Self::open_selected))
            // Before the rows: a right-click outside them is on the task's folder.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, _| {
                if event.button == MouseButton::Right {
                    this.menu_target = None;
                }
            }))
            .context_menu({
                let tree = cx.entity().downgrade();
                move |menu, window, cx| {
                    let Some(this) = tree.upgrade() else {
                        return menu;
                    };
                    let this = this.read(cx);
                    let path = this.menu_target.clone().unwrap_or_else(|| this.root.clone());
                    this.context_menu(path, menu, tree.clone()).separator().panel_items(crate::menu::hide_panel(), window, cx)
                }
            })
            .child(list)
    }
}
