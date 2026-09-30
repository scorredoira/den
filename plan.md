# sik: native IDE (Rust + GPUI)

2026-09-30 · Santiago Corredoira

## Goal and scope

A single native program that combines what herdr and an editor do today: tabbed terminals that survive closing the app, a code viewer with VS Code–level syntax highlighting, and search and navigation. It works the same locally and on the servers added over SSH.

**How we work:** there are several servers (local, bill…). Tasks are opened on each one; each task is a git worktree of any of the repos on that machine (golfmanager, scl…). In a task you want to see the code, its changes and the Claude Code session working on it; one terminal is usually enough, but you can open several. The app is organized around the task.

**In:**

- Terminals with tabs and splits, persistent as in herdr (closing the app doesn't kill them; they come back when you reopen it).
- Tasks: one per worktree, grouped by server, with the state of their Claude Code session in view.
- Code viewer with tree-sitter highlighting and light editing, plus a Markdown viewer.
- File tree, git changes, global search and references in the side panel, with a preview as you move through them.
- F12 (go to definition) and Shift-F12 (references) via LSP.
- Click on a `file:line` path inside a terminal: it opens in the viewer.
- Hosts: local and any SSH server that has been added, transparently.
- Platforms: macOS and Linux now; Windows later. The architecture is portable from the start, but for now it is only tested on the Mac (see Platforms).

**Out:** serious editing (refactors, advanced multi-cursor), debugger, AI, extensions, collaboration, remote Windows servers (the remote agent only runs on Linux and macOS).

## Decisions

Rust with GPUI across the whole app, developed directly on the Mac. macOS is GPUI's most mature backend (Metal) and the one used most; Linux (wgpu, Wayland and X11) shares all the Unix code with macOS and is considered supported without being tested on every change. Windows (GPUI's own backend) is prepared for in the architecture, with its hard parts left unimplemented until its phase.

| Piece | Choice | Why |
| --- | --- | --- |
| Language | Rust | One language for the app and the agent; acceptable rebuild times (target under 10 s) |
| UI | `gpui-kit` (Apache-2.0): GPUI, gpui-component and the backends for all three platforms | Native on the GPU; the same foundation as Zed; a single dependency with a pinned version |
| Viewer | gpui-component's `Editor` | Highlighting, line numbers, large files; light editing is enough. Its base crate, gpui-base, is copied in `vendor/` with a few public functions added (see `vendor/README.md`) |
| Highlighting | tree-sitter, the one built into gpui-component | VS Code level; if it falls short, `highlights.scm` queries from Helix or Zed in our own `syntax` crate |
| Terminal | `alacritty_terminal` (Apache-2.0) | Pure Rust, proven with GPUI inside Zed; no Zig to compile as with `libghostty-vt` |
| Pty | `portable-pty` | The same one herdr uses; ConPTY on Windows |
| Search | `grep` + `ignore` crates (ripgrep's) | Respects `.gitignore`, without depending on a binary |
| Git | calls to `git` | Status, diffs, staging, commits, branches, history |
| LSP | minimal custom client (JSON-RPC over the server's stdio) | `definition`, `references`, `completion` (with `completionItem/resolve`) and `signatureHelp` |
| Agent concurrency | `std` threads and channels, no `tokio` | Dozens of terminals per machine: one thread per pty and per connection is enough and the code is simpler |
| SSH | the system `ssh` binary | Reuses `~/.ssh/config`, keys, agent and ProxyJump with no code of our own |

The project is GPL-3.0. That way code can be copied from Zed (GPL-3.0) as well as herdr (Apache-2.0), keeping their copyright notices in each file. Individual files are copied and adapted; neither repo is forked. So far:

- **From Zed:** `ui-term/src/keys.rs` (keys to escape sequences) is copied and adapted; `ui-term/src/element.rs` follows the approach of its `terminal_element.rs`. If gpui-component's viewer falls short, parts of Zed's `editor` would come next.
- **From herdr:** the rules for telling when Claude Code is waiting for an answer (`agent/src/blocked.rs`). The persistent session model and pty handling follow its ideas; its UI (ratatui) is not reused.

## Architecture

The Mac app only draws; each machine (the Mac itself included) has an agent that keeps the terminals and does the work next to the files.

```
┌─ Mac app ────────────────────────┐
│ ┌──────────────────────────────┐ │
│ │ UI (GPUI)                    │ │
│ │ mode bar, panels,            │ │
│ │ viewer, tabs and terminal    │ │
│ └──────────────────────────────┘ │
│ ┌──────────────────────────────┐ │
│ │ Local, no network round trip │ │
│ │ text + tree-sitter highlights│ │
│ │ copy of the terminal emulator│ │
│ └──────────────────────────────┘ │        Unix socket     ┌─ Local agent (daemon) ────────┐
│ ┌──────────────────────────────┐ │ ─────────────────────▶ │ terminals: pty + emulator     │
│ │ client                       │ │                        │ files, search, git, LSP       │
│ │ connections to each host     │ │                        │ stays alive when app closes   │
│ │ reconnect, agent upload      │ │  ssh host bridge       └───────────────────────────────┘
│ └──────────────────────────────┘ │ ─────────────────────▶ ┌─ Agent on each server ────────┐
└──────────────────────────────────┘                        │ the same binary as the local  │
                                                            │ app uploads it if missing/old │
                                                            │ stays alive if SSH drops      │
                                                            └───────────────────────────────┘
```

Local and remote use the same agent and the same protocol: only the transport changes, a local socket (named pipe on Windows) or `ssh`. That's why remote isn't a special case and persistence comes for free in both.

## UI ↔ agent protocol

A single byte stream with binary frames, the same locally and over SSH; the only thing that changes is how the stream is opened.

- **Local transport:** the agent's Unix socket on macOS and Linux (in the app's state directory); a named pipe (`\\.\pipe\sik-agent-<user>`) on Windows.
- **Remote transport:** `ssh <host> sik-agent bridge`. The `bridge` starts the daemon if it isn't running and connects its stdin/stdout to the socket. The UI doesn't know whether it's talking to a local or a remote host.
- **Frames:** `u32` length + body serialized with `serde` as MessagePack (`rmp-serde`). Each frame is a request with an `id`, a response to that `id`, or an event.
- **Startup:** `Hello { protocol }`. Then `Version` returns the fingerprint of the agent's binary; if it isn't the one this app carries, the server shows "outdated agent · restart" (see Agent versions).

| Group | Requests | Events |
| --- | --- | --- |
| Agent | `Hello`, `Version`, `Shutdown` | |
| Files | `ListDir`, `ReadFile`, `WriteFile`, `CreateFile`, `CreateDir`, `Rename`, `Trash`, `Watch`, `Unwatch` | `FsChanged` |
| Search | `Search { query, regex, case_sensitive, max_hits }`, `FindFiles` | |
| Git | `GitChanges`, `GitDiff`, `Git { op }` (status, stage, unstage, discard, commit, branches, switch, log, commit files and diffs) | |
| LSP | `Lsp { op }` (definition or references) | |
| Tasks | `RepoAdd`, `RepoRemove`, `RepoList`, `TaskList`, `TaskCreate`, `TaskRemove`, `BlockedList` | `Activity`, `Blocked`, `OpenTask` |
| Terminals | `TermCreate { group, cwd, command, cols, rows }`, `TermList`, `TermAttach`, `TermDetach`, `TermInput`, `TermResize`, `TermKill`, `TermCwd`, `SavePastedImage` | `TermOutput`, `TermTitle`, `TermExit` (the snapshot is `TermAttach`'s response) |

Terminal output travels as raw pty bytes, not as already-rendered screens; over SSH that's the cheapest option.

## Persistent terminals

The agent owns each terminal: it holds the pty and an `alacritty_terminal` emulator with the screen and scrollback. Closing the UI or losing SSH only disconnects; the processes stay alive.

1. `TermCreate` opens the pty in the agent and gives it a stable id. The agent reads the pty at all times and feeds its emulator, whether or not a UI is connected.
2. `TermAttach { id }` replies with `TermSnapshot`: the screen, the history (up to 10,000 lines), the cursor, the modes (alternate screen, mouse, bracketed paste…) and the title, as escape sequences that reproduce them in a new emulator of the same size, the way tmux does on reconnect. The snapshot and the subscription happen under the same lock, so no output is lost between them. After that, raw `TermOutput` arrives.
3. The UI has its own `alacritty_terminal`: it processes the snapshot and then the bytes. That way it draws, selects and scrolls locally, with no round trip. Replies to the application's queries (cursor position, attributes) are given only by the agent's emulator, which is always there; the UI answers only what it knows itself: theme colors, pixel size and clipboard.
4. `TermDetach`, or the stream closing, leaves the terminal alive. `TermKill` or the process exiting ends it and emits `TermExit`.
5. If several UIs are connected at once, the size is set by the last one that wrote, as in tmux.

**Tab layout:** which terminal goes in which tab and split is stored in the UI, per task, in `layout.json` inside the app's config directory (see Platforms). When the app opens, it's rebuilt and each id gets a `TermAttach`. Ids that no longer exist (the host rebooted) are dropped from the layout; if none is left, a new terminal opens in the task's folder. An agent restarted to update recreates its terminals under the same ids, so the layout survives that (see Agent versions).

**Agent lifetime:** it stays alive as long as it has terminals. With no terminals and no connected UIs, it exits after 10 minutes.

**Known limitation:** with the alternate screen active (vim, htop), the snapshot only reproduces that screen: `alacritty_terminal` gives no access to the primary one in the meantime. When you exit vim after reconnecting, the view doesn't have the earlier history (the agent does).

## UI

The main unit is the task; the server is an attribute of the task, not a level to navigate through. You jump from one task to another far more often than from one server to another.

```
┌──────────────┬──────────────┬────────────────────────┬───────────────────────┐
│ TASKS        │ ▣ ⎇ ⌕ ⇲      │ workspace.rs  main.rs  │ claude   zsh          │
│              │ ▾ crates     │                        │                       │
│ local        │   ▾ ui       │  1 use std::path…      │ > implement the       │
│  ● scl/fix-  │     main.rs  │  2                     │   lazy tree…          │
│    login     │     work…    │  3 pub struct …        │                       │
│ bill         │              │                        │ ✻ Editing             │
│  ◐ golf/     │              │                        │   workspace.rs…       │
│    reservas  │              │                        │                       │
│  ○ golf/v3   │              │                        │                       │
│  + New Task  │              │ golf/reservas · Ln 3   │                       │
└──────────────┴──────────────┴────────────────────────┴───────────────────────┘
  tasks          side panel     IDE                      task terminals
```

| Area | Contents |
| --- | --- |
| Tasks (resizable, can be hidden) | Tasks grouped by server, each as `repo/branch`, with the state of its terminals (see below). At the bottom, "New Task" and the Settings gear. |
| Side panel (resizable, can be hidden) | Its header has icon tabs for the four modes. Files: the worktree's tree. Changes: git, in three views: Branch (the files changed relative to the base branch), Uncommitted (stage, unstage, discard, commit) and History (commits, their files and diffs); plus the current branch with a picker to switch between local branches. Only the local repo: no remotes, push or pull. Search: the search box and the results grouped by file. References: the result of the last Shift-F12. |
| IDE | File tabs: code, rendered Markdown, images and read-only diffs. |
| Task terminals | Terminal tabs and splits; usually one running Claude Code. |

- **Switching tasks switches everything at once:** tree, changes, search and terminals move to that task's worktree. Each task remembers its tabs, splits and sizes.
- **IDE and terminals side by side**, with adjustable size. One shortcut maximizes either of them; another hides the tasks column.
- **State of each task:** the agent watches whether its terminals are producing output (Claude Code produces it nonstop while it works) and notifies all UIs with `Activity`. Red `●`: waiting for a reply (the screen is read once it goes quiet, using herdr's rules); yellow `◐`: working; green `●`: finished without you looking at it; `○` stopped. It works with any program and without configuring hooks. No separate "agents" view is needed.
- **New Task:** repo (the known ones on that machine) and name. How the worktree is created is up to each repo: if it has an executable `.task/create`, the agent calls it with the name (scl runs `swt`; v3, `sim wt`); otherwise, `git worktree add ../<repo>-<name>`. Then it looks up that branch's worktree in `git worktree list`, so it doesn't matter where the script creates it or what it prints. The task opens with a terminal in its folder. Not done yet: starting `claude` in it by itself (a `task_command` in `config.json`).
- **From the terminal:** `sik task <name>` creates the task in the repo of the current folder (being in a worktree is fine) and prints only its path, for `cd "$(sik task x)"`. `sik` is the agent itself, linked in a folder that comes first in the PATH of sik's terminals, so it talks to the agent on its own machine and works the same over SSH. Inside sik, the app also switches to the task.
- **Deleting a task:** right-click → Delete Task…, with confirmation. The agent calls the repo's `.task/remove <name> <path>` (scl: `swt -r`; v3: `sim wt -r`) or, if there isn't one, `git worktree remove`, which won't delete with uncommitted changes. Then it checks that the worktree is gone and closes its terminals. The main checkout is never deleted.
- **Column:** right-click with New Task in Repo…, Copy Path, Reveal in Finder, Hide and Delete; drag to reorder (the order for Cmd-1…9). The gear at the bottom (or Cmd-,) opens Settings: theme, servers, repos, hidden tasks and keyboard shortcuts. Stored in `config.json`.
- **Agent versions:** each protocol version uses its own socket (`agent-<version>.sock`). A new app never shuts down an old agent: its terminals stay alive for the old app, and the agent shuts itself down once they're gone. Within the same protocol, a rebuilt agent has a different fingerprint: the server shows "outdated agent · restart", and restarting (after confirming) swaps in the new binary. Before exiting, the old agent writes each terminal's id, task, folder, size and, if Claude Code was running in it, its command line to `agent-<protocol>.restart.json` next to the socket; the new one recreates them under the same ids and types the Claude command plus `--continue`. Scrollback and other running processes are lost.
- **Where things are stored:** tasks aren't stored: they're the worktrees of the repos known to each server's agent (`repos.json` in its config; opening sik in a repo adds it). Each task's terminal layout lives in the UI (`layout.json`); servers, open tabs per task, panel sizes and shortcuts in `config.json`.
- **Task shortcuts:** Cmd-1…9 go to task N; Cmd-E goes back to the previous one (again, to the one before, like Alt-Tab); Cmd-K opens a fuzzy finder for tasks across all servers.

**Navigating results:** when moving through a result in Search or References, the viewer shows it in a preview tab (in italics), which the next preview reuses. Enter or double-click turns it into a pinned tab. F4 and Shift-F4 go to the next and previous result without leaving the viewer.

**Focus and shortcut rules** (the same as sid's):

- A shortcut means the same thing wherever focus is; panels never shadow viewer or terminal shortcuts.
- No panel keeps the keyboard on its own: a click in the text always gives the keys back to it. The exception is the file tree: after a click in it, the arrows move the selection (with preview) and Enter renames, as in Finder.
- No message pushes the text around: it floats on top or isn't shown.
- A shortcut is never removed silently: the conflict is reported before the change, in the same dialog.
- A panel only appears when asked for, and the same key that shows it hides it, without moving focus.
- The file tree always sits in a column to the left of the code.

**Shortcuts:** the app's shortcuts use Cmd on Mac and Ctrl on Windows and Linux (GPUI's `secondary` modifier).

**Shortcuts in terminals:** terminals get every key except the app's shortcuts. On Mac the shortcuts use Cmd, so Ctrl is left entirely to the shell, vim and agents. On Windows and Linux, inside a terminal the app's shortcuts become Ctrl-Shift (Ctrl-Shift-W, Ctrl-Shift-B…), as in Windows Terminal and VS Code, and plain Ctrl still goes to the shell.

## Viewer

The text lives in the UI and the agent only reads and writes files. Highlighting is done locally with tree-sitter, so over SSH there's no latency when scrolling.

- **Base:** gpui-component's `Input` in code mode (line numbers, selection, in-file search, files with hundreds of thousands of lines). If it falls short, Zed's `editor` is copied piece by piece.
- **Highlighting:** tree-sitter grammars compiled into the binary. First batch: TypeScript, TSX, JavaScript, Go, Rust, JSON, TOML, YAML, Markdown, SQL, CSS, HTML, Bash. The `highlights.scm` queries come from Helix or Zed.
- **Theme:** light, dark or following the system, with the highlighting colors of VS Code's 2026 theme (`assets/themes/vscode-2026.json`).
- **Light editing:** typing, deleting, undo, saving with Cmd-S (Ctrl-S on Windows and Linux), plus VS Code's multi-cursor and line shortcuts (see Next). No refactors.
- **Changes on disk:** on `FsChanged`, if the file has no unsaved changes it reloads silently; if it does, you're warned.
- **Diff:** in Changes mode, a file opens as a read-only diff tab; Open File from its menu goes to the file itself.
- **Images:** png, jpg, gif, webp, svg, bmp and tiff open in a tab, fitted to it.
- **Markdown:** a `.md` opens rendered (headings, tables, task lists, highlighted code), using gpui-component's Markdown rendering. A shortcut toggles between preview and source; the source is the normal viewer and is edited like any file, and the preview updates as you edit. Links in the preview: URLs open in the browser; relative paths (to the file's folder, or to the task's root with a leading `/`) open in a tab.

## Search, F12 and references

Everything runs in the agent, next to the files, and the UI only receives the results.

- **Global search:** `grep` and `ignore` crates in the agent, in parallel, respecting `.gitignore`. It searches as you type; results come in one response, capped at 5,000 matches (the panel says when there were more).
- **Find file by name (Cmd-P):** the agent lists the project's files (`FindFiles`, respecting `.gitignore`) and the UI filters locally with `nucleo`, Helix's fuzzy matcher.
- **Find in file (Cmd-F):** local, inside the viewer.
- **F12 and Shift-F12:** the agent starts a language server per project and language (rust-analyzer, gopls, clangd, Pyright or pylsp, and TypeScript: the project's or the global one; with TypeScript 7, `tsc --lsp --stdio`). It only implements `initialize`, `didOpen`, `didChange`, `definition`, `references`, `completion` (and `completionItem/resolve` for the selected item) and `signatureHelp`.
- **Completions and signatures:** the menu opens as you type an identifier or after `.`, `::` or `->`, asked once per word and filtered locally while it grows; it's drawn like VS Code's (icon per kind, matched letters, the selected item's detail). The signature shows above the cursor on `(` and `,`, with the current parameter in bold, until the call closes, the cursor leaves the line or Escape.
- **F12 result:** with a single target, it jumps straight there; with several, they're shown in the References panel.
- **Without LSP:** if the language has no server, Shift-F12 searches for the exact word under the cursor and says so in the panel.

## Servers and clicking on paths

A host is a name from `~/.ssh/config` or `user@machine`, and the app does the rest: uploads the agent, starts it and reconnects.

- **Adding:** in Settings → Servers, a name from `~/.ssh/config` (the icon lists its `Host` entries) or `user@host`. Stored in `config.json`. `local` always exists. Each server's repos are known by its agent (the folders added to it, plus those that already have tasks).
- **Agent installation:** on connect, `ssh host uname -sm` detects the system. If `~/.local/share/sik/sik-agent-<protocol>-<fingerprint>` isn't there, the agent is uploaded over the same connection and older ones are deleted. `Sik.app` bundles the agent for Linux x86_64, statically linked with musl; that's the only server system supported so far (aarch64 and macOS servers are pending). The Windows one would only be used locally.
- **Connection:** `ssh -o ControlMaster=auto -o ControlPersist=10m -o ServerAliveInterval=15`, so opening more streams is instant and a drop is detected quickly. Windows' `ssh` (OpenSSH) doesn't support `ControlMaster`: there each stream opens its own connection.
- **Drops:** if the connection is lost, the server's tasks are marked as disconnected, it retries with increasing backoff, and on reconnect each terminal gets a `TermAttach`. Nothing is lost because the agent stays alive.
- **Several servers at once:** the list shows the tasks of all of them; each task talks to its server's agent.

**Clicking on paths inside a terminal:** the UI looks for `path`, `path:line` and `path:line:column` patterns on screen. Relative paths are resolved against that terminal's current directory, which the agent reads from the process (`/proc/<pid>/cwd` on Linux, `proc_pidinfo` on macOS) or, on Windows, from the OSC 7 sequence the shell emits. On Windows paths like `C:\path\file.rs:12` are recognized too. Cmd-click (Ctrl-click on Windows and Linux) opens the file in the viewer, at the given line and on the same host. URLs are recognized as well and open in the browser.

## Platforms

Portable architecture from scratch, but for now only tested on the Mac. Linux uses the same Unix code as macOS. Windows is left prepared: when it's wanted, it'll be a matter of filling gaps, not changing the design.

**Rules to keep it portable from now on:**

- Everything that depends on the OS lives behind a small interface, in a `platform` module in each crate: local transport, daemon startup, reading a terminal's current directory, and config and state paths. The rest of the code doesn't use `#[cfg(target_os)]`.
- Config and state paths come from the `dirs` crate; no hand-written paths.
- The UI never assumes `/` as the separator: the agent's paths travel as text in its platform's format and the UI only displays them and sends them back.
- Shortcuts use GPUI's `secondary` modifier: Cmd on Mac, Ctrl on Linux and Windows.
- On Windows, whatever isn't done compiles and returns a clear error ("not implemented on Windows"), never a `panic!`. That way the app starts and you can see what's missing.

| Topic | macOS (tested) | Linux (same code) | Windows (gap) |
| --- | --- | --- | --- |
| Rendering (GPUI) | Metal | wgpu, Wayland and X11 | GPUI's own backend; should just work |
| App modifier | Cmd | Ctrl | Ctrl |
| App shortcuts inside the terminal | Cmd | Ctrl-Shift | Ctrl-Shift |
| Transport to the local agent | Unix socket | Unix socket | Named pipe: not implemented |
| Daemon startup | `fork` and `setsid` | `fork` and `setsid` | Detached process: not implemented |
| Pty | `portable-pty` | `portable-pty` | `portable-pty` (ConPTY): untested |
| A terminal's current directory | `proc_pidinfo` | `/proc/<pid>/cwd` | Shell's OSC 7: not implemented |
| Config and state | `~/Library/Application Support/sik` | `$XDG_CONFIG_HOME` and `$XDG_STATE_HOME` | `%APPDATA%\sik` via `dirs` |
| SSH | `ssh` with `ControlMaster` | `ssh` with `ControlMaster` | OpenSSH without `ControlMaster`: one connection per stream |
| `C:\path:12` paths in the terminal | — | — | Not implemented |
| Package | `.app` signed with the development certificate (Developer ID and notarization pending) | AppImage or `.tar.gz`: pending | `.msi` or `.zip`: pending |

## Repo structure and build

A Cargo workspace with small crates, so that a change in the UI rebuilds only the UI and the agent never depends on GPUI.

| Crate | What it contains | Depends on |
| --- | --- | --- |
| `proto` | Messages, frames and protocol version | `serde` |
| `agent` | Daemon: terminals, files, search, git, LSP and tasks; `sik-agent` binary with `daemon` and `bridge`, and `sik` for terminals | `proto`, `portable-pty`, `alacritty_terminal` |
| `client` | Connection to hosts (socket or `ssh`), reconnection, agent upload | `proto` |
| `syntax` | Only if gpui-component's highlighting falls short: our own grammars and queries, themes | `tree-sitter` |
| `ui-term` | Terminal view in GPUI | `gpui-kit`, `alacritty_terminal` |
| `ui` | Window, tasks column, panels, viewer, settings; the app binary | all of the above, `gpui-kit` |

**For fast builds and a small disk footprint** (in the workspace `Cargo.toml`):

```toml
[profile.dev]
debug = "line-tables-only"   # less disk and faster linking
split-debuginfo = "unpacked" # the default on macOS

[profile.dev.package."*"]
opt-level = 2                # optimized dependencies, compiled once

```

- The agent for Linux x86_64 servers is built on the Mac, statically with musl, using `cargo zigbuild` (which uses `zig` as the linker); `./run` puts it next to the app.
- `gpui-kit` pinned to an exact version (`=0.7.0`), which in turn pins GPUI, so the API doesn't change by surprise.
- The tree-sitter grammars are dependencies: they're compiled once with `opt-level = 2` and never touched again.
- Expected disk use on the Mac: 6–10 GB in `target/` plus ~1.5 GB for Rust; Xcode is already installed.
- Target: a change in `ui` rebuilds in under 10 s on the Mac. Measured in phase 0: under 1 s.

## Phased plan

Each phase leaves something usable every day; the persistent terminal comes early because it's what replaces herdr.

1. **Phase 0, skeleton:** GPUI window, mode bar, local tree, viewer with highlighting and Markdown viewer. Done when: it opens a repo, navigates the tree, and a change in `ui` rebuilds in under 10 s on the Mac. (Done.)
2. **Phase 1, local terminal:** terminal area next to the IDE, with tabs and splits on top of `alacritty_terminal`, Cmd-click on paths. Done when: Claude Code, vim and htop are used inside it with no rendering glitches on the Mac. (Done: Claude Code and htop tested; the pty lives in `ui-term/src/pty.rs` until the agent takes it over.)
3. **Phase 2, agent, persistence and local tasks:** the agent as a local daemon with persistent sessions; the UI switches to talking to it over a socket. Tasks column: creating a task makes the worktree and opens Claude Code; Claude's state in the list. Done when: closing the app and reopening it restores all tasks and terminals with their contents. (Done: agent with persistent terminals, saved layout, tasks column with `.task/create`, state of each task and Cmd-1…9. Cmd-K, the task finder, too.)
4. **Phase 3, SSH:** adding servers, agent upload, `bridge`, reconnection; tasks from several servers in the list. Done when: working on a server feels the same as locally and cutting the wifi for 1 minute loses nothing. (Done and tested with bill: Linux agent built with cargo-zigbuild and uploaded over the same connection; files, search, changes and terminals go through each server's agent; reconnection with increasing backoff and terminals that reattach.)
5. **Phase 4, search and changes:** global search and Cmd-P in the panel, Changes mode with git relative to the base branch. Done when: it replaces sid's search. (Done.)
6. **Phase 5, LSP:** F12 and Shift-F12 with the References panel. Done when: it works in TypeScript, Go and Rust, locally and over SSH. (Done: `lsp.rs` in the agent, tested against rust-analyzer, gopls and TypeScript 7; Ctrl-Opt-←/→ to go back and forward.)
7. **Phase 6, polish:** themes, settings, signed `.app` and Linux package. (Done on Mac: `./install` builds in release, assembles `Sik.app` with its icon and agents, signs it with the development certificate and installs it in `/Applications`. Still missing: Developer ID with notarization, for distributing it, and the Linux package.)
8. **Phase 7, Windows:** fill in the gaps in the Platforms table (named pipe, daemon startup, ConPTY, OSC 7, `C:\` paths) and package it. Done when: the app is used daily on Windows with PowerShell and Claude Code, against a Linux server over SSH.

## Status and next steps

From here on sik is developed inside sik (Claude Code in a terminal of the `sik/master` task). Done in phases 0–4, in addition to the above:

- Each task remembers its tabs (and the cursor); with no folder at startup, sik returns to the last task (`sessions` and `last` in `config.json`).
- "outdated agent · restart" notice when the connected agent isn't the one from this build (`Version` request with the binary's fingerprint).
- XML grammar added by hand (`language::register`), plus Python, C, C++ and Makefile.
- Warning on quit (Cmd-Q or closing the window) with unsaved tabs in any task.
- Right-click on the empty space in the tree (new file or folder at the task's root).
- When the width of the window or of the tasks column changes, only the code resizes; the side panel and the terminals keep their width.
- Custom dialog when quitting with unsaved changes: a clickable list to go to each file, and Save All and Quit.
- Cmd-E goes back to the previous task; the pickers (Cmd-P, Cmd-K, branches) close with a click outside.
- LSP: F12 (with one target it jumps; with several, to the References panel) and Shift-F12 (References, with F4); without a server, Shift-F12 searches for the word. Servers: rust-analyzer, gopls, clangd, Pyright/pylsp and TypeScript (the project's or the global one; with TypeScript 7, `tsc --lsp --stdio`). Custom editor menu with Go to Definition and Find References.
- Settings in a modal (Cmd-, or the gear) with search and an index: Appearance, Servers, Repos, Hidden Tasks and Keyboard Shortcuts (changed by recording the combination; a conflict is reported before anything is removed; in `config.json`, `keys`).
- Settings: folder browser through the agent for adding repos (remote too) and a finder for the `Host` entries in `~/.ssh/config` for adding servers.
- Jump history: Ctrl-Opt-← goes back and Ctrl-Opt-→ goes forward. When jumping, the target line is centered if it wasn't visible.
- Tasks turn red when Claude is waiting for a reply: the agent reads the bottom of the screen once it goes quiet, using herdr's Claude rules (`blocked.rs`). Still to be tested with real Claude (needs an agent restart).
- Right-click menus show the shortcut for each entry that has one.
- Code highlighting with the colors of VS Code's 2026 theme, light and dark (`assets/themes/vscode-2026.json`).
- Close All Tabs (Cmd-Alt-W) and Collapse All Folders (Cmd-Alt-C), also in the tab and tree menus.
- Links in rendered Markdown: URLs open in the browser; relative paths (to the file's folder, or to the task's root with a leading `/`) open in a tab.
- Restarting an agent to update it reopens its terminals: the old agent writes `agent-<protocol>.restart.json` next to its socket and the new one recreates them under the same ids, so the UIs reattach without noticing.
- Git in Changes mode: Branch, Uncommitted (stage, unstage, discard, commit) and History (commits with their files and diffs) views; current branch with a picker to switch between local branches. `Git { path, op }` request in the agent.

**Careful when working on sik from sik:**

- Rebuilding and restarting the app (`./run`) is safe: the terminals live in the agent and reattach.
- Restarting an agent ("outdated agent · restart") restarts all its terminals, including the one where Claude is working: they reopen in the same place (same ids, folder and size), and where Claude Code was running it's resumed with its options plus `--continue`. The scrollback and whatever else was running are lost.
- Bumping `PROTOCOL` starts a new agent on another socket: the old one's sessions stay alive but the new app doesn't see them. Those changes are better made from Terminal.app.
- On macOS, rebuilding a running binary kills it: that's why the agent and the app run from copies (`agents/`, `builds/` in the state folder).
- Don't simulate keystrokes or open test instances while the user is working: ask them to test.

**Next, in order:**

1. Polish whatever comes up in daily use (highlighting and theme colors, UI details).
2. Try out with real use: tasks turning red, terminals reopening after an agent restart, links in Markdown.
3. New tasks start `claude` in their first terminal (`task_command` in `config.json`).
4. The rest of phase 6 (Developer ID and notarization; Linux package) and phase 7 (Windows).

**Editor and git, requested (VS Code as the model):**

- [x] **Inline blame:** at the end of the cursor's line, in gray, the last commit that touched it: subject, author and age (`Add ACIGRUP PMS integration module, Minnu (7 months ago)`). The agent runs `git blame --porcelain` when the file is read or saved, and again when `HEAD` moves (commit, checkout, reset, also from a terminal): it watches the git folder's `logs/HEAD`, which in a worktree is outside the task, and reports it as a change to `.git`. Uncommitted lines show nothing.
- [x] **Cmd-D:** selects the word under the cursor, and each further press adds a cursor at its next occurrence. As in VS Code, starting from a word it matches whole words with the same case; from any other selection it follows the find bar's Aa.
- [x] **Cmd-Opt-↑/↓:** a cursor on the line above or below (gpui-component's `AddCursorAbove/Below`). "Focus Terminal Above/Below" shadowed it: GPUI ranks a binding without context as the deepest, so the editor's bindings are registered after the app's, in `CodeEditor > Input` (`editing::keymap`).
- [x] **Opt-↑/↓:** moves the line (or the selected lines) up or down.
- [x] **Shift-Opt-↑/↓:** duplicates the line (or the selection) up or down. (On Linux gpui-component binds these to adding a cursor.)
- [x] **Global replace:** a Replace box in the Search panel. A "preserve case" toggle (VS Code's AB): the replacement takes the case of each match (`payment` → `invoice`, `Payment` → `Invoice`, `PAYMENT` → `INVOICE`). The agent writes the files; open tabs without unsaved changes reload on their own.
- [x] **Occurrences of the symbol:** clicking on a name highlights its other occurrences in the file: same exact word, whole words only. (With LSP, `textDocument/documentHighlight` would tell reads from writes; not done.)
- [x] **Ctrl-G:** go to `line` or `line:column`, in Cmd-P's spot; Ctrl-Opt-← comes back.
- [x] **Menu bar (macOS):** Sik, File, Edit, Selection, View, Go, Terminal, Window and Help, as in VS Code; each entry is an existing action and shows its shortcut (`app_menu.rs`).
- [x] **Word wrap:** Opt-Z (and View > Word Wrap), for every tab and task, saved in `config.json` (`word_wrap`).
- [x] **Split editor:** two groups of tabs, side by side (Cmd-Opt-S) or one above the other (Cmd-Opt-Shift-S; not VS Code's Cmd-\, which on a Spanish keyboard needs Opt), or from the tab's menu. As in VS Code, splitting opens the active file on the other side too: another view with its own editor (cursor, scroll, undo), kept in sync with the first by applying the same edit; saving, unsaved changes and the blame are the file's, and if the file's tab closes a view takes over. A Markdown file opens its preview on the other side (Open Preview to the Side, Cmd-Opt-V), updating as you type. Move to Other Side moves a tab; a group left empty closes the split.
- [ ] **Drag tabs** to split the editor or move them between groups (later: the menu and the shortcuts cover it).

## Risks and open questions

| Risk | What we'd do |
| --- | --- |
| GPUI's API changes and there's little documentation | Pin the commit; read Zed's code as documentation; update only when needed |
| gpui-component's viewer falls short (wrap, huge files, IME) | Copy parts of Zed's `editor` |
| The terminal snapshot doesn't reproduce some unusual mode (scroll regions, character sets) | Add it to `snapshot.rs`; there are tests that process the snapshot in a new emulator and compare |
| Rebuilds exceed 10 s | Split `ui` into more crates; try the Cranelift backend |
| Servers with an old glibc or without permission to execute in `~` | Static agent with musl; configurable install path |
| Something Mac-specific leaks out of the `platform` module and breaks Linux or Windows unnoticed | Build for Linux now and then (`cargo check --target`) and review the `cfg`s when closing each phase |
| ConPTY on Windows renders differently or drops sequences with full-screen programs | Found out in phase 7; the terminal view doesn't depend on the pty, so the fix stays in the agent |

**Open questions:**

- [x] Project name: **sik** (binary `sik`, agent `sik-agent`, folders `sik`). From the sid family (the TUI), without clobbering its binary or its folders.
- [x] Is sid abandoned once phase 4 lands, or do they coexist? They coexist: sik is a different tool, not its replacement.
- [x] Is an "agents" view like herdr's needed? No: Claude Code's state goes in the task list.
- [x] Default shortcuts: VS Code's, sid's, or sid's with Cmd on Mac? VS Code's. (The modifier is settled: Cmd on Mac, Ctrl on Windows and Linux, Ctrl-Shift inside the terminal.)

