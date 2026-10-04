use std::{future::Future, path::PathBuf, pin::Pin};

/// What arrives from a terminal's process.
pub enum PtyEvent {
    Output(Vec<u8>),
    Exit,
    /// The connection to the process was lost (it's still alive in its agent).
    Disconnected,
}

/// The other end of a terminal: where keys and the size go. Today it's the
/// local agent; in phase 3, a server's agent over SSH.
pub trait TerminalBackend: 'static {
    fn write(&self, bytes: Vec<u8>);
    fn resize(&self, cols: u16, rows: u16);
    /// Kills the process.
    fn kill(&self);
    /// Current directory of the foreground process, if already known. It may
    /// request it in the background and have it on the next call.
    fn cwd(&self) -> Option<PathBuf>;
    /// Saves a pasted image on the process's machine and returns its path there.
    fn save_image(&self, extension: &str, data: Vec<u8>) -> Pin<Box<dyn Future<Output = anyhow::Result<PathBuf>>>>;
    /// Clears the history and the screen for every view of the process, as
    /// its output. Fails where the other end can't (an older agent).
    fn clear(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<()>>>> {
        Box::pin(async { anyhow::bail!("this terminal can't be cleared at its end") })
    }
}
