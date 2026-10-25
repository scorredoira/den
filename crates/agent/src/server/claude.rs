//! Claude Code (and the other coding agents) in a terminal: telling them
//! apart, and the command that resumes a Claude Code session after a restart.

use super::*;

/// The session `--resume <id>` resumes in a command line; none for a bare
/// `--resume`, which asks.
pub(super) fn resumed_session(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    words.by_ref().find(|word| *word == "--resume")?;
    words.next().filter(|word| !word.starts_with('-'))
}

/// The coding agent a command line runs, by its program or its npm package
/// (run by node): `claude`, `codex`…
pub(super) fn agent_name(args: &str) -> Option<String> {
    const AGENTS: [&str; 7] = ["claude", "codex", "gemini", "opencode", "aider", "amp", "cursor-agent"];
    const PACKAGES: [(&str, &str); 4] =
        [("@anthropic-ai/claude-code", "claude"), ("@openai/codex", "codex"), ("@google/gemini-cli", "gemini"), ("opencode-ai", "opencode")];
    let program = |word: &str| {
        let word = word.trim_matches('"').replace('\\', "/");
        let name = word.rsplit('/').next().unwrap_or("").to_string();
        let name = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".cmd")).unwrap_or(&name).to_string();
        (word, name)
    };
    let mut words = args.split_whitespace();
    let (_, first) = program(words.next()?);
    if AGENTS.contains(&first.as_str()) {
        return Some(first);
    }
    // What an interpreter runs: the agent's package, or its script.
    if !["node", "bun", "deno", "python", "python3"].contains(&first.as_str()) {
        return None;
    }
    let (script, name) = program(words.next()?);
    PACKAGES
        .iter()
        .find(|(package, _)| script.contains(package))
        .map(|(_, name)| name.to_string())
        .or_else(|| AGENTS.contains(&name.as_str()).then_some(name))
}

/// The coding agent a terminal whose foreground process is `foreground`
/// runs, with its pid and command line: that process, or another of its
/// group (`processes`, see `platform::processes`), as when a wrapper runs
/// the agent as its child.
pub(super) fn agent_process(foreground: u32, processes: &[(u32, u32, String)]) -> Option<(u32, String)> {
    let leader = processes
        .iter()
        .find(|(pid, ..)| *pid == foreground)
        .map(|(_, _, args)| args.clone())
        .or_else(|| platform::process_args(foreground));
    leader
        .map(|args| (foreground, args))
        .into_iter()
        .chain(
            processes
                .iter()
                .filter(|(pid, group, _)| *group == foreground && *pid != foreground)
                .map(|(pid, _, args)| (*pid, args.clone())),
        )
        .find(|(_, args)| agent_name(args).is_some())
}

/// Claude Code's title: what it's doing after `✳` or a spinner (braille,
/// or half circles from 2.1.2xx on).
pub(super) fn claude_title(title: &str) -> bool {
    claude_working(title).is_some()
}

/// Whether Claude Code works, by its title: a spinner while it does, `✳`
/// while it doesn't; `None` for a title that isn't Claude Code's.
pub(super) fn claude_working(title: &str) -> Option<bool> {
    match title.chars().next()? {
        '✳' => Some(false),
        '\u{2800}'..='\u{28FF}' | '◐' | '◑' | '◒' | '◓' => Some(true),
        _ => None,
    }
}

/// The Claude Code session a process runs: Claude Code writes it to
/// `sessions/<pid>.json` in its config folder, `CLAUDE_CONFIG_DIR` or `~/.claude`.
pub(super) struct ClaudeSession {
    pub(super) id: Option<String>,
    pub(super) cwd: Option<PathBuf>,
    /// `CLAUDE_CONFIG_DIR` if the process had it: the new shell may not.
    config_dir: Option<String>,
}

pub(super) fn claude_session(pid: u32) -> Option<ClaudeSession> {
    let config_dir = platform::process_env(pid, "CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty());
    let dir = match &config_dir {
        Some(dir) => PathBuf::from(dir),
        None => dirs::home_dir()?.join(".claude"),
    };
    let file = std::fs::read(dir.join("sessions").join(format!("{pid}.json"))).ok();
    let json: Option<serde_json::Value> = file.and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let field = |name: &str| json.as_ref()?.get(name)?.as_str().map(str::to_string);
    Some(ClaudeSession {
        id: field("sessionId").filter(|id| session_id(id)),
        cwd: field("cwd").map(PathBuf::from).filter(|cwd| cwd.is_dir()),
        config_dir,
    })
}

/// Typed into a shell as is, so only letters, digits and `-` (a UUID).
fn session_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

/// If a command line is Claude Code's (the native `claude` binary or the npm
/// package run by node), the command that resumes its session: the same
/// options and config folder plus `--resume <id>`. With no id known, a bare
/// `--resume` lets the user pick the session, rather than `--continue`
/// guessing one: two terminals in the same folder would get the same.
pub(super) fn resume_command(args: &str, session: Option<&ClaudeSession>) -> Option<String> {
    resume_command_in(if cfg!(windows) { Shell::PowerShell } else { Shell::Posix }, args, session)
}

/// The shell a resumed command is typed into.
#[derive(Clone, Copy)]
enum Shell {
    Posix,
    PowerShell,
}

fn resume_command_in(shell: Shell, args: &str, session: Option<&ClaudeSession>) -> Option<String> {
    let words = match shell {
        // `ps` joins the arguments with spaces and loses their quoting.
        Shell::Posix => args.split_whitespace().map(str::to_string).collect(),
        // Windows keeps the command line as typed, quotes and all.
        Shell::PowerShell => windows_words(args),
    };
    let start = words.iter().position(|word| {
        let word = word.trim_matches('"');
        matches!(word.rsplit(['/', '\\']).next(), Some("claude" | "claude.exe" | "claude.cmd"))
            || word.replace('\\', "/").contains("@anthropic-ai/claude-code")
    })?;
    let mut command = Vec::new();
    if let Some(dir) = session.and_then(|session| session.config_dir.as_deref()) {
        command.push(format!("CLAUDE_CONFIG_DIR={}", shell_quote(dir)));
    }
    command.push("claude".to_string());
    // The session it resumed, if it was started with one.
    let mut resumed = None;
    let mut rest = words[start + 1..].iter().peekable();
    // A word that isn't an option goes only as the value of the one before
    // it: the others are the prompt it began with, not to be sent again.
    let mut after_option = false;
    while let Some(word) = rest.next() {
        let option = word.starts_with('-');
        match word.as_str() {
            "-c" | "--continue" | "--fork-session" => {}
            "-r" | "--resume" | "--session-id" => {
                if let Some(value) = rest.next_if(|value| !value.starts_with('-')) {
                    resumed = Some(value.to_string());
                }
            }
            _ => match word.strip_prefix("--resume=").or_else(|| word.strip_prefix("--session-id=")) {
                Some(value) => resumed = Some(value.to_string()),
                // Each word goes quoted, so none can be a command.
                None if option || after_option => command.push(shell_word(shell, word)),
                None => {}
            },
        }
        after_option = option;
    }
    let id = session.and_then(|session| session.id.clone()).or(resumed.filter(|id| session_id(id)));
    command.push("--resume".to_string());
    command.extend(id);
    Some(command.join(" "))
}

/// `value` as one word for `shell`, quoted only if it needs it.
fn shell_word(shell: Shell, value: &str) -> String {
    let safe = match shell {
        Shell::Posix => "-_=./:,@+%",
        // `,` makes a list and `@` a splat there.
        Shell::PowerShell => "-_=./:\\",
    };
    let plain = !value.is_empty() && value.chars().all(|ch| ch.is_ascii_alphanumeric() || safe.contains(ch));
    match shell {
        _ if plain => value.to_string(),
        Shell::Posix => shell_quote(value),
        // PowerShell's single quotes: nothing in them is special but `'`, doubled.
        Shell::PowerShell => format!("'{}'", value.replace('\'', "''")),
    }
}

/// A Windows command line's words: spaces split them except inside double
/// quotes, which go.
fn windows_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            _ => {
                word.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// `value` as one word for a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod restart_tests {
    use super::{ClaudeSession, Shell, agent_name, claude_title, claude_working, resume_command_in, resumed_session, windows_words};

    #[test]
    fn tells_the_agents_apart() {
        assert_eq!(agent_name("claude --model opus").as_deref(), Some("claude"));
        assert_eq!(agent_name("/opt/homebrew/bin/codex").as_deref(), Some("codex"));
        assert_eq!(agent_name("node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js").as_deref(), Some("claude"));
        assert_eq!(agent_name(r"C:\tools\claude.exe").as_deref(), Some("claude"));
        // A shell, or something that only mentions an agent further on.
        assert_eq!(agent_name("-zsh"), None);
        assert_eq!(agent_name("vim notes/claude"), None);
        assert!(claude_title("✳ Fix the login"));
        assert!(claude_title("⠂ Fix the login"));
        assert!(!claude_title("zsh"));
        assert_eq!(claude_working("✳ Fix the login"), Some(false));
        assert_eq!(claude_working("⠐ Fix the login"), Some(true));
        assert_eq!(claude_working("◐ Fix the login"), Some(true));
        assert_eq!(claude_working("◑ Fix the login"), Some(true));
        assert_eq!(claude_working(""), None);
        assert_eq!(claude_working("zsh"), None);
    }

    #[test]
    fn resumes_claude_code_with_its_options() {
        let resume = |args| resume_command_in(Shell::Posix, args, None);
        assert_eq!(resume("claude").as_deref(), Some("claude --resume"));
        assert_eq!(
            resume("claude --dangerously-skip-permissions").as_deref(),
            Some("claude --dangerously-skip-permissions --resume")
        );
        assert_eq!(resume("/Users/me/.local/bin/claude -c").as_deref(), Some("claude --resume"));
        assert_eq!(resume("claude --resume abc123").as_deref(), Some("claude --resume abc123"));
        assert_eq!(resume("claude -r abc123 --model opus").as_deref(), Some("claude --model opus --resume abc123"));
        assert_eq!(resume("claude --resume --model opus").as_deref(), Some("claude --model opus --resume"));
        assert_eq!(
            resume("node /opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js --model opus").as_deref(),
            Some("claude --model opus --resume")
        );
        assert_eq!(resume(r#""C:\Users\me\.local\bin\claude.exe" --model opus"#).as_deref(), Some("claude --model opus --resume"));
        assert_eq!(resume(r"node C:\npm\@anthropic-ai\claude-code\cli.js -c").as_deref(), Some("claude --resume"));
        assert_eq!(resume("-zsh"), None);
        assert_eq!(resume("vim claude.md"), None);
        // An id that isn't one isn't typed into the shell.
        assert_eq!(resume("claude --resume=x;rm").as_deref(), Some("claude --resume"));
        // `ps` lost the quotes of the prompt it began with: none of it is typed.
        assert_eq!(resume("claude fix login; then make clean").as_deref(), Some("claude --resume"));
        assert_eq!(
            resume("claude --allowedTools Bash(git:*) --model opus fix; make clean").as_deref(),
            Some("claude --allowedTools 'Bash(git:*)' --model opus --resume")
        );
        // An option in the prompt goes as an argument, never as a command.
        assert_eq!(resume("claude --verbose do it; rm -rf x").as_deref(), Some("claude --verbose do -rf x --resume"));
    }

    #[test]
    fn resumes_claude_code_in_powershell_with_its_quotes() {
        let resume = |args| resume_command_in(Shell::PowerShell, args, None);
        assert_eq!(
            windows_words(r#""C:\Program Files\claude.exe" --append-system-prompt "be brief" -c"#),
            [r"C:\Program Files\claude.exe", "--append-system-prompt", "be brief", "-c"]
        );
        assert_eq!(
            resume(r#"claude.exe --append-system-prompt "be brief" --add-dir "C:\My Projects" --model opus"#).as_deref(),
            Some(r"claude --append-system-prompt 'be brief' --add-dir 'C:\My Projects' --model opus --resume")
        );
        // A quote inside is doubled: the word can't end early and run the rest.
        assert_eq!(resume(r#"claude --name "x';calc;'""#).as_deref(), Some("claude --name 'x'';calc;''' --resume"));
        // `,` and `@` mean something to PowerShell.
        assert_eq!(resume("claude --tools a,b").as_deref(), Some("claude --tools 'a,b' --resume"));
        // The prompt it began with isn't typed again.
        assert_eq!(resume(r#"claude "fix it; then rm -rf x""#).as_deref(), Some("claude --resume"));
    }

    #[test]
    fn resumes_the_session_the_process_ran_in_its_config_folder() {
        let session = ClaudeSession {
            id: Some("d09e204f-44b6-45ed-8293-f4bf208351c4".to_string()),
            cwd: None,
            config_dir: Some("/Users/me/my claude's".to_string()),
        };
        assert_eq!(
            resume_command_in(Shell::Posix, "claude --dangerously-skip-permissions -c", Some(&session)).as_deref(),
            Some(r"CLAUDE_CONFIG_DIR='/Users/me/my claude'\''s' claude --dangerously-skip-permissions --resume d09e204f-44b6-45ed-8293-f4bf208351c4")
        );
        // The session file wins over the id it was started with (`--fork-session` makes another).
        assert_eq!(
            resume_command_in(Shell::Posix, "claude --resume abc --fork-session", Some(&session)).as_deref(),
            Some(r"CLAUDE_CONFIG_DIR='/Users/me/my claude'\''s' claude --resume d09e204f-44b6-45ed-8293-f4bf208351c4")
        );
        let unknown = ClaudeSession { id: None, cwd: None, config_dir: None };
        assert_eq!(resume_command_in(Shell::Posix, "claude", Some(&unknown)).as_deref(), Some("claude --resume"));
    }

    #[test]
    fn knows_the_session_a_restored_terminal_resumes() {
        assert_eq!(
            resumed_session("CLAUDE_CONFIG_DIR='/x' claude --model opus --resume d09e204f-44b6"),
            Some("d09e204f-44b6")
        );
        assert_eq!(resumed_session("claude --model opus --resume"), None);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn reads_a_process_environment() {
        // Its own: macOS hides the environment of the system's binaries.
        let pid = std::process::id();
        let home = std::env::var("HOME").unwrap();
        assert_eq!(crate::platform::process_env(pid, "HOME"), Some(home));
        assert_eq!(crate::platform::process_env(pid, "DEN_TEST_MISSING"), None);
    }
}
