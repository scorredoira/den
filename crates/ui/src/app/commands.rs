//! The `sik` commands that need the app (see `USAGE` in the agent's
//! `cli.rs`): the agent sends them as `Event::Command`, they run here and
//! the answer goes back with `Request::CommandDone`, to be printed.

use gpui_kit::component::input::Position;

use super::*;
use crate::{splits::Axis, workspace::normalize};

type Answer = Result<String, String>;

/// A command from a terminal of `host`, as the agent sent it.
pub(super) struct Command {
    pub id: u64,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The terminal it ran in, and its workspace.
    pub term: Option<proto::TermId>,
    pub group: Option<String>,
}

impl Sik {
    /// Runs `command` and answers the agent once it's done.
    pub(super) fn run_command(
        &mut self,
        host: SharedString,
        client: Arc<Client>,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = command.id;
        let task = self.command(host, command, window, cx);
        cx.spawn(async move |_, _| {
            let result = task.await;
            client.notify(Request::CommandDone { command: id, result });
        })
        .detach();
    }

    fn command(&mut self, host: SharedString, command: Command, window: &mut Window, cx: &mut Context<Self>) -> Task<Answer> {
        let Command { args, cwd, term, group, .. } = command;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        // Only these are sik's: any other belongs to the command's arguments
        // (`sik term new claude --resume`).
        let (flags, args): (Vec<&str>, Vec<&str>) =
            args.into_iter().partition(|arg| matches!(*arg, "--focus" | "--right" | "--down"));
        let focus = flags.contains(&"--focus");
        let here = |this: &mut Self, activate: bool, window: &mut Window, cx: &mut Context<Self>| {
            this.command_workspace(host.clone(), group.as_deref(), &cwd, activate, window, cx)
        };
        let answer = match args.as_slice() {
            ["show", target] => here(self, true, window, cx).and_then(|(_, workspace)| {
                let (path, from, to) = parse_target(target, &cwd)?;
                workspace.update(cx, |workspace, cx| workspace.show(path, from, to, focus, window, cx));
                Ok(String::new())
            }),
            ["diff", file @ ..] if file.len() <= 1 => here(self, true, window, cx).and_then(|(root, workspace)| {
                let file = match file.first() {
                    Some(file) => Some(
                        normalize(&cwd.join(file))
                            .strip_prefix(&root)
                            .map_err(|_| format!("{file} isn't in the workspace {}", root.display()))?
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    None => None,
                };
                workspace.update(cx, |workspace, cx| workspace.show_changes(file, window, cx));
                Ok(String::new())
            }),
            ["doc", title, text] => here(self, true, window, cx).map(|(_, workspace)| {
                workspace.update(cx, |workspace, cx| workspace.open_doc(title, text.to_string(), window, cx));
                String::new()
            }),
            ["selection"] => here(self, false, window, cx).and_then(|(_, workspace)| workspace.read(cx).selection(cx)),
            ["tabs"] => here(self, false, window, cx).map(|(_, workspace)| workspace.read(cx).tab_list(cx)),
            ["message", words @ ..] if !words.is_empty() => here(self, false, window, cx).map(|(_, workspace)| {
                workspace.update(cx, |workspace, cx| workspace.set_message(words.join(" "), cx));
                String::new()
            }),
            ["workspaces"] => Ok(self.workspace_list(cx)),
            ["term", "list"] => here(self, false, window, cx).map(|(_, workspace)| {
                let mut out = String::new();
                for (term, title, active) in workspace.read(cx).terminal_list(cx) {
                    let mark = if active { "*" } else { " " };
                    out.push_str(&format!("{mark} {term}\t{title}\n"));
                }
                out
            }),
            ["term", "new", command @ ..] => {
                let split = if flags.contains(&"--right") {
                    Some(Axis::Row)
                } else if flags.contains(&"--down") {
                    Some(Axis::Column)
                } else {
                    None
                };
                let line = (!command.is_empty()).then(|| command.join(" "));
                match here(self, false, window, cx) {
                    Ok((_, workspace)) => {
                        let opened = workspace.update(cx, |workspace, cx| workspace.open_terminal(term, split, line, focus, window, cx));
                        return cx.spawn(async move |_, _| {
                            opened.await.map(|term| term.to_string()).ok_or_else(|| "couldn't open the terminal".to_string())
                        });
                    }
                    Err(err) => Err(err),
                }
            }
            ["term", "focus", term] => here(self, true, window, cx).and_then(|(_, workspace)| {
                let term = term.parse().map_err(|_| format!("{term}: not a terminal id"))?;
                if workspace.update(cx, |workspace, cx| workspace.focus_terminal(term, window, cx)) {
                    Ok(String::new())
                } else {
                    Err(format!("terminal {term} isn't in this workspace"))
                }
            }),
            _ => Err(format!("sik {}: unknown command; see sik --help", args.join(" "))),
        };
        Task::ready(answer)
    }

    /// The workspace a command acts on, and its root: that of the terminal
    /// it ran in or, outside sik's terminals, the one containing `cwd`. With
    /// `activate` it's entered (and opened, if it wasn't).
    fn command_workspace(
        &mut self,
        host: SharedString,
        group: Option<&str>,
        cwd: &Path,
        activate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(PathBuf, Entity<Workspace>), String> {
        let path = match group {
            Some(group) => PathBuf::from(group),
            None => self.workspace_containing(&host, cwd),
        };
        let key = TaskKey { host, path };
        if activate && self.active.as_ref() != Some(&key) {
            self.activate(key.clone(), window, cx);
        }
        match self.workspaces.get(&key) {
            Some(workspace) => Ok((key.path, workspace.clone())),
            None => Err(format!("{} isn't open in sik", key.path.display())),
        }
    }

    /// `sik workspaces`: one per line, `*` the active one.
    fn workspace_list(&self, cx: &App) -> String {
        let mut out = String::new();
        for (key, task) in self.ordered(cx) {
            let state = if self.blocked.contains(&key) {
                "waiting"
            } else if task.working {
                "working"
            } else if self.attention.contains(&key) {
                "finished"
            } else {
                "idle"
            };
            let mark = if self.active.as_ref() == Some(&key) { "*" } else { " " };
            let branch = task.branch.as_deref().unwrap_or("-");
            out.push_str(&format!("{mark} {}\t{}\t{branch}\t{state}\n", key.host, key.path.display()));
        }
        out
    }
}

/// `file[:line[:col]][-line[:col]]`, relative to `cwd`: the file, where the
/// cursor goes and, for a range, where it ends (to the end of the line
/// without a column).
fn parse_target(target: &str, cwd: &Path) -> Result<(PathBuf, Position, Option<Position>), String> {
    let invalid = || format!("{target}: expected file[:line[:col]][-line[:col]]");
    // The first `:` followed only by the numbers (a Windows drive isn't).
    let split = target
        .char_indices()
        .filter(|(_, c)| *c == ':')
        .map(|(ix, _)| ix)
        .find(|ix| target[ix + 1..].chars().all(|c| c.is_ascii_digit() || c == ':' || c == '-'));
    let (file, place) = match split {
        Some(ix) => (&target[..ix], Some(&target[ix + 1..])),
        None => (target, None),
    };
    let path = normalize(&cwd.join(file));
    let Some(place) = place else {
        return Ok((path, Position::new(0, 0), None));
    };
    let point = |text: &str| -> Option<(u32, Option<u32>)> {
        let mut parts = text.split(':');
        let line: u32 = parts.next()?.parse().ok().filter(|line| *line > 0)?;
        let column = match parts.next() {
            Some(column) => Some(column.parse().ok().filter(|column: &u32| *column > 0)?),
            None => None,
        };
        parts.next().is_none().then_some((line, column))
    };
    let (start, end) = match place.split_once('-') {
        Some((start, end)) => (point(start).ok_or_else(invalid)?, Some(point(end).ok_or_else(invalid)?)),
        None => (point(place).ok_or_else(invalid)?, None),
    };
    let from = Position::new(start.0 - 1, start.1.map_or(0, |column| column - 1));
    let to = end.map(|(line, column)| Position::new(line - 1, column.map_or(u32::MAX, |column| column - 1)));
    Ok((path, from, to))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use gpui_kit::component::input::Position;

    use super::parse_target;

    #[test]
    fn targets() {
        let cwd = Path::new("/repo/src");
        let (path, from, to) = parse_target("main.rs", cwd).unwrap();
        assert_eq!((path, from, to), (PathBuf::from("/repo/src/main.rs"), Position::new(0, 0), None));
        let (path, from, to) = parse_target("../a.rs:10", cwd).unwrap();
        assert_eq!((path, from, to), (PathBuf::from("/repo/a.rs"), Position::new(9, 0), None));
        let (_, from, to) = parse_target("a.rs:3:5-4:2", cwd).unwrap();
        assert_eq!((from, to), (Position::new(2, 4), Some(Position::new(3, 1))));
        let (_, from, to) = parse_target("a.rs:3-7", cwd).unwrap();
        assert_eq!((from, to), (Position::new(2, 0), Some(Position::new(6, u32::MAX))));
        let (path, ..) = parse_target("/x/odd:name.rs:2", cwd).unwrap();
        assert_eq!(path, PathBuf::from("/x/odd:name.rs"));
        assert!(parse_target("a.rs:0", cwd).is_err());
        assert!(parse_target("a.rs:1:2:3", cwd).is_err());
    }
}
