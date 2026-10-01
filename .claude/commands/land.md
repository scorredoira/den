---
description: Commit the current branch, rebase it onto master, then fast-forward master to it
allowed-tools: Bash
---

Land the current feature branch into `master`. Do this in order, stopping and
reporting if any step fails (never force, never push — pushing is the user's):

1. **Commit — ONLY your own changes.** Another session may be working in this same
   directory, so NEVER `git add -A` / `git add .`. Stage only the specific files
   *you* edited in this session, by explicit path (`git add <file> <file> …`), then
   commit them with a concise message in English you derive from that diff (subject +
   a short body of what changed and why). No `Co-Authored-By` or any other Claude
   attribution line. Leave every other modified/untracked file alone — do not stage,
   stash, or revert it. If you touched nothing, skip this step.

2. **Rebase onto master.** Run `git rebase master`. If it stops on conflicts,
   **resolve them yourself** — read both sides, apply the correct merge, `git add`
   the resolved files and `git rebase --continue` — then carry on. The rebase must
   end clean. STOP and report ONLY when you genuinely cannot tell how to resolve a
   conflict (the two sides are semantically irreconcilable and picking wrong would
   lose work) — never stop just because conflicts exist.

3. **Check before you move master.** Run it on the rebased tree: that tree is exactly
   the master about to exist. It is what CI runs, minus the release build and packaging:

   ```
   cargo test --locked --workspace
   python3 -m unittest discover -s packaging/tests
   ```

   **Red → STOP**: report what failed and do NOT fast-forward — fix it, commit (step 1)
   and come back. A warning the change introduced counts as red: fix it too.

4. **Fast-forward master.** Once the rebase is clean, master fast-forwards to the
   branch tip — never a merge commit. **The one inviolable rule of this step: never
   lose data in master, under any circumstances.** Anything that could discard a
   commit or an uncommitted edit in master is forbidden — no `reset --hard`, no
   `branch -f` over a checked-out master, no `merge` that isn't `--ff-only`, no
   stash/revert/checkout of files you don't own.
   - `master` is normally checked out in a **sibling worktree** (the main checkout,
     `sik/`; the feature branches live in `sik-<branch>/`). Find its path with
     `git worktree list`, then **just try the fast-forward**:
     `git -C <path> merge --ff-only <branch>`. A dirty worktree is fine *as long as the
     merge does not touch the same files* — git applies the ff cleanly in that case
     (this happens often and is the normal happy path). This leaves you on your
     feature branch — do not switch branches here.
   - If the ff is **refused** (the worktree has uncommitted/untracked changes on
     files the merge would update — another session's work), then STOP and report
     it. Do NOT try to make it apply: never stash, revert, checkout, or force over
     those files, and never force master's ref. It is not a rebase conflict — it is
     someone else's data, and master must never lose it. The user resolves it.
   - If `master` is NOT checked out anywhere, update its ref directly with a
     fast-forward only: `git branch -f master <branch>` — but ONLY when this is a
     true fast-forward (master is an ancestor of the branch), never when it would
     drop commits.

5. **Report.** Show `git log --oneline -2` of master so the user sees the new tip.

Do NOT push. Do NOT create or switch to other branches.
