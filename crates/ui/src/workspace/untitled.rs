//! Cmd-N: an untitled tab, text with no file behind it until it's first
//! saved, when it gets a name: with the system's dialog on this machine, in
//! the files panel on a server. Until then it isn't remembered with what's
//! open, nor sent to a language server. Save As names any tab's file again.

use super::*;

/// An untitled tab, and its views, have a path under this, which no file has.
const UNTITLED_PATH: &str = "den:untitled";

impl FileTab {
    pub(super) fn is_untitled(&self) -> bool {
        self.path.starts_with(UNTITLED_PATH)
    }

    /// An edit's tab or an untitled one, or a view of them: there's no file.
    pub(super) fn has_no_file(&self) -> bool {
        self.is_edit() || self.is_untitled()
    }
}

impl Workspace {
    /// Opens an empty untitled tab, `Untitled-1` or the next number free.
    pub(super) fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        let path = (1..)
            .map(|n| Path::new(UNTITLED_PATH).join(format!("Untitled-{n}")))
            .find(|path| !self.tabs.iter().any(|tab| tab.path == *path))
            .expect("a free number");
        let mut tab = self.new_tab_with(path, false, "text", window, cx);
        tab.content = Content::Ready;
        let ix = self.place_tab(tab, true);
        self.activate(ix, window, cx);
        cx.notify();
    }

    /// Saving an untitled tab asks where.
    pub(super) fn save_untitled(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_where(ix, window, cx);
    }

    /// Cmd-Shift-S: the active tab's text in another file, anywhere, which
    /// the tab shows from then on; the one it showed stays as last saved.
    pub(super) fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active_file().filter(|ix| {
            let tab = &self.tabs[*ix];
            tab.is_file() && !tab.is_edit() && matches!(tab.content, Content::Ready)
        }) else {
            return;
        };
        self.ask_where(ix, window, cx);
    }

    /// Asks where to save tab `ix`: with the system's dialog on this
    /// machine, starting at its file's folder; in the files panel on a server.
    fn ask_where(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let path = &self.tabs[ix].path;
        let dir = match path.parent() {
            Some(dir) if !self.tabs[ix].is_untitled() && (self.local || dir.starts_with(&self.root)) => dir.to_path_buf(),
            _ => self.root.clone(),
        };
        if !self.local {
            self.show_panel(Panel::Files, cx);
            self.file_tree.update(cx, |tree, cx| tree.save_as_in(dir, window, cx));
            return;
        }
        let editor = self.tabs[ix].editor.clone();
        let path = cx.prompt_for_new_path(&dir, Some(&file_name(path)));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                if let Some(ix) = this.tab_index(&editor) {
                    this.save_tab_as(ix, path, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// The name typed in the files panel for the tab in front.
    pub(super) fn named_in_tree(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.active_file().filter(|ix| self.tabs[*ix].is_file() && !self.tabs[*ix].is_edit()) {
            self.save_tab_as(ix, path, window, cx);
        }
    }

    /// Writes tab `ix`'s text to `path`, whose tab it becomes. Another tab
    /// that had `path` open closes: what it showed was overwritten.
    pub(super) fn save_tab_as(&mut self, ix: usize, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.message = Some("Couldn't save: no agent".into());
            return cx.notify();
        };
        let editor = self.tabs[ix].editor.clone();
        let text = editor.read(cx).text().to_string();
        let write = client.request(Request::WriteFile { path: path.clone(), data: text.clone().into_bytes() });
        cx.spawn_in(window, async move |this, cx| {
            let result = write.await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(_) => {
                        while let Some(other) =
                            this.tabs.iter().position(|tab| tab.path == path && tab.diff.is_none() && tab.editor != editor)
                        {
                            this.tabs[other].dirty = false;
                            this.close(other, window, cx);
                        }
                        if let Some(ix) = this.tab_index(&editor) {
                            this.became_file(ix, path, text, window, cx);
                        }
                    }
                    Err(err) => this.message = Some(format!("Couldn't save: {err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Tab `ix` (untitled, or another file's) was saved as `path` with
    /// `saved`: it's that file's tab now, with the language of its name,
    /// and the cursor where it was.
    pub(super) fn became_file(&mut self, ix: usize, path: PathBuf, saved: String, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.tabs[ix].path.clone();
        let (text, cursor) = {
            let state = self.tabs[ix].editor.read(cx);
            (state.text().to_string(), state.cursor_position())
        };
        let mut tab = self.new_tab(path.clone(), false, window, cx);
        tab.group = self.tabs[ix].group;
        tab.content = Content::Ready;
        tab.dirty = text != saved;
        tab.saved = saved;
        tab.text = Some(text.as_str().into());
        tab.preview(&text, window, cx);
        tab.editor.update(cx, |state, cx| {
            state.set_tab_size(indentation(&text, cx), cx);
            state.set_value(text, window, cx);
            state.set_cursor_position(cursor, window, cx);
        });
        self.tabs[ix] = tab;
        for view in self.tabs.iter_mut().filter(|tab| tab.view && tab.path == old) {
            view.path = path.clone();
        }
        self.message = None;
        self.watch_file(&path, window, cx);
        self.load_blame(path, cx);
        if self.active == Some(ix) {
            self.activate(ix, window, cx);
        }
    }
}
