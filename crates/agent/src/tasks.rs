//! Repos and tasks. A task is a git worktree; how it's created is up to
//! each repo through its `.task/create` script, which the agent calls if present.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};
use proto::TaskInfo;

/// Script a repo creates its tasks with: it receives the task name.
const CREATE_SCRIPT: &str = ".task/create";

/// Script a repo removes its tasks with: it receives the name (the branch)
/// and the worktree folder.
const REMOVE_SCRIPT: &str = ".task/remove";

fn repos_file() -> Result<PathBuf> {
    Ok(proto::config_dir()?.join("repos.json"))
}

/// Repos this agent knows about (their main checkout).
pub fn repos() -> Vec<PathBuf> {
    repos_file()
        .ok()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Adds the repo of `path` (which may be a worktree) and returns its main checkout.
pub fn add_repo(path: &Path) -> Result<PathBuf> {
    let main = main_checkout(path)?;
    let mut repos = repos();
    if !repos.contains(&main) {
        repos.push(main.clone());
        repos.sort();
        let file = repos_file()?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, serde_json::to_vec_pretty(&repos)?)?;
    }
    Ok(main)
}

/// `~/something` → this machine's home folder plus `something`.
pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// Forgets a repo.
pub fn remove_repo(path: &Path) -> Result<()> {
    let mut repos = repos();
    repos.retain(|repo| repo != path);
    let file = repos_file()?;
    std::fs::write(file, serde_json::to_vec_pretty(&repos)?)?;
    Ok(())
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

/// Tasks from every known repo. The caller fills in `working`.
pub fn list() -> Vec<TaskInfo> {
    repos()
        .iter()
        .filter_map(|repo| worktrees(repo).ok())
        .flatten()
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
                path = Some(PathBuf::from(rest));
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
    let script = repo.join(CREATE_SCRIPT);
    let output = if script.is_file() {
        run_script(repo, &script, &[name])?
    } else {
        let folder = repo
            .file_name()
            .map(|folder| folder.to_string_lossy().into_owned())
            .unwrap_or_default();
        let target = repo.with_file_name(format!("{folder}-{}", name.replace('/', "-")));
        let target = target.to_string_lossy().into_owned();
        let exists = git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]).is_ok();
        let args: Vec<&str> = if exists {
            vec!["worktree", "add", &target, name]
        } else {
            vec!["worktree", "add", "-b", name, &target]
        };
        Command::new("git").args(args).current_dir(repo).output()?
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
pub fn remove(path: &Path) -> Result<()> {
    let repo = main_checkout(path)?;
    let task = worktrees(&repo)?
        .into_iter()
        .find(|task| task.path == path)
        .with_context(|| format!("{} is not a worktree of {}", path.display(), repo.display()))?;
    if task.main {
        bail!("the main checkout cannot be removed");
    }
    let name = task.branch.clone().unwrap_or_default();
    let script = repo.join(REMOVE_SCRIPT);
    let output = if script.is_file() {
        run_script(&repo, &script, &[&name, &path.to_string_lossy()])?
    } else {
        Command::new("git")
            .args(["worktree", "remove", &path.to_string_lossy()])
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
/// login shell alone does not read.
fn run_script(repo: &Path, script: &Path, args: &[&str]) -> Result<std::process::Output> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let script = script.to_string_lossy();
    let mut command = Command::new(shell);
    command.args(["-l", "-i", "-c", "\"$0\" \"$@\"", &script]);
    command.args(args);
    Ok(command.stdin(std::process::Stdio::null()).current_dir(repo).output()?)
}

fn output_log(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").args(args).current_dir(dir).output().unwrap().status.success());
    }

    fn repo(name: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("sik-tasks-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("project");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-q", "-b", "master"]);
        run(&repo, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-q", "--allow-empty", "-m", "initial"]);
        repo.canonicalize().unwrap()
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
    fn calls_the_repo_script() {
        let repo = repo("script");
        std::fs::create_dir_all(repo.join(".task")).unwrap();
        let script = repo.join(".task/create");
        std::fs::write(&script, "#!/bin/sh\ngit worktree add -q -b \"$1\" \"../other-place-$1\"\necho done\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let task = create(&repo, "payments").unwrap();
        assert_eq!(task.path.file_name().unwrap(), "other-place-payments");
    }

    #[test]
    fn removes_with_git_and_refuses_dirty_and_main() {
        let repo = repo("remove");
        let task = create(&repo, "clean").unwrap();
        remove(&task.path).unwrap();
        assert_eq!(worktrees(&repo).unwrap().len(), 1);

        let dirty = create(&repo, "dirty").unwrap();
        std::fs::write(dirty.path.join("change.txt"), "x").unwrap();
        assert!(remove(&dirty.path).is_err());
        assert!(dirty.path.exists());

        assert!(remove(&repo).unwrap_err().to_string().contains("main checkout"));
    }

    #[test]
    fn remove_script_must_really_remove() {
        let repo = repo("noremove");
        let task = create(&repo, "x").unwrap();
        std::fs::create_dir_all(repo.join(".task")).unwrap();
        let script = repo.join(".task/remove");
        std::fs::write(&script, "#!/bin/sh\necho \"doing nothing with $1 in $2\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = remove(&task.path).unwrap_err().to_string();
        assert!(err.contains("still there") && err.contains("doing nothing with x in"), "{err}");
    }

    #[test]
    fn reports_script_failures() {
        let repo = repo("fails");
        std::fs::create_dir_all(repo.join(".task")).unwrap();
        let script = repo.join(".task/create");
        std::fs::write(&script, "#!/bin/sh\necho 'no space left' >&2\nexit 3\n").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = create(&repo, "x").unwrap_err().to_string();
        assert!(err.contains("no space left"), "{err}");
    }
}
