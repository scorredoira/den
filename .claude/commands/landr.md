---
description: Review the work of this session (design, not tests), fix what the review finds, then land it like /land
argument-hint: "[extra focus, e.g. 'look at the concurrency']"
allowed-tools: Bash, Read, Edit, Write, Grep, Glob
---

`/land`, with a **code review of the work first**. The review comes BEFORE any commit,
so what lands is the reviewed version — never the first draft plus a follow-up "fix"
commit.

$ARGUMENTS is an optional extra focus for the review; when present, weight it on top of
the passes below.

## 1. Read what this session actually did

Reconstruct the change under review: the files *you* edited in this session (the same
set `/land` would stage), and for each one its diff — `git diff -- <file>` for tracked
files, the whole file for the ones you created. Another session may share this
directory, so anything you did not write is NOT under review and NOT to be touched.

Read the surrounding code too, not just the diff. A change is judged in its context: the
sibling view it should have copied, the helper that already did this, the way the rest
of the crate does the same thing.

## 2. Review it — DESIGN, not the gate

Do **not** re-run the mechanical checks here (`cargo test`, build, clippy): that is
`/land`'s own step 3, which this command reaches at the end. This pass is the one no
tool does — *is the thing well posed and well resolved?* Go through it honestly, as if
it were somebody else's code you had to defend:

- **Is it the right shape at all?** Does the problem get solved where it belongs — the
  agent instead of the UI, `proto`/`client` instead of every caller, one chokepoint
  instead of N call sites? Would a person reading this in a month understand why it
  lives here?
- **Can it be SIMPLER?** This is the main question, asked ruthlessly. Could 200 lines be
  50? Is there an abstraction that buys nothing (a trait with one impl, a single-use
  function hiding an inline body, a parallel type)? Is there a capability nobody asked
  for (YAGNI)? Is there a limitation invented with no harm behind it? Is anything
  preserved only because it was already there (zero legacy)?
- **Is anything duplicated?** The same logic, element, formatter or shape that already
  exists elsewhere in the repo — find it and reuse it instead.
- **Errors and panics** — a swallowed error (`.ok()`, `let _ =`, an ignored `Result`),
  an `unwrap`/`expect` that can actually fail, a task that dies silently.
- **Local and remote** — den runs workspaces locally and on servers over SSH through the
  agent: does the change work for both, and does it keep an older agent working (or
  say it needs a restart)?
- **Style drift** — the code reads like its neighbours: English in UI text, comments and
  names; the comment density and tone of the file (short, saying *why*); gpui idioms the
  rest of the UI uses (`when`, `children`, theme colors, `text_ui`), no hard-coded
  colors.
- **Sizing** — long sessions, big repos, many terminals and worktrees: does anything grow
  unbounded, re-render or re-read on every frame, or block the UI thread on I/O?
- **Holes** — a stated guarantee with no test, an error path nothing handles, a case the
  change obviously breaks.

## 3. Act on what you found

- **Fix it yourself** — every clear defect and every simplification you are confident in.
  Rewrite it properly; do not layer a patch on top. This is the point of the command: the
  branch improves before it lands.
- **STOP and ask** — when a finding is a genuine design decision (the shape is wrong and
  fixing it means redoing the approach, or two readings of the requirement lead to
  different work). Report it in a few lines with your recommendation, land NOTHING, and
  wait. Do not land a change you would flag as wrongly posed.
- **Say it plainly** — anything you looked at and deliberately left (a pre-existing mess
  your change did not create, a trade-off you chose on purpose) goes in the final report,
  not silently into the commit.

Keep the fixes surgical and inside the scope of what this session already touched;
`/landr` is not a licence to refactor the neighbourhood.

## 4. Land

Nothing found, or everything found is fixed → land it. Read
`.claude/commands/land.md` and follow its procedure **verbatim**, from its step 1 —
commit only your own files by explicit path, rebase onto master, run its check,
fast-forward master, never force, never push. It is the single source of truth for
landing; do not re-derive it here.

The commit message describes the *final* state of the work, not the review — the review
is how you got there, not a change of its own.

## 5. Report

Two parts, short:

1. **The review** — what you found and fixed (one line each), and what you left standing
   with the reason.
2. **The land** — whatever `/land`'s own report asks for (`git log --oneline -2` of
   master).

If nothing needed fixing, say so in one line — a clean review is a real outcome, not a
reason to invent findings.
