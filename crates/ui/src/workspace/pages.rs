//! Tabs of the code that show a view of their own rather than a file: the
//! graph, and the notes out of their place, side by side
//! with the code in a split. Saved with what's open as a `den:` path no file
//! has.

use super::*;

/// What a page tab shows.
#[derive(Clone)]
pub(super) enum Page {
    /// The repo's history, as gitk shows it (one such tab too).
    Graph,
    /// The workspace's notes, out of the terminals' place.
    Notes,
}

/// The graph's (named History before the History panel).
const GRAPH: &str = "den:history";
/// The notes'.
const NOTES: &str = "den:notes";

impl Page {
    pub(super) fn view(&self, workspace: &Workspace) -> AnyElement {
        match self {
            Page::Graph => workspace.graph.clone().into_any_element(),
            Page::Notes => workspace.notes.clone().into_any_element(),
        }
    }

    pub(super) fn focus_handle(&self, workspace: &Workspace, cx: &App) -> FocusHandle {
        match self {
            Page::Graph => workspace.graph.read(cx).focus_handle(),
            Page::Notes => workspace.notes.read(cx).focus_handle(cx),
        }
    }

    /// The tab's name.
    pub(super) fn title(&self) -> String {
        match self {
            Page::Graph => "Graph".to_string(),
            Page::Notes => "Notes".to_string(),
        }
    }

    /// What's saved with the session to open it again.
    pub(super) fn saved_path(&self) -> PathBuf {
        match self {
            Page::Graph => PathBuf::from(GRAPH),
            Page::Notes => PathBuf::from(NOTES),
        }
    }
}

impl Workspace {
    /// The tab the notes are in, if they're in one.
    pub(crate) fn notes_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| matches!(tab.page, Some(Page::Notes)))
    }

    /// Open in Editor Tab: the notes leave the terminals' place for a tab of
    /// the code, in the group with the focus, to write at length.
    pub(crate) fn notes_to_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.notes_tab() {
            return self.activate_with(ix, true, window, cx);
        }
        self.hide_panel(Panel::Notes, cx);
        self.terminals.update(cx, |terminals, cx| terminals.unsplit_notes(window, cx));
        let tab = self.page_tab(Page::Notes, window, cx);
        let ix = self.place_tab(tab, true);
        self.activate_with(ix, true, window, cx);
        self.layout_changed(cx);
    }

    /// Move to Terminals: their tab closes and they're a tab of the
    /// terminals' again, in front.
    pub(crate) fn notes_to_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.notes_tab() {
            self.close(ix, window, cx);
        }
        self.show_notes(window, cx);
    }

    /// The tab the graph is in, if it's open.
    pub(crate) fn graph_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| matches!(tab.page, Some(Page::Graph)))
    }

    /// Whether the graph shows, in front of its group.
    pub(crate) fn graph_visible(&self) -> bool {
        self.graph_tab().is_some_and(|ix| self.shown_in(self.tabs[ix].group) == Some(ix))
    }

    /// Shows the History panel with the commits that changed `file`
    /// (relative; `true`: a folder), or with all of them; the newest shows
    /// in the code.
    pub(crate) fn open_history(&mut self, file: Option<(String, bool)>, cx: &mut Context<Self>) {
        self.history.update(cx, |history, cx| history.show_file(file, cx));
        self.show_panel(Panel::History, cx);
    }

    /// The Graph tab in front, opened if it wasn't, with what it showed.
    fn show_graph_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = match self.graph_tab() {
            Some(ix) => ix,
            None => {
                let tab = self.page_tab(Page::Graph, window, cx);
                self.place_tab(tab, true)
            }
        };
        self.activate_with(ix, true, window, cx);
        self.layout_changed(cx);
    }

    /// The Graph tab, closed if it's the one in front.
    pub(crate) fn toggle_graph(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.graph_tab() {
            Some(ix) if self.active == Some(ix) => self.close(ix, window, cx),
            _ => self.show_graph_tab(window, cx),
        }
    }

    fn page_tab(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) -> FileTab {
        let mut tab = self.new_tab_with(page.saved_path(), false, "text", window, cx);
        tab.doc = true;
        tab.content = Content::Ready;
        tab.grab_focus = false;
        tab.page = Some(page);
        tab
    }

    /// Opens again a page tab saved with the session; false if `saved`
    /// isn't one.
    pub(super) fn restore_page(&mut self, saved: &SavedTab, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let page = match saved.path.to_str() {
            Some(GRAPH) => Page::Graph,
            Some(NOTES) => Page::Notes,
            _ => return false,
        };
        let mut tab = self.page_tab(page, window, cx);
        tab.group = saved.group.min(1);
        self.tabs.push(tab);
        true
    }
}
