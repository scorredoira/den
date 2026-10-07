---
name: den
description: Drive the Den app you are running in (when DEN_TERMINAL is set) with the `den` command. When the user talks about what is on their screen (this file, the app, the simulator, the debugger, "where are you"), run `den where` first - it says the workspace, branch and worktree, the open tabs, the panels and the debug session. Also: show the user a file, a line or a selected range, a diff or a Markdown note; read what the user has selected in the editor or which files are open; open terminals, read them and type in them (e.g. start or watch other agents in their own worktrees); create worktrees. Use it whenever pointing the user at code would help, when the user refers to "this" or "what I selected", or to work with other terminals.
---

# Den

You are running in a terminal of Den, an editor with persistent terminals
(`DEN_TERMINAL` is set). The `den` command, already in the PATH, acts on the
workspace of the terminal it runs in, also over SSH. Run `den --help` for the
full list; the main ones:

- `den where` prints, as JSON, what the user has in front: the workspace
  (server, path, repo, branch, whether it is a worktree), the tabs and the
  active one with its cursor, the panels shown and the debugger's session
  (status, the command it ran, the program's page, the last line the
  program revealed). Run it first when the user refers to their screen.
- `den workspace <path|name|branch>` brings a workspace to the front;
  `den close <file>` (or `--all`) closes tabs, never one with unsaved
  changes; `den panel show|hide <panel>` (files, terminals, debugger,
  console, changes, notes, search, outline…); `den reveal <file>` selects
  it in the files panel.
- `den show <file>:<line>` opens the file at that line for the user;
  `den show <file>:<line>-<line>` (or `<line>:<col>-<line>:<col>`) selects
  that range. Use it to point at what you are talking about instead of
  pasting code. The keyboard stays in the terminal unless `--focus`.
- `den diff [<file>]` shows the uncommitted changes, so the user can review them.
- `echo "# Title ..." | den doc "<title>"` shows Markdown in a tab: plans,
  reports, tables, anything longer than a chat answer.
- Claude Code in Den's terminals is connected to it as to an IDE: the
  file in front and the user's selection come with each message ("Selected
  N lines from …"). `den selection` prints them on demand (the file, the
  range and the text: what they mean by "this"); `den tabs` lists the open
  files.
- `den message <text>` shows a short message in the status bar.
- `den notes` prints the workspace's notes: what's next there, kept by Den
  outside the repo. `den notes add <text>` adds a line (e.g. what's left
  when you stop), `echo ... | den notes set` replaces them.
- `den term new [--right|--down] [<command>...]` opens a terminal next to
  this one and prints its id; `den term list`, `den term read <id> [<lines>]`,
  `den term send <id> <text>` (types it and Enter), `den term focus <id>`,
  `den term close <id>`.
- `den worktree <name>` creates a worktree and prints its path;
  `den workspaces` lists the workspaces and whether their agents are
  working, waiting for an answer or finished.

- `den debug ...` drives the debugger of the workspace and prints JSON:
  `den debug break <file>:<line>`, `den debug start [<file>]`, then
  `den debug wait` (until a VM stops; it prints the state: where, the
  frames, the locals), `den debug eval <expr>`, `den debug next|in|out|continue`,
  `den debug stop`. `den debug state` prints the state at any time.
  `den debug inspect` lets the user pick a widget in the app being
  debugged (in Chrome, an Alt-click in the page); the line that made it
  opens, Den comes to the front, and the line is `revealed` in the state. To test
  a program, start it, trigger what reaches the breakpoint (a request, a
  test) and wait. `den debug restart` stops it and starts it again.
  `den debug target` prints the target the launch command runs the
  program on (`${target}`, from `targets` in `.den/debug.json`) and the
  list; `den debug target <name>` picks one for the next start.
- `den debug join --port <port> -- <cmd> -- <cmd>` runs several programs
  as one debug session (a server and `den chrome`, a server and an app);
  it is what a launch command runs, not something to drive. `den chrome`
  is the Chrome bridge: a page in Chrome debugged like a program.

Lines and columns start at 1. A command that fails prints why and exits
with an error.
