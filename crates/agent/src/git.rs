//! A task's changes against its base branch, and the operations of the
//! Changes mode (stage, commit, branches, history), using `git`. Only the
//! local repo: remotes are never read or touched.

use std::{
    collections::HashMap,
    io::Read as _,
    path::Path,
};

use anyhow::{Result, bail};
use proto::{ChangedFile, CommitInfo, GitOp, GitStatus, Response};

/// The repo's main branch: the local `master` or `main`. Only the local repo
/// counts: remotes are never looked at.
fn default_branch(dir: &Path) -> Option<String> {
    ["master", "main"]
        .into_iter()
        .find(|branch| git(dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok())
        .map(str::to_string)
}

/// Comparison point: the commit where the task branched off the main
/// branch, or `HEAD` to see only what isn't committed yet.
fn base(dir: &Path, uncommitted: bool) -> Result<(String, Option<String>)> {
    if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        // Let Git choose the empty tree's hash (SHA-1 or SHA-256).
        let empty = git(dir, &["hash-object", "-t", "tree", "--stdin"])?;
        return Ok((empty.trim().to_string(), None));
    }
    if uncommitted {
        return Ok(("HEAD".into(), Some("HEAD".into())));
    }
    let Some(branch) = default_branch(dir) else {
        return Ok(("HEAD".into(), None));
    };
    // No common commit (an orphan branch like `gh-pages`): there's nothing
    // to compare against, as when there's no main branch.
    match git(dir, &["merge-base", "HEAD", &branch]) {
        Ok(base) => Ok((base.trim().to_string(), Some(branch))),
        Err(_) => Ok(("HEAD".into(), None)),
    }
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
    for line in git(dir, &[args, &["--numstat", "-z"]].concat())?.split('\0').filter(|line| !line.is_empty()) {
        let mut parts = line.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // Binaries show up as "-".
        counts.insert(path.to_string(), (added.parse().unwrap_or(0), removed.parse().unwrap_or(0)));
    }
    let mut files = Vec::new();
    // With -z Git emits unquoted status/path pairs, even for tabs and newlines.
    let raw = git(dir, &[args, &["--name-status", "-z"]].concat())?;
    let mut records = raw.split_terminator('\0');
    while let (Some(status), Some(path)) = (records.next(), records.next()) {
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
    Ok(git(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?
        .split_terminator('\0')
        .map(|path| ChangedFile {
            path: path.to_string(),
            status: '?',
            added: untracked_lines(&dir.join(path)),
            removed: 0,
        })
        .collect())
}

/// Line counts are a decoration: never read a large dataset or an entire
/// binary just to populate the Changes panel. Keep memory bounded even for
/// a single very long line or a file that grows during the read.
const MAX_COUNT_BYTES: u64 = 1024 * 1024;

fn untracked_lines(path: &Path) -> u32 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else { return 0 };
    if !metadata.is_file() || metadata.len() > MAX_COUNT_BYTES {
        return 0;
    }
    let Ok(mut file) = std::fs::File::open(path) else { return 0 };
    let mut buffer = [0; 8192];
    let (mut bytes, mut lines, mut last) = (0, 0, None);
    loop {
        let n = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return 0,
        };
        bytes += n as u64;
        if bytes > MAX_COUNT_BYTES || buffer[..n].contains(&0) {
            return 0;
        }
        lines += buffer[..n].iter().filter(|&&byte| byte == b'\n').count() as u32;
        last = Some(buffer[n - 1]);
    }
    lines + u32::from(last.is_some_and(|byte| byte != b'\n'))
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
        GitOp::Branches => {
            let current = git(dir, &["branch", "--show-current"])?.trim().to_string();
            let branches = git(dir, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?
                .lines()
                .map(str::to_string)
                .collect();
            Ok(Response::Branches {
                current: (!current.is_empty()).then_some(current),
                branches,
            })
        }
        GitOp::Init => {
            if git(dir, &["rev-parse", "--git-dir"]).is_ok() {
                bail!("{} is already in a git repo", dir.display());
            }
            git(dir, &["init"])?;
            Ok(Response::Ok)
        }
        GitOp::Switch { branch } => {
            git(dir, &["switch", &branch])?;
            Ok(Response::Ok)
        }
        GitOp::Log { skip, limit } => Ok(Response::Commits(log(dir, skip, limit, None)?)),
        GitOp::FileLog { file, skip, limit } => Ok(Response::Commits(log(dir, skip, limit, Some(&file))?)),
        GitOp::Unmerged { limit } => Ok(Response::Commits(unmerged(dir, limit)?)),
        GitOp::CommitFiles { commit } => {
            let files = changed(dir, &["diff-tree", "-r", "--root", "-m", "--first-parent", "--no-commit-id", "--no-renames", &commit])?;
            Ok(Response::Changes { base: None, files })
        }
        GitOp::CommitDiff { commit, file } => Ok(Response::Text(commit_diff(dir, &commit, &file, None)?)),
        GitOp::WholeDiff { file, commit: Some(commit), .. } => {
            Ok(Response::Text(commit_diff(dir, &commit, &file, Some(WHOLE_FILE))?))
        }
        GitOp::WholeDiff { file, commit: None, uncommitted } => {
            Ok(Response::Text(diff_with(dir, &file, uncommitted, Some(WHOLE_FILE))?))
        }
        GitOp::Show { commit } => Ok(Response::Text(git(
            dir,
            &["show", "--no-color", "--format=fuller", "--stat", "--patch", "-m", "--first-parent", "--no-renames", &commit],
        )?)),
        GitOp::FileAt { commit, file } => Ok(Response::Text(
            git(dir, &["show", &format!("{commit}:{file}")]).or_else(|_| git(dir, &["show", &format!("{commit}^:{file}")]))?,
        )),
        GitOp::Search { query, skip, limit } => Ok(Response::Commits(search(dir, &query, skip, limit)?)),
        GitOp::Blame { file } => blame(dir, &file),
    }
}

/// Context for a diff that shows the whole file.
const WHOLE_FILE: &str = "--unified=100000000";

/// What a commit changed in `file`, against its first parent.
fn commit_diff(dir: &Path, commit: &str, file: &str, context: Option<&str>) -> Result<String> {
    let mut args = vec!["diff-tree", "-p", "-r", "--root", "-m", "--first-parent", "--no-commit-id", "--no-renames"];
    args.extend(context);
    args.extend([commit, "--", file]);
    git(dir, &args)
}

fn strs(files: &[String]) -> Vec<&str> {
    files.iter().map(String::as_str).collect()
}

/// `git status` in machine-readable format: branch, and what is staged
/// and what isn't.
fn status(dir: &Path) -> Result<GitStatus> {
    let mut status = GitStatus::default();
    let raw = git(dir, &["status", "--porcelain=v2", "--branch", "--no-renames", "--untracked-files=all", "-z"])?;
    let (mut staged, mut unstaged) = (Vec::new(), Vec::new());
    for entry in raw.split('\0').filter(|entry| !entry.is_empty()) {
        if let Some(header) = entry.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.head" if value != "(detached)" => status.branch = Some(value.to_string()),
                _ => {}
            }
            continue;
        }
        // Only split the fixed metadata fields: the final path can contain spaces.
        let field_count = if entry.starts_with("u ") { 11 } else { 9 };
        let fields: Vec<&str> = entry.splitn(field_count, ' ').collect();
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

const LOG_FORMAT: &str = "--format=%H%x1f%h%x1f%an%x1f%at%x1f%D%x1f%s%x1f%b%x1e";

/// The history from `HEAD`, or only the commits that changed `file`.
/// The commits of `HEAD` the main branch doesn't have: none without one.
fn unmerged(dir: &Path, limit: usize) -> Result<Vec<CommitInfo>> {
    let Some(branch) = default_branch(dir) else {
        return Ok(Vec::new());
    };
    let (limit, range) = (format!("--max-count={limit}"), format!("{branch}..HEAD"));
    let raw = git(dir, &["log", LOG_FORMAT, &limit, &range])?;
    Ok(commits(&raw).map(|(commit, _)| commit).collect())
}

fn log(dir: &Path, skip: usize, limit: usize, file: Option<&str>) -> Result<Vec<CommitInfo>> {
    // With no commits yet there's no history (and `git log` fails).
    if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let (skip, limit) = (format!("--skip={skip}"), format!("--max-count={limit}"));
    let mut args = vec!["log", "--decorate-refs=refs/heads", "--decorate-refs=refs/tags", LOG_FORMAT, &skip, &limit, "HEAD"];
    if let Some(file) = file {
        args.extend(["--", file]);
    }
    let raw = git(dir, &args)?;
    Ok(commits(&raw).map(|(commit, _)| commit).collect())
}

/// Reads the whole local history and filters it here: a single `git log`
/// can't match the hash, or the message or the author, and it's fast (tens
/// of thousands of commits in a fraction of a second).
fn search(dir: &Path, query: &str, skip: usize, limit: usize) -> Result<Vec<CommitInfo>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(Vec::new());
    }
    let raw = git(
        dir,
        &["log", "--decorate-refs=refs/heads", "--decorate-refs=refs/tags", LOG_FORMAT, "HEAD", "--branches", "--tags"],
    )?;
    Ok(commits(&raw)
        .filter(|(commit, body)| {
            let text = format!("{}\n{}\n{}", commit.subject, body, commit.author).to_lowercase();
            words.iter().all(|word| commit.hash.starts_with(word.as_str()) || text.contains(word.as_str()))
        })
        .skip(skip)
        .take(limit)
        .map(|(commit, _)| commit)
        .collect())
}

/// The commits of a `git log` in `LOG_FORMAT`, each with its message body.
fn commits(raw: &str) -> impl Iterator<Item = (CommitInfo, &str)> {
    raw.split('\x1e').filter_map(|record| {
        let mut fields = record.trim_start_matches('\n').split('\x1f');
        let commit = CommitInfo {
            hash: fields.next().filter(|hash| !hash.is_empty())?.to_string(),
            short: fields.next()?.to_string(),
            author: fields.next()?.to_string(),
            time: fields.next()?.parse().unwrap_or(0),
            refs: fields.next()?.to_string(),
            subject: fields.next()?.to_string(),
        };
        Some((commit, fields.next().unwrap_or("")))
    })
}

/// `git blame` of the file on disk, with its lines not yet committed as `None`.
fn blame(dir: &Path, file: &str) -> Result<Response> {
    let raw = git(dir, &["blame", "--porcelain", "--", file])?;
    let mut commits: Vec<CommitInfo> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut lines = Vec::new();
    let mut current: Option<usize> = None;
    for line in raw.lines() {
        if line.starts_with('\t') {
            let ix = current.expect("a blame header comes before each line");
            let uncommitted = commits[ix].hash.bytes().all(|byte| byte == b'0');
            lines.push((!uncommitted).then_some(ix as u32));
            continue;
        }
        let Some(ix) = current.filter(|_| !is_blame_header(line)) else {
            let hash = line.split(' ').next().unwrap_or_default().to_string();
            let ix = *index.entry(hash.clone()).or_insert_with(|| {
                commits.push(CommitInfo {
                    short: hash[..hash.len().min(10)].to_string(),
                    hash,
                    author: String::new(),
                    time: 0,
                    refs: String::new(),
                    subject: String::new(),
                });
                commits.len() - 1
            });
            current = Some(ix);
            continue;
        };
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "author" => commits[ix].author = value.to_string(),
            "author-time" => commits[ix].time = value.parse().unwrap_or(0),
            "summary" => commits[ix].subject = value.to_string(),
            _ => {}
        }
    }
    Ok(Response::Blame { commits, lines })
}

/// `<40 hex> <orig line> <final line>[ <count>]`: the line that starts each
/// entry of `git blame --porcelain`.
fn is_blame_header(line: &str) -> bool {
    let mut parts = line.split(' ');
    parts.next().is_some_and(|hash| hash.len() >= 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && parts.next().is_some_and(|n| n.parse::<u32>().is_ok())
}

/// Unified diff of a file, using the same criteria as `changes`.
pub fn diff(dir: &Path, file: &str, uncommitted: bool) -> Result<String> {
    diff_with(dir, file, uncommitted, None)
}

/// `diff`, with `context` (a `--unified`) if given.
fn diff_with(dir: &Path, file: &str, uncommitted: bool, context: Option<&str>) -> Result<String> {
    let (base, _) = base(dir, uncommitted)?;
    let tracked = git(dir, &["ls-files", "--error-unmatch", "--", file]).is_ok()
        || git(dir, &["cat-file", "-e", &format!("{base}:{file}")]).is_ok();
    if tracked {
        let mut args = vec!["diff", "--no-renames"];
        args.extend(context);
        args.extend([base.as_str(), "--", file]);
        return git(dir, &args);
    }
    // Untracked: the whole file is new. `--no-index` exits with 1 when there are differences.
    let output = crate::platform::command("git")
        .args(["diff", "--no-index"])
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(context)
        .args(["--", "/dev/null", file])
        .current_dir(dir)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    // Reading never writes the index (`git status` refreshes it when it can):
    // the index is watched, and each write would ask for another read.
    let output = crate::platform::command("git")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A file's name is the file, not a pattern: `a[12].txt` isn't `a1.txt`.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .current_dir(dir)
        .output()?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn diffs_staged_files_before_the_first_commit() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        run(dir, &["init", "-q"]);
        std::fs::write(dir.join("first.txt"), "hello\n").unwrap();
        run(dir, &["add", "first.txt"]);
        for uncommitted in [true, false] {
            assert!(diff(dir, "first.txt", uncommitted).unwrap().contains("+hello"));
            let (_, files) = changes(dir, uncommitted).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!((files[0].path.as_str(), files[0].added), ("first.txt", 1));
        }
        std::fs::write(dir.join("first.txt"), "hello\nworld\n").unwrap();
        assert!(diff(dir, "first.txt", true).unwrap().contains("+world"));
    }

    #[test]
    fn untracked_counts_skip_large_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        for (text, count) in [("", 0), ("a", 1), ("a\nb\n", 2), ("a\r\nb", 2), ("a\0b\n", 0)] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(untracked_lines(&path), count);
        }
        let text = format!("{}\nlast", "x".repeat(9000));
        std::fs::write(&path, text).unwrap();
        assert_eq!(untracked_lines(&path), 2);
        std::fs::File::create(&path).unwrap().set_len(MAX_COUNT_BYTES + 1).unwrap();
        assert_eq!(untracked_lines(&path), 0);
        assert_eq!(untracked_lines(dir.path()), 0);
    }

    fn run(dir: &Path, args: &[&str]) {
        let output = crate::platform::command("git").args(args).current_dir(dir).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }

    fn commit(dir: &Path, message: &str) {
        run(dir, &["add", "-A"]);
        run(dir, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "-m", message]);
    }

    fn repo() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("den-git-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "master"]);
        run(&dir, &["config", "core.autocrlf", "false"]);
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

    #[test]
    fn a_branch_unrelated_to_master_compares_against_head() {
        let dir = repo();
        run(&dir, &["switch", "-q", "--orphan", "pages"]);
        std::fs::write(dir.join("index.html"), "hi\n").unwrap();
        commit(&dir, "pages");
        std::fs::write(dir.join("index.html"), "hi\nthere\n").unwrap();
        let (base, files) = changes(&dir, false).unwrap();
        assert_eq!(base, None);
        assert_eq!(paths(&files), [("index.html", 'M')]);
        assert!(diff(&dir, "index.html", false).unwrap().contains("+there"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmerged_commits_are_those_the_main_branch_lacks() {
        let dir = repo();
        let unmerged = |dir: &Path| match run_op(dir, GitOp::Unmerged { limit: 10 }) {
            Response::Commits(commits) => commits.into_iter().map(|c| c.subject).collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        assert!(unmerged(&dir).is_empty());
        run(&dir, &["switch", "-q", "-c", "task"]);
        std::fs::write(dir.join("a.txt"), "changed\n").unwrap();
        commit(&dir, "on the branch");
        assert_eq!(unmerged(&dir), ["on the branch"]);
        // Merged into master, it's no longer pending.
        run(&dir, &["switch", "-q", "master"]);
        run(&dir, &["merge", "-q", "--ff-only", "task"]);
        run(&dir, &["switch", "-q", "task"]);
        assert!(unmerged(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn paths(files: &[ChangedFile]) -> Vec<(&str, char)> {
        files.iter().map(|f| (f.path.as_str(), f.status)).collect()
    }

    #[test]
    fn status_preserves_spaces_in_paths() {
        let dir = repo();
        std::fs::create_dir(dir.join("my folder")).unwrap();
        let file = "my folder/my file.txt";
        std::fs::write(dir.join(file), "original\n").unwrap();
        commit(&dir, "file with spaces");
        std::fs::write(dir.join(file), "staged\n").unwrap();
        run_op(&dir, GitOp::Stage { files: vec![file.into()] });
        std::fs::write(dir.join(file), "unstaged\n").unwrap();
        let Response::GitStatus(status) = run_op(&dir, GitOp::Status) else { panic!() };
        assert_eq!(paths(&status.staged), vec![(file, 'M')]);
        assert_eq!(paths(&status.unstaged), vec![(file, 'M')]);
        assert_eq!((status.staged[0].added, status.staged[0].removed), (1, 1));
        assert_eq!((status.unstaged[0].added, status.unstaged[0].removed), (1, 1));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn git_operations_preserve_special_paths() {
        let dir = repo();
        run(&dir, &["config", "core.quotePath", "true"]);
        let mut names = vec!["niño.txt", "space name.txt", "-dash.txt"];
        // Windows forbids quotes and control characters in filenames.
        #[cfg(unix)]
        names.extend(["quote\".txt", "tab\tname.txt", "line\nname.txt"]);
        names.sort();
        for file in &names {
            std::fs::write(dir.join(file), "original\n").unwrap();
        }
        let (_, files) = changes(&dir, true).unwrap();
        assert_eq!(paths(&files), names.iter().map(|file| (*file, '?')).collect::<Vec<_>>());
        assert!(files.iter().all(|file| (file.added, file.removed) == (1, 0)));

        run_op(&dir, GitOp::Stage { files: names.iter().map(|name| name.to_string()).collect() });
        let (_, files) = changes(&dir, true).unwrap();
        assert_eq!(paths(&files), names.iter().map(|file| (*file, 'A')).collect::<Vec<_>>());
        assert!(files.iter().all(|file| (file.added, file.removed) == (1, 0)));
        commit(&dir, "special paths");
        let Response::Changes { files, .. } = run_op(&dir, GitOp::CommitFiles { commit: "HEAD".into() }) else { panic!() };
        assert_eq!(paths(&files), names.iter().map(|file| (*file, 'A')).collect::<Vec<_>>());

        for file in &names {
            std::fs::write(dir.join(file), "changed\n").unwrap();
            assert!(diff(&dir, file, true).unwrap().contains("+changed"));
        }
        let (_, files) = changes(&dir, true).unwrap();
        assert_eq!(paths(&files), names.iter().map(|file| (*file, 'M')).collect::<Vec<_>>());
        assert!(files.iter().all(|file| (file.added, file.removed) == (1, 1)));
        run_op(&dir, GitOp::Discard { files: names.iter().map(|name| name.to_string()).collect() });
        for file in &names {
            assert_eq!(std::fs::read_to_string(dir.join(file)).unwrap(), "original\n");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_name_with_glob_characters_is_only_that_file() {
        let dir = repo();
        for file in ["a[12].txt", "a1.txt"] {
            std::fs::write(dir.join(file), "original\n").unwrap();
        }
        commit(&dir, "files");
        for file in ["a[12].txt", "a1.txt"] {
            std::fs::write(dir.join(file), "changed\n").unwrap();
        }
        run_op(&dir, GitOp::Stage { files: vec!["a[12].txt".into()] });
        let status = status(&dir).unwrap();
        assert_eq!(status.staged.iter().map(|file| file.path.as_str()).collect::<Vec<_>>(), ["a[12].txt"]);
        run_op(&dir, GitOp::Unstage { files: vec!["a[12].txt".into()] });
        run_op(&dir, GitOp::Discard { files: vec!["a[12].txt".into()] });
        assert_eq!(std::fs::read_to_string(dir.join("a[12].txt")).unwrap(), "original\n");
        assert_eq!(std::fs::read_to_string(dir.join("a1.txt")).unwrap(), "changed\n");
        std::fs::remove_dir_all(dir).unwrap();
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
        let whole = |commit: Option<String>, file: &str| -> String {
            let Response::Text(diff) = run_op(&dir, GitOp::WholeDiff { file: file.into(), commit, uncommitted: true }) else {
                panic!()
            };
            diff
        };
        assert!(whole(Some(commits[0].hash.clone()), "a.txt").contains("@@ -1,2 +1,3 @@\n one\n two\n+three"));
        assert!(whole(None, "sub/new.txt").contains("@@ -0,0 +1 @@\n+x"));
        let Response::Commits(history) = run_op(&dir, GitOp::FileLog { file: "a.txt".into(), skip: 0, limit: 10 }) else {
            panic!()
        };
        assert_eq!(history.len(), 2);
        let Response::Commits(history) = run_op(&dir, GitOp::FileLog { file: "b.txt".into(), skip: 0, limit: 10 }) else {
            panic!()
        };
        assert_eq!(history.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), vec!["initial"]);

        let search = |query: &str| -> Vec<String> {
            let Response::Commits(commits) = run_op(&dir, GitOp::Search { query: query.into(), skip: 0, limit: 10 }) else {
                panic!()
            };
            commits.into_iter().map(|c| c.subject).collect()
        };
        assert_eq!(search("THREE"), vec!["three"]);
        assert_eq!(search(&commits[1].short), vec!["initial"]);
        assert_eq!(search("three a"), vec!["three"]);
        assert!(search("three initial").is_empty());
        let Response::Text(show) = run_op(&dir, GitOp::Show { commit: commits[0].hash.clone() }) else { panic!() };
        assert!(show.contains("three") && show.contains("+three"));
        let Response::Text(old) = run_op(&dir, GitOp::FileAt { commit: commits[1].hash.clone(), file: "a.txt".into() }) else {
            panic!()
        };
        assert_eq!(old, "one\ntwo\n");

        std::fs::write(dir.join("a.txt"), "one\nchanged\nthree\n").unwrap();
        let Response::Blame { commits: blamed, lines } = run_op(&dir, GitOp::Blame { file: "a.txt".into() }) else {
            panic!()
        };
        let subjects: Vec<Option<&str>> =
            lines.iter().map(|line| line.map(|ix| blamed[ix as usize].subject.as_str())).collect();
        assert_eq!(subjects, vec![Some("initial"), None, Some("three")]);
        assert_eq!(blamed.iter().find(|c| c.subject == "three").unwrap().author, "a");
        run(&dir, &["checkout", "-q", "--", "a.txt"]);

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
