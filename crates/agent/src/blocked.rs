//! Is Claude Code (or another agent) waiting for an answer? We look at the
//! bottom of the terminal screen once it stops producing output.
//!
//! The rules are herdr's Claude rules (`src/detect/manifests/claude.toml`,
//! Apache-2.0, © the herdr authors, https://github.com/herdrdev/herdr), simplified: permission dialogs,
//! forms with "Esc to cancel", requests from MCP servers.

/// The non-empty lines at the bottom of the screen, top to bottom.
pub fn is_blocked(lines: &[String]) -> bool {
    let whole = lines.join("\n").to_lowercase();
    // Whatever follows the last horizontal rule: the dialog at the bottom.
    let last_box = lines
        .iter()
        .rposition(|line| is_rule(line))
        .map(|ix| lines[ix + 1..].join("\n").to_lowercase())
        .unwrap_or_else(|| whole.clone());
    let has = |text: &str, needle: &str| text.contains(needle);
    let lines_lower: Vec<String> = lines.iter().map(|line| line.to_lowercase()).collect();
    let option = |prefixes: &[&str]| {
        lines_lower.iter().any(|line| {
            let line = line.trim_start().trim_start_matches('❯').trim_start();
            prefixes.iter().any(|prefix| line.starts_with(prefix))
        })
    };

    // Claude form: "Enter to confirm", or "Enter to select" with arrows.
    let navigate = ["tab/arrow keys to navigate", "arrow keys to navigate", "arrows to navigate", "↑/↓ to navigate", "↑↓ to navigate"];
    if has(&last_box, "esc to cancel")
        && (has(&last_box, "enter to confirm")
            || (has(&last_box, "enter to select") && navigate.iter().any(|hint| has(&last_box, hint))))
    {
        return true;
    }
    if has(&whole, "run a dynamic workflow?") && has(&whole, "esc to cancel") {
        return true;
    }
    // Request from an MCP server: "MCP server "x" requests your input".
    if has(&whole, "esc to cancel")
        && lines_lower.iter().any(|line| line.trim_start().starts_with("mcp server") && line.trim_end().ends_with("requests your input"))
    {
        return true;
    }
    // Permission: "Do you want to proceed?" with its numbered options.
    if has(&whole, "do you want to proceed?") && option(&["1. yes", "yes", "2. yes", "2. no", "3. no"]) {
        return true;
    }
    // Other dialogs from older versions; not with the input box empty.
    let empty_prompt = lines.iter().any(|line| line.trim() == "❯");
    if empty_prompt {
        return false;
    }
    (["do you want to", "would you like to"].iter().any(|ask| has(&whole, ask))
        && (has(&whole, "yes") || has(&whole, "❯")))
        || [
            "waiting for permission",
            "do you want to allow this connection?",
            "tab to amend",
            "ctrl+e to explain",
            "review your answers",
            "skip interview and plan immediately",
        ]
        .iter()
        .any(|text| has(&whole, text))
}

/// A horizontal rule like the ones separating Claude's boxes.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    line.chars().count() >= 10 && line.chars().all(|ch| matches!(ch, '─' | '━' | '╌' | '-'))
}

#[cfg(test)]
mod tests {
    use super::is_blocked;

    fn lines(text: &str) -> Vec<String> {
        text.lines().filter(|line| !line.trim().is_empty()).map(String::from).collect()
    }

    #[test]
    fn bash_permission_prompt() {
        let screen = "
● Bash(cargo test)
──────────────────────────────────────────
 Bash command
   cargo test -p agent
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and don't ask again for cargo test commands
   3. No, and tell Claude what to do differently (esc)
";
        assert!(is_blocked(&lines(screen)));
    }

    #[test]
    fn select_form() {
        let screen = "
──────────────────────────────────────────
 Which do you prefer?
 ❯ 1. Modal
   2. Tab
 Enter to select · Tab/Arrow keys to navigate · Esc to cancel
";
        assert!(is_blocked(&lines(screen)));
    }

    #[test]
    fn idle_prompt_and_work_are_not_blocked() {
        let idle = "
● Done: everything compiles.
──────────────────────────────────────────
❯
──────────────────────────────────────────
  ? for shortcuts
";
        assert!(!is_blocked(&lines(idle)));
        let working = "
✻ Compiling… (12s · esc to interrupt)
──────────────────────────────────────────
❯
";
        assert!(!is_blocked(&lines(working)));
        assert!(!is_blocked(&lines("santiago ~ $ ls\nCargo.toml  src\nsantiago ~ $")));
    }
}
