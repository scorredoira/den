//! What depends on the operating system. See "Platforms" in plan.md.

use std::{
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;

/// Connects to the agent's local socket: one end for reading and one for writing.
#[cfg(unix)]
pub fn connect(socket: &Path) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>, crate::CloseStream)> {
    split(std::os::unix::net::UnixStream::connect(socket)?)
}

#[cfg(unix)]
pub(crate) fn split(stream: std::os::unix::net::UnixStream) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>, crate::CloseStream)> {
    let closer = stream.try_clone()?;
    let close = Box::new(move || {
        let _ = closer.shutdown(std::net::Shutdown::Both);
    });
    Ok((Box::new(stream.try_clone()?), Box::new(stream), close))
}

#[cfg(windows)]
pub fn connect(socket: &Path) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>, crate::CloseStream)> {
    let stream = crate::windows_pipe::Stream::connect(socket)?;
    let closer = stream.clone();
    Ok((Box::new(stream.clone()), Box::new(stream), Box::new(move || closer.close())))
}

/// Starts the agent daemon. It detaches itself from the app's session, so it
/// survives the app closing.
pub fn spawn_daemon(agent_bin: &Path, log: &Path) -> Result<()> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    let mut command = std::process::Command::new(agent_bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No inherited console; the agent owns the ConPTY sessions independently.
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()?;
    Ok(())
}
