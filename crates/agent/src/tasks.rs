//! The workspaces: the folders this agent was given and, for those that are
//! git repos, their worktrees. How a worktree is created is up to each repo
//! through its `.den/create` script, which the agent calls if present.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context as _, Result, bail};
use proto::TaskInfo;

/// Script a repo creates its tasks with: it receives the task name.
const CREATE_SCRIPT: &str = "create";

/// Script a repo removes its tasks with: it receives the name (the branch)
/// and the worktree folder.
const REMOVE_SCRIPT: &str = "remove";

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
fn folder_for(path: &Path) -> Result<PathBuf> {
    match main_checkout(path) {
        Ok(main) => Ok(main),
        Err(_) if path.is_dir() => Ok(path.canonicalize()?),
        Err(err) => Err(err).with_context(|| format!("{} is not a folder", path.display())),
    }
}

/// `~/something` → this machine's home folder plus `something`.
pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), dirs::home_dir()) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
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

/// Creates task `name` in the repo of `path` (a worktree works too) and returns it.
pub fn create(path: &Path, name: &str) -> Result<TaskInfo> {
    let repo = &main_checkout(path)?;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        bail!("invalid task name: use letters, digits, `-`, `_`, `.` or `/`");
    }
    let script = crate::platform::repo_hook(repo, CREATE_SCRIPT);
    let output = if let Some(script) = script {
        run_script(repo, &script, &[name])?
    } else {
        let folder = repo
            .file_name()
            .map(|folder| folder.to_string_lossy().into_owned())
            .unwrap_or_default();
        let target = repo.with_file_name(format!("{folder}-{}", name.replace('/', "-")));
        let target = dunce::simplified(&target).to_string_lossy().into_owned();
        let exists = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
        let args: Vec<&str> = if exists {
            vec!["worktree", "add", &target, name]
        } else {
            vec!["worktree", "add", "-b", name, &target]
        };
        crate::platform::command("git").args(args).current_dir(repo).output()?
    };
    let log = output_log(&output);
    if !output.status.success() {
        bail!("could not create the task:\n{}", log.trim());
    }
    worktrees(repo)?
        .into_iter()
        .find(|task| task.branch.as_deref() == Some(name))
        .with_context(|| format!("created without errors, but there is no worktree for branch {name}:\n{}", log.trim()))
}

/// Removes the task (the worktree) at `path`. Never the main checkout.
/// With `force`, `git worktree remove --force` deletes it along with its
/// uncommitted changes, without the repo's script.
pub fn remove(path: &Path, force: bool) -> Result<()> {
    let repo = main_checkout(path)?;
    let task = worktrees(&repo)?
        .into_iter()
        .find(|task| task.path == path)
        .with_context(|| format!("{} is not a worktree of {}", path.display(), repo.display()))?;
    if task.main {
        bail!("the main checkout cannot be removed");
    }
    let name = task.branch.clone().unwrap_or_default();
    let script = crate::platform::repo_hook(&repo, REMOVE_SCRIPT).filter(|_| !force);
    let output = if let Some(script) = script {
        run_script(&repo, &script, &[&name, &dunce::simplified(path).to_string_lossy()])?
    } else {
        let force = if force { &["--force"][..] } else { &[] };
        crate::platform::command("git")
            .args(["worktree", "remove"])
            .args(force)
            .arg(dunce::simplified(path))
            .current_dir(&repo)
            .output()?
    };
    let log = output_log(&output);
    if !output.status.success() {
        bail!("could not remove the task:\n{}", log.trim());
    }
    if worktrees(&repo)?.iter().any(|task| task.path == path) {
        bail!("the script finished, but the worktree is still there:\n{}", log.trim());
    }
    Ok(())
}

/// Runs a repo script through the user's interactive login shell, so it gets
/// the PATH of their terminal (swt, sim…) even if the app was opened from the
/// Finder: many setups only extend PATH in the rc file (.zshrc), which a
/// login shell alone does not read. Shells that don't speak sh (fish, nu…)
/// can't run the sh command line: from those only their PATH is taken.
fn run_script(repo: &Path, script: &Path, args: &[&str]) -> Result<std::process::Output> {
    #[cfg(unix)]
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    #[cfg(unix)]
    let mut command = if is_posix(&shell) {
        let mut command = std::process::Command::new(shell);
        command.args(["-l", "-i", "-c", "\"$0\" \"$@\""]).arg(script);
        command
    } else {
        let mut command = std::process::Command::new(script);
        if let Some(path) = shell_path(&shell) {
            command.env("PATH", path);
        }
        command
    };
    #[cfg(windows)]
    let mut command = crate::platform::script_command(script);
    command.args(args);
    Ok(command.stdin(std::process::Stdio::null()).current_dir(repo).output()?)
}

/// A shell that runs sh command lines (`"$0" "$@"`).
#[cfg(unix)]
fn is_posix(shell: &str) -> bool {
    let name = Path::new(shell).file_name().and_then(|name| name.to_str()).unwrap_or_default();
    ["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "yash"].contains(&name)
}

/// PATH as the user's interactive login `shell` sets it, asked of `env`
/// (an external program, so any shell runs it the same way).
#[cfg(unix)]
fn shell_path(shell: &str) -> Option<String> {
    let output = std::process::Command::new(shell)
        .args(["-l", "-i", "-c", "/usr/bin/env"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    // The rc files may print things of their own first.
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("PATH="))
        .map(str::to_string)
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
    fn creates_with_git_when_there_is_no_script() {
        let repo = repo("git");
        let task = create(&repo, "bookings").unwrap();
        assert_eq!(task.branch.as_deref(), Some("bookings"));
        assert_eq!(task.path.file_name().unwrap(), "project-bookings");
        assert_eq!(task.repo, repo);
        let all = worktrees(&repo).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all[0].main && !all[1].main);
        assert_eq!(main_checkout(&task.path).unwrap(), repo);
    }

    #[test]
    #[cfg(unix)]
    fn calls_the_repo_script() {
        let repo = repo("script");
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        let script = repo.join(".den/create");
        std::fs::write(&script, "#!/bin/sh\ngit worktree add -q -b \"$1\" \"../other-place-$1\"\necho done\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let task = create(&repo, "payments").unwrap();
        assert_eq!(task.path.file_name().unwrap(), "other-place-payments");
    }

    #[test]
    #[cfg(windows)]
    fn calls_powershell_repo_script() {
        let repo = repo("powershell");
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        std::fs::write(repo.join(".den/create.ps1"),
            "param([string]$TaskName)\ngit worktree add -q -b $TaskName \"../custom-$TaskName\"\nexit $LASTEXITCODE\n").unwrap();
        let task = create(&repo, "windows").unwrap();
        assert_eq!(task.path.file_name().unwrap(), "custom-windows");
    }

    #[test]
    fn removes_with_git_and_refuses_dirty_and_main() {
        let repo = repo("remove");
        let task = create(&repo, "clean").unwrap();
        remove(&task.path, false).unwrap();
        assert_eq!(worktrees(&repo).unwrap().len(), 1);

        let dirty = create(&repo, "dirty").unwrap();
        std::fs::write(dirty.path.join("change.txt"), "x").unwrap();
        assert!(remove(&dirty.path, false).is_err());
        assert!(dirty.path.exists());

        // force deletes it, its changes too; never the main checkout
        remove(&dirty.path, true).unwrap();
        assert!(!dirty.path.exists());
        assert!(remove(&repo, true).unwrap_err().to_string().contains("main checkout"));

        assert!(remove(&repo, false).unwrap_err().to_string().contains("main checkout"));
    }

    #[test]
    #[cfg(unix)]
    fn remove_script_must_really_remove() {
        let repo = repo("noremove");
        let task = create(&repo, "x").unwrap();
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        let script = repo.join(".den/remove");
        std::fs::write(&script, "#!/bin/sh\necho \"doing nothing with $1 in $2\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = remove(&task.path, false).unwrap_err().to_string();
        assert!(err.contains("still there") && err.contains("doing nothing with x in"), "{err}");
        // force doesn't run the script: git removes it
        remove(&task.path, true).unwrap();
        assert!(!task.path.exists());
    }

    /// fish or nu can't run the sh command line: only their PATH is used.
    #[test]
    #[cfg(unix)]
    fn tells_sh_shells_from_others() {
        assert!(is_posix("/bin/zsh") && is_posix("/usr/local/bin/bash") && is_posix("sh"));
        assert!(!is_posix("/opt/homebrew/bin/fish") && !is_posix("/usr/bin/nu"));
        assert!(shell_path("/bin/sh").is_some_and(|path| !path.is_empty()));
    }

    #[test]
    #[cfg(unix)]
    fn reports_script_failures() {
        let repo = repo("fails");
        std::fs::create_dir_all(repo.join(".den")).unwrap();
        let script = repo.join(".den/create");
        std::fs::write(&script, "#!/bin/sh\necho 'no space left' >&2\nexit 3\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = create(&repo, "x").unwrap_err().to_string();
        assert!(err.contains("no space left"), "{err}");
    }
}
