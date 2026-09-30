//! Connection to a server's agent over SSH: uses the system `ssh` (with
//! `~/.ssh/config`, keys, agent and ProxyJump) and, if the server doesn't have
//! this version's agent, uploads it over the same connection.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, Stdio},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use proto::{PROTOCOL, Request, Response};

use crate::Client;

/// Name, next to the app, of the agent built for Linux x86_64 servers.
pub const AGENT_LINUX_X86_64: &str = "sik-agent-linux-x86_64";

/// Server folder (relative to its `$HOME`) where the agent is installed.
const REMOTE_DIR: &str = ".local/share/sik";

/// Common options: never prompt for passwords (keys or agent), and detect a
/// dropped connection in about 45 s.
const OPTIONS: [&str; 8] = [
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=3",
];

/// Shared master connection: opening more streams to the server is instant.
const CONTROL_PATH: &str = "ControlPath=~/.ssh/sik-%C";

/// Ensures the master connection. It's created separately with no input or
/// output: if the first regular `ssh` created it, staying in the background it
/// would hold on to its output and whoever reads it would never finish.
fn ensure_master(destination: &str) -> Result<()> {
    let status = Command::new("ssh")
        .args(OPTIONS)
        .args(["-o", "ControlMaster=auto", "-o", CONTROL_PATH, "-o", "ControlPersist=10m", "-N", "-f"])
        .arg(destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("could not run ssh")?;
    if !status.success() {
        bail!("could not connect to {destination} over SSH (SSH key or agent?)");
    }
    Ok(())
}

/// `ssh` that uses the master connection if it exists (and its own otherwise).
fn ssh(destination: &str) -> Command {
    let mut command = Command::new("ssh");
    command
        .args(OPTIONS)
        .args(["-o", "ControlMaster=no", "-o", CONTROL_PATH])
        .arg(destination);
    command
}

fn run(destination: &str, script: &str, input: Option<&[u8]>) -> Result<String> {
    let mut child = ssh(destination)
        .arg(script)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("could not run ssh")?;
    if let Some(input) = input {
        child.stdin.take().expect("stdin").write_all(input)?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "ssh {destination}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Installs the agent on the server (if needed) and returns its path there
/// and the path of the matching local binary. `agents` is the folder with the
/// agents for other systems.
fn install(destination: &str, agents: &Path) -> Result<(String, std::path::PathBuf)> {
    let system = run(destination, "uname -sm", None)?;
    let name = match system.trim() {
        "Linux x86_64" => AGENT_LINUX_X86_64,
        other => bail!("server system not supported yet: {other}"),
    };
    // Next to the app in development; in `Sik.app`, in `Contents/Resources`
    // (`Contents/MacOS` may only contain macOS code, because of signing).
    let local = [agents.join(name), agents.join("../Resources").join(name)]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| agents.join(name));
    let binary = std::fs::read(&local)
        .with_context(|| format!("missing the agent for the server: {}", local.display()))?;
    let remote = format!("{REMOTE_DIR}/sik-agent-{PROTOCOL}-{}", proto::build_id(&binary));
    let present = run(destination, &format!("test -x {remote} && echo si || true"), None)?;
    if present.trim() != "si" {
        run(
            destination,
            // Old versions are deleted: an agent still using them doesn't notice.
            &format!(
                "mkdir -p {REMOTE_DIR} && cat > {remote}.part && chmod +x {remote}.part && mv {remote}.part {remote} \
                 && find {REMOTE_DIR} -maxdepth 1 -name 'sik-agent-*' ! -path {remote} -delete"
            ),
            Some(&binary),
        )
        .context("could not upload the agent")?;
    }
    Ok((remote, local))
}

/// Connects to the agent on `destination` (a name from `~/.ssh/config` or
/// `user@host`), installing it if needed.
pub fn connect_ssh(destination: &str, agents: &Path) -> Result<Arc<Client>> {
    ensure_master(destination)?;
    let (remote, local) = install(destination, agents)?;
    let mut process = ssh(destination)
        .arg(format!("{remote} bridge"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not run ssh")?;
    let reader = process.stdout.take().expect("stdout");
    let writer = process.stdin.take().expect("stdin");
    let client = Client::from_stream(Box::new(reader), Box::new(writer), Some(process));
    match smol::block_on(client.request(Request::Hello { protocol: PROTOCOL }))? {
        Response::Hello { protocol, .. } if protocol == PROTOCOL => {
            client.check_version(&local);
            Ok(client)
        }
        other => bail!("the agent on {destination} does not speak protocol {PROTOCOL}: {other:?}"),
    }
}
