//! The browser a program's page is shown in. On macOS a tab of Google Chrome
//! already showing the program is brought to the front as it is, instead of
//! opening another one; elsewhere, and without Chrome, the page opens in the
//! default browser.

use anyhow::Result;

/// The port of an `http(s)` URL of this machine: `localhost`, a subdomain of
/// it (a server may put each of its sites on one), `127.0.0.1` or `[::1]`.
fn local_port(url: &str) -> Option<u16> {
    let (scheme, after) = url.split_once("://")?;
    let default = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let (host, port) = match authority.rfind(':') {
        Some(colon) if !authority[colon..].contains(']') => (&authority[..colon], authority[colon + 1..].parse().ok()?),
        _ => (authority, default),
    };
    let local = host == "localhost" || host.ends_with(".localhost") || host == "127.0.0.1" || host == "[::1]";
    local.then_some(port)
}

/// The first of `tabs` (window, tab, URL) showing the server listening on
/// the port of `url`.
fn tab_of(url: &str, tabs: &str) -> Option<(u32, u32)> {
    let port = local_port(url)?;
    tabs.lines().find_map(|line| {
        let mut parts = line.splitn(3, '\t');
        let window = parts.next()?.parse().ok()?;
        let tab = parts.next()?.parse().ok()?;
        (local_port(parts.next()?) == Some(port)).then_some((window, tab))
    })
}

/// Every tab of Chrome, one per line: window, tab and URL separated by tabs.
/// Nothing when Chrome isn't running, which isn't started.
#[cfg(target_os = "macos")]
const LIST_TABS: &str = r#"
if application "Google Chrome" is not running then return ""
set found to {}
-- `tab` inside the tell would be Chrome's tab
set sep to character id 9
tell application "Google Chrome"
    repeat with w from 1 to count of windows
        repeat with t from 1 to count of tabs of window w
            set end of found to (w as text) & sep & (t as text) & sep & (URL of tab t of window w)
        end repeat
    end repeat
end tell
set text item delimiters to linefeed
return found as text
"#;

/// Brings to the front tab `t` (second argument) of window `w` (first).
#[cfg(target_os = "macos")]
const FOCUS_TAB: &str = r#"
on run argv
    set w to (item 1 of argv) as integer
    set t to (item 2 of argv) as integer
    tell application "Google Chrome"
        set active tab index of window w to t
        set index of window w to 1
        activate
    end tell
end run
"#;

#[cfg(target_os = "macos")]
fn osascript(script: &str, args: &[String]) -> Result<String> {
    let output = std::process::Command::new("osascript").arg("-e").arg(script).args(args).output()?;
    if !output.status.success() {
        anyhow::bail!("osascript: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Brings to the front a tab already showing the server of `url` (any page
/// of it), without changing its address. `false` if there's none, or on a
/// system where it can't be looked for: then the caller opens `url`.
#[cfg(target_os = "macos")]
pub fn focus_tab(url: &str) -> Result<bool> {
    let tabs = osascript(LIST_TABS, &[])?;
    let Some((window, tab)) = tab_of(url, &tabs) else {
        return Ok(false);
    };
    osascript(FOCUS_TAB, &[window.to_string(), tab.to_string()])?;
    Ok(true)
}

#[cfg(not(target_os = "macos"))]
pub fn focus_tab(_url: &str) -> Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{local_port, tab_of};

    #[test]
    fn a_tab_of_the_same_server_matches_by_port() {
        assert_eq!(local_port("http://localhost:9092/platform/tenants"), Some(9092));
        assert_eq!(local_port("http://demo.localhost:9092/admin"), Some(9092));
        assert_eq!(local_port("http://127.0.0.1"), Some(80));
        assert_eq!(local_port("https://[::1]:3000/x?y"), Some(3000));
        assert_eq!(local_port("http://example.com:9092/"), None);
        assert_eq!(local_port("http://localhost.example.com:9092/"), None);
        assert_eq!(local_port("chrome://newtab/"), None);

        let tabs = "1\t1\thttps://mail.google.com/\n1\t2\thttp://localhost:8080/main\n2\t3\thttp://demo.localhost:9092/admin/bookings";
        assert_eq!(tab_of("http://localhost:9092/platform/tenants", tabs), Some((2, 3)));
        assert_eq!(tab_of("http://localhost:8080/main/tenants", tabs), Some((1, 2)));
        assert_eq!(tab_of("http://localhost:5173/", tabs), None);
        assert_eq!(tab_of("http://localhost:9092/", ""), None);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod chrome {
    /// Needs Chrome open on a page of a server on port 9092.
    #[test]
    #[ignore]
    fn brings_the_tab_of_the_server_to_the_front() {
        assert!(super::focus_tab("http://localhost:9092/platform/tenants").unwrap());
        assert!(!super::focus_tab("http://localhost:1/").unwrap());
    }
}
