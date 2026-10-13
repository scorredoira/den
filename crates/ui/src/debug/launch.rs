//! The launch file (`.den/debug.json`): which command starts the program,
//! where it listens, its targets and how it runs a test.

use serde::Deserialize;

use super::LAUNCH_FILE;

/// How the program is started and reached.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Launch {
    /// A shell command line that starts the program, run in a terminal.
    /// Without it, the debugger attaches to a program already running.
    /// `${port}` in it is a free port, which `port` then doesn't say.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    4444
}

#[derive(Deserialize)]
pub struct LaunchFile {
    #[serde(flatten)]
    pub launch: Launch,
    /// How the project runs a test, for the Run and Debug on each one's line.
    #[serde(default)]
    pub tests: Option<Tests>,
    /// Where the launch command runs the program (`${target}`): names of
    /// the project's, picked in the panel's toolbar.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Files from when there were several to choose from: the first one.
    #[serde(default)]
    configurations: Vec<Launch>,
}

/// `match` finds a test's declaration on a line, its first group being the
/// test's name; `run` and `debug` start it, with `${file}` and `${test}`.
/// `port` is where `debug` listens, unless it has `${port}`: apart from a
/// program that may be running, so a test is never attached to it.
#[derive(Clone, Debug, Deserialize)]
pub struct Tests {
    #[serde(rename = "match", deserialize_with = "regex_field")]
    pattern: regex::Regex,
    run: String,
    debug: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Tests {
    /// The test declared on `line`, if one is.
    pub fn name_in(&self, line: &str) -> Option<String> {
        let captures = self.pattern.captures(line)?;
        captures.get(1).map(|name| name.as_str().to_string())
    }

    pub fn command(&self, debug: bool, test: &str) -> String {
        let command = if debug { &self.debug } else { &self.run };
        command.replace("${test}", test)
    }
}

fn regex_field<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<regex::Regex, D::Error> {
    let pattern = String::deserialize(deserializer)?;
    regex::Regex::new(&pattern).map_err(serde::de::Error::custom)
}

pub fn parse_launch_file(text: &str) -> Result<LaunchFile, String> {
    let mut file: LaunchFile = serde_json::from_str(text).map_err(|err| format!("{LAUNCH_FILE}: {err}"))?;
    if file.launch.command.is_none() && !file.configurations.is_empty() {
        file.launch = file.configurations.remove(0);
    }
    Ok(file)
}

/// The target a launch runs on: the one picked, if the launch file still
/// lists it, else its first. None without targets.
pub(super) fn current_target<'a>(targets: &'a [String], chosen: Option<&str>) -> Option<&'a str> {
    targets
        .iter()
        .find(|target| Some(target.as_str()) == chosen)
        .or_else(|| targets.first())
        .map(String::as_str)
}

/// `${target}` in a command is the target picked, empty without targets.
pub fn with_target(command: &str, target: Option<&str>) -> String {
    command.replace("${target}", target.unwrap_or_default())
}

/// A configuration's `command` as it is run: `${file}` is the open file,
/// relative to the workspace, quoted for the shell when it needs it. With no
/// file open it is empty, and the program decides what to debug without one.
pub fn command_line(command: &str, file: Option<&str>) -> String {
    let Some(file) = file else {
        return command.replace("${file}", "");
    };
    let quoted = if file.chars().all(|c| c.is_alphanumeric() || "/._-+".contains(c)) {
        file.to_string()
    } else {
        format!("'{}'", file.replace('\'', r"'\''"))
    };
    command.replace("${file}", &quoted)
}
