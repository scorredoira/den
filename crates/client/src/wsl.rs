//! WSL distros as servers (Windows only). A destination `wsl:<distro>` is
//! reached with `wsl.exe` instead of `ssh`: the same Linux agent is installed
//! in the distro's home and spoken to over `bridge`'s stdio. WSL2 forwards
//! the distro's localhost to Windows, so URLs need no tunnel.

use std::process::{Command, Stdio};

use anyhow::{Context as _, Result};

/// Prefix of the destinations that are WSL distros.
pub const PREFIX: &str = "wsl:";

/// The distro of a `wsl:<distro>` destination.
pub fn distro(destination: &str) -> Option<&str> {
    destination.strip_prefix(PREFIX).filter(|distro| !distro.is_empty())
}

fn wsl() -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("wsl.exe");
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    // Its own messages (a missing distro…) in UTF-8 rather than UTF-16.
    command.env("WSL_UTF8", "1");
    command
}

/// `sh -c <next argument>` in the distro, from its home folder like `ssh`.
pub(crate) fn shell(distro: &str) -> Command {
    let mut command = wsl();
    command.args(["-d", distro, "--cd", "~", "-e", "sh", "-c"]);
    command
}

/// Keeps the distro running while the agent `pid` lives: WSL stops a distro
/// shortly after its last `wsl.exe` exits, and with it the agent's terminals.
/// The process isn't tied to the app, so terminals survive closing it; it
/// ends when the agent exits on its own (idle, or shut down from a UI). One
/// per agent: reconnecting doesn't add another. The lock is a `flock`, which
/// goes away with its holder: a restarted distro reuses PIDs, and a leftover
/// lock file would otherwise leave the new agent unheld.
pub(crate) fn hold(distro: &str, pid: u32) -> Result<()> {
    shell(distro)
        .arg(format!(
            "exec 9>>/tmp/sik-hold-{pid}; \
             if command -v flock >/dev/null; then flock -n 9 || exit 0; fi; \
             while kill -0 {pid} 2>/dev/null; do sleep 30; done"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("could not run wsl.exe")?;
    Ok(())
}

/// The installed distros, as destinations.
pub fn destinations() -> Vec<String> {
    let Ok(output) = wsl().args(["-l", "-q"]).stdin(Stdio::null()).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    parse_list(&output.stdout)
        .into_iter()
        .map(|distro| format!("{PREFIX}{distro}"))
        .collect()
}

/// `wsl.exe -l -q` writes one per line, in UTF-16 if it predates `WSL_UTF8`.
fn parse_list(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.len() % 2 == 0 && bytes.iter().skip(1).step_by(2).any(|&b| b == 0) {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.lines()
        .map(|line| line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0'))
        // Docker Desktop's distros aren't for working in.
        .filter(|line| !line.is_empty() && !line.starts_with("docker-desktop"))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_the_distro_list() {
        let utf16: Vec<u8> = "Ubuntu-24.04\r\ndocker-desktop\r\nDebian\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(super::parse_list(&utf16), ["Ubuntu-24.04", "Debian"]);
        assert_eq!(super::parse_list(b"Ubuntu\n\n"), ["Ubuntu"]);
    }

    #[test]
    fn destinations_name_a_distro() {
        assert_eq!(super::distro("wsl:Ubuntu"), Some("Ubuntu"));
        assert_eq!(super::distro("wsl:"), None);
        assert_eq!(super::distro("myserver"), None);
    }
}
