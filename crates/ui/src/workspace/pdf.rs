//! A Markdown preview saved as PDF: its text as HTML, its diagrams as SVG,
//! paged by WebKit the way macOS prints a page, in A4 or the printer's
//! paper, with text that can be selected.

/// `markdown` (its diagrams already images) as a page to print.
pub(super) fn html(markdown: &str, title: &str) -> String {
    let options = markdown::Options {
        parse: markdown::ParseOptions::gfm(),
        compile: markdown::CompileOptions { allow_dangerous_html: true, ..markdown::CompileOptions::gfm() },
    };
    // Parsing Markdown only fails for MDX, which is not on.
    let body = markdown::to_html_with_options(markdown, &options).unwrap_or_default();
    let title = title.replace('&', "&amp;").replace('<', "&lt;");
    format!("<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title><style>{STYLE}</style></head><body>{body}</body></html>")
}

const STYLE: &str = "
body { font: 11pt -apple-system, Helvetica, Arial, sans-serif; line-height: 1.5; color: #1f2329; }
h1, h2, h3, h4 { line-height: 1.25; margin: 1.2em 0 0.5em; break-after: avoid; }
h1 { font-size: 1.9em; } h2 { font-size: 1.45em; } h3 { font-size: 1.2em; }
code, pre { font: 0.9em SFMono-Regular, Menlo, monospace; background: #f4f5f7; border-radius: 4px; }
code { padding: 0.1em 0.3em; }
pre { padding: 0.8em 1em; white-space: pre-wrap; }
pre code { padding: 0; background: none; }
blockquote { margin: 0; padding: 0 1em; color: #59636e; border-left: 3px solid #d3d8e0; }
table { border-collapse: collapse; }
th, td { border: 1px solid #d3d8e0; padding: 0.3em 0.7em; }
img { max-width: 100% !important; height: auto; }
img, pre, tr, blockquote { break-inside: avoid; }
a { color: #0b62d6; }
";

#[cfg(target_os = "macos")]
pub(super) use mac::Export;

#[cfg(not(target_os = "macos"))]
pub(super) struct Export;

#[cfg(not(target_os = "macos"))]
impl Export {
    pub fn start(_: &str, _: Option<&std::path::Path>, _: &std::path::Path) -> Result<Self, String> {
        Err("PDF export is only on macOS".into())
    }
    pub fn loaded(&self) -> bool {
        true
    }
    pub fn print(&self) {}
    pub fn done(&self) -> Option<bool> {
        Some(false)
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::{
        ffi::CString,
        path::Path,
        sync::atomic::{AtomicU8, Ordering},
    };

    use objc2::{
        AnyThread, DefinedClass, class, define_class,
        encode::{Encode, Encoding},
        msg_send,
        rc::{Allocated, Retained},
        runtime::{AnyObject, Bool, NSObject},
        sel,
    };

    #[link(name = "WebKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSPrintJobSavingURL: &'static AnyObject;
        static NSPrintSaveJob: &'static AnyObject;
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct(
            "CGRect",
            &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])],
        );
    }

    /// The page's width while it loads; printing lays it out again on the paper.
    const PAGE: Rect = Rect { x: -10000.0, y: -10000.0, width: 800.0, height: 600.0 };

    /// 0 while printing, then 1 when it failed and 2 when it was saved.
    struct Ivars {
        result: AtomicU8,
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements, and this has no Drop.
        #[unsafe(super(NSObject))]
        #[ivars = Ivars]
        struct Printed;

        impl Printed {
            // AppKit calls it on its printing thread.
            #[unsafe(method(printOperationDidRun:success:contextInfo:))]
            fn did_run(&self, _operation: *mut AnyObject, success: Bool, _context: *mut std::ffi::c_void) {
                self.ivars().result.store(if success.as_bool() { 2 } else { 1 }, Ordering::SeqCst);
            }
        }
    );

    impl Printed {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(Ivars { result: AtomicU8::new(0) });
            unsafe { msg_send![super(this), init] }
        }
    }

    /// A page loading in a web view of its own, off screen, and then printed
    /// to a PDF file. All of it on the main thread; the caller polls it.
    pub struct Export {
        window: Retained<AnyObject>,
        web: Retained<AnyObject>,
        to: Retained<AnyObject>,
        printed: Retained<Printed>,
    }

    fn string(text: &str) -> Result<Retained<AnyObject>, String> {
        let text = CString::new(text).map_err(|_| "a NUL in the text".to_string())?;
        let made: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] };
        made.ok_or_else(|| "not UTF-8".into())
    }

    fn file_url(path: &Path, directory: bool) -> Result<Retained<AnyObject>, String> {
        let path = string(&path.to_string_lossy())?;
        let url: Option<Retained<AnyObject>> = unsafe { msg_send![class!(NSURL), fileURLWithPath: &*path, isDirectory: Bool::new(directory)] };
        url.ok_or_else(|| "not a path".into())
    }

    impl Export {
        /// Starts loading `html`, whose relative links are from `base`, to
        /// be saved at `to`.
        pub fn start(html: &str, base: Option<&Path>, to: &Path) -> Result<Self, String> {
            unsafe {
                let window: Allocated<AnyObject> = msg_send![class!(NSWindow), alloc];
                let window: Option<Retained<AnyObject>> =
                    msg_send![window, initWithContentRect: PAGE, styleMask: 0usize, backing: 2usize, defer: Bool::NO];
                let window = window.ok_or("no window")?;
                let _: () = msg_send![&*window, setReleasedWhenClosed: Bool::NO];
                // The Markdown's own HTML is kept, but not its scripts: a
                // file from anywhere runs nothing, as in the preview.
                let config: Retained<AnyObject> = msg_send![class!(WKWebViewConfiguration), new];
                let preferences: Retained<AnyObject> = msg_send![&*config, defaultWebpagePreferences];
                let _: () = msg_send![&*preferences, setAllowsContentJavaScript: Bool::NO];
                let web: Allocated<AnyObject> = msg_send![class!(WKWebView), alloc];
                let web: Option<Retained<AnyObject>> = msg_send![web, initWithFrame: PAGE, configuration: &*config];
                let web = web.ok_or("no web view")?;
                let _: () = msg_send![&*window, setContentView: &*web];
                let base = base.map(|base| file_url(base, true)).transpose()?;
                let _: Option<Retained<AnyObject>> = msg_send![&*web, loadHTMLString: &*string(html)?, baseURL: base.as_deref()];
                Ok(Self { window, web, to: file_url(to, false)?, printed: Printed::new() })
            }
        }

        /// The page and its images are loaded.
        pub fn loaded(&self) -> bool {
            unsafe {
                let loading: Bool = msg_send![&*self.web, isLoading];
                let progress: f64 = msg_send![&*self.web, estimatedProgress];
                !loading.as_bool() && progress >= 1.0
            }
        }

        /// Prints the page to the file, with no panel: it goes on by itself
        /// until `done`. Run without the window, WebKit's print never ends.
        pub fn print(&self) {
            unsafe {
                let info: Retained<AnyObject> = msg_send![class!(NSPrintInfo), sharedPrintInfo];
                let info: Retained<AnyObject> = msg_send![&*info, copy];
                let _: () = msg_send![&*info, setJobDisposition: NSPrintSaveJob];
                let settings: Retained<AnyObject> = msg_send![&*info, dictionary];
                let _: () = msg_send![&*settings, setObject: &*self.to, forKey: NSPrintJobSavingURL];
                // As wide as the paper, as many pages as it takes.
                let _: () = msg_send![&*info, setHorizontalPagination: 1isize];
                let _: () = msg_send![&*info, setVerticalPagination: 0isize];
                let operation: Retained<AnyObject> = msg_send![&*self.web, printOperationWithPrintInfo: &*info];
                let _: () = msg_send![&*operation, setShowsPrintPanel: Bool::NO];
                let _: () = msg_send![&*operation, setShowsProgressPanel: Bool::NO];
                let view: Option<Retained<AnyObject>> = msg_send![&*operation, view];
                if let Some(view) = view {
                    let _: () = msg_send![&*view, setFrame: PAGE];
                }
                let _: () = msg_send![
                    &*operation,
                    runOperationModalForWindow: &*self.window,
                    delegate: &*self.printed,
                    didRunSelector: sel!(printOperationDidRun:success:contextInfo:),
                    contextInfo: std::ptr::null_mut::<std::ffi::c_void>()
                ];
            }
        }

        /// Whether the file was saved, once printing ended.
        pub fn done(&self) -> Option<bool> {
            match self.printed.ivars().result.load(Ordering::SeqCst) {
                0 => None,
                result => Some(result == 2),
            }
        }
    }

    impl Drop for Export {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.window, close];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_has_the_text_and_the_diagrams() {
        let page = html("# Keys\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n<p align=\"center\"><img src=\"data:image/svg+xml,x\"></p>\n", "a < b");
        assert!(page.contains("<h1>Keys</h1>"));
        assert!(page.contains("<td>1</td>"), "tables, as GitHub's");
        assert!(page.contains("<img src=\"data:image/svg+xml,x\">"), "the diagrams' HTML is kept");
        assert!(page.contains("<title>a &lt; b</title>"));
    }
}
