//! Values: the variables of the frame, their children, the watches,
//! hovering a name, and changing a value.

use super::*;

impl Debugger {
    /// Shows the value of `expr` by `anchor`; an expression without a value
    /// (a function's name, a type) shows nothing. Over another name's card,
    /// it waits a little: the pointer may be on its way to that card.
    pub fn show_hover(&mut self, expr: String, anchor: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.hover_inside && self.hover.is_some() {
            return;
        }
        if self.hover.as_ref().is_some_and(|hover| hover.var.name == expr && hover.anchor == anchor) {
            self.hover_change = None;
            return;
        }
        if self.hover.is_none() {
            self.hover_change = None;
            self.evaluate_hover(expr, anchor);
            return;
        }
        self.change_hover(Some((expr, anchor)), cx);
    }

    pub(super) fn evaluate_hover(&mut self, expr: String, anchor: Bounds<Pixels>) {
        let serial = self.serial;
        self.evaluate(expr.clone(), move |this, result, cx| {
            if this.serial != serial {
                return;
            }
            this.expanded.retain(|key| key != "h" && !key.starts_with("h/"));
            this.hover = result.ok().map(|var| HoverValue { var: Var { name: expr, ..var }, anchor });
            // open, as what's in it is what one hovers for
            if let Some(reference) = this.hover.as_ref().map(|hover| hover.var.reference).filter(|&reference| reference != 0) {
                this.expanded.insert("h".into());
                if !this.children.contains_key(&reference) {
                    this.fetch(reference, 0);
                }
            }
            cx.notify();
        });
    }

    /// The pointer is on no name: the card goes, after the grace.
    pub fn leave_hover(&mut self, cx: &mut Context<Self>) {
        if self.hover.is_none() {
            self.hover_change = None;
        } else if !self.hover_inside {
            self.change_hover(None, cx);
        }
    }

    /// The pointer moved, over the card or its name (`inside`) or not.
    pub fn track_hover(&mut self, inside: bool, cx: &mut Context<Self>) {
        self.hover_inside = inside;
        if inside {
            self.hover_change = None;
        } else if self.hover_change.is_none() {
            self.change_hover(None, cx);
        }
    }

    pub(super) fn change_hover(&mut self, to: Option<(String, Bounds<Pixels>)>, cx: &mut Context<Self>) {
        if self.hover_change.as_ref().is_some_and(|(pending, _)| *pending == to) {
            return;
        }
        let next = to.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOVER_GRACE).await;
            this.update(cx, |this, cx| {
                this.hover_change = None;
                if this.hover_inside || this.hover.is_none() {
                    return;
                }
                // the card stays until the other value comes, or doesn't
                match next {
                    Some((expr, anchor)) => this.evaluate_hover(expr, anchor),
                    None => {
                        this.hover = None;
                        cx.notify();
                    }
                }
            })
            .ok();
        });
        self.hover_change = Some((to, task));
    }

    pub fn clear_hover(&mut self, cx: &mut Context<Self>) {
        self.hover_change = None;
        self.hover_inside = false;
        if self.hover.take().is_some() {
            cx.notify();
        }
    }

    pub fn toggle_expanded(&mut self, key: String, reference: u64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
            // opening it again asks again for children it couldn't have
            if self.children.get(&reference).is_some_and(|children| children.vars.is_empty() && children.error.is_some()) {
                self.children.remove(&reference);
            }
            if !self.children.contains_key(&reference) {
                self.fetch(reference, 0);
            }
        }
        cx.notify();
    }

    pub(super) fn fetch(&mut self, reference: u64, start: u64) {
        if reference == 0 || !self.loading.insert(reference) {
            return;
        }
        let (generation, serial) = (self.generation, self.serial);
        self.send("expand", json!({ "ref": reference, "start": start, "count": PAGE }), move |this, result, cx| {
            // a stop since cleared what was loading, and the ref may name another value now
            if this.generation != generation || this.serial != serial {
                return;
            }
            this.loading.remove(&reference);
            let children = this.children.entry(reference).or_default();
            match result {
                Ok(body) => {
                    let vars: Vec<Var> = protocol::field(&body, "vars").unwrap_or_default();
                    children.more = vars.len() as u64 >= PAGE;
                    children.error = None;
                    children.vars.truncate(start as usize);
                    children.vars.extend(vars);
                    this.refetch();
                }
                Err(error) => children.error = Some(error),
            }
            cx.notify();
        });
    }

    /// Loads the next page of an array.
    pub fn fetch_more(&mut self, reference: u64) {
        let start = self.children.get(&reference).map_or(0, |children| children.vars.len() as u64);
        self.fetch(reference, start);
    }

    /// Asks for the children of every expanded value that hasn't got them,
    /// so values expanded before a stop stay expanded after it.
    pub(super) fn refetch(&mut self) {
        let mut wanted = Vec::new();
        let mut walk = Vec::new();
        for var in &self.locals {
            walk.push((format!("l/{}", var.name), var.clone()));
        }
        if self.globals != 0 && self.expanded.contains("g") {
            if let Some(children) = self.children.get(&self.globals) {
                for var in &children.vars {
                    walk.push((format!("g/{}", var.name), var.clone()));
                }
            } else {
                wanted.push(self.globals);
            }
        }
        for (ix, watch) in self.watches.iter().enumerate() {
            if let Some(Ok(var)) = &watch.result {
                walk.push((format!("w{ix}"), var.clone()));
            }
        }
        for (ix, line) in self.console.iter().enumerate() {
            if let ConsoleLine::Result(var, _) = line {
                walk.push((format!("c{}", self.console_dropped + ix), var.clone()));
            }
        }
        if let Some(hover) = &self.hover {
            walk.push(("h".into(), hover.var.clone()));
        }
        while let Some((key, var)) = walk.pop() {
            if var.reference == 0 || !self.expanded.contains(&key) {
                continue;
            }
            match self.children.get(&var.reference) {
                Some(children) => {
                    for child in &children.vars {
                        walk.push((format!("{key}/{}", child.name), child.clone()));
                    }
                }
                None => wanted.push(var.reference),
            }
        }
        for reference in wanted {
            self.fetch(reference, 0);
        }
    }

    /// Evaluates in the selected frame of the focused VM.
    pub fn evaluate(
        &mut self,
        expr: String,
        reply: impl FnOnce(&mut Debugger, Result<Var, String>, &mut Context<Debugger>) + 'static,
    ) {
        let Some(vm) = self.halted() else {
            return;
        };
        self.send("eval", json!({ "vm": vm, "frame": self.frame, "expr": expr }), move |this, result, cx| {
            let result = result.and_then(|body| protocol::decode::<Var>(Value::Object(body)));
            reply(this, result, cx);
        });
    }

    pub(super) fn evaluate_watches(&mut self, cx: &mut Context<Self>) {
        for ix in 0..self.watches.len() {
            self.evaluate_watch(ix, cx);
        }
    }

    pub(super) fn evaluate_watch(&mut self, ix: usize, _cx: &mut Context<Self>) {
        let expr = self.watches[ix].expr.clone();
        let serial = self.serial;
        self.evaluate(expr.clone(), move |this, result, cx| {
            if this.serial != serial {
                return;
            }
            if let Some(watch) = this.watches.get_mut(ix).filter(|watch| watch.expr == expr) {
                watch.result = Some(result);
                this.refetch();
                cx.notify();
            }
        });
    }

    pub fn add_watch(&mut self, expr: String, cx: &mut Context<Self>) {
        if expr.is_empty() || self.watches.iter().any(|watch| watch.expr == expr) {
            return;
        }
        self.watches.push(Watch { expr, result: None });
        self.evaluate_watch(self.watches.len() - 1, cx);
        self.save(cx);
        cx.notify();
    }

    pub fn remove_watch(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.watches.len() {
            self.watches.remove(ix);
            self.save(cx);
            cx.notify();
        }
    }

    pub fn remove_all_watches(&mut self, cx: &mut Context<Self>) {
        self.watches.clear();
        self.save(cx);
        cx.notify();
    }

    /// After an assignment: the variables, children and watches again.
    pub(super) fn reload_frame(&mut self) {
        let frame = self.frame;
        let Some(vm) = self.focus else {
            return;
        };
        self.send("frame", json!({ "vm": vm, "frame": frame }), move |this, result, cx| {
            if let Ok(body) = result
                && this.focus == Some(vm)
                && this.frame == frame
            {
                this.locals = protocol::field(&body, "locals").unwrap_or_default();
                this.globals = protocol::field(&body, "globals").unwrap_or(0);
                this.children.clear();
                this.loading.clear();
                this.refetch();
                this.evaluate_watches(cx);
                cx.emit(DebugEvent::Marks);
                cx.notify();
            }
        });
    }

    pub fn start_value_edit(&mut self, key: String, target: String, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx);
            input.set_value(value, window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => {
                let value = input.read(cx).value().trim().to_string();
                this.finish_value_edit(Some(value), cx);
            }
            InputEvent::Blur => this.finish_value_edit(None, cx),
            _ => {}
        });
        self._subscriptions.push(subscription);
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.value_edit = Some(ValueEdit { key, target, input });
        cx.notify();
    }

    pub(super) fn finish_value_edit(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        let Some(edit) = self.value_edit.take() else {
            return;
        };
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            self.evaluate(format!("{} = {value}", edit.target), |this, result, _| {
                if let Err(error) = result {
                    this.push_console(ConsoleLine::Error(error));
                }
                this.reload_frame();
            });
        }
        cx.notify();
    }
}
