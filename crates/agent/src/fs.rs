//! Files on the agent's machine: the UI never touches the disk, so it works
//! the same locally as on a server over SSH.

use std::{
    collections::{HashMap, HashSet},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use notify::{RecursiveMode, Watcher as _};
use proto::DirEntryInfo;

/// Names that are never listed, on top of what git ignores.
const HIDDEN: &[&str] = &[".git", ".DS_Store"];

/// Maximum size of a file sent to the UI.
const MAX_READ: u64 = proto::MAX_FILE_BYTES as u64;

/// How long changes accumulate before being sent together.
const DEBOUNCE: Duration = Duration::from_millis(100);

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > MAX_READ {
        bail!("the file is too large ({} MB)", size / 1024 / 1024);
    }
    let mut data = Vec::new();
    file.take(MAX_READ + 1).read_to_end(&mut data)?;
    // The file can grow after checking its metadata.
    if data.len() > proto::MAX_FILE_BYTES {
        bail!("the file is too large (maximum {} bytes)", proto::MAX_FILE_BYTES);
    }
    Ok(data)
}

pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    atomic_write(path, |file| file.write_all(data))
        .with_context(|| format!("could not write {}", path.display()))
}

/// Keep the old contents until the complete replacement is on disk. Follow
/// symlinks so saving through one updates its target instead of removing it.
fn atomic_write(path: &Path, write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>) -> Result<()> {
    let resolved = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => path.canonicalize()?,
        _ => path.to_path_buf(),
    };
    let permissions = match std::fs::metadata(&resolved) {
        Ok(metadata) => {
            if !metadata.is_file() {
                bail!("{} is not a regular file", resolved.display());
            }
            // Replacing the directory entry must not bypass file permissions.
            std::fs::OpenOptions::new().write(true).open(&resolved)?;
            Some(metadata.permissions())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let parent = resolved.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".den-save-");
    // A new file gets the usual mode (0666 minus the umask), not the
    // temporary file's private 0600.
    #[cfg(unix)]
    if permissions.is_none() {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut temporary = builder.tempfile_in(parent)?;
    write(temporary.as_file_mut())?;
    if let Some(permissions) = permissions {
        temporary.as_file().set_permissions(permissions)?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist(&resolved)?;
    Ok(())
}

/// A folder, honoring `.gitignore` (including those of parent folders)
/// unless `ignored`: folders first and then files, by name.
pub fn list(dir: &Path, ignored: bool) -> Result<Vec<DirEntryInfo>> {
    if !dir.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    let walk = ignore::WalkBuilder::new(dir)
        .max_depth(Some(1))
        .standard_filters(!ignored)
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
    // On a case-insensitive disk `README.md` exists when renaming
    // `readme.md` to it: it's the same file, not another one to overwrite.
    if std::fs::symlink_metadata(to).is_ok() && !same_file(from, to) {
        bail!("{} already exists", to.display());
    }
    Ok(std::fs::rename(from, to)?)
}

/// Whether both paths name the same entry on disk (not what a symlink
/// points to: renaming onto a link to `from` would replace the link).
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

/// std has no stable file id on Windows: compare the real paths, which
/// carry the case on disk.
#[cfg(windows)]
fn same_file(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Copies the file or folder `from` to `to`, or, if `to` exists, to the
/// first free `copy_name` next to it; returns where it went. Links are copied
/// as links, not followed.
pub fn copy(from: &Path, to: &Path) -> Result<PathBuf> {
    if to.starts_with(from) && to != from {
        bail!("can't copy {} into itself", from.display());
    }
    let meta = std::fs::symlink_metadata(from).with_context(|| format!("could not read {}", from.display()))?;
    let name = to.file_name().context("no name to copy to")?.to_string_lossy().into_owned();
    let to = (0..1000)
        .map(|n| to.with_file_name(proto::copy_name(&name, n)))
        .find(|path| std::fs::symlink_metadata(path).is_err())
        .with_context(|| format!("{} has too many copies", to.display()))?;
    copy_entry(from, &to, &meta).with_context(|| format!("could not copy {} to {}", from.display(), to.display()))?;
    Ok(to)
}

fn copy_entry(from: &Path, to: &Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    if meta.is_symlink() {
        let target = std::fs::read_link(from)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, to);
        #[cfg(windows)]
        return if std::fs::metadata(from).is_ok_and(|meta| meta.is_dir()) {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        };
    }
    if meta.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_entry(&entry.path(), &to.join(entry.file_name()), &entry.metadata()?)?;
        }
        return std::fs::set_permissions(to, meta.permissions());
    }
    std::fs::copy(from, to).map(drop)
}

pub fn create_file(path: &Path) -> Result<()> {
    std::fs::File::create_new(path).with_context(|| format!("could not create {}", path.display()))?;
    Ok(())
}

pub fn create_dir(path: &Path) -> Result<()> {
    std::fs::create_dir(path).with_context(|| format!("could not create {}", path.display()))
}

/// To the Trash (recoverable), not deleted. Returns what `untrash` puts
/// back, if the system says where it went.
pub fn trash(path: &Path) -> Result<Option<PathBuf>> {
    trash_item(path).map_err(|err| anyhow::anyhow!("could not move to the Trash: {err:#}"))
}

/// The system's own call, not Finder's: Finder, scripted, asks for a
/// password to move some files (links in `/tmp`…), and it doesn't say
/// where they went.
#[cfg(target_os = "macos")]
fn trash_item(path: &Path) -> Result<Option<PathBuf>> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let Some(text) = path.to_str() else {
        trash::delete(path)?;
        return Ok(None);
    };
    let url = NSURL::fileURLWithPath(&NSString::from_str(text));
    let mut went = None;
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, Some(&mut went))
        .map_err(|err| anyhow::anyhow!("{}", err.localizedDescription()))?;
    Ok(went.and_then(|url| url.path()).map(|path| PathBuf::from(path.to_string())))
}

#[cfg(target_os = "macos")]
fn untrash_item(item: &Path, to: &Path) -> Result<()> {
    if std::fs::symlink_metadata(item).is_err() {
        bail!("{} is no longer in the Trash", to.display());
    }
    rename(item, to)
}

/// The item the system just put in its Trash for `path`, found by where
/// it was.
#[cfg(not(target_os = "macos"))]
fn trash_item(path: &Path) -> Result<Option<PathBuf>> {
    // Where the Trash will say it was: the folder's real path.
    let original = match (path.parent().and_then(|dir| dunce::canonicalize(dir).ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    };
    trash::delete(path)?;
    let Ok(items) = trash::os_limited::list() else {
        return Ok(None);
    };
    Ok(items
        .into_iter()
        .filter(|item| item.original_path() == original || item.original_path() == path)
        .max_by_key(|item| item.time_deleted)
        .map(|item| PathBuf::from(item.id)))
}

#[cfg(not(target_os = "macos"))]
fn untrash_item(item: &Path, to: &Path) -> Result<()> {
    let found = trash::os_limited::list()?
        .into_iter()
        .find(|trashed| Path::new(&trashed.id) == item)
        .with_context(|| format!("{} is no longer in the Trash", to.display()))?;
    if std::fs::symlink_metadata(found.original_path()).is_ok() {
        bail!("{} already exists", found.original_path().display());
    }
    trash::os_limited::restore_all([found])?;
    Ok(())
}

/// Puts back where it was (`to`) what `trash` sent to the Trash as `item`;
/// never over something that took its place.
pub fn untrash(item: &Path, to: &Path) -> Result<()> {
    untrash_item(item, to).map_err(|err| anyhow::anyhow!("could not put back {}: {err:#}", to.display()))
}

/// Watches `root` and calls `changed` with batches of changed paths until
/// the returned value is dropped.
/// Nothing git ignores (logs, temp files, `target/`…) is reported, nor what
/// changes inside `.git`. With `git`, what moves the repo's state — a
/// commit, checkout, reset, staging or a merge starting or ending — is
/// reported as `root/.git`, also in a worktree, whose git folder is
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
                    if is_state(git_dir, &path) {
                        let _ = tx.send(marker.clone());
                    }
                } else if !ignored.matches(&path) {
                    let _ = tx.send(path);
                }
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    // `logs/HEAD` gets a line on every commit, checkout or reset; the git
    // folder itself has the index, `HEAD` and `MERGE_HEAD`.
    if let Some(git_dir) = &git_dir {
        watcher.watch(git_dir, RecursiveMode::NonRecursive)?;
        let logs = git_dir.join("logs");
        if logs.is_dir() {
            watcher.watch(&logs, RecursiveMode::NonRecursive)?;
        }
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
    let output = crate::platform::command("git")
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

/// A file in the git folder whose change means the status changed.
fn is_state(git_dir: &Path, path: &Path) -> bool {
    path.strip_prefix(git_dir)
        .is_ok_and(|rest| ["index", "HEAD", "MERGE_HEAD", "logs/HEAD"].iter().any(|state| rest == Path::new(state)))
}

/// What git ignores inside a folder: the `.gitignore` of each folder from
/// the root down to a changed path, read the first time a change happens
/// there (reading them all up front walks the whole tree, which in a home
/// folder takes a minute), plus `.git`.
struct Ignored {
    root: PathBuf,
    matchers: Mutex<HashMap<PathBuf, Option<ignore::gitignore::Gitignore>>>,
}

impl Ignored {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            matchers: Mutex::default(),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        if relative.components().any(|part| part.as_os_str() == ".git") {
            return true;
        }
        let mut matchers = self.matchers.lock().unwrap();
        // An edited `.gitignore` is read again.
        if path.file_name().is_some_and(|name| name == ".gitignore")
            && let Some(dir) = path.parent()
        {
            matchers.remove(dir);
        }
        let is_dir = path.is_dir();
        path.ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&self.root))
            .any(|dir| {
                let matcher = matchers.entry(dir.to_path_buf()).or_insert_with(|| {
                    let (matcher, _) = ignore::gitignore::Gitignore::new(dir.join(".gitignore"));
                    (!matcher.is_empty()).then_some(matcher)
                });
                matcher
                    .as_ref()
                    .is_some_and(|matcher| matcher.matched_path_or_any_parents(path, is_dir).is_ignore())
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("den-fs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn rejects_files_that_cannot_fit_in_a_response() {
        let dir = dir("read-limit");
        let path = dir.join("large.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_READ + 1).unwrap();
        assert!(read(&path).unwrap_err().to_string().contains("too large"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_save_keeps_original_and_cleans_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("document.txt");
        std::fs::write(&path, b"original document").unwrap();
        let result = atomic_write(&path, |file| {
            file.write_all(b"partial replacement")?;
            Err(std::io::Error::other("simulated write failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original document");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        write(&path, b"complete replacement").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"complete replacement");
    }

    #[test]
    #[cfg(unix)]
    fn atomic_save_follows_symlinks_and_preserves_executable_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        let link = dir.path().join("link");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        symlink("script", &link).unwrap();
        write(&link, b"new").unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o751);
    }

    #[test]
    #[cfg(unix)]
    fn new_files_get_the_default_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::File::create(&plain).unwrap();
        let saved = dir.path().join("saved");
        write(&saved, b"new").unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&saved), mode(&plain));
    }

    #[test]
    fn copies_never_overwrite() {
        let dir = dir("copy");
        std::fs::create_dir_all(dir.join("src/inner")).unwrap();
        write(&dir.join("src/inner/a.txt"), b"a").unwrap();
        write(&dir.join("b.txt"), b"b").unwrap();
        assert_eq!(copy(&dir.join("b.txt"), &dir.join("b.txt")).unwrap(), dir.join("b copy.txt"));
        assert_eq!(copy(&dir.join("b.txt"), &dir.join("b.txt")).unwrap(), dir.join("b copy 2.txt"));
        assert_eq!(read(&dir.join("b copy 2.txt")).unwrap(), b"b");
        assert_eq!(copy(&dir.join("src"), &dir.join("dst")).unwrap(), dir.join("dst"));
        assert_eq!(read(&dir.join("dst/inner/a.txt")).unwrap(), b"a");
        assert!(copy(&dir.join("src"), &dir.join("src/inner/src")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("b.txt", dir.join("link")).unwrap();
            let link = copy(&dir.join("link"), &dir.join("link")).unwrap();
            assert_eq!(std::fs::read_link(link).unwrap(), PathBuf::from("b.txt"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sends a file to the real Trash and puts it back.
    #[cfg(target_os = "macos")]
    #[test]
    fn what_goes_to_the_trash_comes_back() {
        let dir = dir("trash");
        write(&dir.join("a.txt"), b"a").unwrap();
        std::os::unix::fs::symlink("a.txt", dir.join("link")).unwrap();
        let item = trash(&dir.join("a.txt")).unwrap().expect("where it went");
        assert!(std::fs::symlink_metadata(dir.join("a.txt")).is_err());
        // A link goes to the Trash itself, not what it points to.
        let link = trash(&dir.join("link")).unwrap().expect("where the link went");
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        // Nothing is put back over what took its place.
        write(&dir.join("a.txt"), b"new").unwrap();
        assert!(untrash(&item, &dir.join("a.txt")).is_err());
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        untrash(&item, &dir.join("a.txt")).unwrap();
        untrash(&link, &dir.join("link")).unwrap();
        assert_eq!(read(&dir.join("a.txt")).unwrap(), b"a");
        assert!(std::fs::symlink_metadata(dir.join("link")).unwrap().is_symlink());
        assert!(untrash(&item, &dir.join("a.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_reads_writes_and_renames() {
        let dir = dir("ops");
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::create_dir(dir.join("src")).unwrap();
        create_file(&dir.join("b.txt")).unwrap();
        write(&dir.join("a.txt"), b"hello").unwrap();

        let names: Vec<(String, bool)> = list(&dir, false).unwrap().into_iter().map(|e| (e.name, e.is_dir)).collect();
        assert_eq!(
            names,
            vec![
                ("src".into(), true),
                (".gitignore".into(), false),
                ("a.txt".into(), false),
                ("b.txt".into(), false)
            ]
        );
        // With what git ignores, too.
        let names: Vec<String> = list(&dir, true).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["src", "target", ".gitignore", "a.txt", "b.txt"]);
        assert_eq!(read(&dir.join("a.txt")).unwrap(), b"hello");
        assert!(create_file(&dir.join("a.txt")).is_err());
        rename(&dir.join("a.txt"), &dir.join("c.txt")).unwrap();
        assert!(rename(&dir.join("b.txt"), &dir.join("c.txt")).is_err());
        // Only the case changes: on a case-insensitive disk the target exists.
        rename(&dir.join("c.txt"), &dir.join("C.txt")).unwrap();
        assert!(std::fs::read_dir(&dir).unwrap().any(|entry| entry.unwrap().file_name() == "C.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watches_changes() {
        let dir = dir("watch");
        std::fs::write(dir.join(".gitignore"), "logs/\n").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/.gitignore"), "*.tmp\n").unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let _watcher = watch(&dir, false, {
            let seen = seen.clone();
            move |paths| seen.lock().unwrap().extend(paths)
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        std::fs::write(dir.join("logs/noise.log"), "x").unwrap();
        std::fs::write(dir.join("sub/noise.tmp"), "x").unwrap();
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        let start = std::time::Instant::now();
        while !seen.lock().unwrap().iter().any(|path| path.ends_with("new.txt")) {
            assert!(start.elapsed() < Duration::from_secs(5), "the change never arrived");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!seen.lock().unwrap().iter().any(|path| path.starts_with(dir.join("logs"))));
        assert!(!seen.lock().unwrap().iter().any(|path| path.ends_with("noise.tmp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reports_commits_and_staging_in_a_worktree() {
        let dir = dir("git");
        let git = |dir: &Path, args: &[&str]| {
            let status = crate::platform::command("git")
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
        // Staging only writes the index.
        std::fs::write(task.join("b.txt"), "b").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        seen.lock().unwrap().clear();
        git(&task, &["add", "b.txt"]);
        let start = std::time::Instant::now();
        while !seen.lock().unwrap().contains(&task.join(".git")) {
            assert!(start.elapsed() < Duration::from_secs(5), "staging was never reported");
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
