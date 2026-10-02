---
name: sik
description: Drive the Sik app you are running in (when SIK_TERMINAL is set) with the `sik` command - show the user a file, a line or a selected range, a diff or a Markdown note; read what the user has selected in the editor or which files are open; open terminals, read them and type in them (e.g. start or watch other agents in their own worktrees); create worktrees. Use it whenever pointing the user at code would help, when the user refers to "this" or "what I selected", or to work with other terminals.
---

# Sik

You are running in a terminal of Sik, an editor with persistent terminals
(`SIK_TERMINAL` is set). The `sik` command, already in the PATH, acts on the
workspace of the terminal it runs in, also over SSH. Run `sik --help` for the
full list; the main ones:

- `sik show <file>:<line>` opens the file at that line for the user;
  `sik show <file>:<line>-<line>` (or `<line>:<col>-<line>:<col>`) selects
  that range. Use it to point at what you are talking about instead of
  pasting code. The keyboard stays in the terminal unless `--focus`.
- `sik diff [<file>]` shows the uncommitted changes, so the user can review them.
- `echo "# Title ..." | sik doc "<title>"` shows Markdown in a tab: plans,
  reports, tables, anything longer than a chat answer.
- `sik selection` prints the file, the range and the text the user has
  selected: what they mean by "this". `sik tabs` lists the open files.
- `sik message <text>` shows a short message in the status bar.
- `sik term new [--right|--down] [<command>...]` opens a terminal next to
  this one and prints its id; `sik term list`, `sik term read <id> [<lines>]`,
  `sik term send <id> <text>` (types it and Enter), `sik term focus <id>`,
  `sik term close <id>`.
- `sik worktree <name>` creates a worktree and prints its path;
  `sik workspaces` lists the workspaces and whether their agents are
  working, waiting for an answer or finished.

Lines and columns start at 1. A command that fails prints why and exits
with an error.
