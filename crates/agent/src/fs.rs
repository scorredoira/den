//! Files on the agent's machine: the UI never touches the disk, so it works
//! the same locally as on a server over SSH.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use notify::{RecursiveMode, Watcher as _};
use proto::DirEntryInfo;

/// Names that are never listed, on top of what git ignores.
const HIDDEN: &[&str] = &[".git", ".DS_Store"];

/// Maximum size of a file sent to the UI.
const MAX_READ: u64 = 64 * 1024 * 1024;

/// How long changes accumulate before being sent together.
const DEBOUNCE: Duration = Duration::from_millis(100);

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_READ {
        bail!("the file is too large ({} MB)", size / 1024 / 1024);
    }
    Ok(std::fs::read(path)?)
}

pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    std::fs::write(path, data).with_context(|| format!("could not write {}", path.display()))
}

/// A folder, honoring `.gitignore` (including those of parent folders):
/// folders first and then files, by name.
pub fn list(dir: &Path) -> Result<Vec<DirEntryInfo>> {
    if !dir.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    let walk = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| !HIDDEN.contains(&entry.file_name().to_string_lossy().as_ref()))
        .build();
    let mut entries: Vec<DirEntryInfo> = walk
        .filter_map(Result::ok)
        .filter(|entry| entry.depth() == 1)
        .map(|entry| {
            let path = entry.into_path();
            // Follows symlinks to find out whether they point to a folder.
            let is_dir = std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
            DirEntryInfo {
                name: path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                is_dir,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        bail!("{} already exists", to.display());
    }
    Ok(std::fs::rename(from, to)?)
}

pub fn create_file(path: &Path) -> Result<()> {
    std::fs::File::create_new(path).with_context(|| format!("could not create {}", path.display()))?;
    Ok(())
}

pub fn create_dir(path: &Path) -> Result<()> {
    std::fs::create_dir(path).with_context(|| format!("could not create {}", path.display()))
}

/// To the Trash (recoverable), not deleted.
pub fn trash(path: &Path) -> Result<()> {
    trash::delete(path).map_err(|err| anyhow::anyhow!("could not move to the Trash: {err}"))
}

/// Watches `root` and calls `changed` with batches of changed paths until
/// the returned value is dropped.
/// Nothing git ignores (logs, temp files, `target/`…) is reported, nor what
/// changes inside `.git`. With `git`, a commit, checkout or reset (which move
/// `HEAD`) is reported as `root/.git`, also in a worktree, whose git folder is
/// elsewhere.
pub fn watch(root: &Path, git: bool, changed: impl Fn(Vec<PathBuf>) + Send + 'static) -> Result<notify::RecommendedWatcher> {
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let ignored = Ignored::new(root);
    let git_dir = git.then(|| git_dir(root)).flatten();
    let marker = root.join(".git");
    let watched_git = git_dir.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // Opening or reading isn't a change (on Linux inotify reports it): git
        // reading the tree would otherwise trigger a refresh that reads it again.
        if let Ok(event) = event
            && !event.kind.is_access()
        {
            for path in event.paths {
                if let Some(git_dir) = &watched_git
                    && path.starts_with(git_dir)
                {
                    if is_head_log(git_dir, &path) {
                        let _ = tx.send(marker.clone());
                    }
                } else if !ignored.matches(&path) {
                    let _ = tx.send(path);
                }
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    // `logs/HEAD` gets a line on every commit, checkout or reset.
    if let Some(logs) = git_dir.map(|dir| dir.join("logs"))
        && logs.is_dir()
    {
        watcher.watch(&logs, RecursiveMode::NonRecursive)?;
    }
    std::thread::spawn(move || {
        // Ends when the watcher (and with it the sender) is dropped.
        while let Ok(first) = rx.recv() {
            let mut batch = HashSet::from([first]);
            std::thread::sleep(DEBOUNCE);
            batch.extend(rx.try_iter());
            changed(batch.into_iter().collect());
        }
    });
    Ok(watcher)
}

/// The repo's own git folder: `root/.git`, or in a worktree the one git
/// keeps for it inside the main repo.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()))
        .and_then(|dir| dir.canonicalize().ok())
}

fn is_head_log(git_dir: &Path, path: &Path) -> bool {
    path.strip_prefix(git_dir).is_ok_and(|rest| rest == Path::new("logs/HEAD"))
}

/// What git ignores inside a folder: its `.gitignore` and those of the
/// folders within it (read when watching starts), plus `.git`.
struct Ignored {
    root: PathBuf,
    matchers: Vec<ignore::gitignore::Gitignore>,
}

impl Ignored {
    fn new(root: &Path) -> Self {
        let matchers = ignore::WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() == ".gitignore")
            .filter_map(|entry| {
                let (matcher, _) = ignore::gitignore::Gitignore::new(entry.path());
                (!matcher.is_empty()).then_some(matcher)
            })
            .collect();
        Self {
            root: root.to_path_buf(),
            matchers,
        }
    }

    fn matches(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.components().any(|part| part.as_os_str() == ".git") {
            return true;
        }
        let is_dir = path.is_dir();
        self.matchers.iter().any(|matcher| {
            path.starts_with(matcher.path())
                && matcher.matched_path_or_any_parents(path, is_dir).is_ignore()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sik-fs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn lists_reads_writes_and_renames() {
        let dir = dir("ops");
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();
        create_file(&dir.join("b.txt")).unwrap();
        write(&dir.join("a.txt"), b"hello").unwrap();

        let names: Vec<(String, bool)> = list(&dir).unwrap().into_iter().map(|e| (e.name, e.is_dir)).collect();
        assert_eq!(
            names,
            vec![
                ("src".into(), true),
                (".gitignore".into(), false),
                ("a.txt".into(), false),
                ("b.txt".into(), false)
            ]
        );
        assert_eq!(read(&dir.join("a.txt")).unwrap(), b"hello");
        assert!(create_file(&dir.join("a.txt")).is_err());
        rename(&dir.join("a.txt"), &dir.join("c.txt")).unwrap();
        assert!(rename(&dir.join("b.txt"), &dir.join("c.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watches_changes() {
        let dir = dir("watch");
        std::fs::write(dir.join(".gitignore"), "logs/\n").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let _watcher = watch(&dir, false, {
            let seen = seen.clone();
            move |paths| seen.lock().unwrap().extend(paths)
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs/noise.log"), "x").unwrap();
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let start = std::time::Instant::now();
        while !seen.lock().unwrap().iter().any(|path| path.ends_with("new.txt")) {
            assert!(start.elapsed() < Duration::from_secs(5), "the change never arrived");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!seen.lock().unwrap().iter().any(|path| path.starts_with(dir.join("logs"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_commits_in_a_worktree() {
        let dir = dir("git");
        let git = |dir: &Path, args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.email=a@b", "-c", "user.name=a"])
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
                .status;
            assert!(status.success(), "git {args:?}");
        };
        let main = dir.join("main");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "master"]);
        std::fs::write(main.join("a.txt"), "a").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-qm", "one"]);
        git(&main, &["worktree", "add", "-q", "-b", "task", "../task"]);
        let task = dir.join("task");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let _watcher = watch(&task, true, {
            let seen = seen.clone();
            move |paths| seen.lock().unwrap().extend(paths)
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        git(&task, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let start = std::time::Instant::now();
        while !seen.lock().unwrap().contains(&task.join(".git")) {
            assert!(start.elapsed() < Duration::from_secs(5), "the commit was never reported");
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
