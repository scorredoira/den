//! Tabs of the code that show a view of their own rather than a file: the
//! device, out of its column, side by side with the code in a split. Saved
//! with what's open as a `den:` path no file has.

use super::*;

/// What a page tab shows.
#[derive(Clone)]
pub(super) enum Page {
    /// The workspace's device (there's one, so one such tab).
    Device,
}

/// The device's tab, as saved.
const DEVICE: &str = "den:device";

impl Page {
    pub(super) fn view(&self, workspace: &Workspace) -> AnyElement {
        match self {
            Page::Device => workspace.device.clone().into_any_element(),
        }
    }

    pub(super) fn focus_handle(&self, workspace: &Workspace, cx: &App) -> FocusHandle {
        match self {
            Page::Device => workspace.device.read(cx).focus_handle(cx),
        }
    }

    /// The tab's name.
    pub(super) fn title(&self) -> String {
        match self {
            Page::Device => "Device".to_string(),
        }
    }

    /// What's saved with the session to open it again.
    pub(super) fn saved_path(&self) -> PathBuf {
        match self {
            Page::Device => PathBuf::from(DEVICE),
        }
    }
}

impl Workspace {
    /// The tab the device is in, if it's in one.
    pub(crate) fn device_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|tab| matches!(tab.page, Some(Page::Device)))
    }

    /// Shows the device where it is: its tab, or its column.
    pub(crate) fn show_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.device_tab() {
            Some(ix) => self.activate_with(ix, false, window, cx),
            None => self.show_panel(Panel::Device, cx),
        }
    }

    /// Open in Editor Tab: the device leaves its column for a tab of the
    /// code, in the group with the focus.
    pub(crate) fn device_to_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.device_tab() {
            return self.activate_with(ix, true, window, cx);
        }
        self.hide_panel(Panel::Device, cx);
        let tab = self.page_tab(Page::Device, window, cx);
        let ix = self.place_tab(tab, true);
        self.sync_device_place(cx);
        self.device.update(cx, |device, cx| device.shown(cx));
        self.activate_with(ix, true, window, cx);
        self.layout_changed(cx);
    }

    /// Move to Side Column: its tab closes and its column shows.
    pub(crate) fn device_to_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.device_tab() {
            self.close(ix, window, cx);
        }
        self.show_panel(Panel::Device, cx);
    }

    /// Tells the device whether it's in a tab, for its menu.
    pub(super) fn sync_device_place(&mut self, cx: &mut Context<Self>) {
        let in_tab = self.device_tab().is_some();
        self.device.update(cx, |device, cx| device.set_in_tab(in_tab, cx));
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
        if saved.path != Path::new(DEVICE) {
            return false;
        }
        let mut tab = self.page_tab(Page::Device, window, cx);
        tab.group = saved.group.min(1);
        self.tabs.push(tab);
        self.sync_device_place(cx);
        true
    }
}
