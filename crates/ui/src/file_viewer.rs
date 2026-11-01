//! Showing a file or folder of this machine in its file browser: Explorer
//! when it's installed on the Mac, else Finder (or the system's).

use std::path::Path;

use gpui_kit::App;

#[cfg(target_os = "macos")]
const EXPLORER: &str = "com.scorredoira.explorer";

/// Explorer is installed.
#[cfg(target_os = "macos")]
fn explorer() -> bool {
    use std::ffi::CString;

    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let id = CString::new(EXPLORER).unwrap();
    unsafe {
        let id: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: id.as_ptr()];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let url: *mut AnyObject = msg_send![workspace, URLForApplicationWithBundleIdentifier: id];
        !url.is_null()
    }
}

#[cfg(not(target_os = "macos"))]
fn explorer() -> bool {
    false
}

/// "Reveal in Explorer" or "Reveal in Finder".
pub fn reveal_label() -> &'static str {
    if explorer() { "Reveal in Explorer" } else { "Reveal in Finder" }
}

/// Shows `path`: a folder open, a file selected in its folder.
pub fn reveal(path: &Path, cx: &App) {
    #[cfg(target_os = "macos")]
    if explorer() {
        let path = path.to_path_buf();
        cx.background_executor()
            .spawn(async move {
                let _ = std::process::Command::new("open").arg("-b").arg(EXPLORER).arg(&path).status();
            })
            .detach();
        return;
    }
    cx.reveal_path(path);
}

/// Opens `path` as the system would, a folder in the file browser.
pub fn open(path: &Path, cx: &App) {
    if path.is_dir() {
        reveal(path, cx);
    } else {
        cx.open_with_system(path);
    }
}
