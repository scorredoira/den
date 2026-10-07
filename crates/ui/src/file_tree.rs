//! File tree: lazy loading, keyboard (arrows with preview, Enter renames),
//! selecting several (Cmd/Ctrl- and Shift-click, Shift-arrows), cut, copy,
//! paste, duplicate, drag and drop (Option/Ctrl copies), undo, and files
//! brought in from Finder or the Explorer, dropped or pasted. Everything goes
//! through the agent on the task's machine, so it works the same locally as
//! on a server.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
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

use crate::{CollapseFileTree, config::{Config, UiText}, drag_drop::TabDragPreview};

actions!(
    file_tree,
    [
        SelectPrev,
        SelectNext,
        ExtendPrev,
        ExtendNext,
        SelectAll,
        ClearSelection,
        Collapse,
        Expand,
        Rename,
        Trash,
        OpenSelected,
        CutFiles,
        CopyFiles,
        PasteFiles,
        UndoFiles,
    ]
);

/// Tree shortcuts: they only apply when the tree has focus.
pub fn keymap() -> Vec<KeyBinding> {
    let context = Some("FileTree");
    vec![
        KeyBinding::new("up", SelectPrev, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("shift-up", ExtendPrev, context),
        KeyBinding::new("shift-down", ExtendNext, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("escape", ClearSelection, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("enter", Rename, context),
        KeyBinding::new("f2", Rename, context),
        KeyBinding::new("secondary-backspace", Trash, context),
        KeyBinding::new("delete", Trash, context),
        KeyBinding::new("secondary-down", OpenSelected, context),
        KeyBinding::new("secondary-x", CutFiles, context),
        KeyBinding::new("secondary-c", CopyFiles, context),
        KeyBinding::new("secondary-v", PasteFiles, context),
        KeyBinding::new("secondary-z", UndoFiles, context),
    ]
}

const ROW_HEIGHT: Pixels = px(24.);

/// How long a dragged item rests on a closed folder before it opens.
const OPEN_ON_HOVER: Duration = Duration::from_millis(700);

/// Folders of an open folder read ahead, at most.
const READ_AHEAD: usize = 64;

/// How many operations Undo goes back.
const UNDO_LIMIT: usize = 100;

/// What was cut or copied in a files panel, for any of the app's trees to
/// paste: one on the same agent, or both on this machine.
#[derive(Clone)]
struct Clipped {
    paths: Vec<PathBuf>,
    cut: bool,
    client: Option<Arc<Client>>,
    local: bool,
}

#[derive(Default)]
struct FileClipboard(Option<Clipped>);

impl Global for FileClipboard {}

/// A row dragged from the tree: with the rest of the selection if it's in it.
#[derive(Clone)]
pub(crate) struct FileDrag {
    row: PathBuf,
}

/// A change on disk, run by the agent.
#[derive(Debug, PartialEq)]
enum Op {
    Move { from: PathBuf, to: PathBuf },
    /// Next to `to` if it exists ("a copy.txt").
    Copy { from: PathBuf, to: PathBuf },
    Trash(PathBuf),
    /// `item`, from the Trash, back to `to`.
    Untrash { item: PathBuf, to: PathBuf },
    /// A file or folder of this machine into `dir` on the agent's.
    Upload { from: PathBuf, dir: PathBuf },
}

/// What an `Op` did, which Undo reverts.
#[derive(Clone, Debug, PartialEq)]
enum Done {
    Moved { from: PathBuf, to: PathBuf },
    Created(PathBuf),
    /// `item` is where the Trash keeps it, if the agent says.
    Trashed { path: PathBuf, item: Option<PathBuf> },
}

pub enum FileTreeEvent {
    /// Open a file; `pin` is false for the preview.
    Open { path: PathBuf, pin: bool },
    Renamed { from: PathBuf, to: PathBuf },
    Trashed { path: PathBuf },
    /// Show the commits that changed a file or folder.
    ShowHistory { path: PathBuf, dir: bool },
    /// Open a terminal in a folder.
    OpenTerminal { dir: PathBuf },
    /// Open a file in the other editor group, side by side.
    OpenToSide { path: PathBuf },
    /// Search in the files under a folder.
    FindInFolder { dir: PathBuf },
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
    /// The row with the cursor: the arrows move it, Enter renames it.
    selected: Option<PathBuf>,
    /// Rows selected together with Cmd/Ctrl or Shift; empty, only `selected`.
    marked: Vec<PathBuf>,
    /// Where Shift extends the selection from.
    anchor: Option<PathBuf>,
    edit: Option<Edit>,
    /// What Undo reverts, the last at the end.
    undo: Vec<Vec<Done>>,
    /// The folder something dragged over the tree would land in.
    drop_target: Option<PathBuf>,
    /// Opens the closed folder held under a drag.
    _open_on_hover: Option<Task<()>>,
    /// A row took the mouse press the container sees next.
    row_pressed: bool,
    /// Row the right-click menu was opened on; `None` is the empty space (the
    /// task's folder).
    menu_target: Option<PathBuf>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// Revealing a file whose folders are still being read: each listing
    /// that arrives scrolls to it, until it's there. Other listings (the
    /// disk changed) leave the scroll alone.
    revealing: bool,
    /// What git ignores is listed too (Show Ignored Files), as read.
    ignored: bool,
    _config: Subscription,
    _clipboard: Subscription,
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
            marked: Vec::new(),
            anchor: None,
            edit: None,
            undo: Vec::new(),
            drop_target: None,
            _open_on_hover: None,
            row_pressed: false,
            menu_target: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            revealing: false,
            ignored: Config::get(cx).show_ignored,
            // Show Ignored Files changes every window's trees.
            _config: cx.observe_global::<Config>(|tree: &mut Self, cx| {
                let ignored = Config::get(cx).show_ignored;
                if tree.ignored != ignored {
                    // Reread, showing what it had until the new lists come.
                    tree.ignored = ignored;
                    tree.refresh(cx);
                }
            }),
            // What's cut shows dimmed in every tree.
            _clipboard: cx.observe_global::<FileClipboard>(|_, cx| cx.notify()),
        };
        tree.rebuild(cx);
        tree
    }

    /// Marks the path as selected, expands its folders and scrolls it into view.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.selected.as_deref() == Some(path) {
            return;
        }
        self.select_only(path.to_path_buf());
        let mut dir = path.parent();
        while let Some(d) = dir {
            if !d.starts_with(&self.root) {
                break;
            }
            self.expanded.insert(d.to_path_buf());
            dir = d.parent();
        }
        self.rebuild(cx);
        self.revealing = !self.scroll_to_selected();
        cx.notify();
    }

    /// Whether the selected file is in the rows (and so scrolled to).
    fn scroll_to_selected(&mut self) -> bool {
        let selected = self.selected.as_deref();
        let ix = self.rows.iter().position(|row| row.path() == selected);
        if let Some(ix) = ix {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        ix.is_some()
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
        // A new item's name is typed where it will be once made: among its
        // folder's, in their order, moving as the name changes.
        let mut new = match &self.edit {
            Some(Edit { kind: EditKind::NewFile { dir: target }, input, .. }) if target == dir => {
                Some(order(false, input.read(cx).value().trim()))
            }
            Some(Edit { kind: EditKind::NewFolder { dir: target }, input, .. }) if target == dir => {
                Some(order(true, input.read(cx).value().trim()))
            }
            _ => None,
        };
        let Some(entries) = self.children.get(dir).cloned() else {
            // Ask the agent; rebuild when it arrives.
            self.load_dir(dir.to_path_buf(), cx);
            if new.is_some() {
                rows.push(Row { kind: RowKind::New, depth });
            }
            return;
        };
        for entry in entries {
            if new.as_ref().is_some_and(|new| *new <= order(entry.is_dir, &entry.name)) {
                new = None;
                rows.push(Row { kind: RowKind::New, depth });
            }
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
        if new.is_some() {
            rows.push(Row { kind: RowKind::New, depth });
        }
    }

    /// Puts `entry` in its folder's list, in its place, if the folder was read.
    fn insert_entry(&mut self, entry: DirEntry) {
        let Some(entries) = entry.path.parent().and_then(|dir| self.children.get_mut(dir)) else {
            return;
        };
        if entries.iter().any(|other| other.path == entry.path) {
            return;
        }
        let key = order(entry.is_dir, &entry.name);
        let ix = entries.iter().position(|other| order(other.is_dir, &other.name) > key).unwrap_or(entries.len());
        entries.insert(ix, entry);
    }

    /// Refresh: rereads every folder shown, also one whose listing never
    /// came back.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.loading.clear();
        self.children.retain(|dir, _| self.expanded.contains(dir));
        let dirs: Vec<PathBuf> = self.children.keys().cloned().collect();
        for dir in dirs {
            self.load_dir(dir, cx);
        }
        self.rebuild(cx);
        cx.notify();
    }

    /// Switches to a new connection with the agent: everything is reread.
    pub fn set_client(&mut self, client: Arc<Client>, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn load_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.loading.insert(dir.clone()) {
            return;
        }
        let ignored = self.ignored;
        cx.spawn(async move |this, cx| {
            let mut response = Err(anyhow::anyhow!("not asked"));
            if ignored {
                response = client.request(Request::ListDirAll { path: dir.clone() }).await;
            }
            // An agent from before Show Ignored Files lists what it can.
            if response.is_err() {
                response = client.request(Request::ListDir { path: dir.clone() }).await;
            }
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
                // An open folder's folders are read ahead: opening one shows
                // its contents at once.
                let shown = this.expanded.contains(&dir);
                let ahead: Vec<PathBuf> = match shown {
                    true => entries
                        .iter()
                        .filter(|entry| entry.is_dir && !this.children.contains_key(&entry.path))
                        .take(READ_AHEAD)
                        .map(|entry| entry.path.clone())
                        .collect(),
                    false => Vec::new(),
                };
                this.children.insert(dir, entries);
                for dir in ahead {
                    this.load_dir(dir, cx);
                }
                // A closed folder's list changes no row.
                if !shown {
                    return;
                }
                this.rebuild(cx);
                if this.revealing {
                    this.revealing = !this.scroll_to_selected();
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

    fn click(&mut self, path: PathBuf, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry(&path).cloned() else {
            return;
        };
        self.cancel_edit(cx);
        let modifiers = event.modifiers();
        if modifiers.secondary() || modifiers.shift {
            if modifiers.shift {
                self.extend_to(path, cx);
            } else {
                self.toggle_marked(path);
            }
            self.focus_handle.focus(window, cx);
            return cx.notify();
        }
        let click_count = event.click_count();
        self.select_only(path.clone());
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
            self.select_only(top);
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
        self.select_only(paths[next].clone());
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
            self.select_only(parent.to_path_buf());
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
        self.trash(self.selection(), cx);
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
            let mut open = Some(dir.as_path());
            while let Some(d) = open
                && d.starts_with(&self.root)
            {
                self.expanded.insert(d.to_path_buf());
                open = d.parent();
            }
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Blur => this.cancel_edit(cx),
            // The new item's row follows its name to its place.
            InputEvent::Change if matches!(this.edit.as_ref().map(|edit| &edit.kind), Some(EditKind::NewFile { .. } | EditKind::NewFolder { .. })) => {
                this.rebuild(cx);
                cx.notify();
            }
            _ => {}
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
                self.run(vec![Op::Move { from, to }], true, None, cx);
            }
            EditKind::NewFile { dir } => self.create(dir.join(&name), false, cx),
            EditKind::NewFolder { dir } => self.create(dir.join(&name), true, cx),
        }
    }

    /// Makes the new item, shown already where its name was typed so it
    /// doesn't move; taken out again if the agent can't.
    fn create(&mut self, path: PathBuf, is_dir: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return cx.emit(FileTreeEvent::Error("No agent".into()));
        };
        let name: SharedString = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default().into();
        let exists = self.entry(&path).is_some();
        if !exists {
            self.insert_entry(DirEntry { path: path.clone(), name, is_dir });
        }
        self.select_only(path.clone());
        self.rebuild(cx);
        self.scroll_to_selected();
        cx.notify();
        let request = match is_dir {
            true => Request::CreateDir { path: path.clone() },
            false => Request::CreateFile { path: path.clone() },
        };
        cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                match response {
                    Ok(_) => {
                        this.push_undo(vec![Done::Created(path.clone())]);
                        if !is_dir {
                            cx.emit(FileTreeEvent::Open { path: path.clone(), pin: true });
                        }
                    }
                    Err(err) => {
                        if !exists
                            && let Some(entries) = path.parent().and_then(|dir| this.children.get_mut(dir))
                        {
                            entries.retain(|entry| entry.path != path);
                        }
                        cx.emit(FileTreeEvent::Error(format!("{err:#}").into()));
                    }
                }
                if let Some(dir) = path.parent() {
                    this.load_dir(dir.to_path_buf(), cx);
                }
                this.rebuild(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Moves to the Trash (recoverable) and selects the row after the last.
    fn trash(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|path| *path != self.root).collect();
        if paths.is_empty() {
            return;
        }
        let rows: Vec<&Path> = self.rows.iter().filter_map(Row::path).collect();
        let gone = |row: &Path| paths.iter().any(|path| row.starts_with(path));
        let last = rows.iter().rposition(|row| paths.iter().any(|path| path == row));
        let next = last.and_then(|last| {
            rows[last + 1..]
                .iter()
                .find(|row| !gone(row))
                .or_else(|| rows[..last].iter().rev().find(|row| !gone(row)))
                .map(|row| row.to_path_buf())
        });
        self.run(paths.into_iter().map(Op::Trash).collect(), true, next, cx);
    }

    /// Selects `path` alone.
    fn select_only(&mut self, path: PathBuf) {
        self.marked.clear();
        self.anchor = Some(path.clone());
        self.selected = Some(path);
    }

    /// Cmd/Ctrl-click: adds `path` to the selection, or takes it out.
    fn toggle_marked(&mut self, path: PathBuf) {
        if self.marked.is_empty() {
            self.marked.extend(self.selected.clone());
        }
        match self.marked.iter().position(|marked| *marked == path) {
            Some(ix) => {
                self.marked.remove(ix);
            }
            None => self.marked.push(path.clone()),
        }
        self.anchor = Some(path.clone());
        self.selected = Some(path);
    }

    /// Shift: selects the rows from the anchor to `path`.
    fn extend_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let paths: Vec<&Path> = self.rows.iter().filter_map(Row::path).collect();
        let anchor = self.anchor.clone().or_else(|| self.selected.clone()).unwrap_or_else(|| path.clone());
        let (Some(from), Some(to)) = (
            paths.iter().position(|row| *row == anchor),
            paths.iter().position(|row| *row == path),
        ) else {
            return self.select_only(path);
        };
        self.marked = paths[from.min(to)..=from.max(to)].iter().map(|row| row.to_path_buf()).collect();
        self.anchor = Some(anchor);
        self.selected = Some(path);
        self.scroll_to_selected();
        cx.notify();
    }

    fn is_marked(&self, path: &Path) -> bool {
        if self.marked.is_empty() {
            self.selected.as_deref() == Some(path)
        } else {
            self.marked.iter().any(|marked| marked == path)
        }
    }

    /// What the selection acts on, in the tree's order: never the task's
    /// folder, nor what's inside a folder that's selected too.
    fn selection(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = if self.marked.is_empty() {
            self.selected.iter().cloned().collect()
        } else {
            self.marked.clone()
        };
        paths.retain(|path| *path != self.root);
        let all = paths.clone();
        paths.retain(|path| !all.iter().any(|other| other != path && path.starts_with(other)));
        let order = |path: &PathBuf| self.rows.iter().position(|row| row.path() == Some(path)).unwrap_or(usize::MAX);
        paths.sort_by_key(order);
        paths
    }

    fn extend_prev(&mut self, _: &ExtendPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.extend_offset(-1, cx);
    }

    fn extend_next(&mut self, _: &ExtendNext, _: &mut Window, cx: &mut Context<Self>) {
        self.extend_offset(1, cx);
    }

    fn extend_offset(&mut self, offset: isize, cx: &mut Context<Self>) {
        let paths: Vec<&Path> = self.rows.iter().filter_map(Row::path).collect();
        let Some(current) = self.selected.as_ref().and_then(|selected| paths.iter().position(|path| path == selected)) else {
            return self.select_offset(offset, cx);
        };
        let next = (current as isize + offset).clamp(0, paths.len() as isize - 1) as usize;
        let path = paths[next].to_path_buf();
        self.extend_to(path, cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = self.rows.iter().filter_map(Row::path).map(Path::to_path_buf).collect();
        cx.notify();
    }

    /// Escape: back to the row with the cursor, and nothing cut.
    fn clear_selection(&mut self, _: &ClearSelection, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            self.cancel_edit(cx);
            return self.focus_handle.focus(window, cx);
        }
        self.marked.clear();
        if cx.try_global::<FileClipboard>().is_some_and(|clipboard| clipboard.0.as_ref().is_some_and(|clip| clip.cut)) {
            cx.set_global(FileClipboard(None));
        }
        cx.notify();
    }

    fn cut(&mut self, _: &CutFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.clip(self.selection(), true, cx);
    }

    fn copy(&mut self, _: &CopyFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.clip(self.selection(), false, cx);
    }

    /// Keeps `paths` for a paste; their paths go to the clipboard as text,
    /// replacing whatever Finder copied before.
    fn clip(&mut self, paths: Vec<PathBuf>, cut: bool, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let text = paths.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.set_global(FileClipboard(Some(Clipped {
            paths,
            cut,
            client: self.client.clone(),
            local: self.local,
        })));
    }

    /// The folder a paste from the keyboard goes in: the selected one, or
    /// the selected file's.
    fn target_dir(&self) -> PathBuf {
        match &self.selected {
            Some(path) => self.dir_for(path),
            None => self.root.clone(),
        }
    }

    fn paste_action(&mut self, _: &PasteFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.paste(self.target_dir(), cx);
    }

    /// Files copied in Finder (or the Explorer) first, else what was cut or
    /// copied in a tree.
    fn paste(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if let Some(paths) = external_clipboard(cx) {
            return self.import(paths, dir, cx);
        }
        let Some(clip) = cx.try_global::<FileClipboard>().and_then(|clipboard| clipboard.0.clone()) else {
            return;
        };
        let same_agent = match (&clip.client, &self.client) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same_agent && !(clip.local && self.local) {
            return cx.emit(FileTreeEvent::Error("Those files are on another machine".into()));
        }
        if clip.cut {
            // What's cut moves once.
            cx.set_global(FileClipboard(None));
        }
        self.place(clip.paths, dir, !clip.cut, cx);
    }

    /// A copy of each next to it ("a copy.txt").
    fn duplicate_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let ops = paths.into_iter().map(|path| Op::Copy { from: path.clone(), to: path }).collect();
        self.run(ops, true, None, cx);
    }

    /// Moves (or copies) `paths` into `dir`; a folder never goes into itself,
    /// and what's already there doesn't move.
    fn place(&mut self, paths: Vec<PathBuf>, dir: PathBuf, copy: bool, cx: &mut Context<Self>) {
        self.run(place_ops(paths, &dir, copy), true, None, cx);
    }

    /// Files from this machine (Finder, the Explorer) into `dir`: copied by
    /// the agent if it's here too, sent to it otherwise.
    fn import(&mut self, paths: Vec<PathBuf>, dir: PathBuf, cx: &mut Context<Self>) {
        let ops = paths
            .into_iter()
            .filter_map(|from| {
                let to = dir.join(from.file_name()?);
                Some(match self.local {
                    true => Op::Copy { from, to },
                    false => Op::Upload { from, dir: dir.clone() },
                })
            })
            .collect();
        self.run(ops, true, None, cx);
    }

    fn undo_action(&mut self, _: &UndoFiles, _: &mut Window, cx: &mut Context<Self>) {
        let Some(batch) = self.undo.pop() else {
            return;
        };
        self.run(undo_ops(batch), false, None, cx);
    }

    fn push_undo(&mut self, batch: Vec<Done>) {
        // What an older agent sent to the Trash without saying where can't come back.
        let batch: Vec<Done> = batch.into_iter().filter(|done| !matches!(done, Done::Trashed { item: None, .. })).collect();
        if batch.is_empty() {
            return;
        }
        self.undo.push(batch);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Runs `ops` one after another on the agent, then shows what they did:
    /// what they made or moved, selected (or `then_select`), and what failed.
    fn run(&mut self, ops: Vec<Op>, undoable: bool, then_select: Option<PathBuf>, cx: &mut Context<Self>) {
        if ops.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            return cx.emit(FileTreeEvent::Error("No agent".into()));
        };
        let work = cx.background_spawn(async move {
            let mut done = Vec::new();
            let mut errors = Vec::new();
            for op in ops {
                match run_op(&client, op).await {
                    Ok(result) => done.push(result),
                    Err(err) => errors.push(format!("{err:#}")),
                }
            }
            (done, errors)
        });
        cx.spawn(async move |this, cx| {
            let (done, errors) = work.await;
            this.update(cx, |this, cx| this.finish(done, errors, undoable, then_select, cx)).ok();
        })
        .detach();
    }

    fn finish(
        &mut self,
        done: Vec<Done>,
        errors: Vec<String>,
        undoable: bool,
        then_select: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut select: Vec<PathBuf> = Vec::new();
        for result in &done {
            match result {
                Done::Moved { from, to } => {
                    // A folder that was open stays open where it went.
                    let open: Vec<PathBuf> = self.expanded.iter().filter(|dir| dir.starts_with(from)).cloned().collect();
                    for dir in open {
                        self.expanded.remove(&dir);
                        if let Ok(rest) = dir.strip_prefix(from) {
                            self.expanded.insert(to.join(rest));
                        }
                    }
                    // It goes to its new place in the list right away.
                    let moved = from.parent().and_then(|dir| self.children.get_mut(dir)).and_then(|entries| {
                        let ix = entries.iter().position(|entry| entry.path == *from)?;
                        Some(entries.remove(ix))
                    });
                    if let Some(entry) = moved {
                        let name = to.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
                        self.insert_entry(DirEntry { path: to.clone(), name: name.into(), is_dir: entry.is_dir });
                    }
                    self.children.retain(|dir, _| !dir.starts_with(from));
                    changed.extend([from.clone(), to.clone()]);
                    select.push(to.clone());
                    cx.emit(FileTreeEvent::Renamed { from: from.clone(), to: to.clone() });
                }
                Done::Created(path) => {
                    changed.push(path.clone());
                    select.push(path.clone());
                }
                Done::Trashed { path, .. } => {
                    self.children.retain(|dir, _| !dir.starts_with(path));
                    self.expanded.retain(|dir| !dir.starts_with(path));
                    changed.push(path.clone());
                    cx.emit(FileTreeEvent::Trashed { path: path.clone() });
                }
            }
        }
        // Reread the folders that changed, keeping what's shown while they
        // arrive.
        let dirs: HashSet<PathBuf> = changed.iter().filter_map(|path| path.parent().map(Path::to_path_buf)).collect();
        for dir in dirs {
            self.expanded.insert(dir.clone());
            self.load_dir(dir, cx);
        }
        if let Some(last) = select.last().cloned() {
            let mut open = last.parent();
            while let Some(dir) = open
                && dir.starts_with(&self.root)
            {
                self.expanded.insert(dir.to_path_buf());
                open = dir.parent();
            }
            self.anchor = select.first().cloned();
            self.marked = if select.len() > 1 { select } else { Vec::new() };
            self.selected = Some(last);
        } else if let Some(path) = then_select {
            self.select_only(path);
        } else if let Some(cursor) = &self.selected
            && done.iter().any(|result| matches!(result, Done::Trashed { path, .. } if cursor.starts_with(path)))
        {
            self.selected = None;
            self.marked.clear();
        }
        self.marked.retain(|path| !done.iter().any(|result| matches!(result, Done::Trashed { path: gone, .. } if path.starts_with(gone))));
        if undoable {
            self.push_undo(done);
        }
        self.rebuild(cx);
        self.scroll_to_selected();
        if !errors.is_empty() {
            cx.emit(FileTreeEvent::Error(errors.join("; ").into()));
        }
        cx.notify();
    }

    /// The row at `position` in the list laid out at `bounds`.
    fn row_at(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> Option<&Row> {
        let offset = self.scroll.0.borrow().base_handle.offset().y;
        let y = position.y - bounds.top() - offset;
        if y < px(0.) {
            return None;
        }
        self.rows.get((y / ROW_HEIGHT) as usize)
    }

    /// Where something dragged over `position` would land: the folder under
    /// it, the file's folder, or the task's below the rows.
    fn drag_moved(&mut self, position: Point<Pixels>, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let target = if !bounds.contains(&position) {
            None
        } else {
            Some(match self.row_at(position, bounds) {
                Some(Row { kind: RowKind::Entry(entry), .. }) if entry.is_dir => entry.path.clone(),
                Some(Row { kind: RowKind::Entry(entry), .. }) => self.dir_for(&entry.path),
                _ => self.root.clone(),
            })
        };
        if target == self.drop_target {
            return;
        }
        self.drop_target = target.clone();
        // Held on a closed folder, it opens.
        self._open_on_hover = target.filter(|dir| !self.expanded.contains(dir)).map(|dir| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(OPEN_ON_HOVER).await;
                this.update(cx, |this, cx| {
                    if this.drop_target.as_ref() == Some(&dir) && cx.has_active_drag() {
                        this.expanded.insert(dir);
                        this.rebuild(cx);
                        cx.notify();
                    }
                })
                .ok();
            })
        });
        cx.notify();
    }

    fn take_drop_target(&mut self) -> PathBuf {
        self._open_on_hover = None;
        self.drop_target.take().unwrap_or_else(|| self.root.clone())
    }

    /// What dragging `row` takes: the selection if the row is in it.
    fn dragged(&self, row: &Path) -> Vec<PathBuf> {
        match self.is_marked(row) {
            true => self.selection(),
            false => vec![row.to_path_buf()],
        }
    }

    fn context_menu(&self, path: PathBuf, can_paste: bool, menu: PopupMenu, tree: WeakEntity<Self>) -> PopupMenu {
        let dir = self.dir_for(&path);
        // On a selected row it acts on all the selection.
        let paths = match path != self.root && self.is_marked(&path) {
            true => self.selection(),
            false => vec![path.clone()],
        };
        let several = paths.len() > 1;
        let relative = paths
            .iter()
            .map(|path| path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        let absolute = paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n");
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
        let is_dir = dir == path;
        menu.when(!is_dir, |menu| {
            menu.item(item("Open to the Side", {
                let path = path.clone();
                Box::new(move |_, _, cx| cx.emit(FileTreeEvent::OpenToSide { path: path.clone() }))
            }))
            .separator()
        })
        .item(item("New File…", {
            let dir = dir.clone();
            Box::new(move |tree, window, cx| tree.start_edit(EditKind::NewFile { dir: dir.clone() }, window, cx))
        }))
        .item(item("New Folder…", {
            let dir = dir.clone();
            Box::new(move |tree, window, cx| tree.start_edit(EditKind::NewFolder { dir: dir.clone() }, window, cx))
        }))
        .separator()
        // The task's folder (the empty space) can't be cut, copied, renamed or deleted.
        .when(path != self.root, |menu| {
            menu.item(item("Cut", {
                let paths = paths.clone();
                Box::new(move |tree, _, cx| tree.clip(paths.clone(), true, cx))
            }).action(Box::new(CutFiles)))
            .item(item("Copy", {
                let paths = paths.clone();
                Box::new(move |tree, _, cx| tree.clip(paths.clone(), false, cx))
            }).action(Box::new(CopyFiles)))
        })
        .item(item("Paste", {
            let dir = dir.clone();
            Box::new(move |tree, _, cx| tree.paste(dir.clone(), cx))
        }).action(Box::new(PasteFiles)).disabled(!can_paste))
        .when(path != self.root, |menu| {
            menu.item(item("Duplicate", {
                let paths = paths.clone();
                Box::new(move |tree, _, cx| tree.duplicate_paths(paths.clone(), cx))
            }))
        })
        .separator()
        .when(path != self.root, |menu| {
            menu.when(!several, |menu| {
                menu.item(item("Rename", {
                    let path = path.clone();
                    Box::new(move |tree, window, cx| {
                        tree.select_only(path.clone());
                        tree.start_edit(EditKind::Rename(path.clone()), window, cx)
                    })
                }).action(Box::new(Rename)))
            })
            .item(item(if several { "Move Them to Trash" } else { "Move to Trash" }, {
                let paths = paths.clone();
                Box::new(move |tree, _, cx| tree.trash(paths.clone(), cx))
            }).action(Box::new(Trash)))
            .separator()
        })
        .item(item(if several { "Copy Paths" } else { "Copy Path" }, Box::new(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(absolute.clone()))
        })))
        .item(item(if several { "Copy Relative Paths" } else { "Copy Relative Path" }, Box::new(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()))
        })))
        .when(self.local, |menu| {
            menu.item(item("Reveal in Finder", {
                let path = path.clone();
                Box::new(move |_, _, cx| cx.reveal_path(&path))
            }))
        })
        .item(item("Open in Terminal", {
            let dir = dir.clone();
            Box::new(move |_, _, cx| cx.emit(FileTreeEvent::OpenTerminal { dir: dir.clone() }))
        }))
        .when(is_dir, |menu| {
            menu.item(item("Find in Folder…", {
                let dir = dir.clone();
                Box::new(move |_, _, cx| cx.emit(FileTreeEvent::FindInFolder { dir: dir.clone() }))
            }))
        })
        .when(path != self.root, |menu| {
            menu.separator().item(item(if is_dir { "Show Folder History" } else { "Show File History" }, {
                let path = path.clone();
                Box::new(move |_, _, cx| cx.emit(FileTreeEvent::ShowHistory { path: path.clone(), dir: is_dir }))
            }))
        })
        .separator()
        .item(item("Refresh", Box::new(|tree, _, cx| tree.refresh(cx))))
        .item(item("Collapse All Folders", Box::new(|tree, _, cx| tree.collapse_all(cx))).action(Box::new(CollapseFileTree)))
        .item(item("Show Ignored Files", Box::new(|tree, _, cx| {
            let show = !tree.ignored;
            Config::update(cx, |config| config.show_ignored = show);
        })).checked(self.ignored))
    }
}

impl Render for FileTree {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let focused = self.focus_handle.contains_focused(_window, cx);
        let dragging = cx.has_active_drag();
        let drop_target = self.drop_target.clone().filter(|_| dragging);
        let root_target = drop_target.as_ref() == Some(&self.root);
        let cut: Vec<PathBuf> = cx
            .try_global::<FileClipboard>()
            .and_then(|clipboard| clipboard.0.as_ref())
            .filter(|clip| clip.cut)
            .map(|clip| clip.paths.clone())
            .unwrap_or_default();
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
                    let is_selected = path.as_ref().is_some_and(|path| view.read(cx).is_marked(path));
                    let is_cursor = path.is_some() && selected == path;
                    let is_cut = path.as_ref().is_some_and(|path| cut.iter().any(|cut| path.starts_with(cut)));
                    let in_drop = !root_target
                        && path.as_ref().is_some_and(|path| drop_target.as_ref().is_some_and(|dir| path.starts_with(dir)));
                    let expanded = is_dir && path.as_ref().is_some_and(|path| view.read(cx).expanded.contains(path));
                    let input = editing.as_ref().and_then(|(kind, input)| match (kind, &path) {
                        (EditKind::Rename(target), Some(path)) if target == path => Some(input.clone()),
                        (EditKind::NewFile { .. } | EditKind::NewFolder { .. }, None) => Some(input.clone()),
                        _ => None,
                    });
                    let renaming = input.is_some();
                    let new_folder = matches!(editing, Some((EditKind::NewFolder { .. }, _)));
                    let (chevron, icon) = match (is_dir || (path.is_none() && new_folder), expanded) {
                        (true, true) => (Some("icons/tree-chevron-down.svg"), "icons/tree-folder-open.svg"),
                        (true, false) => (Some("icons/tree-chevron-right.svg"), "icons/tree-folder.svg"),
                        _ => (None, "icons/tree-file.svg"),
                    };
                    let row_el = div()
                        .id(ix)
                        .h(ROW_HEIGHT)
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pl(px(8. + row.depth as f32 * 14.))
                        .pr_2()
                        .text_ui(cx)
                        .text_color(theme.sidebar_foreground)
                        // VS Code's: the selection filled, and outlined while the tree has the keyboard.
                        .border_1()
                        .border_color(transparent_black())
                        .when(is_selected, |el| el.bg(crate::app::selected_row(cx)))
                        .when(is_cursor && focused, |el| el.border_color(theme.list_active_border))
                        .when(!is_selected && in_drop, |el| el.bg(theme.primary.opacity(0.12)))
                        .when(!is_selected && !dragging, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                        .when(is_cut, |el| el.opacity(0.5))
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
                            let drag = FileDrag { row: path.clone() };
                            let drag_view = view.clone();
                            row_el
                                .on_click(move |event, window, cx| {
                                    let path = click_path.clone();
                                    click_view.update(cx, |tree, cx| tree.click(path, event, window, cx));
                                })
                                .on_mouse_down(MouseButton::Left, {
                                    let view = menu_view.clone();
                                    move |_, _, cx| view.update(cx, |tree, _| tree.row_pressed = true)
                                })
                                // The menu belongs to the container; the row only says what it opens on.
                                // In the capture phase: the container's menu stops the bubble before the row.
                                .capture_any_mouse_down(move |event, _, cx| {
                                    if event.button != MouseButton::Right {
                                        return;
                                    }
                                    menu_view.update(cx, |tree, cx| {
                                        tree.row_pressed = true;
                                        // Outside the selection, the row is selected alone.
                                        if !tree.is_marked(&path) {
                                            tree.select_only(path.clone());
                                            cx.notify();
                                        }
                                        tree.menu_target = Some(path.clone());
                                    });
                                })
                                .when(!renaming, |el| {
                                    el.on_drag(drag, move |drag: &FileDrag, _, _, cx| {
                                        let label = match drag_view.read(cx).dragged(&drag.row).as_slice() {
                                            [one] => one.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
                                            many => format!("{} items", many.len()),
                                        };
                                        cx.new(|_| TabDragPreview(label.into()))
                                    })
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
            .on_action(cx.listener(Self::extend_prev))
            .on_action(cx.listener(Self::extend_next))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::clear_selection))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste_action))
            .on_action(cx.listener(Self::undo_action))
            .when(root_target, |el| el.bg(cx.theme().primary.opacity(0.08)))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<FileDrag>, _, cx| {
                this.drag_moved(event.event.position, event.bounds, cx);
            }))
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                this.drag_moved(event.event.position, event.bounds, cx);
            }))
            // Option/Alt (or Ctrl outside the Mac) copies instead of moving.
            .on_drop(cx.listener(|this, drag: &FileDrag, window, cx| {
                let dir = this.take_drop_target();
                let modifiers = window.modifiers();
                let copy = modifiers.alt || (!cfg!(target_os = "macos") && modifiers.control);
                this.place(this.dragged(&drag.row), dir, copy, cx);
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                let dir = this.take_drop_target();
                this.import(paths.paths().to_vec(), dir, cx);
            }))
            // Before the rows: a right-click outside them is on the task's folder.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, _| {
                this.row_pressed = false;
                if event.button == MouseButton::Right {
                    this.menu_target = None;
                }
            }))
            // A click on no row selects nothing: what's done next (New
            // Folder, Paste…) goes in the task's folder, as nothing shows.
            .on_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                // Anywhere in the panel, the keys (Cmd-V…) come here.
                if this.edit.is_none() {
                    this.focus_handle.focus(window, cx);
                }
                if !this.row_pressed && (this.selected.is_some() || !this.marked.is_empty()) {
                    this.selected = None;
                    this.marked.clear();
                    this.anchor = None;
                    cx.notify();
                }
            }))
            .context_menu({
                let tree = cx.entity().downgrade();
                move |menu, window, cx| {
                    let Some(this) = tree.upgrade() else {
                        return menu;
                    };
                    let can_paste = external_clipboard(cx).is_some()
                        || cx.try_global::<FileClipboard>().is_some_and(|clipboard| clipboard.0.is_some());
                    let this = this.read(cx);
                    let path = this.menu_target.clone().unwrap_or_else(|| this.root.clone());
                    this.context_menu(path, can_paste, menu, tree.clone()).separator().panel_items(crate::menu::hide_panel(), window, cx)
                }
            })
            .child(list)
    }
}

fn place_ops(paths: Vec<PathBuf>, dir: &Path, copy: bool) -> Vec<Op> {
    paths
        .into_iter()
        .filter(|from| !dir.starts_with(from))
        .filter(|from| copy || from.parent() != Some(dir))
        .filter_map(|from| {
            let to = dir.join(from.file_name()?);
            Some(if copy { Op::Copy { from, to } } else { Op::Move { from, to } })
        })
        .collect()
}

/// What reverts `batch`, the last first.
fn undo_ops(batch: Vec<Done>) -> Vec<Op> {
    batch
        .into_iter()
        .rev()
        .filter_map(|done| match done {
            Done::Moved { from, to } => Some(Op::Move { from: to, to: from }),
            Done::Created(path) => Some(Op::Trash(path)),
            Done::Trashed { path, item } => Some(Op::Untrash { item: item?, to: path }),
        })
        .collect()
}

/// Where an item goes among its folder's, as the agent lists them: folders
/// first, by name.
fn order(is_dir: bool, name: &str) -> (bool, String) {
    (!is_dir, name.to_lowercase())
}

/// The files Finder (or the Explorer) has on the clipboard.
fn external_clipboard(cx: &App) -> Option<Vec<PathBuf>> {
    cx.read_from_clipboard()?.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => Some(paths.paths().to_vec()),
        _ => None,
    })
}

async fn run_op(client: &Client, op: Op) -> anyhow::Result<Done> {
    Ok(match op {
        Op::Move { from, to } => {
            client.request(Request::Rename { from: from.clone(), to: to.clone() }).await?;
            Done::Moved { from, to }
        }
        Op::Copy { from, to } => match client.request(Request::Copy { from: from.clone(), to: to.clone() }).await {
            Ok(Response::Path(Some(path))) => Done::Created(path),
            Ok(other) => anyhow::bail!("unexpected response: {other:?}"),
            // An agent from before Copy (still running since an update):
            // the copy is read and written through it.
            Err(err) if err.to_string().contains("does not know the request") => {
                let dir = to.parent().map(Path::to_path_buf).unwrap_or_default();
                let to = free_path(client, &dir, &to.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).await?;
                let is_dir = list(client, from.parent().unwrap_or(&dir))
                    .await
                    .iter()
                    .any(|entry| entry.is_dir && Some(entry.name.as_str()) == from.file_name().and_then(|name| name.to_str()));
                copy_through(client, from, to.clone(), is_dir).await?;
                Done::Created(to)
            }
            Err(err) => return Err(err),
        },
        Op::Trash(path) => match client.request(Request::Trash { path: path.clone() }).await? {
            Response::Path(item) => Done::Trashed { path, item },
            _ => Done::Trashed { path, item: None },
        },
        Op::Untrash { item, to } => {
            client.request(Request::Untrash { item, to: to.clone() }).await?;
            Done::Created(to)
        }
        Op::Upload { from, dir } => Done::Created(upload(client, &from, &dir).await?),
    })
}

/// What the agent lists in `dir`, what git ignores too where it can.
async fn list(client: &Client, dir: &Path) -> Vec<proto::DirEntryInfo> {
    match client.request(Request::ListDirAll { path: dir.to_path_buf() }).await {
        Ok(Response::Dir(entries)) => entries,
        _ => match client.request(Request::ListDir { path: dir.to_path_buf() }).await {
            Ok(Response::Dir(entries)) => entries,
            _ => Vec::new(),
        },
    }
}

/// `name` in `dir`, or the first of its copies' names that's free.
async fn free_path(client: &Client, dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let taken: HashSet<String> = list(client, dir).await.into_iter().map(|entry| entry.name).collect();
    let name = (0..1000)
        .map(|n| proto::copy_name(name, n))
        .find(|name| !taken.contains(name))
        .ok_or_else(|| anyhow::anyhow!("{name} has too many copies"))?;
    Ok(dir.join(name))
}

/// Sends a file or folder of this machine into `dir` on the agent's, named
/// as a copy if the name is taken.
async fn upload(client: &Client, from: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
    let name = from.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let to = free_path(client, dir, &name).await?;
    upload_entry(client, from.to_path_buf(), to.clone()).await?;
    Ok(to)
}

/// Copies on the agent's machine with what any agent knows: listing,
/// reading and writing.
fn copy_through(
    client: &Client,
    from: PathBuf,
    to: PathBuf,
    is_dir: bool,
) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
    Box::pin(async move {
        if is_dir {
            client.request(Request::CreateDir { path: to.clone() }).await?;
            for entry in list(client, &from).await {
                copy_through(client, from.join(&entry.name), to.join(&entry.name), entry.is_dir).await?;
            }
        } else {
            let data = match client.request(Request::ReadFile { path: from.clone() }).await? {
                Response::Bytes(data) => data,
                other => anyhow::bail!("unexpected response: {other:?}"),
            };
            client.request(Request::CreateFile { path: to.clone() }).await?;
            client.request(Request::WriteFile { path: to, data }).await?;
        }
        Ok(())
    })
}

fn upload_entry(client: &Client, from: PathBuf, to: PathBuf) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
    Box::pin(async move {
        let meta = std::fs::metadata(&from)?;
        if meta.is_dir() {
            client.request(Request::CreateDir { path: to.clone() }).await?;
            for entry in std::fs::read_dir(&from)? {
                let entry = entry?;
                upload_entry(client, entry.path(), to.join(entry.file_name())).await?;
            }
        } else {
            if meta.len() > proto::MAX_FILE_BYTES as u64 {
                anyhow::bail!("{} is too large to send", from.display());
            }
            let data = std::fs::read(&from)?;
            client.request(Request::CreateFile { path: to.clone() }).await?;
            client.request(Request::WriteFile { path: to, data }).await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    /// /t with src/ (a.rs, b.rs) open, docs/ closed, and c.md.
    fn tree(cx: &mut TestAppContext) -> Entity<FileTree> {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        cx.new(sample)
    }

    fn sample(cx: &mut Context<FileTree>) -> FileTree {
        let mut tree = FileTree::new(p("/t"), None, true, cx);
        let entry = |path: &str, is_dir| DirEntry {
            path: p(path),
            name: Path::new(path).file_name().unwrap().to_string_lossy().into_owned().into(),
            is_dir,
        };
        tree.children.insert(p("/t"), vec![entry("/t/docs", true), entry("/t/src", true), entry("/t/c.md", false)]);
        tree.children.insert(p("/t/src"), vec![entry("/t/src/a.rs", false), entry("/t/src/b.rs", false)]);
        tree.children.insert(p("/t/docs"), vec![]);
        tree.expanded.insert(p("/t/src"));
        tree.rebuild(cx);
        tree
    }

    /// Right-clicking a row opens the menu on that row, selected alone,
    /// whichever row was right-clicked before; below them, on the folder.
    #[gpui_kit::test]
    fn right_click_opens_the_menu_of_the_row_under_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (tree, cx) = cx.add_window_view(|_, cx| sample(cx));
        cx.run_until_parked();
        // docs, src, a.rs, b.rs, c.md.
        let row = |ix: usize| point(px(40.), ROW_HEIGHT * (ix as f32 + 0.5));
        for (ix, path) in [(4, "/t/c.md"), (2, "/t/src/a.rs"), (1, "/t/src")] {
            cx.simulate_mouse_down(row(ix), MouseButton::Right, Modifiers::default());
            cx.simulate_mouse_up(row(ix), MouseButton::Right, Modifiers::default());
            cx.simulate_keystrokes("escape");
            tree.read_with(cx, |tree, _| {
                assert_eq!(tree.menu_target.as_deref(), Some(Path::new(path)));
                assert!(tree.is_marked(Path::new(path)));
                assert_eq!(tree.selection(), [p(path)], "{path} selected alone");
            });
        }
        // Below the rows, the menu is the task folder's.
        cx.simulate_mouse_down(row(8), MouseButton::Right, Modifiers::default());
        cx.simulate_mouse_up(row(8), MouseButton::Right, Modifiers::default());
        tree.read_with(cx, |tree, _| assert_eq!(tree.menu_target, None));
    }

    /// New Folder on a folder never opened: its name is typed in it while
    /// the agent lists it, and the edit isn't dropped.
    #[gpui_kit::test]
    fn a_new_folder_goes_in_a_folder_not_read_yet(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (tree, cx) = cx.add_window_view(|_, cx| {
            let mut tree = FileTree::new(p("/t"), None, true, cx);
            let docs = DirEntry { path: p("/t/docs"), name: "docs".into(), is_dir: true };
            tree.children.insert(p("/t"), vec![docs]);
            tree.rebuild(cx);
            tree
        });
        tree.update_in(cx, |tree, window, cx| {
            tree.focus_handle.focus(window, cx);
            tree.start_edit(EditKind::NewFolder { dir: p("/t/docs") }, window, cx);
        });
        cx.run_until_parked();
        tree.read_with(cx, |tree, _| {
            assert!(tree.edit.is_some(), "the edit was dropped");
            let new = tree.rows.iter().position(|row| matches!(row.kind, RowKind::New));
            assert_eq!(new, Some(1), "typed under docs");
            assert_eq!(tree.rows[1].depth, 1);
        });
    }

    /// The name is typed in the row the new item will have, and made, it's
    /// there: nothing jumps.
    #[gpui_kit::test]
    fn a_new_item_is_typed_where_it_will_be(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(Config::default());
        });
        let (tree, cx) = cx.add_window_view(|_, cx| sample(cx));
        let rows = |tree: &FileTree| -> Vec<String> {
            tree.rows
                .iter()
                .map(|row| match &row.kind {
                    RowKind::Entry(entry) => entry.name.to_string(),
                    RowKind::New => "<new>".to_string(),
                })
                .collect()
        };
        tree.update_in(cx, |tree, window, cx| {
            tree.start_edit(EditKind::NewFile { dir: p("/t/src") }, window, cx);
            // Empty, a file goes after the folders.
            assert_eq!(rows(tree), ["docs", "src", "<new>", "a.rs", "b.rs", "c.md"]);
            let input = tree.edit.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("ab.rs", window, cx));
            tree.rebuild(cx);
            assert_eq!(rows(tree), ["docs", "src", "a.rs", "<new>", "b.rs", "c.md"]);
            tree.edit = None;
            tree.insert_entry(DirEntry { path: p("/t/src/ab.rs"), name: "ab.rs".into(), is_dir: false });
            tree.rebuild(cx);
            assert_eq!(rows(tree), ["docs", "src", "a.rs", "ab.rs", "b.rs", "c.md"]);
            // A folder goes among the folders.
            tree.start_edit(EditKind::NewFolder { dir: p("/t") }, window, cx);
            let input = tree.edit.as_ref().unwrap().input.clone();
            input.update(cx, |input, cx| input.set_value("e", window, cx));
            tree.rebuild(cx);
            assert_eq!(rows(tree), ["docs", "<new>", "src", "a.rs", "ab.rs", "b.rs", "c.md"]);
        });
    }

    #[gpui_kit::test]
    fn shift_selects_a_range_and_secondary_adds_to_it(cx: &mut TestAppContext) {
        let tree = tree(cx);
        tree.update(cx, |tree, cx| {
            tree.select_only(p("/t/src/a.rs"));
            tree.extend_to(p("/t/c.md"), cx);
            assert_eq!(tree.marked, [p("/t/src/a.rs"), p("/t/src/b.rs"), p("/t/c.md")]);
            // Back up past the anchor: the range turns around it.
            tree.extend_to(p("/t/src"), cx);
            assert_eq!(tree.marked, [p("/t/src"), p("/t/src/a.rs")]);
            // What's inside a selected folder goes with it.
            assert_eq!(tree.selection(), [p("/t/src")]);
            tree.toggle_marked(p("/t/c.md"));
            tree.toggle_marked(p("/t/src"));
            assert_eq!(tree.selection(), [p("/t/src/a.rs"), p("/t/c.md")]);
            assert!(tree.is_marked(Path::new("/t/c.md")) && !tree.is_marked(Path::new("/t/src")));
            tree.select_only(p("/t/docs"));
            assert_eq!(tree.selection(), [p("/t/docs")]);
        });
    }

    #[gpui_kit::test]
    fn dragging_a_selected_row_takes_the_selection(cx: &mut TestAppContext) {
        let tree = tree(cx);
        tree.update(cx, |tree, _| {
            tree.select_only(p("/t/src/a.rs"));
            tree.toggle_marked(p("/t/c.md"));
            assert_eq!(tree.dragged(Path::new("/t/c.md")), [p("/t/src/a.rs"), p("/t/c.md")]);
            assert_eq!(tree.dragged(Path::new("/t/src/b.rs")), [p("/t/src/b.rs")]);
        });
    }

    #[test]
    fn a_folder_never_moves_into_itself_nor_where_it_is() {
        let ops = place_ops(vec![p("/t/src"), p("/t/c.md"), p("/t/docs/x")], Path::new("/t/src/inner"), false);
        assert_eq!(ops, [
            Op::Move { from: p("/t/c.md"), to: p("/t/src/inner/c.md") },
            Op::Move { from: p("/t/docs/x"), to: p("/t/src/inner/x") },
        ]);
        assert!(place_ops(vec![p("/t/src/a.rs")], Path::new("/t/src"), false).is_empty());
        // A copy where it is is a duplicate.
        assert_eq!(place_ops(vec![p("/t/src/a.rs")], Path::new("/t/src"), true), [
            Op::Copy { from: p("/t/src/a.rs"), to: p("/t/src/a.rs") },
        ]);
    }

    #[test]
    fn undo_reverts_the_last_first() {
        let batch = vec![
            Done::Moved { from: p("/t/a"), to: p("/t/b") },
            Done::Created(p("/t/c")),
            Done::Trashed { path: p("/t/d"), item: Some(p("/trash/d")) },
        ];
        assert_eq!(undo_ops(batch), [
            Op::Untrash { item: p("/trash/d"), to: p("/t/d") },
            Op::Trash(p("/t/c")),
            Op::Move { from: p("/t/b"), to: p("/t/a") },
        ]);
    }

    #[gpui_kit::test]
    fn what_moves_is_selected_and_its_open_folders_stay_open(cx: &mut TestAppContext) {
        let tree = tree(cx);
        tree.update(cx, |tree, cx| {
            let done = vec![
                Done::Moved { from: p("/t/src"), to: p("/t/docs/src") },
                Done::Moved { from: p("/t/c.md"), to: p("/t/docs/c.md") },
            ];
            tree.finish(done.clone(), vec![], true, None, cx);
            assert!(tree.expanded.contains(Path::new("/t/docs/src")) && !tree.expanded.contains(Path::new("/t/src")));
            assert!(tree.expanded.contains(Path::new("/t/docs")));
            assert_eq!(tree.marked, [p("/t/docs/src"), p("/t/docs/c.md")]);
            assert_eq!(tree.undo, [done]);
        });
    }

    #[gpui_kit::test]
    fn trashing_selects_the_row_after(cx: &mut TestAppContext) {
        let tree = tree(cx);
        tree.update(cx, |tree, cx| {
            tree.select_only(p("/t/src/a.rs"));
            let trashed = Done::Trashed { path: p("/t/src/a.rs"), item: Some(p("/trash/a.rs")) };
            tree.finish(vec![trashed.clone()], vec![], true, Some(p("/t/src/b.rs")), cx);
            assert_eq!(tree.selected, Some(p("/t/src/b.rs")));
            assert_eq!(tree.undo, [vec![trashed]]);
            // Where an older agent doesn't say where it went, it can't come back.
            tree.finish(vec![Done::Trashed { path: p("/t/c.md"), item: None }], vec![], true, None, cx);
            assert_eq!(tree.undo.len(), 1);
        });
    }
}
