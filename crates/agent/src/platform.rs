//! Everything that depends on the operating system. See "Platforms" in plan.md.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Connection with a UI: read on one thread and written on another.
pub trait Stream: std::io::Read + std::io::Write + Send + 'static {
    fn try_clone_stream(&self) -> Result<Box<dyn Stream>>;
    /// Wakes both a blocked reader and writer without waiting for either.
    fn close_handle(&self) -> Result<std::sync::Arc<dyn Fn() + Send + Sync>>;
}

#[cfg(unix)]
mod unix {
    use std::{
        os::unix::{
            fs::PermissionsExt as _,
            net::{UnixListener, UnixStream},
        },
        path::Path,
    };

    use anyhow::{Context as _, Result, bail};

    use super::Stream;

    impl Stream for UnixStream {
        fn try_clone_stream(&self) -> Result<Box<dyn Stream>> {
            Ok(Box::new(self.try_clone()?))
        }

        fn close_handle(&self) -> Result<std::sync::Arc<dyn Fn() + Send + Sync>> {
            let stream = self.try_clone()?;
            Ok(std::sync::Arc::new(move || { let _ = stream.shutdown(std::net::Shutdown::Both); }))
        }
    }

    pub struct Listener(UnixListener);

    impl Listener {
        /// Listens on `path`. Fails if a live agent is already on that socket.
        pub fn bind(path: &Path) -> Result<Self> {
            // A folder the agent creates can only be opened by its user; one that
            // already existed (e.g. the system temp folder) is left alone.
            if let Some(dir) = path.parent()
                && !dir.exists()
            {
                std::fs::create_dir_all(dir)?;
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
            if path.exists() {
                if UnixStream::connect(path).is_ok() {
                    bail!("an agent is already listening on {}", path.display());
                }
                std::fs::remove_file(path)?;
            }
            let listener = UnixListener::bind(path)
                .with_context(|| format!("could not listen on {}", path.display()))?;
            Ok(Self(listener))
        }

        /// The socket the agent this process was before listened on.
        pub fn handed(fd: std::os::fd::OwnedFd) -> Self {
            Self(UnixListener::from(fd))
        }

        pub fn raw_fd(&self) -> std::os::fd::RawFd {
            std::os::fd::AsRawFd::as_raw_fd(&self.0)
        }

        pub fn accept(&self) -> Result<Box<dyn Stream>> {
            let (stream, _) = self.0.accept()?;
            Ok(Box::new(stream))
        }
    }

    pub fn connect(path: &Path) -> Result<Box<dyn Stream>> {
        Ok(Box::new(UnixStream::connect(path)?))
    }

    /// Detaches from the launcher's session so as not to die with it.
    pub fn detach_session() {
        unsafe {
            libc::setsid();
        }
    }

    pub fn foreground_pid(master: &dyn portable_pty::MasterPty) -> Option<u32> {
        master.process_group_leader().map(|pid| pid as u32)
    }
}

#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows {
    use std::path::Path;
    use anyhow::Result;
    use super::Stream;

    impl Stream for client::windows_pipe::Stream {
        fn try_clone_stream(&self) -> Result<Box<dyn Stream>> {
            Ok(Box::new(self.clone()))
        }

        fn close_handle(&self) -> Result<std::sync::Arc<dyn Fn() + Send + Sync>> {
            let stream = self.clone();
            Ok(std::sync::Arc::new(move || stream.close()))
        }
    }

    pub struct Listener(client::windows_pipe::Listener);
    impl Listener {
        pub fn bind(path: &Path) -> Result<Self> {
            Ok(Self(client::windows_pipe::Listener::bind(path)?))
        }
        pub fn accept(&self) -> Result<Box<dyn Stream>> {
            Ok(Box::new(self.0.accept()?))
        }
    }
    pub fn connect(path: &Path) -> Result<Box<dyn Stream>> {
        Ok(Box::new(client::windows_pipe::Stream::connect(path)?))
    }
    // The launcher creates a process without an inherited console.
    pub fn detach_session() {}
    pub fn foreground_pid(_master: &dyn portable_pty::MasterPty) -> Option<u32> { None }
}

#[cfg(windows)]
pub use windows::*;

/// Current directory of a process, via `proc_pidinfo(PROC_PIDVNODEPATHINFO)`.
#[cfg(target_os = "macos")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::{
        ffi::{CStr, c_int, c_void},
        os::unix::ffi::OsStrExt as _,
    };

    unsafe extern "C" {
        fn proc_pidinfo(pid: c_int, flavor: c_int, arg: u64, buffer: *mut c_void, size: c_int) -> c_int;
    }
    const PROC_PIDVNODEPATHINFO: c_int = 9;
    // `struct proc_vnodepathinfo`: two `vnode_info_path` (current directory and
    // root), each with a 152-byte `vnode_info` and a 1024-byte path.
    const VNODE_INFO: usize = 152;
    const PATH_MAX: usize = 1024;
    let mut buffer = [0u8; 2 * (VNODE_INFO + PATH_MAX)];
    let size = unsafe {
        proc_pidinfo(
            pid as c_int,
            PROC_PIDVNODEPATHINFO,
            0,
            buffer.as_mut_ptr().cast(),
            buffer.len() as c_int,
        )
    };
    if size <= 0 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&buffer[VNODE_INFO..VNODE_INFO + PATH_MAX]).ok()?;
    let path = std::ffi::OsStr::from_bytes(path.to_bytes());
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Current directory of a process.
#[cfg(target_os = "linux")]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Windows directories are tracked from OSC 7 by `shell_cwd`, rather than
/// reading another process's undocumented memory layout.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// A variable of a process's environment, via `sysctl(KERN_PROCARGS2)`:
/// `argc`, the executable's path and its padding, the arguments, then the
/// environment, each ending in NUL.
#[cfg(target_os = "macos")]
pub fn process_env(pid: u32, name: &str) -> Option<String> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    let null = std::ptr::null_mut();
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, null, &mut size, null, 0) } != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buffer.as_mut_ptr().cast(), &mut size, null, 0) } != 0 {
        return None;
    }
    buffer.truncate(size);
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    let strings = buffer[4..].split(|byte| *byte == 0).filter(|string| !string.is_empty());
    env_value(strings.skip(1 + argc.max(0) as usize), name)
}

/// A variable of a process's environment.
#[cfg(target_os = "linux")]
pub fn process_env(pid: u32, name: &str) -> Option<String> {
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env_value(environ.split(|byte| *byte == 0), name)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_env(_pid: u32, _name: &str) -> Option<String> {
    None
}

/// The value of `name` among `NAME=value` entries.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn env_value<'a>(entries: impl Iterator<Item = &'a [u8]>, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    entries
        .filter_map(|entry| entry.strip_prefix(prefix.as_bytes()))
        .map(|value| String::from_utf8_lossy(value).into_owned())
        .next()
}

/// Command line of a process (program and arguments, separated by spaces).
#[cfg(unix)]
pub fn process_args(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let args = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !args.is_empty()).then_some(args)
}

#[cfg(windows)]
pub fn process_args(pid: u32) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Duration;
    // ConPTY doesn't expose a foreground process group. At restart, inspect
    // descendants of this terminal's shell and find the Claude process there.
    // This runs only when explicitly restarting the agent, never while typing.
    let script = format!(r#"
$all = @(Get-CimInstance Win32_Process)
$ids = @([uint32]{pid})
do {{
    $children = @($all | Where-Object {{ $_.ParentProcessId -in $ids -and $_.ProcessId -notin $ids }})
    $ids += @($children | ForEach-Object {{ $_.ProcessId }})
}} while ($children.Count -gt 0)
$all | Where-Object {{ $_.ProcessId -in $ids -and ($_.Name -eq 'claude.exe' -or ($_.Name -eq 'node.exe' -and $_.CommandLine -match '@anthropic-ai[\\/]claude-code')) }} | Select-Object -First 1 -ExpandProperty CommandLine
"#);
    let mut child = command("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(Duration::from_secs(3)).ok();
    let _ = child.kill();
    let _ = child.wait();
    out.map(|text| text.trim().to_string()).filter(|text| !text.is_empty())
}

/// Link `link` pointing to `target` (replaces any previous one).
#[cfg(unix)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

/// Hard links need no symlink privilege, and keep the exact agent binary.
#[cfg(windows)]
pub fn symlink(target: &Path, link: &Path) -> Result<()> {
    let link = link.with_extension("exe");
    let _ = std::fs::remove_file(&link);
    std::fs::hard_link(target, &link).or_else(|_| std::fs::copy(target, &link).map(|_| ()))?;
    Ok(())
}

/// Starts the daemon from this same binary, detached from its launcher.
/// The terminals' `LANG` when the agent has none (the app opened from the
/// Dock doesn't get one): the macOS language in UTF-8, as Terminal.app
/// does. Without it, the shell can't type characters like ñ.
#[cfg(target_os = "macos")]
pub fn default_lang() -> Option<String> {
    let output = std::process::Command::new("defaults").args(["read", "-g", "AppleLocale"]).output().ok()?;
    let locale = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // `es_ES@rg=eszzzz` → `es_ES`.
    let locale = locale.split('@').next().unwrap_or_default();
    Some(if locale.contains('_') { format!("{locale}.UTF-8") } else { "en_US.UTF-8".to_string() })
}

#[cfg(not(target_os = "macos"))]
pub fn default_lang() -> Option<String> {
    None
}

/// Starts the app, opening `path`: the one that last ran on this machine;
/// on macOS, through `open` if it's in a bundle, so it starts as an app.
pub fn launch_app(args: &[&std::ffi::OsStr]) -> Result<()> {
    let app = std::fs::read_to_string(proto::app_file()?)
        .map(|app| PathBuf::from(app.trim()))
        .ok()
        .filter(|app| app.exists())
        .context("could not find the den app: open it once")?;
    let bundle = app.ancestors().find(|dir| dir.extension().is_some_and(|ext| ext == "app"));
    let mut command = match bundle {
        Some(bundle) if cfg!(target_os = "macos") => {
            let mut command = std::process::Command::new("open");
            command.arg("-a").arg(bundle).arg("--args");
            command
        }
        _ => std::process::Command::new(&app),
    };
    configure_background(&mut command);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn spawn_daemon(log: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut command = std::process::Command::new(exe);
    configure_background(&mut command);
    command.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hooks_in_den() {
        let repo = std::env::temp_dir().join(format!("den-hooks-{}", std::process::id()));
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        assert_eq!(super::repo_hook(&repo, "create"), None);
        std::fs::write(repo.join(".den/create"), "").unwrap();
        assert_eq!(super::repo_hook(&repo, "create"), Some(repo.join(".den/create")));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    #[cfg(unix)]
    fn reads_own_cwd() {
        let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
        let read = super::process_cwd(std::process::id()).map(|path| path.canonicalize().unwrap());
        assert_eq!(read, Some(cwd));
    }
}

/// Avoid flashing console windows when a GUI-launched agent runs a helper.
pub fn configure_background(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Resolve executable extensions on Windows (including npm's .cmd shims).
pub fn executable(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    #[cfg(windows)]
    {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for ext in extensions.split(';') {
            let candidate = dir.join(format!("{name}{}", ext.to_ascii_lowercase()));
            if candidate.is_file() { return Some(candidate); }
        }
    }
    path.is_file().then_some(path)
}

/// On Unix hooks are executable files. Windows supports .ps1/.cmd/.bat/.exe,
/// plus extensionless shell scripts via Git for Windows' sh on PATH.
pub fn repo_script(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    for ext in ["ps1", "cmd", "bat", "exe"] {
        let candidate = path.with_extension(ext);
        if candidate.is_file() { return Some(candidate); }
    }
    path.is_file().then(|| path.to_path_buf())
}

/// A repo's hook `name` (`create`, `remove`, `format`), in its `.den` folder.
pub fn repo_hook(repo: &Path, name: &str) -> Option<PathBuf> {
    repo_script(&repo.join(".den").join(name))
}

pub fn script_command(path: &Path) -> std::process::Command {
    let path = dunce::simplified(path);
    #[cfg(windows)]
    {
        let ext = path.extension().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
        let mut command = match ext.as_str() {
            "ps1" => {
                let mut c = std::process::Command::new("powershell.exe");
                c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]).arg(path);
                c
            }
            "exe" | "com" | "cmd" | "bat" => std::process::Command::new(path),
            _ => {
                let mut c = std::process::Command::new("sh");
                c.arg(path);
                c
            }
        };
        configure_background(&mut command);
        command
    }
    #[cfg(unix)]
    std::process::Command::new(path)
}

/// Background helper with the same behavior on every desktop platform.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    configure_background(&mut command);
    command
}

pub fn is_executable_script(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    path.is_file()
}

#[cfg(unix)]
pub fn default_shell() -> portable_pty::CommandBuilder { portable_pty::CommandBuilder::new_default_prog() }

#[cfg(windows)]
pub fn default_shell() -> portable_pty::CommandBuilder {
    // Prefer PowerShell 7, falling back to Windows PowerShell. Load the user's
    // profile normally, preserve their prompt, and append a directory report.
    let shell = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path).find_map(|dir| crate::platform::executable(&dir, "pwsh"))
    }).unwrap_or_else(|| "powershell.exe".into());
    let mut command = portable_pty::CommandBuilder::new(shell);
    command.args(["-NoLogo", "-NoExit", "-Command", r#"$global:DenOriginalPrompt = $function:prompt; function global:prompt { $p = & $global:DenOriginalPrompt; if ($PWD.Provider.Name -eq 'FileSystem') { $u = [Uri]::new($PWD.ProviderPath).AbsoluteUri; [Console]::Write(([char]27).ToString() + ']7;' + $u + [char]7) }; $p }"#]);
    command
}

/// Closing ConPTY can wait for its output to drain on Windows 10 / Server 2022.
/// Never do it while holding the agent state lock needed by the output reader.
pub fn close_pty(master: Box<dyn portable_pty::MasterPty + Send>) {
    #[cfg(windows)]
    std::thread::spawn(move || drop(master));
    #[cfg(unix)]
    drop(master);
}

pub fn close_failed_pty(pair: portable_pty::PtyPair) {
    #[cfg(windows)]
    {
        use std::io::Write;
        if let Ok(mut output) = pair.master.try_clone_reader() {
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut output, &mut std::io::sink());
            });
        }
        // portable-pty requests cursor inheritance. Answer even if the child
        // failed to start, so closing an unused pseudoconsole can complete.
        if let Ok(mut input) = pair.master.take_writer() {
            let _ = input.write_all(b"\x1b[1;1R");
        }
        std::thread::spawn(move || drop(pair));
    }
    #[cfg(unix)]
    drop(pair);
}
