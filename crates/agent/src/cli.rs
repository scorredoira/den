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
  sik task <name>     creates a task (a worktree) in the repo of the current
                      folder, running its .task/create if it has one, and
                      prints its path. Inside sik, the app also opens it.
";

/// Folder holding the `sik` link, which the agent puts in its terminals' PATH.
pub fn bin_dir() -> Result<PathBuf> {
    Ok(proto::state_dir()?.join("bin"))
}

/// Links `sik` to this binary.
pub fn install() -> Result<()> {
    let dir = bin_dir()?;
    std::fs::create_dir_all(&dir)?;
    platform::symlink(&std::env::current_exe()?, &dir.join(proto::APP))
}

/// Whether we were invoked through the `sik` link rather than as `sik-agent`.
pub fn invoked_as_sik() -> bool {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| path.file_name().map(|name| name == proto::APP))
        .unwrap_or(false)
}

/// `sik task <name>`: prints only the path on stdout (messages go to
/// stderr), so that `cd "$(sik task x)"` works.
pub fn task(args: &[String]) -> Result<()> {
    let [name] = args else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let cwd = std::env::current_dir()?;
    let inside_sik = std::env::var_os("SIK_TERMINAL").is_some();
    let client = Client::connect_local(&std::env::current_exe()?).context("could not talk to the agent")?;
    eprintln!("creating task {name}…");
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
