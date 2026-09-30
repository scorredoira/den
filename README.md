<p align="center">
  <img src="packaging/macos/sik.svg" width="112" alt="sik icon">
</p>

<h1 align="center">sik</h1>

<p align="center">
  A native workspace for coding agents: persistent terminals and a code editor side by side,<br>
  where every task is a git worktree, on your machine or on any server over SSH.
</p>

<p align="center">
  <img src="docs/screenshots/main.png" alt="sik: the task column, the code and the task's Claude Code terminal">
</p>

- **Tasks.** The tasks of all your servers in one column; switching tasks switches the file tree, changes, search and terminals at once. A dot per task: red when Claude is asking something, yellow while it works, green when it finished unseen. No hooks: sik reads the terminals.
- **Terminals that don't die.** They live in an agent that survives closing the app or losing SSH, and reattach with their history.
- **Code next to the agent.** Tree-sitter highlighting, F12, Shift-F12, completions and signatures over LSP, search, Cmd-P, Markdown and images; Cmd-click a `file:line` in a terminal to open it.
- **Git.** Changes against the local base branch, stage, commit, and the history with its diffs. Only the local repo: pushing and pulling is left to you.
- **Remote like local.** A server is a name from `~/.ssh/config`; sik uploads its agent and everything works as it does locally.

## Install

macOS for now. With [Rust](https://rustup.rs) and Xcode:

```sh
git clone https://github.com/scorredoira/sik && cd sik && ./install
```

For Linux servers, first `cargo install cargo-zigbuild` and `brew install zig`. To work on sik, `./run` builds in debug and opens it.

## Tasks

New Task (Cmd-N) creates a worktree with the repo's executable `.task/create <name>` if it has one, or `git worktree add` otherwise. From a sik terminal: `cd "$(sik task <name>)"`.

Cmd-1…9 go to a task, Cmd-E back to the previous one, Cmd-K finds one across servers. Every shortcut can be changed in Settings (Cmd-,).

## How it's built

Rust and [GPUI](https://www.gpui.rs) with [gpui-component](https://github.com/longbridge/gpui-component). The app only draws; every machine runs `sik-agent`, which keeps the terminals and does search, git and LSP next to the files, over a local socket or `ssh`. `plan.md` has the design and what's left.

## License

GPL-3.0. Inspired by [herdr](https://github.com/herdrdev/herdr) (Apache-2.0), whose rules for detecting when Claude Code is waiting for an answer sik uses, with code adapted from [Zed](https://github.com/zed-industries/zed) (GPL-3.0).
