//! Pty for one of the agent's terminals.

use std::{
    io::{Read, Write as _},
    path::Path,
    sync::mpsc,
};

use anyhow::{Context as _, Result};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: Option<u32>,
}

/// What `Pty::spawn` returns to read the output and wait for the process.
pub struct PtyIo {
    pub output: Box<dyn Read + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

impl Pty {
    /// Starts `command` (or the user's shell) in `cwd`, as terminal `term`.
    pub fn spawn(term: proto::TermId, cwd: &Path, command: Option<&[String]>, cols: u16, rows: u16) -> Result<(Self, PtyIo)> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .context("could not open the pty")?;

        let mut cmd = match command {
            Some([program, args @ ..]) => {
                let mut cmd = CommandBuilder::new(program);
                cmd.args(args);
                cmd
            }
            _ => crate::platform::default_shell(),
        };
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", proto::APP);
        // `den task` knows it runs inside den, and the app opens the task.
        cmd.env("DEN_TERMINAL", "1");
        // `den` commands act on the workspace of the terminal they run in.
        cmd.env("DEN_TERM", term.to_string());
        if std::env::var_os("LANG").is_none()
            && let Some(lang) = crate::platform::default_lang()
        {
            cmd.env("LANG", lang);
        }
        // `den` in the PATH is the agent itself: `den task <name>` creates tasks.
        if let Ok(bin) = proto::bin_dir() {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut paths = vec![bin];
            paths.extend(std::env::split_paths(&path));
            if let Ok(path) = std::env::join_paths(paths) {
                cmd.env("PATH", path);
            }
        }
        // Each terminal is an independent session: it doesn't inherit the markers
        // of a Claude Code session the app may have been launched from.
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy();
            if key.starts_with("CLAUDECODE") || key.starts_with("CLAUDE_CODE_") {
                cmd.env_remove(key.as_ref());
            }
        }

        // Claude Code connects to the agent as to an IDE (`ide.rs`).
        if let Some(port) = crate::ide::port() {
            cmd.env("CLAUDE_CODE_SSE_PORT", port.to_string());
        }

        let child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(error) => {
                crate::platform::close_failed_pty(pair);
                return Err(error).context("could not start the shell");
            }
        };
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let output = pair.master.try_clone_reader()?;

        // Writing happens on its own thread so it doesn't block if the pty is full.
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = pair.master.take_writer()?;
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });

        Ok((
            Self {
                master: Some(pair.master),
                input: input_tx,
                killer,
                pid,
            },
            PtyIo { output, child },
        ))
    }

    /// A terminal an agent this process was before left running: its pty
    /// (`master`) and its process (`pid`, still this process's child), as
    /// `spawn` would have returned them (see `handover.rs`).
    #[cfg(unix)]
    pub fn adopt(master: std::os::fd::OwnedFd, pid: u32) -> Result<(Self, PtyIo)> {
        let master = adopted::Master(master);
        let output = master.try_clone_reader()?;
        let child = adopted::Child(pid);
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        let mut writer = master.take_writer()?;
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer.write_all(&bytes).and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        });
        let pty = Self {
            master: Some(Box::new(master)),
            input: input_tx,
            killer: child.clone_killer(),
            pid: Some(pid),
        };
        Ok((pty, PtyIo { output, child: Box::new(child) }))
    }

    /// The pty's own descriptor, to hand it to the agent that replaces this one.
    #[cfg(unix)]
    pub fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        self.master.as_ref()?.as_raw_fd()
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.as_ref().unwrap().resize(size(cols, rows));
    }

    /// The shell (or command) the terminal started.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The pty's foreground process, or the shell if unknown.
    pub fn foreground_pid(&self) -> Option<u32> {
        crate::platform::foreground_pid(self.master.as_ref().unwrap().as_ref()).or(self.pid)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.killer.kill();
        if let Some(master) = self.master.take() {
            crate::platform::close_pty(master);
        }
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// A pty and a process this agent didn't start but took over: what
/// portable-pty does for its own, on a descriptor and a pid.
#[cfg(unix)]
mod adopted {
    use std::{
        fs::File,
        io::{Error, Result as IoResult},
        os::fd::{AsRawFd as _, OwnedFd, RawFd},
    };

    use portable_pty::{ExitStatus, PtySize};

    pub struct Master(pub OwnedFd);

    impl Master {
        fn file(&self) -> anyhow::Result<File> {
            Ok(File::from(self.0.try_clone()?))
        }
    }

    impl portable_pty::MasterPty for Master {
        fn resize(&self, size: PtySize) -> anyhow::Result<()> {
            let size = libc::winsize { ws_row: size.rows, ws_col: size.cols, ws_xpixel: 0, ws_ypixel: 0 };
            if unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCSWINSZ as _, &size) } != 0 {
                return Err(Error::last_os_error().into());
            }
            Ok(())
        }

        fn get_size(&self) -> anyhow::Result<PtySize> {
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            if unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCGWINSZ as _, &mut size) } != 0 {
                return Err(Error::last_os_error().into());
            }
            Ok(PtySize { rows: size.ws_row, cols: size.ws_col, pixel_width: 0, pixel_height: 0 })
        }

        fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
            Ok(Box::new(self.file()?))
        }

        fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
            Ok(Box::new(self.file()?))
        }

        fn process_group_leader(&self) -> Option<libc::pid_t> {
            match unsafe { libc::tcgetpgrp(self.0.as_raw_fd()) } {
                pid if pid > 0 => Some(pid),
                _ => None,
            }
        }

        fn as_raw_fd(&self) -> Option<RawFd> {
            Some(self.0.as_raw_fd())
        }

        fn tty_name(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[derive(Debug, Clone)]
    pub struct Child(pub u32);

    impl portable_pty::ChildKiller for Child {
        /// A hangup, as portable-pty sends its own.
        fn kill(&mut self) -> IoResult<()> {
            match unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGHUP) } {
                0 => Ok(()),
                _ => Err(Error::last_os_error()),
            }
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    impl portable_pty::Child for Child {
        fn try_wait(&mut self) -> IoResult<Option<ExitStatus>> {
            self.wait_with(libc::WNOHANG)
        }

        fn wait(&mut self) -> IoResult<ExitStatus> {
            loop {
                if let Some(status) = self.wait_with(0)? {
                    return Ok(status);
                }
            }
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.0)
        }
    }

    impl Child {
        fn wait_with(&self, options: libc::c_int) -> IoResult<Option<ExitStatus>> {
            let mut status = 0;
            match unsafe { libc::waitpid(self.0 as libc::pid_t, &mut status, options) } {
                0 => Ok(None),
                -1 if Error::last_os_error().kind() == std::io::ErrorKind::Interrupted => Ok(None),
                -1 => Err(Error::last_os_error()),
                _ if libc::WIFEXITED(status) => Ok(Some(ExitStatus::with_exit_code(libc::WEXITSTATUS(status) as u32))),
                _ => Ok(Some(ExitStatus::with_signal("killed"))),
            }
        }
    }
}
