# Using Den

## Opening from a terminal

`den <path>` opens a folder, or a file in its repo, as a workspace in Den. It works from any terminal: the agent links `den` into `~/.local/bin` if that folder exists. Over SSH it opens in the app connected to that server, and in Den's terminals in the window of that terminal.

`den -n <path>` opens it in a window of its own instead, also from Den's terminals (on a server, a window on that server). If it's already open in a window, that window comes to the front. Like a `den -s` window, what's open in it isn't remembered unless kept.

`den -s <server> [<path>]` opens a window of its own on a server (a name from `~/.ssh/config` or `user@host`) with the path there, relative to its home folder, or the folder picker without one. That window is for a quick look: its workspaces column starts hidden, and neither the server nor the folders opened in it are remembered once it closes, unless kept with Keep in Workspaces (in its title bar or the server's right-click menu). Running it again for the same server opens in that window.

In Den's terminals, more commands act on the workspace of the terminal they run in, the same over SSH (`den --help` lists them all):

| Command | What it does |
| --- | --- |
| `den where` | Prints as JSON what's in front: the workspace (server, path, repo, branch, worktree or not), its tabs, the panels shown and the debugger's session. |
| `den workspace <path\|name\|branch>` | Brings that workspace to the front. |
| `den close <file>`, `den close --all` | Closes the file's tabs, or all of them; none if one has unsaved changes. |
| `den panel show\|hide <panel>`, `den reveal <file>` | Shows or hides a panel; selects the file in the files panel. |
| `den show <file>:<line>` | Opens the file at that line; `<file>:10-20` or `<file>:10:5-12:3` selects that range. The keyboard stays in the terminal unless `--focus`. |
| `den diff [<file>]` | Shows the uncommitted changes. |
| `den doc [<title>]` | Shows the Markdown read from stdin in a tab. |
| `den selection`, `den tabs` | Print what's selected in the editor, and the open files. |
| `den message <text>` | Shows a message in the status bar. |
| `den notes [add \| set] [<text>]` | Prints the workspace's notes, adds a line to them or replaces them (the text, or stdin). |
| `den workspaces` | Lists the workspaces and whether each is working, waiting for an answer or finished. |
| `den term new [--right\|--down] [<command>]` | Opens a terminal, runs the command in its shell and prints its id. |
| `den term list`, `read <id>`, `send <id> <text>`, `focus <id>`, `close <id>` | Lists, reads, types in, shows and closes terminals. |
| `den debug state`, `inspect`, `start [<file>]`, `break <file>:<line>`, `wait`, `eval <expr>`, `next`, `continue`, `stop`… | Drives the debugger and prints its state as JSON: an agent sets a breakpoint, starts the program, triggers the code and waits for the stop. |
| `den debug target [<name>]` | Prints the target the launch command runs the program on, and the list; with a name, picks it. |
| `den debug join --port <port> -- <command> -- <command>…` | Runs several programs as one debug session: a server and the page in Chrome, or a server and an app. |
| `den chrome …` | The Chrome bridge: a page in Chrome debugged like any program. See [chrome.md](chrome.md). |

They are meant for coding agents: when `~/.claude` exists, the agent installs a Claude Code skill (`~/.claude/skills/den`) that tells Claude about them. Other agents can read `den --help`. A path that is also a command's name opens with `./`, as in `den ./tabs`.

## Workspaces

The Workspaces panel, at the top of the explorer, lists per server the folders opened and the known repos, a row each: a repo's checkout and its worktrees as `repo / branch`, a folder that isn't a repo by its name. Drag to reorder: a checkout moves along with its worktrees. Its + adds a folder or a server (any name from `~/.ssh/config` or `user@host`), and the + beside a server opens a folder there, or creates one by typing a new name. Hovering a repo's checkout shows a + that makes a new worktree of it, and hovering a worktree, a bin that deletes it (after asking, and warning if it has uncommitted changes or commits not merged into the main branch; if git still refuses, it asks again to force it). Right-click removes them, and a folder that isn't a repo yet can be made one (Initialize Git Repository). Cmd-Shift-B shows or hides it.

New Worktree (Cmd-Shift-N) creates one with the repo's executable `.den/create <name>` if it has one, or `git worktree add` otherwise. From a Den terminal: `cd "$(den worktree <name>)"`. To leave out the worktrees agents make on their own, turn on Only My Worktrees in Settings: the panel then lists only those made with New Worktree.

On Windows, repository hooks can use `.den/create.ps1`, `.den/remove.ps1` and `.den/format.ps1` (also `.cmd`, `.bat` or `.exe`). Extensionless hooks need `sh` on PATH. PowerShell terminals report their current directory automatically; custom shells should emit OSC 7 for directory tracking.

Format Document (Shift-Opt-F), and Format on Save for the types chosen in Settings, use the repo's executable `.den/format <file>` if it has one (the text on stdin, the result on stdout; exiting with 2 leaves that type to the next way), else the language server; JSON is formatted even without either.

Cmd-E goes straight into the next one with a coding agent, in the panel's order, and Cmd-Alt-Shift-E into the next one of all. Cmd-Alt-E switches between the ones being worked on as Cmd-Tab does between apps: the previous one, then those with a coding agent, the ones waiting for an answer first and each the most recently used first (all of them while no other has an agent); holding Cmd, each E goes one further (Shift-E back) and letting go enters it. Cmd-K finds one across servers, the most recently used first. On Linux and Windows these are Ctrl-Alt-E, Ctrl-Alt-Shift-E, Ctrl-Tab and Ctrl-Shift-K, so that Ctrl plus a letter stays the shell's inside a terminal. Every shortcut can be changed in Settings (Cmd-,).

Cmd-D splits a terminal down and Cmd-Alt-D to the right (Ctrl-Alt-D and Ctrl-Shift-5 on Linux and Windows), and Cmd-Alt-arrows move between the panes. Drag a terminal tab to the left, right, top or bottom edge of another terminal to split the area. In a split, drag a pane's title back to the tab bar to separate it again. Escape cancels the drag; sessions and their history stay open.

## Layout

The window has a place for each thing. On the left, the activity bar and the side column; the code in the middle, always; the terminals on its right or, with View > Terminals Under the Code (Cmd-Alt-J, or the terminals' bar right-click), under it.

The activity bar has an icon for each group of the side column: the explorer (Workspaces, Files, Outline and, once shown, Agents), Search (and References), Source Control (Changes). A click shows its group or, if it's the one showing, closes the column; Cmd-B closes it or brings it back. Drag the icons up or down to reorder them. A group's panels go one above the other: a click on a panel's header folds it to that header, dragged onto another's header it goes above it, and its lower edge, dragged, sizes it; the files, the search and the changes take the height the others leave. Any panel can go in any icon's column: drag its header onto another icon to move it there, or onto another panel's header to put it above; drag an icon onto the column to bring all its panels. Right-click a panel to hide it, or to give it an icon of its own, where it has the column to itself. The bar's right-click menu, and Show Panel in any panel's, lists every panel, checked if it's in the column showing: a click brings it there, from wherever it is, or takes it off. The icons carry what's going on in their group: the number of files changed, and the most urgent of the agents and of the other workspaces on the explorer's. At its bottom, the notes, Add Server (a host from `~/.ssh/config`, `user@host` or, on Windows, a WSL distro) and Settings.

Each workspace in the Workspaces panel has a dot for the coding agents (Claude Code, Codex, Gemini…) running in its terminals: red when one is waiting for an answer, a half yellow one while one works, green when they finished while you weren't looking, and an empty circle with none running or all idle; only the dot, no words. The Agents panel lists every agent of every workspace and server the same way, with what it's on (Claude Code's title) on hover. A click goes to that terminal.

The side column, what it shows and where things go are the same in every workspace: going from one to another moves nothing. Each workspace keeps whether its terminals show; a new one shows the terminals. Reset Layout (View, or the activity bar's right-click) puts it all back as it starts.

Debugging has a layout of its own, as Visual Studio does. While the workspace in front has a debug session (from F5, a restart or a debugged test until it stops, ends or fails to start), Den uses the debugging layout and that workspace's debugging panels; when the session ends, the editing ones come back as they were. What changes while debugging stays for the next session. The first time, the debugging layout closes the side column and puts the debugger's tab in front. `den where` says which is in use (`layout`).

Each workspace has its notes, a tab at the far end of the terminals' bar that the activity bar or Cmd-Alt-N brings in front (again, back to the terminals): plain Markdown for what's next there, kept by Den in its config folder, never in the repo, and forgotten when the worktree is removed. To write at length, Open in Editor Tab (the tab's right-click) puts them in a tab of the code; Move to Terminals, or closing that tab, brings them back. Their icon is a sticky note written on while they have something, and Cmd-E shows their first line on the way in.

## Debugging

A workspace says how to start its program in `.den/debug.json`:

```json
{ "command": "sim -d -dp 127.0.0.1:${port} ${file}" }
```

`${file}` is the open file: the program decides what debugging it means (a script, a test, the server it belongs to). `${port}` is a free port the agent picks for this session. With `"targets": ["ios", "android", "chrome"]`, the debugger's toolbar shows the target picked, kept per workspace, and `${target}` in the command is it. `den debug join` runs several programs as one session. F5 runs the command in the debugger's own terminal, shown in its console tab rather than among the terminals, and connects to the port. With a fixed `"port"` instead of `${port}`, it attaches when something already answers on it, or when there's no command. With `"open": "http://localhost:<port>/<page>"`, the browser opens that page once the program listens on that port (a server, not a script); on macOS, a Chrome tab already showing that server comes to the front instead. A program started this way stops at its first line, as Visual Studio does (F5 goes on), except a server with `open`, which runs and shows its page as soon as it listens. Everything of the debugger is one tab after the terminals', with the debugger's state on it (yellow while stopped, green while running): the toolbar at its top; the call stack, the variables, the watches and the breakpoints side by side, in two rows of two when the tab is narrow; and the console under them at the tab's width, the line between them dragged to size them. The side column stays as the debugging layout has it (see Layout). A line where the debugger stops goes to the middle of the code unless it's well inside the view. F9 toggles a breakpoint (or click the gutter; right-click it for a condition, a hit count or a log message), F10 steps over, F11 into, Shift-F11 out, Ctrl-F10 runs to the cursor, Ctrl-Shift-F10 makes the cursor's line the next statement, F6 pauses, Shift-F5 stops and Cmd-Shift-D shows or hides the debugger's tab. Inspect, in the debugger's right-click menu (or `den debug inspect`), lets you pick a widget in a phone app and opens the line that made it; in Chrome, Alt-click an element. A pick brings Den to the front. See [debugger.md](debugger.md).

## Updates

An installed Den (`Den.app` on macOS, or installed with the Linux package's `install.sh`) checks for a new release every few hours and installs it in the background; Settings → Updates turns this off, and Check for Updates still works. It never restarts by itself: the title bar shows a discreet Restart to update button, which asks before restarting. Workspaces and open files reopen as they were, and terminals keep running in the agent across the restart.

## How it's built

Rust and [GPUI](https://www.gpui.rs) with [gpui-component](https://github.com/longbridge/gpui-component). The app only draws; every machine runs `den-agent`, which keeps the terminals and does search, git and LSP next to the files, over a local socket or `ssh`. [`plan.md`](../plan.md) has the design and what's left.
