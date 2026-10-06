//! Tabs of the code that show a view of their own rather than a file: the
//! history, and the notes out of their place, side by side
//! with the code in a split. Saved with what's open as a `den:` path no file
//! has.

use super::*;

/// What a page tab shows.
#[derive(Clone)]
pub(super) enum Page {
    /// The repo's history, as gitk shows it (one such tab too).
    History,
    /// The workspace's notes, out of the terminals' place.
    Notes,
}

/// The history's.
const HISTORY: &str = "den:history";
/// The notes'.
const NOTES: &str = "den:notes";

impl Page {
    pub(super) fn view(&self, workspace: &Workspace) -> AnyElement {
        match self {
            Page::History => workspace.history.clone().into_any_element(),
            Page::Notes => workspace.notes.clone().into_any_element(),
        }
    }

    pub(super) fn focus_handle(&self, workspace: &Workspace, cx: &App) -> FocusHandle {
        match self {
            Page::History => workspace.history.read(cx).focus_handle(),
            Page::Notes => workspace.notes.read(cx).focus_handle(cx),
        }
    }

    /// The tab's name.
    pub(super) fn title(&self) -> String {
        match self {
            Page::History => "History".to_string(),
            Page::Notes => "Notes".to_string(),
        }
    }

    /// What's saved with the session to open it again.
    pub(super) fn saved_path(&self) -> PathBuf {
        match self {
            Page::History => PathBuf::from(HISTORY),
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

    /// The tab the history is in, if it's open.
    pub(crate) fn history_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| matches!(tab.page, Some(Page::History)))
    }

    /// Whether the history shows, in front of its group.
    pub(crate) fn history_visible(&self) -> bool {
        self.history_tab().is_some_and(|ix| self.shown_in(self.tabs[ix].group) == Some(ix))
    }

    /// Opens the History tab with the commits that changed `file` (relative;
    /// `true`: a folder), or with all of them.
    pub(crate) fn open_history(&mut self, file: Option<(String, bool)>, window: &mut Window, cx: &mut Context<Self>) {
        self.history.update(cx, |history, cx| history.show_file(file, cx));
        self.show_history_tab(window, cx);
    }

    /// The History tab in front, opened if it wasn't, with what it showed.
    fn show_history_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = match self.history_tab() {
            Some(ix) => ix,
            None => {
                let tab = self.page_tab(Page::History, window, cx);
                self.place_tab(tab, true)
            }
        };
        self.activate_with(ix, true, window, cx);
        self.layout_changed(cx);
    }

    /// Cmd-Shift-H: the History tab, closed if it's the one in front.
    pub(crate) fn toggle_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.history_tab() {
            Some(ix) if self.active == Some(ix) => self.close(ix, window, cx),
            _ => self.show_history_tab(window, cx),
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
            Some(HISTORY) => Page::History,
            Some(NOTES) => Page::Notes,
            _ => return false,
        };
        let mut tab = self.page_tab(page, window, cx);
        tab.group = saved.group.min(1);
        self.tabs.push(tab);
        true
    }
}
