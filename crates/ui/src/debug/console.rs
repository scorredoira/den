//! The console: what the program prints, what's evaluated in it and
//! what it said.

use super::*;

impl Debugger {
    /// Adds a line to the console, dropping the oldest ones past `MAX_CONSOLE`
    /// (a few hundred at a time).
    pub(super) fn push_console(&mut self, line: ConsoleLine) {
        self.console.push(line);
        self.console_written += 1;
        if self.console.len() > MAX_CONSOLE + 500 {
            let drop = self.console.len() - MAX_CONSOLE;
            self.console.drain(..drop);
            self.console_dropped += drop;
        }
    }

    pub(super) fn info(&mut self, text: String, cx: &mut Context<Self>) {
        if !text.is_empty() {
            self.push_console(ConsoleLine::Info(text));
            cx.notify();
        }
    }

    pub(super) fn submit_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expr = self.console_input.read(cx).value().trim().to_string();
        if expr.is_empty() {
            return;
        }
        self.console_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.history.retain(|old| *old != expr);
        self.history.push(expr.clone());
        self.history_at = None;
        self.evaluate_in_console(expr, cx);
    }

    /// Writes `expr` and its value in the console.
    pub fn evaluate_in_console(&mut self, expr: String, cx: &mut Context<Self>) {
        self.push_console(ConsoleLine::Input(expr.clone()));
        let Some((vm, serial)) = self.halted().and_then(|vm| Some((vm, self.stops.get(&vm)?.serial))) else {
            self.push_console(ConsoleLine::Error("Nothing is stopped to evaluate in".into()));
            cx.notify();
            return;
        };
        let assigns = is_assignment(&expr);
        self.evaluate(expr, move |this, result, cx| {
            match result {
                Ok(mut var) => {
                    // the VM went on before the answer: its ref names nothing
                    if !this.stops.get(&vm).is_some_and(|stop| stop.serial == serial && !stop.resumed) {
                        var.reference = 0;
                    }
                    this.push_console(ConsoleLine::Result(var, vm));
                }
                Err(error) => this.push_console(ConsoleLine::Error(error)),
            }
            if assigns {
                this.reload_frame();
            }
            cx.notify();
        });
        cx.notify();
    }

    /// Up and down in the console go through what was evaluated.
    pub fn console_history(&mut self, up: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_empty() {
            return;
        }
        let at = match (self.history_at, up) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => None,
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) if at + 1 < self.history.len() => Some(at + 1),
            (Some(_), false) => None,
        };
        self.history_at = at;
        let text = at.map(|at| self.history[at].clone()).unwrap_or_default();
        self.console_input.update(cx, |input, cx| input.set_value(text, window, cx));
    }
}
