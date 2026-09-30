//! What depends on the operating system. See "Platforms" in plan.md.

use std::{
    io::{Read, Write},
    path::Path,
};

use anyhow::Result;

/// Connects to the agent's local socket: one end for reading and one for writing.
#[cfg(unix)]
pub fn connect(socket: &Path) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    let stream = std::os::unix::net::UnixStream::connect(socket)?;
    Ok((Box::new(stream.try_clone()?), Box::new(stream)))
}

/// Not implemented on Windows: the named pipe will go here.
#[cfg(not(unix))]
pub fn connect(_socket: &Path) -> Result<(Box<dyn Read + Send>, Box<dyn Write + Send>)> {
    anyhow::bail!("not implemented on Windows")
}

/// Starts the agent daemon. It detaches itself from the app's session, so it
/// survives the app closing.
pub fn spawn_daemon(agent_bin: &Path, log: &Path) -> Result<()> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    std::process::Command::new(agent_bin)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()?;
    Ok(())
}
