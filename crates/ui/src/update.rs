//! Keeping an installed sik on its latest release: every few hours (unless
//! turned off in Settings) or with Check for Updates, it asks GitHub for the
//! latest release and, if it's newer, installs it in the background in place
//! of this one. It never restarts by itself: the title bar says there's an
//! update, and its button asks before restarting into it. Workspaces and
//! their tabs reopen as they were, and terminals live in the agent, so
//! they're still there after restarting.
//!
//! Only an installed app updates: `Sik.app` on macOS, or what the Linux
//! package's `install.sh` installed. A build run from `target` doesn't.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::{App, Global};

const RELEASES: &str = "https://github.com/scorredoira/sik/releases";

/// The first check, once the app has settled.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// How this sik was installed, which is what an update replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Install {
    /// `…/Sik.app` on macOS.
    Bundle(PathBuf),
    /// `<data>/sik/app/sik`, by the Linux package's `install.sh`.
    Linux(PathBuf),
}

/// Where updating is at, for About.
#[derive(Clone, Default, PartialEq)]
pub enum Status {
    /// Not checked yet.
    #[default]
    Idle,
    Checking,
    /// The latest release, which is this one or older.
    UpToDate(String),
    Failed(String),
    /// A build run from `target`: it doesn't update.
    NotInstalled,
    /// Installed: restarting runs it.
    Ready(String),
}

#[derive(Default)]
pub struct Updates {
    pub status: Status,
    /// What to start once the app has quit, when restarting into it.
    relaunch: Option<Vec<String>>,
    restarting: bool,
}

impl Global for Updates {}

impl Updates {
    /// The version installed and waiting for the restart.
    pub fn ready(&self) -> Option<&str> {
        match &self.status {
            Status::Ready(version) => Some(version),
            _ => None,
        }
    }
}

/// Where updating is at.
pub fn status(cx: &App) -> Status {
    cx.try_global::<Updates>().map(|updates| updates.status.clone()).unwrap_or_default()
}

/// Starts checking for updates in the background: shortly after starting,
/// then every few hours, while Settings has it on.
pub fn init(cx: &mut App) {
    cx.set_global(Updates::default());
    if install().is_none() {
        cx.global_mut::<Updates>().status = Status::NotInstalled;
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(FIRST_CHECK).await;
        loop {
            cx.update(|cx| {
                if crate::config::Config::get(cx).checks_for_updates() {
                    check_now(cx);
                }
            });
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

/// Asks for the latest release now and, if it's newer, installs it for the
/// next start. Check for Updates, and the periodic check.
pub fn check_now(cx: &mut App) {
    let Some(install) = install() else {
        cx.default_global::<Updates>().status = Status::NotInstalled;
        cx.refresh_windows();
        return;
    };
    let updates = cx.default_global::<Updates>();
    if matches!(updates.status, Status::Checking | Status::Ready(_)) {
        return;
    }
    updates.status = Status::Checking;
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        let result = cx.background_executor().spawn(async move { check_and_install(&install) }).await;
        cx.update(|cx| {
            let updates = cx.global_mut::<Updates>();
            match result {
                Ok(Ok(latest)) => updates.status = Status::UpToDate(latest),
                Ok(Err((version, relaunch))) => {
                    updates.status = Status::Ready(version);
                    updates.relaunch = Some(relaunch);
                }
                Err(err) => updates.status = Status::Failed(format!("{err:#}")),
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Quits (asking about unsaved files, as Cmd-Q does) and starts the
/// updated app.
pub fn restart(cx: &mut App) {
    cx.default_global::<Updates>().restarting = true;
    crate::app::quit(cx);
}

/// The quit dialog was cancelled: no restart until asked again.
pub fn cancel_restart(cx: &mut App) {
    cx.default_global::<Updates>().restarting = false;
}

/// Right before quitting: if restarting into an update, starts it once
/// this process is gone, so it doesn't find this one still running.
pub fn relaunch_if_restarting(cx: &App) {
    let Some(updates) = cx.try_global::<Updates>() else {
        return;
    };
    let (true, Some(relaunch)) = (updates.restarting, &updates.relaunch) else {
        return;
    };
    let script = r#"pid=$1; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done; exec "$@""#;
    let spawned = Command::new("sh")
        .args(["-c", script, "sh", &std::process::id().to_string()])
        .args(relaunch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(err) = spawned {
        eprintln!("update: could not restart: {err}");
    }
}

/// `true` with a `v1.2.3` newer than this build. A prerelease never updates
/// anything, nor is it updated.
fn is_newer(latest: &str, current: &str) -> bool {
    let release = |version: &str| -> Option<[u64; 3]> {
        let version = version.strip_prefix('v').unwrap_or(version);
        let mut parts = version.split('.').map(|part| part.parse().ok());
        let release = [parts.next()??, parts.next()??, parts.next()??];
        parts.next().is_none().then_some(release)
    };
    match (release(latest), release(current)) {
        (Some(latest), Some(current)) => latest > current,
        _ => false,
    }
}

fn install() -> Option<Install> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    install_of(&exe)
}

fn install_of(exe: &Path) -> Option<Install> {
    if cfg!(target_os = "macos") {
        let bundle = exe.ancestors().find(|dir| dir.extension().is_some_and(|ext| ext == "app"))?;
        return Some(Install::Bundle(bundle.to_path_buf()));
    }
    let app = exe.parent()?;
    let named = |path: Option<&Path>, name: &str| path.and_then(Path::file_name).is_some_and(|file| file == name);
    (cfg!(target_os = "linux") && named(Some(app), "app") && named(app.parent(), "sik"))
        .then(|| Install::Linux(exe.to_path_buf()))
}

/// The tag of the latest release: where GitHub's "latest" link lands.
fn latest() -> Result<String> {
    let output = Command::new("curl")
        .args(["-fsSLI", "-o", "/dev/null", "-w", "%{url_effective}", &format!("{RELEASES}/latest")])
        .stderr(Stdio::piped())
        .output()
        .context("could not run curl")?;
    if !output.status.success() {
        bail!("could not reach GitHub: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let url = String::from_utf8_lossy(&output.stdout);
    Ok(url.trim().rsplit('/').next().unwrap_or_default().to_string())
}

/// Installs the latest release if it's newer. `Ok` with the latest version
/// if there's nothing to install; `Err` with the version installed and what
/// starts it.
#[allow(clippy::type_complexity)]
fn check_and_install(install: &Install) -> Result<std::result::Result<String, (String, Vec<String>)>> {
    let tag = latest()?;
    if !is_newer(&tag, env!("CARGO_PKG_VERSION")) {
        return Ok(Ok(tag.trim_start_matches('v').to_string()));
    }
    let version = tag.trim_start_matches('v').to_string();
    let (platform, extension) = match install {
        Install::Bundle(_) => ("macos", "zip"),
        Install::Linux(_) => ("linux", "tar.gz"),
    };
    let label = format!("sik-{version}-{platform}-{}", std::env::consts::ARCH);
    let archive_name = format!("{label}.{extension}");
    let work = tempdir()?;
    let result = (|| {
        let archive = work.join(&archive_name);
        download(&format!("{RELEASES}/download/{tag}/{archive_name}"), &archive)?;
        let sums = work.join("SHA256SUMS");
        download(&format!("{RELEASES}/download/{tag}/SHA256SUMS"), &sums)?;
        let expected = std::fs::read_to_string(&sums)?
            .lines()
            .find_map(|line| {
                let (sum, name) = line.split_once(char::is_whitespace)?;
                (name.trim().trim_start_matches('*') == archive_name).then(|| sum.to_lowercase())
            })
            .with_context(|| format!("{archive_name} is not in SHA256SUMS"))?;
        if sha256(&archive)? != expected {
            bail!("{archive_name} does not match its checksum");
        }
        match install {
            Install::Bundle(bundle) => replace_bundle(&archive, bundle, &work),
            Install::Linux(exe) => {
                run(Command::new("tar").arg("-xzf").arg(&archive).arg("-C").arg(&work))?;
                run(Command::new("sh").arg(work.join(&label).join("install.sh")))?;
                Ok(vec![exe.to_string_lossy().into_owned()])
            }
        }
    })();
    let _ = std::fs::remove_dir_all(&work);
    Ok(Err((version, result?)))
}

/// Unpacks the release's `Sik.app` beside `bundle` and swaps it in. The
/// running app keeps its files; the next start is the new one.
fn replace_bundle(archive: &Path, bundle: &Path, work: &Path) -> Result<Vec<String>> {
    run(Command::new("ditto").args(["-x", "-k"]).arg(archive).arg(work))?;
    let unpacked = work.join("Sik.app");
    if !unpacked.join("Contents/MacOS/sik").is_file() {
        bail!("the release has no Sik.app");
    }
    let parent = bundle.parent().context("the app is not in a folder")?;
    let name = bundle.file_name().context("the app has no name")?.to_string_lossy().into_owned();
    // Copied next to it first: a rename only works within a disk.
    let staged = parent.join(format!(".{name}.update"));
    let old = parent.join(format!(".{name}.old"));
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    run(Command::new("ditto").arg(&unpacked).arg(&staged))?;
    std::fs::rename(bundle, &old).with_context(|| format!("could not replace {}", bundle.display()))?;
    if let Err(err) = std::fs::rename(&staged, bundle) {
        let _ = std::fs::rename(&old, bundle);
        return Err(err).with_context(|| format!("could not replace {}", bundle.display()));
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(vec!["open".into(), "-a".into(), bundle.to_string_lossy().into_owned()])
}

fn download(url: &str, to: &Path) -> Result<()> {
    run(Command::new("curl").args(["-fsSL", "-o"]).arg(to).arg(url)).with_context(|| format!("could not download {url}"))
}

fn sha256(path: &Path) -> Result<String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| Command::new("sha256sum").arg(path).output())
        .context("no shasum or sha256sum to check the download")?;
    if !output.status.success() {
        bail!("could not compute the checksum of {}", path.display());
    }
    let out = String::from_utf8_lossy(&output.stdout);
    Ok(out.split_whitespace().next().unwrap_or_default().to_lowercase())
}

fn run(command: &mut Command) -> Result<()> {
    let output = command.stdin(Stdio::null()).output().context("could not run it")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{}", stderr.lines().last().unwrap_or("failed"));
    }
    Ok(())
}

fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("sik-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_later_release_is_newer() {
        assert!(is_newer("v0.1.5", "0.1.4"));
        assert!(is_newer("v0.2.0", "0.1.10"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.4", "0.1.4"));
        assert!(!is_newer("v0.1.3", "0.1.4"));
        // Prereleases neither update nor are updated.
        assert!(!is_newer("v0.2.0-beta.1", "0.1.4"));
        assert!(!is_newer("v0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("", "0.1.4"));
    }

    #[test]
    fn where_the_app_is_tells_how_it_was_installed() {
        if cfg!(target_os = "macos") {
            assert_eq!(
                install_of(Path::new("/Applications/Sik.app/Contents/MacOS/sik")),
                Some(Install::Bundle(PathBuf::from("/Applications/Sik.app")))
            );
            assert_eq!(install_of(Path::new("/Users/u/sik/target/release/sik")), None);
        }
        if cfg!(target_os = "linux") {
            let exe = Path::new("/home/u/.local/share/sik/app/sik");
            assert_eq!(install_of(exe), Some(Install::Linux(exe.to_path_buf())));
            assert_eq!(install_of(Path::new("/home/u/sik/target/release/sik")), None);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod network {
    use super::*;

    /// Downloads a published release and swaps it into a bundle in a temporary folder.
    #[test]
    #[ignore = "downloads a release from GitHub"]
    fn installs_a_release_over_a_bundle() {
        let work = tempdir().unwrap();
        let apps = work.join("Applications");
        let bundle = apps.join("Sik.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/old"), "").unwrap();
        let tag = "v0.1.4";
        let name = format!("sik-0.1.4-macos-{}.zip", std::env::consts::ARCH);
        let archive = work.join(&name);
        download(&format!("{RELEASES}/download/{tag}/{name}"), &archive).unwrap();
        download(&format!("{RELEASES}/download/{tag}/SHA256SUMS"), &work.join("SHA256SUMS")).unwrap();
        let sums = std::fs::read_to_string(work.join("SHA256SUMS")).unwrap();
        assert!(sums.contains(&sha256(&archive).unwrap()));
        let unpack = work.join("unpack");
        std::fs::create_dir_all(&unpack).unwrap();
        let relaunch = replace_bundle(&archive, &bundle, &unpack).unwrap();
        assert!(bundle.join("Contents/MacOS/sik").is_file());
        assert!(!bundle.join("Contents/MacOS/old").exists());
        assert!(!apps.join(".Sik.app.old").exists() && !apps.join(".Sik.app.update").exists());
        assert_eq!(relaunch[..2], ["open", "-a"]);
        let _ = std::fs::remove_dir_all(&work);
    }
}
