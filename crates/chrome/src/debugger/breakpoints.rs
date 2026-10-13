//! Breakpoints, stepping and stops: placing the client's breakpoints in
//! the scripts their files were bundled into, and what the page does when
//! it stops there.

use super::*;

impl Core {
    pub(super) fn set_breakpoints(&mut self, file: &str, list: &[Value]) -> Vec<Value> {
        self.remove_placed(file);
        let old = self.breakpoints.remove(file).unwrap_or_default();
        let mut results = Vec::new();
        let mut kept = Vec::new();
        for item in list {
            let line = item.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            let text = |key: &str| {
                item.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
            };
            if line == 0 {
                results.push(json!({ "line": line, "error": "a breakpoint needs a line" }));
                continue;
            }
            let hit = match Hit::parse(&text("hit").unwrap_or_default()) {
                Ok(hit) => hit,
                Err(err) => {
                    results.push(json!({ "line": line, "error": err.to_string() }));
                    continue;
                }
            };
            // a breakpoint that stays keeps its hit count
            let hits = old.iter().find(|bp| bp.line == line).map(|bp| bp.hits).unwrap_or(0);
            kept.push(Breakpoint { line, condition: text("condition"), hit, log: text("log"), hits });
            results.push(json!({ "line": line }));
        }
        self.breakpoints.insert(file.to_string(), kept);

        // place them in every script that has the file; the first answer is
        // where each went
        let mut outcome: HashMap<u32, Result<u32, String>> = HashMap::new();
        let targets: Vec<(u64, String)> = self
            .pages
            .iter()
            .flat_map(|(vm, page)| {
                page.scripts
                    .iter()
                    .filter(|(_, script)| script.map.as_ref().is_some_and(|map| map.has_file(file)))
                    .map(move |(id, _)| (*vm, id.clone()))
            })
            .collect();
        for (vm, script) in targets {
            for (line, result) in self.place_file(vm, &script, file) {
                let entry = outcome.entry(line).or_insert_with(|| result.clone());
                if entry.is_err() && result.is_ok() {
                    *entry = result;
                }
            }
        }
        for result in &mut results {
            if result.get("error").is_some() {
                continue;
            }
            let line = result["line"].as_u64().unwrap_or(0) as u32;
            match outcome.get(&line) {
                Some(Ok(placed)) => result["line"] = json!(placed),
                Some(Err(error)) => result["error"] = json!(error),
                // no script has the file yet: it waits for one
                None => {}
            }
        }
        results
    }

    /// Places the breakpoints of a file in a script. Returns where each one
    /// went (1-based), by the line asked.
    pub(super) fn place_file(&mut self, vm: u64, script_id: &str, file: &str) -> Vec<(u32, Result<u32, String>)> {
        let wanted: Vec<(u32, Option<String>)> = match self.breakpoints.get(file) {
            Some(list) => list.iter().map(|bp| (bp.line, bp.condition.clone())).collect(),
            None => return Vec::new(),
        };
        wanted
            .into_iter()
            .map(|(line, condition)| {
                let result = self.place(vm, script_id, file, line, condition).map_err(|err| format!("{err:#}"));
                (line, result)
            })
            .collect()
    }

    fn place(&mut self, vm: u64, script_id: &str, file: &str, line: u32, condition: Option<String>) -> Result<u32> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let script = page.scripts.get(script_id).context("the script is gone")?;
        let map = script.map.clone().context("the script has no source map")?;
        let (original_line, (gen_line, gen_column)) =
            map.generated(file, line - 1).with_context(|| format!("no code on line {line} or after it"))?;
        let (line_number, column_number) = script.to_resource(gen_line, gen_column);
        let mut params =
            json!({ "location": { "scriptId": script_id, "lineNumber": line_number, "columnNumber": column_number } });
        if let Some(condition) = condition {
            params["condition"] = json!(condition);
        }
        let result = self.page_call(vm, "Debugger.setBreakpoint", params)?;
        let id = result.get("breakpointId").and_then(Value::as_str).context("setBreakpoint gave no id")?.to_string();
        // where Chrome put it, when that is still on the file
        let actual = result.get("actualLocation").and_then(|location| self.original_of(vm, location));
        let placed_line = match actual {
            Some(original) if original.file == file && original.line >= original_line => original.line,
            _ => original_line,
        };
        if let Some(page) = self.pages.get_mut(&vm) {
            page.placed.insert(id, Placed { file: file.to_string(), line, script: script_id.to_string() });
        }
        Ok(placed_line + 1)
    }

    /// Removes a file's breakpoints from Chrome, in every page.
    pub(super) fn remove_placed(&mut self, file: &str) {
        let mut removals = Vec::new();
        for (vm, page) in &mut self.pages {
            let ids: Vec<String> =
                page.placed.iter().filter(|(_, placed)| placed.file == file).map(|(id, _)| id.clone()).collect();
            for id in ids {
                page.placed.remove(&id);
                removals.push((*vm, id));
            }
        }
        for (vm, id) in removals {
            if let Err(err) = self.page_call(vm, "Debugger.removeBreakpoint", json!({ "breakpointId": id })) {
                eprintln!("chrome: remove breakpoint {id}: {err:#}");
            }
        }
    }

    pub(super) fn apply_exceptions(&mut self) {
        let state = if self.client.is_some() { self.exceptions.state() } else { "none" };
        let vms: Vec<u64> = self.pages.keys().copied().collect();
        for vm in vms {
            if let Err(err) = self.page_call(vm, "Debugger.setPauseOnExceptions", json!({ "state": state })) {
                self.report(&format!("vm {vm}: pause on exceptions: {err:#}"));
            }
        }
    }

    // ---- running and stepping ----

    /// Resumes a VM the client sees stopped: `resumed` first, then Chrome.
    pub(super) fn resume_stopped(&mut self, vm: u64, method: &str, params: Value) -> Result<()> {
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        page.stop = None;
        page.paused = None;
        self.refs.retain(|_, entry| entry.vm != vm);
        self.out.send(&json!({ "event": "resumed", "vm": vm }));
        self.page_call(vm, method, params)?;
        Ok(())
    }

    pub(super) fn start_step(&mut self, request: &Value, kind: StepKind) -> Result<()> {
        let vm = self.stopped_vm(request)?;
        let lines = self.frame_lines(vm)?;
        let (depth, at) = (lines.len(), lines.first().cloned().flatten());
        if let Some(page) = self.pages.get_mut(&vm) {
            page.step = Some(Step { kind, depth, at, lines, count: 0 });
        }
        self.resume_stopped(vm, step_method(kind), json!({}))
    }

    /// The original line of each frame of a paused page, frame 0 first.
    fn frame_lines(&self, vm: u64) -> Result<Vec<Option<Original>>> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let frames = paused.get("callFrames").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(frames.iter().map(|frame| self.original_of(vm, &frame["location"])).collect())
    }

    pub(super) fn pause(&mut self, request: &Value) -> Result<()> {
        if request.get("vm").is_some_and(|vm| !vm.is_null()) {
            let vm = self.vm_arg(request)?;
            let Some(page) = self.pages.get_mut(&vm) else { return Ok(()) };
            if page.stop.is_some() {
                return Ok(());
            }
            page.pause_request = Some(false);
            self.page_call(vm, "Debugger.pause", json!({}))?;
            return Ok(());
        }
        self.pause_any = true;
        let vms: Vec<u64> = self.pages.iter().filter(|(_, page)| page.stop.is_none() && self.listed(page)).map(|(vm, _)| *vm).collect();
        for vm in vms {
            if let Some(page) = self.pages.get_mut(&vm)
                && page.pause_request.is_none()
            {
                page.pause_request = Some(true);
            }
            self.page_call(vm, "Debugger.pause", json!({}))?;
        }
        Ok(())
    }

    pub(super) fn run_to(&mut self, request: &Value) -> Result<()> {
        let vm = self.stopped_vm(request)?;
        let file = self.file_arg(request)?;
        let line = request.get("line").and_then(Value::as_u64).context("line is missing")? as u32;
        if line == 0 {
            bail!("line is missing");
        }
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let location = page
            .scripts
            .iter()
            .find_map(|(id, script)| {
                let map = script.map.as_ref()?;
                let (_, (gen_line, gen_column)) = map.generated(&file, line - 1)?;
                let (line_number, column_number) = script.to_resource(gen_line, gen_column);
                Some(json!({ "scriptId": id, "lineNumber": line_number, "columnNumber": column_number }))
            })
            .with_context(|| format!("no code at {file}:{line} in this page"))?;
        if let Some(page) = self.pages.get_mut(&vm) {
            page.run_to = true;
        }
        self.resume_stopped(
            vm,
            "Debugger.continueToLocation",
            json!({ "location": location, "targetCallFrames": "any" }),
        )
    }

    // ---- Chrome's events ----

    pub(super) fn on_paused(&mut self, vm: u64, params: Value) -> Result<()> {
        let reason = values::str_of(&params, "reason").to_string();
        let script_debugged = {
            let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
            page.paused = Some(params.clone());
            let script_id = if reason == "instrumentation" {
                values::str_of(&params["data"], "scriptId").to_string()
            } else {
                params["callFrames"][0]["location"]["scriptId"].as_str().unwrap_or("").to_string()
            };
            page.scripts.get(&script_id).is_some_and(|script| page.contexts.get(&script.context) == Some(&true))
        };
        // Before a script with a source map runs: its breakpoints were placed
        // when it was parsed, an event that comes before this one.
        if reason == "instrumentation" || self.client.is_none() || !script_debugged {
            return self.resume_quietly(vm);
        }

        let (pause_request, step_active) = {
            let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
            page.last_exception = None;
            (page.pause_request.take(), page.step.is_some())
        };

        let hit_ids: Vec<String> = params["hitBreakpoints"]
            .as_array()
            .map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let placed = self.pages.get(&vm).and_then(|page| {
            hit_ids.iter().find_map(|id| page.placed.get(id)).map(|placed| (placed.file.clone(), placed.line))
        });
        if let Some((file, line)) = placed {
            match self.breakpoint_hit(vm, &file, line)? {
                true => return self.stop(vm, "breakpoint", None),
                false if step_active => return self.step_landed(vm),
                false => return self.resume_quietly(vm),
            }
        }

        if reason == "exception" || reason == "promiseRejection" {
            let data = &params["data"];
            let description = match data.get("description").and_then(Value::as_str) {
                Some(description) => description.to_string(),
                None => values::plain(data),
            };
            let stack = self.map_stack(vm, &description);
            if let Some(page) = self.pages.get_mut(&vm) {
                page.last_exception = Some(description.clone());
            }
            let exception = json!({ "message": values::first_line(&description), "stack": stack });
            return self.stop(vm, "exception", Some(exception));
        }

        if let Some(any) = pause_request {
            if any && !self.pause_any {
                // another VM took the pause asked for any
                return self.resume_quietly(vm);
            }
            if any {
                self.pause_any = false;
            }
            return self.stop(vm, "pause", None);
        }
        if step_active {
            return self.step_landed(vm);
        }
        if self.pages.get(&vm).is_some_and(|page| page.run_to) {
            return self.stop(vm, "step", None);
        }
        // a `debugger` statement
        self.stop(vm, "breakpoint", None)
    }

    /// A breakpoint was reached: counts it, prints its log message, and
    /// says whether the VM stops.
    fn breakpoint_hit(&mut self, vm: u64, file: &str, line: u32) -> Result<bool> {
        let Some(bp) = self.breakpoints.get_mut(file).and_then(|list| list.iter_mut().find(|bp| bp.line == line))
        else {
            return Ok(false);
        };
        bp.hits += 1;
        if let Some(hit) = bp.hit
            && !hit.matches(bp.hits)
        {
            return Ok(false);
        }
        let Some(message) = bp.log.clone() else { return Ok(true) };
        let text = self.interpolate(vm, &message);
        let (file, line) = self.top_location(vm);
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
        Ok(false)
    }

    /// A log message with each `{expr}` replaced by its value in frame 0.
    fn interpolate(&mut self, vm: u64, message: &str) -> String {
        let mut out = String::new();
        let mut rest = message;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else { break };
            out.push_str(&rest[..open]);
            let expr = &rest[open + 1..open + close];
            match self.evaluate_on_frame(vm, 0, expr, true) {
                Ok(object) => out.push_str(&values::plain(&object)),
                Err(err) => out.push_str(&format!("<{err:#}>")),
            }
            rest = &rest[open + close + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Where frame 0 is, as the client names it.
    fn top_location(&self, vm: u64) -> (String, u32) {
        let Some(top) =
            self.pages.get(&vm).and_then(|page| page.paused.as_ref()).and_then(|paused| paused["callFrames"].get(0))
        else {
            return (String::new(), 0);
        };
        self.location_of(vm, &top["location"], values::str_of(top, "url"))
    }

    /// A step landed somewhere: it stops there, or steps on.
    fn step_landed(&mut self, vm: u64) -> Result<()> {
        let lines = self.frame_lines(vm)?;
        let (depth, at) = (lines.len(), lines.first().cloned().flatten());
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        let Some(step) = page.step.as_mut() else { return self.stop(vm, "step", None) };
        step.count += 1;
        if step.count > MAX_STEPS {
            return self.stop(vm, "step", None);
        }
        let next = match &at {
            // code without a map: on until code with one
            None if step.kind == StepKind::Into => Some(StepKind::Into),
            None if step.kind == StepKind::Over && depth == step.depth => Some(StepKind::Over),
            None => Some(StepKind::Out),
            // in a function the line called
            Some(_) if depth > step.depth => match step.kind {
                StepKind::Into => None,
                _ => Some(StepKind::Out),
            },
            Some(original) if depth == step.depth => match step.kind {
                StepKind::Out => Some(StepKind::Out),
                // more code of the same line: a TS line can be several
                // statements of JS (a one-line loop is stepped through whole)
                kind if step.at.as_ref() == Some(original) => Some(kind),
                _ => None,
            },
            // back in a caller: still on the line that made the call, it
            // goes on to the next line, as sim does
            Some(original) => {
                let returned = step.depth - depth;
                if step.lines.get(returned).is_some_and(|line| line.as_ref() == Some(original)) {
                    step.lines.drain(..returned.min(step.lines.len()));
                    step.depth = depth;
                    step.at = at.clone();
                    if step.kind == StepKind::Out {
                        step.kind = StepKind::Over;
                    }
                    Some(step.kind)
                } else {
                    None
                }
            }
        };
        match next {
            Some(kind) => {
                page.paused = None;
                if let Err(err) = self.page_call(vm, step_method(kind), json!({})) {
                    self.report(&format!("vm {vm}: step: {err:#}"));
                    return self.stop(vm, "step", None);
                }
                Ok(())
            }
            None => self.stop(vm, "step", None),
        }
    }

    fn resume_quietly(&mut self, vm: u64) -> Result<()> {
        if let Some(page) = self.pages.get_mut(&vm) {
            page.paused = None;
        }
        self.page_call(vm, "Debugger.resume", json!({}))?;
        Ok(())
    }

    /// The VM stops where Chrome holds it: the client is told everything
    /// about it.
    fn stop(&mut self, vm: u64, reason: &str, exception: Option<Value>) -> Result<()> {
        let frames = self.frames(vm);
        let (locals, globals) = match self.scope_vars(vm, 0) {
            Ok(vars) => vars,
            Err(err) => {
                self.report(&format!("vm {vm}: the variables: {err:#}"));
                (Vec::new(), 0)
            }
        };
        let (file, line) =
            frames.first().map(|frame| (frame["file"].clone(), frame["line"].clone())).unwrap_or((json!(""), json!(0)));
        let mut stop = json!({
            "vm": vm,
            "reason": reason,
            "file": file,
            "line": line,
            "frames": frames,
            "locals": locals,
            "globals": globals,
        });
        if let Some(exception) = exception {
            stop["exception"] = exception;
        }
        let page = self.pages.get_mut(&vm).with_context(|| format!("no vm {vm}"))?;
        page.step = None;
        page.run_to = false;
        page.stop = Some(stop.clone());
        stop["event"] = json!("stopped");
        self.out.send(&stop);
        Ok(())
    }

    fn frames(&self, vm: u64) -> Vec<Value> {
        let Some(paused) = self.pages.get(&vm).and_then(|page| page.paused.as_ref()) else { return Vec::new() };
        let frames = paused["callFrames"].as_array().cloned().unwrap_or_default();
        frames
            .iter()
            .map(|frame| {
                let (file, line) = self.location_of(vm, &frame["location"], values::str_of(frame, "url"));
                let name = values::str_of(frame, "functionName");
                let name = if name.is_empty() { "(anonymous)" } else { name };
                json!({ "function": name, "file": file, "line": line })
            })
            .collect()
    }

    /// A CDP location as the client names it: the original file and line,
    /// or the generated URL and line when it has no mapping.
    pub(super) fn location_of(&self, vm: u64, location: &Value, url: &str) -> (String, u32) {
        if let Some(original) = self.original_of(vm, location) {
            return (original.file, original.line + 1);
        }
        let url = if url.is_empty() {
            self.pages
                .get(&vm)
                .and_then(|page| page.scripts.get(values::str_of(location, "scriptId")))
                .map(|script| script.url.clone())
                .unwrap_or_default()
        } else {
            url.to_string()
        };
        (url, location["lineNumber"].as_u64().unwrap_or(0) as u32 + 1)
    }

    pub(super) fn original_of(&self, vm: u64, location: &Value) -> Option<Original> {
        let page = self.pages.get(&vm)?;
        let script = page.scripts.get(values::str_of(location, "scriptId"))?;
        let map = script.map.as_ref()?;
        let line = location["lineNumber"].as_u64()? as u32;
        let column = location["columnNumber"].as_u64().unwrap_or(0) as u32;
        let (line, column) = script.to_map(line, column)?;
        map.original(line, column)
    }
}

fn step_method(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Over => "Debugger.stepOver",
        StepKind::Into => "Debugger.stepInto",
        StepKind::Out => "Debugger.stepOut",
    }
}
