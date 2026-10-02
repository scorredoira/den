<p align="center">
  <img src="packaging/macos/sik.svg" width="112" alt="Sik icon">
</p>

<h1 align="center">Sik</h1>

<p align="center">
  A native environment for working with coding agents, written in Rust.<br>
  A code editor, a multiplexer of persistent terminals and git in one app, on your machine or on any server over SSH.
</p>

<p align="center">
  <img src="docs/screenshots/main.png" alt="Sik: the workspaces column, the code, and Claude Code next to a shell in split terminals">
</p>

- **Native, in Rust.** GPU-rendered with GPUI, no Electron. On macOS, Linux and Windows.
- **Code editor.** Tree-sitter highlighting, language servers (go to definition, references, completions, signatures, formatting), project search, go to file by name, split editors, Markdown and images. Cmd-click a `file:line` in a terminal to open it.
- **Persistent sessions.** A terminal multiplexer per workspace, with tabs and splits. Every terminal lives in `sik-agent`, not in the app, like in tmux. Close Sik, update it or lose the connection and Claude keeps working; reopening reattaches each terminal with its screen and history.
- **Remote like local.** A server is a name from `~/.ssh/config`. Sik uploads its agent, which runs the terminals, search, git and language servers next to the files; only terminal output and results travel. On Windows, WSL distros work the same way.
- **Git built in.** Every workspace is a folder, a checkout or a worktree, and New Worktree (Cmd-N) starts one per task. Uncommitted changes, the history of the repo or of a file, side-by-side diffs (in one column when there's no room for two), commits with all their files, and the blame of the current line. Sik only reads: commit and push from a terminal.
- **A debugger.** Breakpoints in the gutter (with conditions, hit counts and logpoints), stepping, the values of the variables written in the code as it stops, hover, watches and a console that evaluates and assigns. For any program that speaks [Sik's debug protocol](docs/debugger.md), on your machine or on a server.
- **Every agent at a glance.** The workspaces of all your servers in one column, each with the coding agents (Claude Code, Codex…) running in its terminals under it: red when one is asking something, yellow while it works, green when it finished unseen. No hooks: Sik reads the terminals. Cmd-1…9 and Cmd-E jump between them.
- **Agents drive Sik.** With the `sik` command, Claude shows you the code it's talking about with the range selected, the diff to review or a Markdown report, reads what you selected, and opens terminals, reads them and types in them. Sik installs a Claude Code skill so Claude knows how.

## Install

Download a package from [Releases](https://github.com/scorredoira/sik/releases).

| Platform | Package | Installation |
| --- | --- | --- |
| macOS 15+, Apple Silicon | `sik-<version>-macos-aarch64.zip` | Unzip and drag `Sik.app` to Applications. |
| macOS 15+, Intel | `sik-<version>-macos-x86_64.zip` | Unzip and drag `Sik.app` to Applications. |
| Linux x86_64, Ubuntu 24.04 or compatible | `sik-<version>-linux-x86_64.tar.gz` | Extract and run `./install.sh`, or run `./sik` directly. |
| Windows 10/11 x64 | `sik-<version>-windows-x86_64.zip` | Extract the whole folder and open `sik.exe`. Optional: run `install.ps1` for a per-user install and Start menu shortcut. |

**macOS first launch:** releases are ad hoc signed, without notarization. After trying to open Sik, go to System Settings → Privacy & Security → Open Anyway. See [Apple's instructions](https://support.apple.com/102445). A normal download may be blocked until you authorize it.

**Linux:** a graphical Wayland or X11 session and a working Vulkan driver are required. On Ubuntu 24.04, install runtime dependencies with `sudo apt install libfontconfig1 libwayland-client0 libwebkit2gtk-4.1-0 libxkbcommon-x11-0 libx11-xcb1 libssl3t64 libzstd1 libvulkan1`. The optional installer also needs Python 3. Packages built on Ubuntu 24.04 require its glibc baseline; they are not universal binaries for older distributions.

**Windows:** releases are unsigned, so Windows may show a publisher/SmartScreen warning. Install Git for Windows and the Windows OpenSSH client and make `git` and `ssh` available on PATH. The default terminal is PowerShell (PowerShell 7 when installed). Keep the bundled agent next to `sik.exe`. Close the app before replacing an installed release. To work inside WSL, add the distro as a server (`wsl:<distro>`, also listed in the server picker): Sik installs its Linux agent there and runs files, git, language servers and terminals inside the distro, which it keeps running while the agent has terminals.

Git must be installed on every machine where you use repositories. SSH connections currently support Linux x86_64 servers; every desktop package includes their static agent. Install Claude Code and any language servers you use separately.

Every release includes `SHA256SUMS`. On macOS use `shasum -a 256 <archive>`; on Linux use `sha256sum <archive>`; on Windows use `Get-FileHash <archive> -Algorithm SHA256`.

To build from source, install Rust and the native dependencies from the [GPUI Kit installation guide](https://gpui-kit.com/docs/installation/), then run:

```sh
git clone https://github.com/scorredoira/sik && cd sik
cargo build --release --locked -p ui -p agent
```

Both executables are in `target/release`. On macOS, `./install` builds and installs `Sik.app`; `./run` opens a development build. To include the Linux SSH agent when building on macOS, install `cargo-zigbuild` and Zig first.

## Opening from a terminal

`sik <path>` opens a folder, or a file in its repo, as a workspace in Sik. It works from any terminal: the agent links `sik` into `~/.local/bin` if that folder exists. Over SSH it opens in the app connected to that server.

In Sik's terminals, more commands act on the workspace of the terminal they run in, the same over SSH (`sik --help` lists them all):

| Command | What it does |
| --- | --- |
| `sik show <file>:<line>` | Opens the file at that line; `<file>:10-20` or `<file>:10:5-12:3` selects that range. The keyboard stays in the terminal unless `--focus`. |
| `sik diff [<file>]` | Shows the uncommitted changes. |
| `sik doc [<title>]` | Shows the Markdown read from stdin in a tab. |
| `sik selection`, `sik tabs` | Print what's selected in the editor, and the open files. |
| `sik message <text>` | Shows a message in the status bar. |
| `sik workspaces` | Lists the workspaces and whether each is working, waiting for an answer or finished. |
| `sik term new [--right\|--down] [<command>]` | Opens a terminal, runs the command in its shell and prints its id. |
| `sik term list`, `read <id>`, `send <id> <text>`, `focus <id>`, `close <id>` | Lists, reads, types in, shows and closes terminals. |

They are meant for coding agents: when `~/.claude` exists, the agent installs a Claude Code skill (`~/.claude/skills/sik`) that tells Claude about them. Other agents can read `sik --help`. A path that is also a command's name opens with `./`, as in `sik ./tabs`.

## Workspaces

The workspaces column lists, per server, the folders opened and the known repos, each repo's worktrees (named by their branch) folded under its checkout. Drag to reorder: a checkout moves along with its worktrees. Its + adds a folder or a server (any name from `~/.ssh/config` or `user@host`), and the + beside a server opens a folder there, or creates one by typing a new name; right-click removes them, and a folder that isn't a repo yet can be made one (Initialize Git Repository). With only folders open it stays hidden until toggled (Cmd-Shift-B); it shows by itself once there's a server or a worktree.

New Worktree (Cmd-N) creates one with the repo's executable `.sik/create <name>` if it has one, or `git worktree add` otherwise. From a Sik terminal: `cd "$(sik worktree <name>)"`. To leave out the worktrees agents make on their own, turn on Only My Worktrees in Settings: the column then lists only those made with New Worktree.

On Windows, repository hooks can use `.sik/create.ps1`, `.sik/remove.ps1` and `.sik/format.ps1` (also `.cmd`, `.bat` or `.exe`). Extensionless hooks need `sh` on PATH. PowerShell terminals report their current directory automatically; custom shells should emit OSC 7 for directory tracking.

Format Document (Shift-Opt-F), and Format on Save for the types chosen in Settings, use the repo's executable `.sik/format <file>` if it has one (the text on stdin, the result on stdout; exiting with 2 leaves that type to the next way), else the language server; JSON is formatted even without either.

Cmd-1…9 go to a workspace; Cmd-E switches between them as Cmd-Tab does between apps: holding Cmd, each E goes one further back through the most recently used (Shift-E forward) and letting go enters it. Cmd-K finds one across servers, the most recently used first. Every shortcut can be changed in Settings (Cmd-,).

Drag a terminal tab to the left, right, top or bottom edge of another terminal to split the area. In a split, drag a pane's title back to the tab bar to separate it again. Escape cancels the drag; sessions and their history stay open.

## Layout

The activity bar, on the window's left edge, has an icon for each panel but the code: a click shows the panel wherever it's placed, or hides it if it shows. The icons carry what's going on in their panel: the number of files changed, Claude's state in this workspace's terminals and, on the workspaces', the most urgent of the others; the debugger's is yellow while stopped and green while running. Drag the icons up or down to reorder them. Right-click the bar, or go to View > Activity Bar, to take an icon off it: its panel still opens with its shortcut and from the menus. At its bottom, Add Server (a host from `~/.ssh/config`, `user@host` or, on Windows, a WSL distro) and Settings.

Under each workspace in the workspaces column, a row for every terminal running a coding agent (Claude Code, Codex, Gemini…): what it's on (Claude Code's title) and its state, working, waiting for an answer, or done while you weren't looking. A click goes to that terminal. A folded repo sums up its agents in a dot.

Drag an icon onto a panel's bar to put them in the same place, one showing at a time; to the left or right edge of a panel for a column of its own; or to its top or bottom edge to go above or below it in that column. The code takes the space the others leave and, as in any editor, stays put: it has no icon, and the others go around it. Its place never closes (hiding a panel that shares it shows the code), and opening a file brings it to the front. Cmd-B shows or hides the place with the files. Right-click a panel's title to hide its place; Reset Layout, in that menu, the activity bar's, the workspaces column's, the terminals', the debugger's and View, puts everything back. The places are the same for every workspace.

## Debugging

A workspace says how to start its program in `.sik/debug.json`:

```json
{
    "configurations": [
        { "name": "Server", "command": "sim -d server", "port": 4444 },
        { "name": "Attach", "port": 4444 }
    ]
}
```

F5 runs the command in a terminal and connects to the port; a configuration without a command attaches to a program already running, and so does F5 when something already answers on the port. F9 toggles a breakpoint (or click the gutter; right-click it for a condition, a hit count or a log message), F10 steps over, F11 into, Shift-F11 out, Ctrl-F10 runs to the cursor, Ctrl-Shift-F10 makes the cursor's line the next statement, F6 pauses, Shift-F5 stops and Cmd-Shift-Y shows or hides the panel. See [docs/debugger.md](docs/debugger.md).

## Updates

An installed Sik (`Sik.app` on macOS, or installed with the Linux package's `install.sh`) checks for a new release every few hours and installs it in the background; Settings → Updates turns this off, and Check for Updates still works. It never restarts by itself: the title bar shows a discreet Restart to update button, which asks before restarting. Workspaces and open files reopen as they were, and terminals keep running in the agent across the restart.

## How it's built

Rust and [GPUI](https://www.gpui.rs) with [gpui-component](https://github.com/longbridge/gpui-component). The app only draws; every machine runs `sik-agent`, which keeps the terminals and does search, git and LSP next to the files, over a local socket or `ssh`. `plan.md` has the design and what's left.

## Publishing releases

CI builds, tests and packages macOS Apple Silicon, macOS Intel, Linux and Windows on every branch push and pull request. Packages can be downloaded from the successful workflow's artifacts before publishing. No signing certificates or extra repository secrets are needed.

Once your changes are committed, publish a new version from the repository root:

```sh
./release 0.1.1
# Windows: python packaging/release.py 0.1.1
# Preview without changes: ./release 0.1.1 --dry-run
```

This updates the workspace version and lockfile, creates a version commit and an annotated tag, and pushes the branch and tag together. The `Release` workflow runs the tests and publishes all four packages with checksums and generated release notes only after every build succeeds. Use a version such as `0.2.0-beta.1` for a prerelease. Publishing takes several minutes; the first build is slower while caches fill.

Alternatively, update `Cargo.toml` and `Cargo.lock`, commit and push, then open **Actions → Release → Run workflow** on that branch. It publishes the version in `Cargo.toml` and creates its tag. A manually pushed `v<version>` tag also triggers publication; the tag must match the manifest. Existing releases are never overwritten. Uploads are assembled in a draft before becoming public; if an upload fails, delete that incomplete draft before rerunning the publication. If a build fails before publication, fix the failure and use a new version, or rerun a transient failure on the same commit.

macOS packages use ad hoc signing and Windows packages are unsigned. GUI behavior and installation should also be checked on real machines; CI tests the code and builds packages but does not exercise a real desktop session.

## License

GPL-3.0. Inspired by [herdr](https://github.com/herdrdev/herdr) (Apache-2.0), whose rules for detecting when Claude Code is waiting for an answer Sik uses, with code adapted from [Zed](https://github.com/zed-industries/zed) (GPL-3.0).
