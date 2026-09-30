//! Pty for one of the agent's terminals.

use std::{
    io::{Read, Write as _},
    path::Path,
    sync::mpsc,
};

use anyhow::{Context as _, Result};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
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
    /// Starts `command` (or the user's shell) in `cwd`.
    pub fn spawn(cwd: &Path, command: Option<&[String]>, cols: u16, rows: u16) -> Result<(Self, PtyIo)> {
        let pair = native_pty_system()
            .openpty(size(cols, rows))
            .context("could not open the pty")?;

        let mut cmd = match command {
            Some([program, args @ ..]) => {
                let mut cmd = CommandBuilder::new(program);
                cmd.args(args);
                cmd
            }
            _ => CommandBuilder::new_default_prog(),
        };
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", proto::APP);
        // `sik task` knows it runs inside sik, and the app opens the task.
        cmd.env("SIK_TERMINAL", "1");
        if std::env::var_os("LANG").is_none()
            && let Some(lang) = crate::platform::default_lang()
        {
            cmd.env("LANG", lang);
        }
        // `sik` in the PATH is the agent itself: `sik task <name>` creates tasks.
        if let Ok(bin) = crate::cli::bin_dir() {
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

        let child = pair
            .slave
            .spawn_command(cmd)
            .context("could not start the shell")?;
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
                master: pair.master,
                input: input_tx,
                killer,
                pid,
            },
            PtyIo { output, child },
        ))
    }

    pub fn write(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(size(cols, rows));
    }

    /// The shell (or command) the terminal started.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// The pty's foreground process, or the shell if unknown.
    pub fn foreground_pid(&self) -> Option<u32> {
        crate::platform::foreground_pid(self.master.as_ref()).or(self.pid)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.killer.kill();
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
