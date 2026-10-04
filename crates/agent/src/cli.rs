//! The `den` command in terminals: the agent binary itself, linked as `den`
//! in a folder that comes first in each terminal's PATH. It talks to the
//! agent on its own machine, so it works the same over SSH.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use client::Client;
use proto::{Request, Response, TermId};

use crate::platform;

pub const USAGE: &str = "\
Usage:
  den <path>          opens a folder, or a file in its repo, in den. Over
                      SSH, in the app connected to this server; in den's
                      terminals, in the terminal's window.
  den -n <path>       opens it in a window of its own, even in den's
                      terminals; in the window it's open in, if it is.
  den -s <server> [<path>]
                      opens a window on the server (a name from
                      ~/.ssh/config or user@host) with the path there,
                      relative to its home folder, or a folder to pick.
                      What's open in it isn't remembered unless kept.
  den worktree <name> creates a worktree in the repo of the current folder,
                      running its .den/create if it has one, and prints its
                      path. Inside den, the app also opens it.

In den's terminals, these act on the workspace of the terminal they run in.
Paths are relative to the current folder; lines and columns start at 1. The
keyboard stays in the terminal unless --focus.

  den where           prints what's in front as JSON: the workspace (server,
                      path, repo, branch, worktree or not), its tabs and
                      the active one with its cursor, the panels shown,
                      the Device panel (its devices, the one on screen)
                      and the debugger's session (status, the command it
                      ran, the program's page and device, the last line
                      it revealed).
  den workspace <path | name | branch>
                      brings that workspace to the front.
  den show <file>[:<line>[:<col>]] [--focus]
                      opens the file with the cursor at that line; on a
                      file already open, goes to its tab.
  den show <file>:<line>[:<col>]-<line>[:<col>] [--focus]
                      opens it with that range selected (whole lines
                      without columns).
  den diff [<file>]   shows the uncommitted changes of the file, or the
                      list of changed files.
  den doc [<title>]   shows the Markdown read from stdin in a tab.
  den selection       prints the file and range selected in the editor
                      (path:line:col-line:col), then the selected text.
  den tabs            lists the files open in the editor, the active one
                      with its cursor.
  den close <file> | --all
                      closes the file's tabs, or every tab; none if one
                      has unsaved changes.
  den panel show | hide <panel>
                      workspaces, agents, files, outline, search,
                      references, changes, history, debugger, callstack,
                      variables, watch, breakpoints, terminals, console,
                      notes or device.
  den reveal <file>   selects the file in the files panel.
  den message <text>  shows a message in the status bar.
  den notes           prints the workspace's notes (its Notes panel).
  den notes add [<text>]
                      adds a line to them: the text, or stdin.
  den notes set [<text>]
                      replaces them with the text, or stdin; with ''
                      it clears them.
  den workspaces      lists the workspaces open on every server: working,
                      waiting (asking something) or finished unseen.
  den term list       the workspace's terminals: id, title, `*` the active.
  den term new [--right | --down] [--focus] [<command>...]
                      opens a terminal (a new tab, or split from this
                      one), types the command in its shell and prints its id.
  den term read <id> [<lines>]
                      prints its last lines (50 by default).
  den term send <id> [--no-enter] <text>...
                      types the text in it, and Enter.
  den term focus <id> shows it and gives it the keyboard.
  den term close <id> closes it, ending what runs in it.

  den debug state     prints the debugger's state as JSON: the session, the
                      stopped VMs, the focused stop with its frames and
                      locals, the breakpoints and the end of the console.
  den debug start [<file>]
                      starts a session (F5) on the file, or the open one.
  den debug inspect   lets the user pick a widget on the app being debugged;
                      the line that made it opens, and `den debug state`
                      gives it as `revealed`.
  den debug stop | continue | next | in | out | pause
                      Shift-F5, F5, F10, F11, Shift-F11 and F6.
  den debug break <file>:<line>
                      sets a breakpoint there.
  den debug clear [<file>:<line>]
                      removes that breakpoint, or all of them.
  den debug eval <expr>...
                      evaluates it in the focused frame: {value, type}.
  den debug wait [stop | connected | idle] [<seconds>]
                      waits for a VM to stop (or for the session to
                      connect, or to end), 30 seconds at most, and prints
                      the state. A session that fails ends the wait too.
";

/// Folder holding the `den` link, which the agent puts in its terminals' PATH.
pub fn bin_dir() -> Result<PathBuf> {
    Ok(proto::state_dir()?.join("bin"))
}

/// Links `den` to this binary, for den's terminals and, in `~/.local/bin`
/// if there is one, for any other (unless something else is called `den` there).
/// An agent with a state of its own (tests, a development agent) leaves
/// `~/.local/bin` alone: its link would outlive it.
pub fn install() -> Result<()> {
    let dir = bin_dir()?;
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    platform::symlink(&exe, &dir.join(proto::APP))?;
    let isolated = std::env::var_os("DEN_STATE_DIR").is_some() || std::env::var_os("DEN_AGENT_SOCKET").is_some();
    if !isolated
        && let Some(local) = std::env::home_dir().map(|home| home.join(".local/bin")).filter(|dir| dir.is_dir()) {
        let link = local.join(proto::APP);
        let ours = match std::fs::read_link(&link) {
            // The agent runs from versioned copies: `den-agent-6-…`.
            Ok(target) => target.file_name().is_some_and(|name| name.to_string_lossy().starts_with("den-agent")),
            Err(_) => !link.exists(),
        };
        if ours {
            platform::symlink(&exe, &link)?;
        }
    }
    Ok(())
}

/// The Claude Code skill that tells Claude about the `den` commands.
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

/// Whether we were invoked through the `den` link rather than as `den-agent`.
pub fn invoked_as_den() -> bool {
    std::env::args_os()
        .next()
        .map(PathBuf::from)
        .and_then(|path| path.file_stem().map(|name| name == proto::APP))
        .unwrap_or(false)
}

/// `den <path>`: asks the app to open it. In den's terminals, the window
/// showing the terminal does (if no app answers, as outside them). With no
/// app connected, it starts one on this machine; over SSH, there's no app to
/// start.
pub fn open(arg: &str) -> Result<()> {
    let path = std::path::absolute(arg)?;
    let path = path.canonicalize().with_context(|| format!("{arg}: no such file or folder"))?;
    let (root, file) = proto::open_target(&path);
    if own_term().is_some() && open_here(root.clone(), file.clone()).is_ok() {
        return Ok(());
    }
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
        bail!("no den app is connected to this server: add it in den's Settings → Servers");
    }
    platform::launch_app(&[path.as_os_str()])
}

/// `den -n <path>`: asks the app to open it in a window of its own. With
/// no app connected, it starts one on this machine (its first window is
/// one of its own); over SSH, there's no app to start.
pub fn open_new(args: &[String]) -> Result<()> {
    let [arg] = args else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let path = std::path::absolute(arg)?;
    let path = path.canonicalize().with_context(|| format!("{arg}: no such file or folder"))?;
    let (root, file) = proto::open_target(&path);
    let mut command = vec!["window".to_string(), root.to_string_lossy().into_owned()];
    command.extend(file.map(|file| file.to_string_lossy().into_owned()));
    let asked = connect().and_then(|client| {
        smol::block_on(client.request(Request::Command {
            args: command,
            cwd: std::env::current_dir()?,
            term: own_term(),
        }))
    });
    match asked {
        Ok(_) => Ok(()),
        Err(_) if std::env::var_os("SSH_CONNECTION").is_none() => platform::launch_app(&[path.as_os_str()]),
        Err(err) => Err(err).with_context(|| format!("could not open {arg}")),
    }
}

/// `den -s <server> [<path>]`: asks the app to open a window on `server`,
/// with `path` there. With no app connected, it starts one on this machine.
pub fn server(args: &[String]) -> Result<()> {
    let ([server] | [server, _]) = args else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let mut command = vec!["-s".to_string()];
    command.extend(args.iter().cloned());
    let asked = connect().and_then(|client| {
        smol::block_on(client.request(Request::Command {
            args: command.clone(),
            cwd: std::env::current_dir()?,
            term: own_term(),
        }))
    });
    match asked {
        Ok(_) => Ok(()),
        Err(_) if std::env::var_os("SSH_CONNECTION").is_none() => {
            let command: Vec<&std::ffi::OsStr> = command.iter().map(|arg| arg.as_ref()).collect();
            platform::launch_app(&command)
        }
        Err(err) => Err(err).with_context(|| format!("could not open {server}")),
    }
}

/// `den worktree <name>`: prints only the path on stdout (messages go to
/// stderr), so that `cd "$(den task x)"` works.
pub fn task(args: &[String]) -> Result<()> {
    let [name] = args else {
        eprint!("{USAGE}");
        std::process::exit(2);
    };
    let cwd = std::env::current_dir()?;
    let client = Client::connect_local(&std::env::current_exe()?).context("could not talk to the agent")?;
    eprintln!("creating worktree {name}…");
    let response = smol::block_on(client.request(Request::TaskCreate {
        repo: cwd,
        name: name.clone(),
        open: false,
    }))?;
    let Response::Task(task) = response else {
        bail!("unexpected response from the agent: {response:?}");
    };
    if own_term().is_some()
        && let Err(err) = open_here(task.path.clone(), None)
    {
        eprintln!("could not open it: {err:#}");
    }
    println!("{}", task.path.display());
    Ok(())
}

/// Opens `root`, with `file` in it, in the window showing this terminal.
fn open_here(root: PathBuf, file: Option<PathBuf>) -> Result<()> {
    let mut args = vec!["open".to_string(), root.to_string_lossy().into_owned()];
    args.extend(file.map(|file| file.to_string_lossy().into_owned()));
    let response = smol::block_on(connect()?.request(Request::Command {
        args,
        cwd: std::env::current_dir()?,
        term: own_term(),
    }))?;
    match response {
        Response::Text(_) => Ok(()),
        other => bail!("unexpected response from the agent: {other:?}"),
    }
}

/// The terminal of den this runs in, if any.
fn own_term() -> Option<TermId> {
    std::env::var("DEN_TERM").ok()?.parse().ok()
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
            bail!("pipe the Markdown into it: echo \"# Title\" | den doc");
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
        let title = match args[1..].join(" ") {
            title if title.is_empty() => "Notes".to_string(),
            title => title,
        };
        args = vec!["doc".into(), title, text];
    }
    // `den notes add|set`: the words, or stdin.
    if args[0] == "notes" && args.len() >= 2 && matches!(args[1].as_str(), "add" | "set") {
        let text = if args.len() > 2 {
            args[2..].join(" ")
        } else if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
            text
        } else {
            bail!("the text? den notes {} <text>, or pipe it in", args[1]);
        };
        args = vec!["notes".into(), args[1].clone(), text];
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

/// `den term …`: reading, typing in and closing a terminal is the agent's
/// own; opening one, or anything about where it is, the app's.
pub fn term(args: &[String]) -> Result<()> {
    let id = |arg: Option<&String>| -> Result<TermId> {
        let arg = arg.context("which terminal? (`den term list`)")?;
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
