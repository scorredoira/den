//! The workspaces: the folders this agent was given and, for those that are
//! git repos, their worktrees. The agent creates and removes worktrees itself
//! with git; a repo only adds what a new one needs to run, through its
//! `.den/setup` script, which the agent calls if present.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result, bail};
use proto::TaskInfo;

/// Script a repo finishes a new worktree with (its config, its data…): it
/// receives the worktree's folder and its branch, and runs in the repo's main
/// checkout. If it fails, the worktree is taken back.
const SETUP_SCRIPT: &str = "setup";

/// How long `.den/setup` is waited for before it is ended and the worktree
/// taken back: a script that waits on something that never comes would
/// otherwise leave the new worktree's row turning for good.
const SETUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

fn repos_file() -> Result<PathBuf> {
    Ok(proto::config_dir()?.join("repos.json"))
}

/// Held while `repos.json` is read and written back, so two changes at
/// once don't lose one of them.
static REPOS: Mutex<()> = Mutex::new(());

/// Folders this agent knows about: a repo's main checkout, or any folder.
pub fn repos() -> Vec<PathBuf> {
    load_repos().unwrap_or_default()
}

/// The saved folders; none if the file isn't there yet. A file that can't
/// be read is an error, so a change never writes over it as if it were empty.
fn load_repos() -> Result<Vec<PathBuf>> {
    let file = repos_file()?;
    match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("could not read {}", file.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err).with_context(|| format!("could not read {}", file.display())),
    }
}

/// Changes the saved folders with `change`, written whole (never a
/// half-written file for someone reading it at the same time).
fn update_repos(change: impl FnOnce(&mut Vec<PathBuf>)) -> Result<()> {
    let _lock = REPOS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut repos = load_repos()?;
    let before = repos.clone();
    change(&mut repos);
    if repos != before {
        let file = repos_file()?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::fs::write(&file, &serde_json::to_vec_pretty(&repos)?)?;
    }
    Ok(())
}

/// Adds `path` (see `folder_for`) and returns what it added.
pub fn add_repo(path: &Path) -> Result<PathBuf> {
    let main = folder_for(path)?;
    update_repos(|repos| {
        if !repos.contains(&main) {
            repos.push(main.clone());
            repos.sort();
        }
    })?;
    Ok(main)
}

/// What adding `path` keeps: its repo's main checkout if it's in a git repo
/// (a worktree adds its repo), the folder itself otherwise.
pub fn folder_for(path: &Path) -> Result<PathBuf> {
    match main_checkout(path) {
        Ok(main) => Ok(main),
        Err(_) if path.is_dir() => Ok(path.canonicalize()?),
        Err(err) => Err(err).with_context(|| format!("{} is not a folder", path.display())),
    }
}

/// `~/something` → this machine's home folder plus `something`.
pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// Forgets a folder (nothing on disk is touched).
pub fn remove_repo(path: &Path) -> Result<()> {
    update_repos(|repos| repos.retain(|repo| repo != path))
}

/// Main checkout of the repo `path` belongs to.
fn main_checkout(path: &Path) -> Result<PathBuf> {
    let worktrees = worktrees(path).with_context(|| format!("{} is not a git repo", path.display()))?;
    worktrees
        .into_iter()
        .next()
        .map(|task| task.path)
        .context("git listed no worktrees")
}

/// Every known folder's workspaces: a repo's worktrees, the main one
/// first, or the folder alone (no branch) if it isn't a repo. The caller
/// fills in `working`.
pub fn list() -> Vec<TaskInfo> {
    repos()
        .iter()
        .filter(|folder| folder.is_dir())
        .flat_map(|folder| {
            worktrees(folder).unwrap_or_else(|_| {
                vec![TaskInfo { repo: folder.clone(), path: folder.clone(), branch: None, main: true, working: false }]
            })
        })
        .collect()
}

/// A repo's worktrees, the main one first.
fn worktrees(repo: &Path) -> Result<Vec<TaskInfo>> {
    let output = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut tasks = Vec::new();
    for block in output.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut bare = false;
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(rest).canonicalize().unwrap_or_else(|_| PathBuf::from(rest)));
            } else if let Some(rest) = line.strip_prefix("branch ") {
                branch = Some(rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string());
            } else if line == "bare" {
                bare = true;
            }
        }
        if let Some(path) = path
            && !bare
        {
            tasks.push(TaskInfo {
                repo: PathBuf::new(),
                main: tasks.is_empty(),
                path,
                branch,
                working: false,
            });
        }
    }
    let main = tasks.first().map(|task| task.path.clone()).unwrap_or_default();
    for task in &mut tasks {
        task.repo = main.clone();
    }
    Ok(tasks)
}

/// Creates task `name` in the repo of `path` (a worktree works too) and
/// returns it: a worktree at `<repo>_worktrees/<name>` on branch `name`,
/// new off the main checkout's HEAD unless it exists, then the repo's
/// `.den/setup` if it has one.
pub fn create(path: &Path, name: &str) -> Result<TaskInfo> {
    let repo = &main_checkout(path)?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        bail!("invalid task name: use letters, digits, `-`, `_`, `.` or `/`");
    }
    let target = container(repo).join(name.replace('/', "-"));
    if target.exists() {
        bail!("{} already exists", target.display());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let target_text = dunce::simplified(&target).to_string_lossy().into_owned();
    let exists = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
    let args: Vec<&str> = if exists {
        vec!["worktree", "add", &target_text, name]
    } else {
        vec!["worktree", "add", "-b", name, &target_text]
    };
    let output = crate::platform::command("git").args(args).current_dir(repo).output()?;
    if !output.status.success() {
        bail!("could not create the task:\n{}", output_log(&output).trim());
    }
    let task = worktrees(repo)?
        .into_iter()
        .find(|task| task.branch.as_deref() == Some(name))
        .with_context(|| format!("created without errors, but there is no worktree for branch {name}"))?;
    if let Some(script) = crate::platform::repo_hook(repo, SETUP_SCRIPT)
        && let Err(err) = setup(repo, &script, &task.path, name)
    {
        // Never half made: what `worktree add` made goes, and the branch if it was new.
        let path = dunce::simplified(&task.path).to_string_lossy().into_owned();
        let _ = git(repo, &["worktree", "remove", "--force", &path]);
        if !exists {
            let _ = git(repo, &["branch", "-D", name]);
        }
        return Err(err);
    }
    Ok(task)
}

/// Where a repo's worktrees go: `<repo>_worktrees`, beside it.
fn container(repo: &Path) -> PathBuf {
    let folder = repo.file_name().map(|folder| folder.to_string_lossy().into_owned()).unwrap_or_default();
    repo.with_file_name(format!("{folder}_worktrees"))
}

fn setup(repo: &Path, script: &Path, path: &Path, name: &str) -> Result<()> {
    let path = dunce::simplified(path).to_string_lossy().into_owned();
    let output = run_script(repo, script, &[&path, name], SETUP_DEADLINE)?;
    if !output.status.success() {
        bail!("the repo's .den/setup failed:\n{}", output_log(&output).trim());
    }
    Ok(())
}

/// Removes the task (the worktree) at `path`, never the main checkout: the
/// folder goes to the Trash, uncommitted changes and all, git forgets it,
/// and its branch is deleted if the main branch has all its commits (else it
/// stays, and them with it). A worktree git holds (locked, or in the middle
/// of a rebase, merge, cherry-pick, revert or bisect) is refused unless `force`.
pub fn remove(path: &Path, force: bool) -> Result<()> {
    let repo = main_checkout(path)?;
    let task = worktrees(&repo)?
        .into_iter()
        .find(|task| task.path == path)
        .with_context(|| format!("{} is not a worktree of {}", path.display(), repo.display()))?;
    if task.main {
        bail!("the main checkout cannot be removed");
    }
    if !force {
        let held = held(&repo, path);
        if !held.is_empty() {
            bail!("git is holding it: {}", held.join(", "));
        }
    }
    let merged = task.branch.as_deref().is_some_and(|branch| merged(&repo, branch));
    crate::fs::trash(path)?;
    git(&repo, &["worktree", "prune"])?;
    if worktrees(&repo)?.iter().any(|task| task.path == path) {
        bail!("it went to the Trash, but git still lists the worktree");
    }
    if let (true, Some(branch)) = (merged, &task.branch) {
        git(&repo, &["branch", "-D", branch])?;
    }
    Ok(())
}

/// Whether the repo's main branch has every commit of `branch`.
fn merged(repo: &Path, branch: &str) -> bool {
    let Some(main) = crate::git::default_branch(repo) else {
        return false;
    };
    main != branch && git(repo, &["merge-base", "--is-ancestor", branch, &main]).is_ok()
}

/// What git is holding the worktree at `path` for, in words.
fn held(repo: &Path, path: &Path) -> Vec<&'static str> {
    let mut held = Vec::new();
    let list = git(repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    for block in list.split("\n\n") {
        let mut lines = block.lines();
        let listed = lines.next().and_then(|line| line.strip_prefix("worktree ")).map(PathBuf::from);
        if listed.is_some_and(|listed| listed.canonicalize().unwrap_or(listed) == path)
            && block.lines().any(|line| line == "locked" || line.starts_with("locked "))
        {
            held.push("locked");
        }
    }
    let Ok(dir) = git(path, &["rev-parse", "--absolute-git-dir"]) else {
        return held;
    };
    let dir = PathBuf::from(dir.trim());
    for (file, what) in [
        ("rebase-merge", "a rebase under way"),
        ("rebase-apply", "a rebase under way"),
        ("MERGE_HEAD", "a merge under way"),
        ("CHERRY_PICK_HEAD", "a cherry-pick under way"),
        ("REVERT_HEAD", "a revert under way"),
        ("BISECT_LOG", "a bisect under way"),
    ] {
        if dir.join(file).exists() && !held.contains(&what) {
            held.push(what);
        }
    }
    held
}

/// Runs a repo script through the user's interactive login shell, so it gets
/// the PATH of their terminal (swt, sim…) even if the app was opened from the
/// Finder: many setups only extend PATH in the rc file (.zshrc), which a
/// login shell alone does not read. Shells that don't speak sh (fish, nu…)
/// can't run the sh command line: from those only their PATH is taken. Past
/// `deadline` it is ended, with whatever it started, and says so.
fn run_script(repo: &Path, script: &Path, args: &[&str], deadline: std::time::Duration) -> Result<std::process::Output> {
    #[cfg(unix)]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    #[cfg(unix)]
    let mut command = if is_posix(&shell) {
        let mut command = std::process::Command::new(shell);
        command.args(["-l", "-i", "-c", "\"$0\" \"$@\""]).arg(script);
        command
    } else {
        let mut command = std::process::Command::new(script);
        if let Some(path) = crate::platform::shell_path(&shell) {
            command.env("PATH", path);
        }
        command
    };
    #[cfg(windows)]
    let mut command = crate::platform::script_command(script);
    command.args(args);
    // A group of its own, so that ending it ends what it started too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(repo)
        .spawn()?;
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = read(child.stderr.take().map(|pipe| Box::new(pipe) as _));
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > deadline {
            #[cfg(unix)]
            unsafe {
                libc::killpg(child.id() as libc::pid_t, libc::SIGKILL)
            };
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} did not finish in {} s, and was ended", script.display(), deadline.as_secs());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    Ok(std::process::Output { status, stdout: stdout.join().unwrap_or_default(), stderr: stderr.join().unwrap_or_default() })
}

/// A shell that runs sh command lines (`"$0" "$@"`).
#[cfg(unix)]
fn is_posix(shell: &str) -> bool {
    let name = Path::new(shell).file_name().and_then(|name| name.to_str()).unwrap_or_default();
    ["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "yash"].contains(&name)
}

fn output_log(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = crate::platform::command("git").args(args).current_dir(dir).output()?;
    if !output.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        assert!(crate::platform::command("git").args(args).current_dir(dir).output().unwrap().status.success());
    }

    fn repo(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("den-tasks-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "master"]);
        run(&repo, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "initial"]);
        repo.canonicalize().unwrap()
    }

    /// A folder that isn't a repo is kept as it is.
    #[test]
    fn plain_folders_are_kept_as_they_are() {
        let base = std::env::temp_dir().join(format!("den-tasks-{}-plain", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let folder = base.canonicalize().unwrap();
        assert_eq!(folder_for(&folder).unwrap(), folder);
        assert!(folder_for(&folder.join("missing")).is_err());
        assert!(create(&folder, "x").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn creates_with_git_beside_the_repo() {
        let repo = repo("git");
        let task = create(&repo, "bookings").unwrap();
        assert_eq!(task.branch.as_deref(), Some("bookings"));
        assert_eq!(task.path, repo.with_file_name("project_worktrees").join("bookings"));
        assert_eq!(task.repo, repo);
        let all = worktrees(&repo).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].main && !all[1].main);
        assert_eq!(main_checkout(&task.path).unwrap(), repo);
        assert!(create(&repo, "bookings").unwrap_err().to_string().contains("already exists"));
    }

    #[cfg(unix)]
    fn script(repo: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        let script = repo.join(".den").join(name);
        std::fs::write(&script, text).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The repo's .den/setup finishes the new worktree in its folder.
    #[test]
    #[cfg(unix)]
    fn runs_the_repo_setup() {
        let repo = repo("setup");
        script(&repo, "setup", "#!/bin/sh\necho \"$2\" > \"$1/set-up\"\n");
        let task = create(&repo, "payments").unwrap();
        assert_eq!(std::fs::read_to_string(task.path.join("set-up")).unwrap(), "payments\n");
    }

    /// A setup that fails takes back the worktree and its new branch, and says why.
    #[test]
    #[cfg(unix)]
    fn a_failed_setup_leaves_nothing() {
        let repo = repo("fails");
        script(&repo, "setup", "#!/bin/sh\necho 'no space left' >&2\nexit 3\n");
        let err = create(&repo, "x").unwrap_err().to_string();
        assert!(err.contains("no space left"), "{err}");
        assert_eq!(worktrees(&repo).unwrap().len(), 1);
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/x"]).is_err());
    }

    /// One that never ends is ended at the deadline, with what it started.
    #[test]
    #[cfg(unix)]
    fn a_setup_that_never_ends_is_ended() {
        let repo = repo("hangs");
        script(&repo, "setup", "#!/bin/sh\nsleep 600 &\nsleep 600\n");
        let start = std::time::Instant::now();
        let hook = crate::platform::repo_hook(&repo, SETUP_SCRIPT).unwrap();
        let err = run_script(&repo, &hook, &["a", "b"], std::time::Duration::from_millis(500)).unwrap_err();
        assert!(err.to_string().contains("did not finish"), "{err}");
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn removes_to_the_trash_but_never_the_main_checkout() {
        let repo = repo("remove");
        let task = create(&repo, "clean").unwrap();
        remove(&task.path, false).unwrap();
        assert_eq!(worktrees(&repo).unwrap().len(), 1);

        let dirty = create(&repo, "dirty").unwrap();
        std::fs::write(dirty.path.join("change.txt"), "x").unwrap();
        remove(&dirty.path, false).unwrap();
        assert!(!dirty.path.exists());
        assert_eq!(worktrees(&repo).unwrap().len(), 1);

        assert!(remove(&repo, true).unwrap_err().to_string().contains("main checkout"));

        assert!(remove(&repo, false).unwrap_err().to_string().contains("main checkout"));
    }

    /// A merged branch goes with its worktree; one with commits of its own stays.
    #[test]
    fn deletes_the_branch_only_if_merged() {
        let repo = repo("branch");
        let merged = create(&repo, "merged").unwrap();
        remove(&merged.path, false).unwrap();
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/merged"]).is_err());

        let own = create(&repo, "own").unwrap();
        run(&own.path, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "mine"]);
        remove(&own.path, false).unwrap();
        assert!(!own.path.exists());
        assert!(git(&repo, &["rev-parse", "--verify", "--quiet", "refs/heads/own"]).is_ok());
    }

    /// A worktree git holds is refused, unless forced.
    #[test]
    fn refuses_what_git_holds() {
        let repo = repo("held");
        let locked = create(&repo, "locked").unwrap();
        run(&repo, &["worktree", "lock", &locked.path.to_string_lossy()]);
        assert!(remove(&locked.path, false).unwrap_err().to_string().contains("locked"));
        run(&repo, &["worktree", "unlock", &locked.path.to_string_lossy()]);

        let merging = create(&repo, "merging").unwrap();
        let dir = git(&merging.path, &["rev-parse", "--absolute-git-dir"]).unwrap();
        std::fs::write(Path::new(dir.trim()).join("MERGE_HEAD"), "x").unwrap();
        assert!(remove(&merging.path, false).unwrap_err().to_string().contains("merge under way"));
        remove(&merging.path, true).unwrap();
        assert!(!merging.path.exists());
        remove(&locked.path, false).unwrap();
    }

    /// fish or nu can't run the sh command line: only their PATH is used.
    #[test]
    #[cfg(unix)]
    fn tells_sh_shells_from_others() {
        assert!(is_posix("/bin/zsh") && is_posix("/usr/local/bin/bash") && is_posix("sh"));
        assert!(!is_posix("/opt/homebrew/bin/fish") && !is_posix("/usr/bin/nu"));
        assert!(crate::platform::shell_path("/bin/sh").is_some_and(|path| !path.is_empty()));
    }
}
