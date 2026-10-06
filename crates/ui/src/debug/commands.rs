//! What `den debug` does with the debugger (see `app::commands`): the same
//! things as the keys and the panel, and its state as JSON, so an agent can
//! drive a session and read it.

use super::*;

/// Lines of the console that `den debug state` gives, the last ones.
const STATE_CONSOLE: usize = 20;

/// What `den debug wait` waits for.
#[derive(Clone, Copy, PartialEq)]
pub enum WaitFor {
    /// A VM stopped (and not resuming).
    Stop,
    /// The session connected to the program.
    Connected,
    /// No session.
    Idle,
}

impl WaitFor {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "stop" => Some(Self::Stop),
            "connected" => Some(Self::Connected),
            "idle" => Some(Self::Idle),
            _ => None,
        }
    }
}

impl Debugger {
    /// Whether what `wait` waits for has happened. A session that failed
    /// while waiting for a stop or a connection has too: there is nothing
    /// more to wait for, and the state says why.
    pub fn reached(&self, what: WaitFor) -> bool {
        match what {
            WaitFor::Stop => self.stopped_now() || self.status == Status::Idle,
            WaitFor::Connected => self.status == Status::Connected || self.status == Status::Idle,
            WaitFor::Idle => self.status == Status::Idle,
        }
    }

    /// A VM is stopped and not resuming, nor asked to.
    fn stopped_now(&self) -> bool {
        self.stops.values().any(|stop| !stop.resumed && !stop.going)
    }

    /// `den debug break`: a breakpoint at `line` (0-based) of `path`, if
    /// there isn't one.
    pub fn set_breakpoint(&mut self, path: &Path, line: u32, cx: &mut Context<Self>) {
        if self.breakpoints.at(path, line).is_none() {
            self.toggle_breakpoint(path, line, cx);
        }
    }

    /// `den debug state`: the session, its stops, the focused one in full,
    /// the breakpoints and the end of the console.
    pub fn state(&self) -> Value {
        let status = match &self.status {
            Status::Idle => "idle".to_string(),
            Status::Connecting(what) => format!("connecting: {what}"),
            Status::Connected => "connected".to_string(),
        };
        let stopped: Vec<u64> = self.stops.iter().filter(|(_, stop)| !stop.resumed).map(|(vm, _)| *vm).collect();

        let focus = self.focus.and_then(|vm| self.stops.get(&vm)).filter(|stop| !stop.resumed).map(|stop| {
            let frames: Vec<String> = stop
                .stop
                .frames
                .iter()
                .map(|frame| format!("{} {}:{}", frame.function, frame.file, frame.line))
                .collect();
            let locals: Map<String, Value> =
                self.locals.iter().map(|var| (var.name.clone(), Value::String(var.value.clone()))).collect();
            let mut out = json!({
                "vm": stop.stop.vm,
                "reason": stop.stop.reason,
                "file": stop.stop.file,
                "line": stop.stop.line,
                "frame": self.frame,
                "frames": frames,
                "locals": locals,
            });
            if let Some(exc) = &stop.stop.exception {
                out["exception"] = json!({ "message": exc.message, "stack": exc.stack });
            }
            out
        });

        let mut breakpoints = Vec::new();
        for (path, list) in self.breakpoints.files() {
            let file = self.program_path(path);
            for bp in list {
                let mut out = json!({ "file": file, "line": bp.line + 1, "enabled": bp.enabled });
                if let Some(error) = &bp.error {
                    out["error"] = Value::String(error.clone());
                }
                breakpoints.push(out);
            }
        }

        let console: Vec<String> = self.console[self.console.len().saturating_sub(STATE_CONSOLE)..]
            .iter()
            .map(|line| match line {
                ConsoleLine::Info(text) => text.clone(),
                ConsoleLine::Output { text, .. } => text.clone(),
                ConsoleLine::Input(text) => format!("> {text}"),
                ConsoleLine::Result(var, _) => var.value.clone(),
                ConsoleLine::Error(text) => format!("error: {text}"),
            })
            .collect();

        let mut out = json!({
            "status": status,
            "running": self.running,
            "stopped": stopped,
            "breakpoints": breakpoints,
            "console": console,
        });
        if let Some(focus) = focus {
            out["focus"] = focus;
        }
        if let Some(error) = &self.launch_error {
            out["launchError"] = Value::String(error.clone());
        }
        if let Some(command) = &self.ran {
            out["command"] = Value::String(command.clone());
        }
        if let Some(page) = &self.page {
            out["page"] = Value::String(page.clone());
        }
        if let Some((file, line)) = &self.revealed {
            out["revealed"] = json!({ "file": file, "line": line });
        }
        out
    }
}
