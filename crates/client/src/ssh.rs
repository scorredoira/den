//! Connection to a server's agent over SSH: uses the system `ssh` (with
//! `~/.ssh/config`, keys, agent and ProxyJump) and, if the server doesn't have
//! this version's agent, uploads it over the same connection (compressed:
//! it's a few MB, and a home uplink takes seconds for each). On Windows a
//! `wsl:<distro>` destination goes through `wsl.exe` the same way (`wsl.rs`).

use std::{
    io::Write as _,
    net::{Ipv4Addr, Ipv6Addr, TcpListener},
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
#[cfg(unix)]
const CONTROL_PATH: &str = "ControlPath=~/.ssh/sik-%C";

/// Ensures the master connection. It's created separately with no input or
/// output: if the first regular `ssh` created it, staying in the background it
/// would hold on to its output and whoever reads it would never finish.
#[cfg(unix)]
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

#[cfg(windows)]
fn ensure_master(_destination: &str) -> Result<()> { Ok(()) }

fn ssh_command() -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new("ssh");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

/// `ssh` that uses the master connection if it exists (and its own otherwise).
/// The next argument is the command to run there.
fn ssh(destination: &str) -> Command {
    #[cfg(windows)]
    if let Some(distro) = crate::wsl::distro(destination) {
        return crate::wsl::shell(distro);
    }
    let mut command = ssh_command();
    command.args(OPTIONS);
    #[cfg(unix)]
    command.args(["-o", "ControlMaster=no", "-o", CONTROL_PATH]);
    command.arg(destination);
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
/// agents for other systems. `step` is told what it's doing.
fn install(destination: &str, agents: &Path, step: &dyn Fn(&'static str)) -> Result<(String, std::path::PathBuf)> {
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
    // Whether it's there and, if not, whether the server can unpack gzip.
    let present = run(
        destination,
        &format!("test -x {remote} && echo si || {{ command -v gzip >/dev/null && echo gzip || echo raw; }}"),
        None,
    )?;
    let present = present.trim();
    if present != "si" {
        step("uploading the agent…");
        let (unpack, payload) = if present == "gzip" {
            ("gzip -dc", gzip(&binary)?)
        } else {
            ("cat", binary)
        };
        run(
            destination,
            // Old versions are deleted: an agent still using them doesn't notice.
            &format!(
                "mkdir -p {REMOTE_DIR} && {unpack} > {remote}.part && chmod +x {remote}.part && mv {remote}.part {remote} \
                 && find {REMOTE_DIR} -maxdepth 1 -name 'sik-agent-*' ! -path {remote} -delete"
            ),
            Some(&payload),
        )
        .context("could not upload the agent")?;
        step("starting the agent…");
    }
    Ok((remote, local))
}

/// `bytes` as `gzip -dc` reads them.
fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

/// Connects to the agent on `destination` (a name from `~/.ssh/config` or
/// `user@host`), installing it if needed. `step` is told what it's doing
/// when it isn't just connecting (uploading the agent takes a while).
pub fn connect_ssh(destination: &str, agents: &Path, step: &dyn Fn(&'static str)) -> Result<Arc<Client>> {
    ensure_master(destination)?;
    let (remote, local) = install(destination, agents, step)?;
    let mut process = ssh(destination)
        .arg(format!("{remote} bridge"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not run ssh")?;
    let reader = process.stdout.take().expect("stdout");
    let writer = process.stdin.take().expect("stdin");
    let client = Client::from_stream(Box::new(reader), Box::new(writer), Some(process), None, Some(destination.to_string()));
    match smol::block_on(client.request(Request::Hello { protocol: PROTOCOL }))? {
        #[cfg_attr(unix, allow(unused_variables))]
        Response::Hello { protocol, pid } if protocol == PROTOCOL => {
            #[cfg(windows)]
            if let Some(distro) = crate::wsl::distro(destination) {
                crate::wsl::hold(distro, pid)?;
            }
            client.check_version(&local);
            Ok(client)
        }
        other => bail!("the agent on {destination} does not speak protocol {PROTOCOL}: {other:?}"),
    }
}

/// An `http(s)` URL pointing at the machine it was seen on.
pub(crate) struct LoopbackUrl<'a> {
    scheme: &'a str,
    /// As written in the URL (`[::1]` with its brackets).
    host: &'a str,
    pub port: u16,
    /// Everything after the port: path, query and fragment.
    rest: &'a str,
}

impl<'a> LoopbackUrl<'a> {
    pub fn parse(url: &'a str) -> Option<Self> {
        let (scheme, after) = url.split_once("://")?;
        let default_port = match scheme {
            "http" => 80,
            "https" => 443,
            _ => return None,
        };
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let (authority, rest) = after.split_at(end);
        let (host, port) = match authority.rfind(':') {
            Some(colon) if !authority[colon..].contains(']') => (&authority[..colon], Some(&authority[colon + 1..])),
            _ => (authority, None),
        };
        let port = match port {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        let loopback = match host.trim_start_matches('[').trim_end_matches(']') {
            "localhost" | "0.0.0.0" | "::" => true,
            host => host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        };
        loopback.then_some(Self { scheme, host, port, rest })
    }

    /// Where the server should connect: `0.0.0.0` and `[::]` listen on every
    /// address, loopback among them.
    fn target_host(&self) -> &str {
        match self.host {
            "0.0.0.0" => "127.0.0.1",
            "[::]" => "[::1]",
            host => host,
        }
    }

    /// The URL on this machine, where the server's port is `port`.
    pub fn with_port(&self, port: u16) -> String {
        let host = match self.host {
            "0.0.0.0" | "[::]" => "localhost",
            host => host,
        };
        format!("{}://{host}:{port}{}", self.scheme, self.rest)
    }
}

/// Whether nothing on this machine listens on `port`, on IPv4 or IPv6 (only
/// "in use" counts: a machine without IPv6 can't bind `::1` at all).
fn port_free(port: u16) -> bool {
    let in_use = |result: std::io::Result<TcpListener>| {
        matches!(result, Err(err) if err.kind() == std::io::ErrorKind::AddrInUse)
    };
    !in_use(TcpListener::bind((Ipv4Addr::LOCALHOST, port))) && !in_use(TcpListener::bind((Ipv6Addr::LOCALHOST, port)))
}

/// Forwards the URL's port on the server to a port on this machine through
/// the master connection, and returns the local port: the same one if it's
/// free (dev servers often check the `Host` or put the port in redirects),
/// any free one otherwise.
#[cfg(unix)]
pub(crate) fn forward(destination: &str, url: &LoopbackUrl) -> Result<Forward> {
    let add = |local: u16| -> Result<()> {
        let output = Command::new("ssh")
            .args(OPTIONS)
            .args(["-o", CONTROL_PATH, "-O", "forward", "-L"])
            .arg(format!("{local}:{}:{}", url.target_host(), url.port))
            .arg(destination)
            .stdin(Stdio::null())
            .output()
            .context("could not run ssh")?;
        if !output.status.success() {
            bail!(
                "could not forward port {} of {destination}: {}",
                url.port,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    };
    if port_free(url.port) && add(url.port).is_ok() {
        return Ok(Forward { port: url.port, process: None });
    }
    let local = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port();
    add(local)?;
    Ok(Forward { port: local, process: None })
}

/// A Windows tunnel has its own SSH process, owned by the client connection.
pub(crate) struct Forward {
    pub port: u16,
    process: Option<std::process::Child>,
}

impl Drop for Forward {
    fn drop(&mut self) {
        if let Some(child) = self.process.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(windows)]
pub(crate) fn forward(destination: &str, url: &LoopbackUrl) -> Result<Forward> {
    use std::{net::TcpStream, time::{Duration, Instant}};
    // WSL2 already forwards the distro's localhost.
    if crate::wsl::distro(destination).is_some() {
        return Ok(Forward { port: url.port, process: None });
    }
    let port = if port_free(url.port) { url.port } else {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?.local_addr()?.port()
    };
    let child = ssh_command().args(OPTIONS)
        .args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
        .arg(format!("127.0.0.1:{port}:{}:{}", url.target_host(), url.port))
        .arg(destination)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let mut forward = Forward { port, process: Some(child) };
    let start = Instant::now();
    loop {
        if let Some(status) = forward.process.as_mut().unwrap().try_wait()? {
            bail!("SSH port forwarding failed ({status})");
        }
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() { return Ok(forward); }
        if start.elapsed() > Duration::from_secs(10) { bail!("SSH port forwarding timed out"); }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::LoopbackUrl;

    /// The upload is unpacked on the server with `gzip -dc`.
    #[test]
    #[cfg(unix)]
    fn the_upload_unpacks_with_gzip() {
        use std::io::Write as _;
        let binary: Vec<u8> = (0..200_000u32).flat_map(|n| (n % 251).to_le_bytes()).collect();
        let packed = super::gzip(&binary).unwrap();
        assert!(packed.len() < binary.len() / 2);
        let mut child = std::process::Command::new("gzip")
            .arg("-dc")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&packed).unwrap();
        assert_eq!(child.wait_with_output().unwrap().stdout, binary);
    }

    #[test]
    fn parses_loopback_urls() {
        let url = LoopbackUrl::parse("http://localhost:5173/app?x=1").unwrap();
        assert_eq!((url.port, url.target_host()), (5173, "localhost"));
        assert_eq!(url.with_port(5174), "http://localhost:5174/app?x=1");

        let url = LoopbackUrl::parse("http://0.0.0.0:8000").unwrap();
        assert_eq!((url.port, url.target_host()), (8000, "127.0.0.1"));
        assert_eq!(url.with_port(8000), "http://localhost:8000");

        let url = LoopbackUrl::parse("http://[::1]:3000/").unwrap();
        assert_eq!((url.port, url.target_host()), (3000, "[::1]"));
        assert_eq!(LoopbackUrl::parse("https://127.0.0.1/").unwrap().port, 443);

        assert!(LoopbackUrl::parse("https://example.com:8080/").is_none());
        assert!(LoopbackUrl::parse("http://192.168.1.10:8080/").is_none());
        assert!(LoopbackUrl::parse("ftp://localhost:21/").is_none());
    }
}
