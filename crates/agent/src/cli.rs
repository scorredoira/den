//! The `sik` command in terminals: the agent binary itself, linked as `sik`
//! in a folder that comes first in each terminal's PATH. It talks to the
//! agent on its own machine, so it works the same over SSH.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use client::Client;
use proto::{Request, Response};

use crate::platform;

pub const USAGE: &str = "\
Usage:
  sik <path>          opens a folder, or a file in its repo, in sik. Over
                      SSH, in the app connected to this server.
  sik worktree <name> creates a worktree in the repo of the current folder,
                      running its .sik/create if it has one, and prints its
                      path. Inside sik, the app also opens it.
";

/// Folder holding the `sik` link, which the agent puts in its terminals' PATH.
pub fn bin_dir() -> Result<PathBuf> {
    Ok(proto::state_dir()?.join("bin"))
}

/// Links `sik` to this binary, for sik's terminals and, in `~/.local/bin`
/// if there is one, for any other (unless something else is called `sik` there).
pub fn install() -> Result<()> {
    let dir = bin_dir()?;
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    platform::symlink(&exe, &dir.join(proto::APP))?;
    if let Some(local) = std::env::home_dir().map(|home| home.join(".local/bin")).filter(|dir| dir.is_dir()) {
        let link = local.join(proto::APP);
        let ours = match std::fs::read_link(&link) {
            // The agent runs from versioned copies: `sik-agent-6-…`.
            Ok(target) => target.file_name().is_some_and(|name| name.to_string_lossy().starts_with("sik-agent")),
            Err(_) => !link.exists(),
        };
        if ours {
            platform::symlink(&exe, &link)?;
        }
    }
    Ok(())
}

/// Whether we were invoked through the `sik` link rather than as `sik-agent`.
pub fn invoked_as_sik() -> bool {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| path.file_stem().map(|name| name == proto::APP))
        .unwrap_or(false)
}

/// `sik <path>`: asks the app to open it. With no app connected, it starts
/// one on this machine; over SSH, there's no app to start.
pub fn open(arg: &str) -> Result<()> {
    let path = std::path::absolute(arg)?;
    let path = path.canonicalize().with_context(|| format!("{arg}: no such file or folder"))?;
    let (root, file) = proto::open_target(&path);
    let exe = std::env::current_exe()?;
    let client = Client::connect_local(&exe).context("could not talk to the agent")?;
    let response = smol::block_on(client.request(Request::Open { root, file }))?;
    let Response::Count(count) = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    if count > 0 {
        return Ok(());
    }
    if std::env::var_os("SSH_CONNECTION").is_some() {
        bail!("no sik app is connected to this server: add it in sik's Settings → Servers");
    }
    platform::launch_app(&path)
}

/// `sik worktree <name>`: prints only the path on stdout (messages go to
/// stderr), so that `cd "$(sik task x)"` works.
pub fn task(args: &[String]) -> Result<()> {
    let [name] = args else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let cwd = std::env::current_dir()?;
    let inside_sik = std::env::var_os("SIK_TERMINAL").is_some();
    let client = Client::connect_local(&std::env::current_exe()?).context("could not talk to the agent")?;
    eprintln!("creating worktree {name}…");
    let response = smol::block_on(client.request(Request::TaskCreate {
        repo: cwd,
        name: name.clone(),
        open: inside_sik,
    }))?;
    let Response::Task(task) = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    println!("{}", task.path.display());
    Ok(())
}
