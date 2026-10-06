//! `den-agent`: the daemon that keeps a machine's terminals.
//!
//! - `den-agent daemon`: listens on the local socket (started by the UI).
//! - `den-agent bridge`: joins stdin/stdout to the socket, starting the daemon
//!   if needed. It's what `ssh host den-agent bridge` runs (phase 3).
//! - `den <path>`, `den worktree <name>`, `den show <file>`…: from a
//!   terminal (see `cli.rs`).
//! - `den debug join …` (crate `join`) and `den chrome …` (crate `chrome`):
//!   programs of their own, run in a terminal.

mod blocked;
mod cli;
mod format;
mod fs;
mod git;
mod lsp;
mod platform;
mod ports;
mod pty;
mod relay;
mod search;
mod server;
mod snapshot;
mod shell_cwd;
mod tasks;

use std::{io::Write as _, time::Duration};

use anyhow::{Context as _, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("den-agent {} (protocol {})", env!("CARGO_PKG_VERSION"), proto::PROTOCOL);
            Ok(())
        }
        Some("daemon") => daemon(),
        None if !cli::invoked_as_den() => daemon(),
        Some("bridge") => bridge(),
        Some("worktree" | "wt" | "task") => cli::task(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Some("term") => cli::term(&args[1..]),
        // the agent isn't needed: join runs in the terminal, as its programs do
        Some("debug") if args.get(1).is_some_and(|arg| arg == "join") => join::run(&args[2..]),
        Some("chrome") => chrome::run(&args[1..]),
        Some("-s" | "--server") if cli::invoked_as_den() => cli::server(&args[1..]),
        Some("-n" | "--new-window") if cli::invoked_as_den() => cli::open_new(&args[1..]),
        Some(
            "show" | "diff" | "doc" | "selection" | "tabs" | "message" | "notes" | "workspaces" | "debug" | "where"
            | "workspace" | "close" | "panel" | "reveal",
        ) => cli::command(&args),
        Some(path) if cli::invoked_as_den() && !path.starts_with('-') && args.len() == 1 => cli::open(path),
        _ => {
            eprint!("{}", cli::USAGE);
            std::process::exit(2);
        }
    }
}

fn daemon() -> Result<()> {
    platform::detach_session();
    if let Err(err) = cli::install() {
        eprintln!("could not install the `den` command for terminals: {err:#}");
    }
    if let Err(err) = cli::install_skill() {
        eprintln!("could not install the skill for Claude Code: {err:#}");
    }
    let socket = proto::socket_path()?;
    let listener = platform::Listener::bind(&socket)?;
    eprintln!(
        "agent {} listening on {} (protocol {})",
        std::process::id(),
        socket.display(),
        proto::PROTOCOL
    );
    server::run(listener)
}

fn bridge() -> Result<()> {
    let socket = proto::socket_path()?;
    let stream = match platform::connect(&socket) {
        Ok(stream) => stream,
        Err(_) => {
            let log = proto::state_dir()?.join("agent.log");
            std::fs::create_dir_all(proto::state_dir()?)?;
            platform::spawn_daemon(&log)?;
            let mut attempt = 0;
            loop {
                std::thread::sleep(Duration::from_millis(50));
                match platform::connect(&socket) {
                    Ok(stream) => break stream,
                    Err(err) if attempt > 100 => {
                        return Err(err).context("the agent did not start");
                    }
                    Err(_) => attempt += 1,
                }
            }
        }
    };

    let mut reader = stream.try_clone_stream()?;
    let mut writer = stream;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
        std::process::exit(0);
    });
    // Rust's stdout buffers until it sees a newline, and the frames are
    // binary: flush on every write.
    let mut stdout = std::io::stdout().lock();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut reader, &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        stdout.write_all(&buf[..n])?;
        stdout.flush()?;
    }
}
