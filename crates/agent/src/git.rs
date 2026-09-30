//! A task's changes against its base branch, and the operations of the
//! Changes mode (stage, commit, push, branches, history), using `git`.

use std::{
    collections::HashMap,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Result, bail};
use proto::{ChangedFile, CommitInfo, GitOp, GitStatus, Response};

/// The repo's main branch: `origin/HEAD`'s, or else `master` or `main`.
fn default_branch(dir: &Path) -> Option<String> {
    if let Ok(head) = git(dir, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        let head = head.trim();
        if !head.is_empty() {
            return Some(head.to_string());
        }
    }
    ["master", "main"]
        .into_iter()
        .find(|branch| git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok())
        .map(str::to_string)
}

/// Comparison point: the commit where the task branched off the main
/// branch, or `HEAD` to see only what isn't committed yet.
fn base(dir: &Path, uncommitted: bool) -> Result<(String, Option<String>)> {
    if uncommitted {
        return Ok(("HEAD".into(), Some("HEAD".into())));
    }
    let Some(branch) = default_branch(dir) else {
        return Ok(("HEAD".into(), None));
    };
    let base = git(dir, &["merge-base", "HEAD", &branch])?.trim().to_string();
    Ok((base, Some(branch)))
}

pub fn changes(dir: &Path, uncommitted: bool) -> Result<(Option<String>, Vec<ChangedFile>)> {
    let (base, label) = base(dir, uncommitted)?;
    let mut files = changed(dir, &["diff", "--no-renames", &base])?;
    files.extend(untracked(dir)?);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((label, files))
}

/// Files from a `git diff` (or `diff-tree`) with their added and removed
/// lines: `args` is the command without `--numstat` or `--name-status`.
fn changed(dir: &Path, args: &[&str]) -> Result<Vec<ChangedFile>> {
    let mut counts: HashMap<String, (u32, u32)> = HashMap::new();
    for line in git(dir, &[args, &["--numstat"]].concat())?.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // Binaries show up as "-".
        counts.insert(path.to_string(), (added.parse().unwrap_or(0), removed.parse().unwrap_or(0)));
    }
    let mut files = Vec::new();
    for line in git(dir, &[args, &["--name-status"]].concat())?.lines() {
        let Some((status, path)) = line.split_once('\t') else {
            continue;
        };
        let (added, removed) = counts.get(path).copied().unwrap_or_default();
        files.push(ChangedFile {
            path: path.to_string(),
            status: status.chars().next().unwrap_or('M'),
            added,
            removed,
        });
    }
    Ok(files)
}

/// Untracked files (minus ignored ones); all their lines are new.
fn untracked(dir: &Path) -> Result<Vec<ChangedFile>> {
    Ok(git(dir, &["ls-files", "--others", "--exclude-standard"])?
        .lines()
        .map(|path| ChangedFile {
            path: path.to_string(),
            status: '?',
            added: std::fs::read_to_string(dir.join(path)).map_or(0, |text| text.lines().count() as u32),
            removed: 0,
        })
        .collect())
}

pub fn run(dir: &Path, op: GitOp) -> Result<Response> {
    match op {
        GitOp::Status => Ok(Response::GitStatus(status(dir)?)),
        GitOp::Stage { files } => {
            git(dir, &[&["add", "-A", "--"], strs(&files).as_slice()].concat())?;
            Ok(Response::Ok)
        }
        GitOp::Unstage { files } => {
            // With no commits yet there's no `HEAD` to go back to.
            if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok() {
                git(dir, &[&["restore", "--staged", "--"], strs(&files).as_slice()].concat())?;
            } else {
                git(dir, &[&["rm", "--cached", "-r", "-q", "--"], strs(&files).as_slice()].concat())?;
            }
            Ok(Response::Ok)
        }
        GitOp::Discard { files } => {
            let untracked: Vec<String> = untracked(dir)?.into_iter().map(|file| file.path).collect();
            let (new, tracked): (Vec<&String>, Vec<&String>) = files.iter().partition(|file| untracked.contains(file));
            if !tracked.is_empty() {
                let tracked: Vec<&str> = tracked.iter().map(|file| file.as_str()).collect();
                git(dir, &[&["restore", "--worktree", "--"], tracked.as_slice()].concat())?;
            }
            for file in new {
                crate::fs::trash(&dir.join(file))?;
            }
            Ok(Response::Ok)
        }
        GitOp::Commit { message, all } => {
            if message.trim().is_empty() {
                bail!("The commit message is missing");
            }
            if all {
                git(dir, &["add", "-A"])?;
            }
            git(dir, &["commit", "-m", &message])?;
            Ok(Response::Ok)
        }
        GitOp::Push => {
            let status = status(dir)?;
            match (&status.upstream, &status.branch) {
                (Some(_), _) => remote(dir, &["push"])?,
                // First push of the branch: it gets linked to the remote one.
                (None, Some(branch)) => remote(dir, &["push", "-u", "origin", branch])?,
                (None, None) => bail!("There is no branch to push (detached HEAD)"),
            };
            Ok(Response::Ok)
        }
        GitOp::Pull => {
            remote(dir, &["pull", "--ff-only"])?;
            Ok(Response::Ok)
        }
        GitOp::Branches => {
            let current = git(dir, &["branch", "--show-current"])?.trim().to_string();
            let remotes = git(dir, &["remote"])?;
            let branches = git(dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads", "refs/remotes"])?
                .lines()
                // `refs/remotes/origin/HEAD` shows up as plain `origin`.
                .filter(|branch| !branch.ends_with("/HEAD") && !remotes.lines().any(|remote| remote == *branch))
                .map(str::to_string)
                .collect();
            Ok(Response::Branches {
                current: (!current.is_empty()).then_some(current),
                branches,
            })
        }
        GitOp::Switch { branch } => {
            // `git switch x` with only `origin/x` creates the local branch tracking it.
            let local = match branch.split_once('/') {
                Some((remote, name))
                    if is_remote(dir, remote)
                        && git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_err() =>
                {
                    name
                }
                _ => branch.as_str(),
            };
            git(dir, &["switch", local])?;
            Ok(Response::Ok)
        }
        GitOp::Log { skip, limit } => Ok(Response::Commits(log(dir, skip, limit)?)),
        GitOp::CommitFiles { commit } => {
            let files = changed(dir, &["diff-tree", "-r", "--root", "-m", "--first-parent", "--no-commit-id", "--no-renames", &commit])?;
            Ok(Response::Changes { base: None, files })
        }
        GitOp::CommitDiff { commit, file } => Ok(Response::Text(git(
            dir,
            &["diff-tree", "-p", "-r", "--root", "-m", "--first-parent", "--no-commit-id", "--no-renames", &commit, "--", &file],
        )?)),
    }
}

fn strs(files: &[String]) -> Vec<&str> {
    files.iter().map(String::as_str).collect()
}

fn is_remote(dir: &Path, name: &str) -> bool {
    git(dir, &["remote"]).is_ok_and(|remotes| remotes.lines().any(|remote| remote == name))
}

/// `git status` in machine-readable format: branch, distance from the
/// remote one, and what is staged and what isn't.
fn status(dir: &Path) -> Result<GitStatus> {
    let mut status = GitStatus::default();
    let raw = git(dir, &["status", "--porcelain=v2", "--branch", "--no-renames", "--untracked-files=all", "-z"])?;
    let (mut staged, mut unstaged) = (Vec::new(), Vec::new());
    for entry in raw.split('\0').filter(|entry| !entry.is_empty()) {
        if let Some(header) = entry.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.head" if value != "(detached)" => status.branch = Some(value.to_string()),
                "branch.upstream" => status.upstream = Some(value.to_string()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            status.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let fields: Vec<&str> = entry.splitn(11, ' ').collect();
        match fields[0] {
            "?" => unstaged.push((entry[2..].to_string(), '?')),
            // Ordinary change: `1 XY sub mH mI mW hH hI path`.
            "1" if fields.len() == 9 => {
                let mut xy = fields[1].chars();
                let (x, y) = (xy.next().unwrap_or('.'), xy.next().unwrap_or('.'));
                if x != '.' {
                    staged.push((fields[8].to_string(), x));
                }
                if y != '.' {
                    unstaged.push((fields[8].to_string(), y));
                }
            }
            // Conflicted: `u XY sub m1 m2 m3 mW h1 h2 h3 path`.
            "u" if fields.len() == 11 => unstaged.push((fields[10].to_string(), 'U')),
            _ => {}
        }
    }
    let counts = |args: &[&str]| -> HashMap<String, (u32, u32)> {
        changed(dir, args)
            .unwrap_or_default()
            .into_iter()
            .map(|file| (file.path, (file.added, file.removed)))
            .collect()
    };
    let staged_counts = counts(&["diff", "--cached", "--no-renames"]);
    let unstaged_counts = counts(&["diff", "--no-renames"]);
    let untracked_counts: HashMap<String, (u32, u32)> = untracked(dir)?
        .into_iter()
        .map(|file| (file.path, (file.added, 0)))
        .collect();
    let build = |files: Vec<(String, char)>, counts: &HashMap<String, (u32, u32)>| -> Vec<ChangedFile> {
        let mut files: Vec<ChangedFile> = files
            .into_iter()
            .map(|(path, status)| {
                let (added, removed) = counts.get(&path).or_else(|| untracked_counts.get(&path)).copied().unwrap_or_default();
                ChangedFile { path, status, added, removed }
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    };
    status.staged = build(staged, &staged_counts);
    status.unstaged = build(unstaged, &unstaged_counts);
    Ok(status)
}

fn log(dir: &Path, skip: usize, limit: usize) -> Result<Vec<CommitInfo>> {
    // With no commits yet there's no history (and `git log` fails).
    if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let raw = git(
        dir,
        &[
            "log",
            "--format=%H%x1f%h%x1f%an%x1f%at%x1f%D%x1f%s%x1e",
            &format!("--skip={skip}"),
            &format!("--max-count={limit}"),
            "HEAD",
        ],
    )?;
    Ok(raw
        .split('\x1e')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split('\x1f');
            Some(CommitInfo {
                hash: fields.next().filter(|hash| !hash.is_empty())?.to_string(),
                short: fields.next()?.to_string(),
                author: fields.next()?.to_string(),
                time: fields.next()?.parse().unwrap_or(0),
                refs: fields.next()?.to_string(),
                subject: fields.next()?.to_string(),
            })
        })
        .collect())
}

/// Push and pull: they never sit waiting for a password or passphrase that
/// nobody will type (the agent has no terminal).
fn remote(dir: &Path, args: &[&str]) -> Result<String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    let output = command.output()?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Unified diff of a file, using the same criteria as `changes`.
pub fn diff(dir: &Path, file: &str, uncommitted: bool) -> Result<String> {
    let (base, _) = base(dir, uncommitted)?;
    let tracked = git(dir, &["ls-files", "--error-unmatch", "--", file]).is_ok()
        || git(dir, &["cat-file", "-e", &format!("{base}:{file}")]).is_ok();
    if tracked {
        return git(dir, &["diff", "--no-renames", &base, "--", file]);
    }
    // Untracked: the whole file is new. `--no-index` exits with 1 when there are differences.
    let output = Command::new("git")
        .args(["diff", "--no-index", "--", "/dev/null", file])
        .current_dir(dir)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git").args(args).current_dir(dir).output()?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let output = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    fn commit(dir: &Path, message: &str) {
        run(dir, &["add", "-A"]);
        run(dir, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "-m", message]);
    }

    fn repo() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sik-git-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "master"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.log\n").unwrap();
        commit(&dir, "initial");
        dir
    }

    #[test]
    fn changes_against_base_and_uncommitted() {
        let dir = repo();
        run(&dir, &["switch", "-q", "-c", "task"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        commit(&dir, "on the branch");
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::write(dir.join("new.txt"), "x\ny\n").unwrap();
        std::fs::write(dir.join("ignored.log"), "x\n").unwrap();

        let (base, files) = changes(&dir, false).unwrap();
        assert_eq!(base.as_deref(), Some("master"));
        let summary: Vec<(String, char, u32, u32)> =
            files.iter().map(|f| (f.path.clone(), f.status, f.added, f.removed)).collect();
        assert_eq!(
            summary,
            vec![
                ("a.txt".into(), 'M', 1, 0),
                ("b.txt".into(), 'D', 0, 1),
                ("new.txt".into(), '?', 2, 0),
            ]
        );

        // Uncommitted: a.txt is already committed on the branch, so it doesn't show up.
        let (_, files) = changes(&dir, true).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["b.txt", "new.txt"]);

        assert!(diff(&dir, "a.txt", false).unwrap().contains("+three"));
        assert!(diff(&dir, "new.txt", false).unwrap().contains("+x"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn paths(files: &[ChangedFile]) -> Vec<(&str, char)> {
        files.iter().map(|f| (f.path.as_str(), f.status)).collect()
    }

    #[test]
    fn stage_commit_log_and_switch() {
        let dir = repo();
        run(&dir, &["config", "user.email", "a@b"]);
        run(&dir, &["config", "user.name", "a"]);
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/new.txt"), "x\n").unwrap();

        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert_eq!(status.branch.as_deref(), Some("master"));
        assert!(status.staged.is_empty());
        assert_eq!(paths(&status.unstaged), vec![("a.txt", 'M'), ("b.txt", 'D'), ("sub/new.txt", '?')]);
        assert_eq!((status.unstaged[0].added, status.unstaged[0].removed), (1, 0));

        run_op(&dir, GitOp::Stage { files: vec!["a.txt".into(), "b.txt".into()] });
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert_eq!(paths(&status.staged), vec![("a.txt", 'M'), ("b.txt", 'D')]);
        assert_eq!(paths(&status.unstaged), vec![("sub/new.txt", '?')]);

        run_op(&dir, GitOp::Unstage { files: vec!["b.txt".into()] });
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert_eq!(paths(&status.staged), vec![("a.txt", 'M')]);

        run_op(&dir, GitOp::Discard { files: vec!["b.txt".into()] });
        assert!(dir.join("b.txt").exists());

        run_op(&dir, GitOp::Commit { message: "three".into(), all: false });
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert!(status.staged.is_empty());
        assert_eq!(paths(&status.unstaged), vec![("sub/new.txt", '?')]);

        let Response::Commits(commits) = run_op(&dir, GitOp::Log { skip: 0, limit: 10 }) else { panic!() };
        let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["three", "initial"]);
        let Response::Changes { files, .. } = run_op(&dir, GitOp::CommitFiles { commit: commits[0].hash.clone() }) else {
            panic!()
        };
        assert_eq!(paths(&files), vec![("a.txt", 'M')]);
        let Response::Changes { files, .. } = run_op(&dir, GitOp::CommitFiles { commit: commits[1].hash.clone() }) else {
            panic!()
        };
        assert_eq!(files.len(), 3);
        let Response::Text(diff) = run_op(&dir, GitOp::CommitDiff { commit: commits[0].hash.clone(), file: "a.txt".into() })
        else {
            panic!()
        };
        assert!(diff.contains("+three"));

        run(&dir, &["branch", "other"]);
        let Response::Branches { current, branches } = run_op(&dir, GitOp::Branches) else { panic!() };
        assert_eq!(current.as_deref(), Some("master"));
        assert_eq!(branches, vec!["master", "other"]);
        run_op(&dir, GitOp::Switch { branch: "other".into() });
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert_eq!(status.branch.as_deref(), Some("other"));

        run_op(&dir, GitOp::Commit { message: "everything".into(), all: true });
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert!(status.staged.is_empty() && status.unstaged.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn run_op(dir: &Path, op: GitOp) -> Response {
        super::run(dir, op).unwrap()
    }
}
