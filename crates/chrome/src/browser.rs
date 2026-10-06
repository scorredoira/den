//! Finding Chrome, launching it with a profile of its own, or reusing the one
//! already running with that profile.

use std::{
    fs,
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};

/// A Chrome to talk to: where its DevTools endpoint is, and the process when
/// this bridge launched it.
pub struct Browser {
    pub port: u16,
    pub path: String,
    pub child: Option<Child>,
    /// Launched headless: killed when the bridge ends.
    pub kill_on_exit: bool,
    /// Launched now, rather than found running.
    pub launched: bool,
}

impl Browser {
    pub fn stop(&mut self) {
        if !self.kill_on_exit {
            return;
        }
        let Some(child) = &mut self.child else { return };
        if let Err(err) = child.kill() {
            eprintln!("chrome: stop Chrome: {err}");
            return;
        }
        if let Err(err) = child.wait() {
            eprintln!("chrome: wait for Chrome to end: {err}");
        }
        self.child = None;
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The Chrome binary: `DEN_CHROME`, or where it is usually installed.
pub fn find() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("DEN_CHROME") {
        let path = PathBuf::from(path);
        if !path.is_file() {
            bail!("DEN_CHROME is {}, which is not a file", path.display());
        }
        return Ok(path);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        }
        candidates.push("/Applications/Chromium.app/Contents/MacOS/Chromium".into());
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"] {
                candidates.push(dir.join(name));
            }
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .context("Chrome not found: install Google Chrome, or set DEN_CHROME to its binary")
}

/// The default profile: kept in Den's data folder, so logins survive between
/// sessions. Chrome refuses remote debugging on its default profile.
pub fn default_profile() -> Result<PathBuf> {
    let data = dirs::data_dir().context("no data folder for the Chrome profile; pass --profile")?;
    Ok(data.join("den").join("chrome"))
}

/// A Chrome with this profile: the one running, or a new one.
pub fn open(profile: &Path, headless: bool) -> Result<Browser> {
    fs::create_dir_all(profile).with_context(|| format!("create the profile folder {}", profile.display()))?;
    let active = profile.join("DevToolsActivePort");
    if let Some((port, path)) = read_active_port(&active)? {
        if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(500)).is_ok() {
            return Ok(Browser { port, path, child: None, kill_on_exit: false, launched: false });
        }
        // left by a Chrome that ended without removing it
        fs::remove_file(&active).with_context(|| format!("remove the stale {}", active.display()))?;
    }

    let binary = find()?;
    let mut command = Command::new(&binary);
    command
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check");
    if headless {
        command.arg("--headless=new");
    }
    command.arg("about:blank").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    if !headless {
        // its own process group: a Chrome with windows outlives the bridge
        // and the terminal that started it
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().with_context(|| format!("start {}", binary.display()))?;

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((port, path)) = read_active_port(&active)? {
            return Ok(Browser { port, path, child: Some(child), kill_on_exit: headless, launched: true });
        }
        if let Some(status) = child.try_wait().context("check on Chrome")? {
            bail!(
                "Chrome ended before it listened ({status}); is another Chrome using the profile {}?",
                profile.display()
            );
        }
        if Instant::now() > deadline {
            if let Err(err) = child.kill() {
                eprintln!("chrome: stop the Chrome that didn't start: {err}");
            }
            bail!("Chrome didn't write {} in 30 seconds", active.display());
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The port and browser path Chrome wrote, once it wrote both lines.
fn read_active_port(file: &Path) -> Result<Option<(u16, String)>> {
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", file.display())),
    };
    let mut lines = text.lines();
    let (Some(port), Some(path)) = (lines.next(), lines.next()) else {
        // being written
        return Ok(None);
    };
    let port = port.trim().parse::<u16>().with_context(|| format!("{} names port {port:?}", file.display()))?;
    Ok(Some((port, path.trim().to_string())))
}
