//! Everything that depends on the operating system. See "Platforms" in plan.md.

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Connection with a UI: read on one thread and written on another.
pub trait Stream: std::io::Read + std::io::Write + Send + 'static {
    fn try_clone_stream(&self) -> Result<Box<dyn Stream>>;
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

/// Current directory of a process. Not implemented: on Windows it will be
/// read from the OSC 7 sequence the shell emits.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
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

#[cfg(not(unix))]
pub fn process_args(_pid: u32) -> Option<String> {
    None
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path.metadata().ok().filter(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).map(|_| path.to_path_buf());
    }
    #[cfg(windows)]
    path.is_file().then(|| path.to_path_buf())
}

pub fn script_command(path: &Path) -> std::process::Command {
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
