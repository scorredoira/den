//! The `sik` command in terminals: the agent binary itself, linked as `sik`
//! in a folder that comes first in each terminal's PATH. It talks to the
//! agent on its own machine, so it works the same over SSH.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use client::Client;
use proto::{Request, Response, TermId};

use crate::platform;

pub const USAGE: &str = "\
Usage:
  sik <path>          opens a folder, or a file in its repo, in sik. Over
                      SSH, in the app connected to this server.
  sik worktree <name> creates a worktree in the repo of the current folder,
                      running its .sik/create if it has one, and prints its
                      path. Inside sik, the app also opens it.

In sik's terminals, these act on the workspace of the terminal they run in.
Paths are relative to the current folder; lines and columns start at 1. The
keyboard stays in the terminal unless --focus.

  sik show <file>[:<line>[:<col>]] [--focus]
                      opens the file with the cursor at that line.
  sik show <file>:<line>[:<col>]-<line>[:<col>] [--focus]
                      opens it with that range selected (whole lines
                      without columns).
  sik diff [<file>]   shows the uncommitted changes of the file, or the
                      list of changed files.
  sik doc [<title>]   shows the Markdown read from stdin in a tab.
  sik selection       prints the file and range selected in the editor
                      (path:line:col-line:col), then the selected text.
  sik tabs            lists the files open in the editor, the active one
                      with its cursor.
  sik message <text>  shows a message in the status bar.
  sik workspaces      lists the workspaces open on every server: working,
                      waiting (asking something) or finished unseen.
  sik term list       the workspace's terminals: id, title, `*` the active.
  sik term new [--right | --down] [--focus] [<command>...]
                      opens a terminal (a new tab, or split from this
                      one), types the command in its shell and prints its id.
  sik term read <id> [<lines>]
                      prints its last lines (50 by default).
  sik term send <id> [--no-enter] <text>...
                      types the text in it, and Enter.
  sik term focus <id> shows it and gives it the keyboard.
  sik term close <id> closes it, ending what runs in it.
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

/// The Claude Code skill that tells Claude about the `sik` commands.
const SKILL: &str = include_str!("skill.md");

/// Installs the skill where Claude Code looks for the user's, if Claude
/// Code is installed (`~/.claude` exists). Kept up to date with the agent.
pub fn install_skill() -> Result<()> {
    let Some(claude) = std::env::home_dir().map(|home| home.join(".claude")).filter(|dir| dir.is_dir()) else {
        return Ok(());
    };
    let dir = claude.join("skills").join(proto::APP);
    let file = dir.join("SKILL.md");
    if std::fs::read_to_string(&file).is_ok_and(|text| text == SKILL) {
        return Ok(());
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(file, SKILL)?;
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

/// The terminal of sik this runs in, if any.
fn own_term() -> Option<TermId> {
    std::env::var("SIK_TERM").ok()?.parse().ok()
}

fn connect() -> Result<std::sync::Arc<Client>> {
    Client::connect_local(&std::env::current_exe()?).context("could not talk to the agent")
}

/// A command the app runs (see `USAGE`): it prints the app's answer.
pub fn command(args: &[String]) -> Result<()> {
    let mut args = args.to_vec();
    // Markdown comes from stdin; the app gets it as the last argument.
    if args[0] == "doc" {
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            bail!("pipe the Markdown into it: echo \"# Title\" | sik doc");
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        let title = match args[1..].join(" ") {
            title if title.is_empty() => "Notes".to_string(),
            title => title,
        };
        args = vec!["doc".into(), title, text];
    }
    let client = connect()?;
    let response = smol::block_on(client.request(Request::Command {
        args,
        cwd: std::env::current_dir()?,
        term: own_term(),
    }))?;
    let Response::Text(text) = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    print_text(&text);
    Ok(())
}

/// `sik term …`: reading, typing in and closing a terminal is the agent's
/// own; opening one, or anything about where it is, the app's.
pub fn term(args: &[String]) -> Result<()> {
    let id = |arg: Option<&String>| -> Result<TermId> {
        let arg = arg.context("which terminal? (`sik term list`)")?;
        arg.parse().with_context(|| format!("{arg}: not a terminal id"))
    };
    match args.first().map(String::as_str) {
        Some("read") => {
            let term = id(args.get(1))?;
            let lines = match args.get(2) {
                Some(lines) => lines.parse().with_context(|| format!("{lines}: not a number of lines"))?,
                None => 50,
            };
            let response = smol::block_on(connect()?.request(Request::TermRead { term, lines }))?;
            let Response::Text(text) = response else {
                bail!("unexpected response from the agent: {response:?}");
            };
            print_text(&text);
            Ok(())
        }
        Some("send") => {
            let term = id(args.get(1))?;
            let mut words = &args[2..];
            let enter = words.first().is_none_or(|word| word != "--no-enter");
            if !enter {
                words = &words[1..];
            }
            let client = connect()?;
            let input = |data: Vec<u8>| smol::block_on(client.request(Request::TermInput { term, data }));
            input(words.join(" ").into_bytes())?;
            if enter {
                // Apart from the text: a TUI (Claude Code) takes an Enter in
                // the same burst as part of a paste, not as sending it.
                std::thread::sleep(std::time::Duration::from_millis(100));
                input(b"\r".to_vec())?;
            }
            Ok(())
        }
        Some("close") => {
            let term = id(args.get(1))?;
            smol::block_on(connect()?.request(Request::TermKill { term }))?;
            Ok(())
        }
        Some(_) => command(&[&["term".to_string()], args].concat()),
        None => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn print_text(text: &str) {
    if text.is_empty() {
        return;
    }
    if text.ends_with('\n') {
        print!("{text}");
    } else {
        println!("{text}");
    }
}
