//! The `den` commands that need the app (see `USAGE` in the agent's
//! `cli.rs`): the agent sends them as `Event::Command`, they run here and
//! the answer goes back with `Request::CommandDone`, to be printed.

use gpui_kit::component::input::Position;

use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::{debug::WaitFor, splits::Axis, workspace::normalize};

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

impl Den {
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
        // Only these are den's: any other belongs to the command's arguments
        // (`den term new claude --resume`).
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
            ["where"] => {
                let terminal = here(self, false, window, cx).ok().map(|(root, _)| root);
                self.whereabouts(terminal, cx)
            }
            ["workspace", target] => self.switch_workspace(&host, target, &cwd, window, cx),
            ["close", "--all"] => here(self, false, window, cx).and_then(|(_, workspace)| {
                workspace.update(cx, |workspace, cx| workspace.close_files(None, window, cx)).map(|()| String::new())
            }),
            ["close", file] => here(self, false, window, cx).and_then(|(_, workspace)| {
                let path = normalize(&cwd.join(file));
                workspace.update(cx, |workspace, cx| workspace.close_files(Some(&path), window, cx)).map(|()| String::new())
            }),
            ["panel", action @ ("show" | "hide"), name] => match parse_panel(name) {
                Ok(panel) => here(self, *action == "show", window, cx).map(|(_, workspace)| {
                    workspace.update(cx, |workspace, cx| workspace.set_panel(panel, *action == "show", cx));
                    String::new()
                }),
                Err(err) => Err(err),
            },
            ["reveal", file] => here(self, true, window, cx).map(|(_, workspace)| {
                let path = normalize(&cwd.join(file));
                workspace.update(cx, |workspace, cx| workspace.reveal_file(&path, cx));
                String::new()
            }),
            ["notes", rest @ ..] => {
                let key = self.command_key(host.clone(), group.as_deref(), &cwd);
                self.notes_command(key, rest, window, cx)
            }
            ["debug", rest @ ..] => match here(self, false, window, cx) {
                Ok((_, workspace)) => return debug_command(rest, &cwd, workspace, window, cx),
                Err(err) => Err(err),
            },
            // `den -s <server> [<path>]`: in a window of its own.
            ["-s", server, path @ ..] if path.len() <= 1 => {
                let (server, path) = (server.to_string(), path.first().map(PathBuf::from));
                cx.defer(move |cx| open_server_window(server, path, cx));
                Ok(String::new())
            }
            // `den -n <path>`: in a window of its own.
            ["window", root, file @ ..] if file.len() <= 1 => {
                let destination = self.host(&host).and_then(|host| host.destination.clone());
                let (root, file) = (PathBuf::from(root), file.first().map(PathBuf::from));
                cx.defer(move |cx| open_new_window(host, destination, root, file, cx));
                Ok(String::new())
            }
            // `den <path>` and `den worktree` in a terminal of this window.
            ["open", root, file @ ..] if file.len() <= 1 => {
                let (root, file) = (PathBuf::from(root), file.first().map(PathBuf::from));
                return self.open_command(host, root, file, window, cx);
            }
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
            _ => Err(format!("den {}: unknown command; see den --help", args.join(" "))),
        };
        Task::ready(answer)
    }

    /// Opens `root` with `file` in it, once the server's worktrees are read
    /// again (`den worktree` has just made one), and brings the window up.
    fn open_command(
        &mut self,
        host: SharedString,
        root: PathBuf,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Answer> {
        let client = self.client(&host);
        cx.spawn_in(window, async move |this, cx| {
            if let Some(client) = client
                && let Ok(tasks) = list_tasks(&client).await
            {
                this.update(cx, |this, _| {
                    if let Some(host) = this.host_mut(&host) {
                        host.tasks = tasks;
                    }
                })
                .ok();
            }
            this.update_in(cx, |this, window, cx| {
                this.open_from_terminal(host, root, file, window, cx);
                window.activate_window();
                cx.activate(true);
            })
            .map_err(|_| "den's window is closed".to_string())?;
            Ok(String::new())
        })
    }

    /// The workspace a command acts on, and its root: that of the terminal
    /// it ran in or, outside den's terminals, the one containing `cwd`. With
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
        let key = self.command_key(host, group, cwd);
        if activate && self.active.as_ref() != Some(&key) {
            self.activate(key.clone(), window, cx);
        }
        match self.workspaces.get(&key) {
            Some(workspace) => Ok((key.path, workspace.clone())),
            None => Err(format!("{} isn't open in den", key.path.display())),
        }
    }

    /// The workspace of the terminal a command ran in or, outside den's
    /// terminals, the one containing `cwd`, open or not.
    fn command_key(&self, host: SharedString, group: Option<&str>, cwd: &Path) -> TaskKey {
        let path = match group {
            Some(group) => PathBuf::from(group),
            None => self.workspace_containing(&host, cwd),
        };
        TaskKey { host, path }
    }

    /// `den notes [add | set] <text>`: prints the workspace's notes, adds a
    /// line to them or replaces them, in its panel if it's open. Without
    /// one, they're kept for when it is.
    fn notes_command(&mut self, key: TaskKey, args: &[&str], window: &mut Window, cx: &mut Context<Self>) -> Answer {
        let config = key.config();
        let text = match args {
            [] => return Ok(crate::notes::get(&config, cx)),
            ["add", line] => match crate::notes::get(&config, cx).trim_end() {
                "" => format!("{}\n", line.trim_end()),
                notes => format!("{notes}\n{}\n", line.trim_end()),
            },
            ["set", text] => text.to_string(),
            _ => return Err(NOTES_USAGE.to_string()),
        };
        if let Some(workspace) = self.workspaces.get(&key) {
            let notes = workspace.read(cx).notes();
            notes.update(cx, |notes, cx| notes.set_text(text.clone(), window, cx));
        }
        crate::notes::set(&config, text, cx);
        Ok(String::new())
    }

    /// `den where`: the workspace in front of the window, as JSON: its
    /// server, path, repo and branch, whether it's a worktree, and what it
    /// shows (`Workspace::whereabouts`). `terminal` is the workspace of the
    /// terminal the command ran in, said when it's another.
    fn whereabouts(&self, terminal: Option<PathBuf>, cx: &App) -> Answer {
        let key = self.active.as_ref().ok_or("no workspace is open")?;
        let workspace = self.workspaces.get(key).ok_or("the workspace in front isn't open")?;
        let mut out = workspace.read(cx).whereabouts(cx);
        out["server"] = key.host.to_string().into();
        if let Some(task) = self.task(key) {
            out["repo"] = json!(task.repo);
            out["worktree"] = (!task.main).into();
            if out.get("branch").is_none()
                && let Some(branch) = &task.branch
            {
                out["branch"] = branch.clone().into();
            }
        }
        if let Some(terminal) = terminal.filter(|path| path != &key.path) {
            out["terminal"] = json!(terminal);
        }
        serde_json::to_string_pretty(&out).map_err(|err| err.to_string())
    }

    /// `den workspace <target>`: brings the workspace to the front. The
    /// target is its path (relative to `cwd`), its folder's name or its
    /// branch; one of the terminal's server wins over the others'.
    fn switch_workspace(&mut self, host: &SharedString, target: &str, cwd: &Path, window: &mut Window, cx: &mut Context<Self>) -> Answer {
        let path = normalize(&cwd.join(target));
        let mut found: Vec<TaskKey> = Vec::new();
        for server in &self.hosts {
            for task in server.tasks.iter().chain(&server.loose) {
                let hit = task.path == path || folder_name(&task.path) == target || task.branch.as_deref() == Some(target);
                let key = TaskKey { host: server.name.clone(), path: task.path.clone() };
                if hit && !found.contains(&key) {
                    found.push(key);
                }
            }
        }
        if found.len() > 1 && found.iter().any(|key| &key.host == host) {
            found.retain(|key| &key.host == host);
        }
        let key = match found.as_slice() {
            [] => return Err(format!("{target}: no such workspace; see den workspaces")),
            [key] => key.clone(),
            keys => {
                let names: Vec<String> = keys.iter().map(|key| format!("{} {}", key.host, key.path.display())).collect();
                return Err(format!("{target}: more than one workspace: {}", names.join(", ")));
            }
        };
        let path = key.path.display().to_string();
        self.activate(key, window, cx);
        Ok(path)
    }

    /// `den workspaces`: one per line, `*` the active one.
    fn workspace_list(&self, cx: &App) -> String {
        let mut out = String::new();
        for (key, task) in self.ordered_all(cx) {
            let state = match self.workspace_state(&key, cx).2 {
                "done" => "finished",
                state => state,
            };
            let mark = if self.active.as_ref() == Some(&key) { "*" } else { " " };
            let branch = task.branch.as_deref().unwrap_or("-");
            out.push_str(&format!("{mark} {}\t{}\t{branch}\t{state}\n", key.host, key.path.display()));
        }
        out
    }
}

/// A panel by the name `den panel` takes: as the layout saves it
/// (files, terminals, debugger, changes, notes…).
fn parse_panel(name: &str) -> Result<Panel, String> {
    let panel: Option<Panel> = serde_json::from_value(json!(name.to_lowercase())).ok();
    panel.filter(|panel| Panel::ALL.contains(panel)).ok_or_else(|| {
        let names: Vec<String> =
            Panel::ALL.iter().map(|panel| json!(panel).as_str().unwrap_or_default().to_string()).collect();
        format!("{name}: not a panel; one of {}", names.join(", "))
    })
}

const NOTES_USAGE: &str = "den notes: [add <text> | set <text>], or the text from stdin";

const DEBUG_USAGE: &str = "den debug: state | inspect | start [<file>] | stop | restart | continue | next | in | out | pause \
    | break <file>:<line> | clear [<file>:<line>] | eval <expr> | wait [stop|connected|idle] [<seconds>] | target [<name>]";

/// How long `den debug wait` waits when not told.
const DEBUG_WAIT: Duration = Duration::from_secs(30);

/// `den debug …`: drives the workspace's debugger like the keys and the panel
/// do, and answers with its state as JSON (`Debugger::state`).
fn debug_command(
    args: &[&str],
    cwd: &Path,
    workspace: Entity<Workspace>,
    window: &mut Window,
    cx: &mut Context<Den>,
) -> Task<Answer> {
    let debugger = workspace.read(cx).debugger();
    let stopped = |cx: &App| {
        if debugger.read(cx).is_stopped() { Ok(()) } else { Err("no VM is stopped".to_string()) }
    };
    let answer = match args {
        ["state"] => Ok(debugger.read(cx).state().to_string()),
        ["target"] => {
            let debugger = debugger.read(cx);
            Ok(json!({ "target": debugger.target(), "targets": debugger.targets }).to_string())
        }
        ["target", name] => debugger.update(cx, |debugger, cx| debugger.set_target(name, cx)).map(|()| String::new()),
        ["inspect"] => {
            if debugger.update(cx, |debugger, _| debugger.inspect()) {
                Ok(String::new())
            } else {
                Err("nothing is being debugged: den debug start first".to_string())
            }
        }
        ["start", file @ ..] if file.len() <= 1 => {
            if debugger.read(cx).is_active() {
                Err("a session is active: den debug stop first".to_string())
            } else {
                // the launch command's ${file} is the open file
                if let Some(file) = file.first() {
                    let path = normalize(&cwd.join(file));
                    workspace.update(cx, |workspace, cx| workspace.show(path, Position::new(0, 0), None, false, window, cx));
                }
                debugger.update(cx, |debugger, cx| debugger.start(window, cx));
                Ok(String::new())
            }
        }
        ["stop"] => {
            debugger.update(cx, |debugger, cx| debugger.stop(cx));
            Ok(String::new())
        }
        ["restart"] => {
            debugger.update(cx, |debugger, cx| debugger.restart(window, cx));
            Ok(String::new())
        }
        [step @ ("continue" | "next" | "in" | "out")] => stopped(cx).map(|()| {
            debugger.update(cx, |debugger, cx| match *step {
                "continue" => debugger.continue_(cx),
                "next" => debugger.step_over(cx),
                "in" => debugger.step_in(cx),
                _ => debugger.step_out(cx),
            });
            String::new()
        }),
        ["pause"] => {
            debugger.update(cx, |debugger, cx| debugger.pause(cx));
            Ok(String::new())
        }
        ["break", target] => parse_target(target, cwd).map(|(path, from, _)| {
            debugger.update(cx, |debugger, cx| debugger.set_breakpoint(&path, from.line, cx));
            String::new()
        }),
        ["clear"] => {
            debugger.update(cx, |debugger, cx| debugger.remove_all_breakpoints(cx));
            Ok(String::new())
        }
        ["clear", target] => parse_target(target, cwd).map(|(path, from, _)| {
            debugger.update(cx, |debugger, cx| debugger.remove_breakpoint(&path, from.line, cx));
            String::new()
        }),
        ["eval", expr @ ..] if !expr.is_empty() => {
            if let Err(err) = stopped(cx) {
                return Task::ready(Err(err));
            }
            let (tx, rx) = smol::channel::bounded(1);
            let expr = expr.join(" ");
            debugger.update(cx, |debugger, _| {
                debugger.evaluate(expr, move |_, result, _| {
                    // nobody waits for it once the command gave up
                    tx.try_send(result).ok();
                })
            });
            return cx.spawn(async move |_, _| match rx.recv().await {
                Ok(Ok(var)) => Ok(json!({ "value": var.value, "type": var.kind }).to_string()),
                Ok(Err(error)) => Err(error),
                Err(_) => Err("the session ended before the answer".to_string()),
            });
        }
        ["wait", rest @ ..] if rest.len() <= 2 => {
            let mut what = WaitFor::Stop;
            let mut wait = DEBUG_WAIT;
            for word in rest {
                if let Some(parsed) = WaitFor::parse(word) {
                    what = parsed;
                } else if let Ok(seconds) = word.parse::<f64>().map(Duration::try_from_secs_f64) {
                    match seconds {
                        Ok(seconds) => wait = seconds,
                        Err(_) => return Task::ready(Err(format!("{word}: not a number of seconds"))),
                    }
                } else {
                    return Task::ready(Err(DEBUG_USAGE.to_string()));
                }
            }
            let debugger = debugger.downgrade();
            return cx.spawn(async move |_, cx| {
                let deadline = Instant::now() + wait;
                loop {
                    let reached = debugger
                        .read_with(cx, |debugger, _| debugger.reached(what).then(|| debugger.state().to_string()))
                        .map_err(|_| "the workspace closed".to_string())?;
                    if let Some(state) = reached {
                        return Ok(state);
                    }
                    if Instant::now() >= deadline {
                        let state = debugger
                            .read_with(cx, |debugger, _| debugger.state().to_string())
                            .map_err(|_| "the workspace closed".to_string())?;
                        return Err(format!("timed out; the state: {state}"));
                    }
                    cx.background_executor().timer(Duration::from_millis(50)).await;
                }
            });
        }
        _ => Err(DEBUG_USAGE.to_string()),
    };
    Task::ready(answer)
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

    use super::{parse_panel, parse_target};
    use crate::config::Panel;

    #[test]
    fn panels_by_name() {
        assert_eq!(parse_panel("files"), Ok(Panel::Files));
        assert_eq!(parse_panel("agents"), Ok(Panel::Agents));
        assert!(parse_panel("nothing").unwrap_err().contains("terminals"));
    }

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
