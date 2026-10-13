//! Inspecting an element: picking it on the page (or Alt+click), and the
//! line of the root's files that made it.

use super::*;

impl Core {
    /// Picking an element on the debugged pages, with Chrome's own highlight.
    pub(super) fn set_inspecting(&mut self, on: bool) -> Result<()> {
        let vms: Vec<u64> = self.pages.iter().filter(|(_, page)| self.listed(page)).map(|(vm, _)| *vm).collect();
        for vm in vms {
            if on {
                self.page_call(vm, "Overlay.enable", json!({}))?;
                self.page_call(
                    vm,
                    "Overlay.setInspectMode",
                    json!({
                        "mode": "searchForNode",
                        "highlightConfig": {
                            "showInfo": true,
                            "contentColor": { "r": 111, "g": 168, "b": 220, "a": 0.66 },
                        },
                    }),
                )?;
            } else {
                self.page_call(vm, "Overlay.setInspectMode", json!({ "mode": "none", "highlightConfig": {} }))?;
            }
        }
        Ok(())
    }

    /// The person picked an element: the line that made it.
    pub(super) fn on_inspect_requested(&mut self, vm: u64, params: &Value) -> Result<()> {
        self.set_inspecting(false)?;
        let backend = params["backendNodeId"].as_i64().context("a pick without a node")?;
        self.page_call(vm, "DOM.getDocument", json!({ "depth": 0 }))?;
        let pushed = self.page_call(vm, "DOM.pushNodesByBackendIdsToFrontend", json!({ "backendNodeIds": [backend] }))?;
        let node = pushed["nodeIds"][0].as_i64().context("DOM.pushNodesByBackendIdsToFrontend gave no node")?;
        self.pick(vm, node)?;
        Ok(())
    }

    /// The person Alt+clicked an element of the page: the line that made it,
    /// with the element highlighted a moment.
    pub(super) fn on_alt_click(&mut self, vm: u64) -> Result<()> {
        let node = self.node_of(vm, "window.__denPicked")?;
        let session = self.pages.get(&vm).map(|page| page.session.clone()).with_context(|| format!("no vm {vm}"))?;
        self.page_call(vm, "Overlay.enable", json!({}))?;
        self.page_call(
            vm,
            "Overlay.highlightNode",
            json!({ "nodeId": node, "highlightConfig": { "contentColor": { "r": 111, "g": 168, "b": 220, "a": 0.66 } } }),
        )?;
        let cdp = self.cdp.clone();
        let out = self.out.clone();
        let hide = thread::Builder::new().name("chrome-highlight".into()).spawn(move || {
            thread::sleep(Duration::from_millis(600));
            if let Err(err) = cdp.call(Some(&session), "Overlay.hideHighlight", json!({})) {
                out.send(&json!({ "event": "output", "text": format!("hide the highlight: {err:#}\n"), "file": "", "line": 0 }));
            }
        });
        if let Err(err) = hide {
            self.report(&format!("hide the highlight: start its thread: {err}"));
        }
        self.pick(vm, node)?;
        Ok(())
    }

    /// The node of the element an expression gives, evaluated in the page.
    pub(super) fn node_of(&mut self, vm: u64, expr: &str) -> Result<i64> {
        // run with every pause skipped: a breakpoint in what it calls would stop the
        // page while this thread waits for the answer
        self.page_call(vm, "Debugger.setSkipAllPauses", json!({ "skip": true }))?;
        let result = self.page_call(vm, "Runtime.evaluate", json!({ "expression": expr, "silent": true }));
        self.page_call(vm, "Debugger.setSkipAllPauses", json!({ "skip": false }))?;
        let result = result?;
        if let Some(details) = result.get("exceptionDetails") {
            bail!("{expr}: {}", values::exception_text(details));
        }
        let object = &result["result"];
        if values::str_of(object, "subtype") != "node" {
            bail!("{expr} is not an element: {}", values::preview(object));
        }
        let object_id = values::str_of(object, "objectId").to_string();
        self.page_call(vm, "DOM.getDocument", json!({ "depth": 0 }))?;
        let node = self.page_call(vm, "DOM.requestNode", json!({ "objectId": object_id }))?;
        node["nodeId"].as_i64().context("DOM.requestNode gave no node")
    }

    /// The line that made a node, or its nearest ancestor that a script made:
    /// the first frame of its creation stack in a file not skipped. It is
    /// revealed and the whole stack goes to the console; answers
    /// `{file, line, stack}`, empty when no script made it.
    pub(super) fn pick(&mut self, vm: u64, node: i64) -> Result<Value> {
        let mut node = node;
        let mut frames = Vec::new();
        for _ in 0..1000 {
            let traces = self.page_call(vm, "DOM.getNodeStackTraces", json!({ "nodeId": node }))?;
            let mut trace = traces.get("creation").cloned();
            while let Some(current) = trace {
                for frame in current["callFrames"].as_array().cloned().unwrap_or_default() {
                    let name = values::str_of(&frame, "functionName");
                    let name = if name.is_empty() { "(anonymous)" } else { name }.to_string();
                    if let Some(original) = self.original_of(vm, &frame) {
                        frames.push((original.file, original.line + 1, name));
                    }
                }
                trace = current.get("parent").cloned();
            }
            if !frames.is_empty() {
                break;
            }
            // made by the HTML parser: the nearest ancestor a script made
            let resolved = self.page_call(vm, "DOM.resolveNode", json!({ "nodeId": node }))?;
            let object = values::str_of(&resolved["object"], "objectId").to_string();
            let parent = self.page_call(
                vm,
                "Runtime.callFunctionOn",
                json!({ "functionDeclaration": "function() { return this.parentNode }", "objectId": object, "silent": true }),
            )?;
            let parent = values::str_of(&parent["result"], "objectId").to_string();
            if parent.is_empty() {
                break;
            }
            let requested = self.page_call(vm, "DOM.requestNode", json!({ "objectId": parent }))?;
            node = requested["nodeId"].as_i64().context("DOM.requestNode gave no node")?;
        }
        if frames.is_empty() {
            self.out.send(&json!({ "event": "output", "text": "No script made this element.\n", "file": "", "line": 0 }));
            return Ok(json!({}));
        }
        let chosen = frames
            .iter()
            .find(|(file, _, _)| !self.settings.inspect_skip.iter().any(|glob| glob_matches(glob, file)))
            .unwrap_or(&frames[0])
            .clone();
        let stack: Vec<String> = frames.iter().map(|(file, line, name)| format!("{file}:{line} {name}")).collect();
        self.out.send(&json!({ "event": "reveal", "file": chosen.0, "line": chosen.1 }));
        let text = format!("{}\n", stack.join("\n"));
        self.out.send(&json!({ "event": "output", "text": text, "file": chosen.0, "line": chosen.1 }));
        Ok(json!({ "file": chosen.0, "line": chosen.1, "stack": stack }))
    }

    // ---- values ----
    // ---- values ----
}

/// Whether a file of the root matches a glob: `*` is any text within a
/// folder, `**` any number of folders.
fn glob_matches(glob: &str, file: &str) -> bool {
    let glob: Vec<&str> = glob.split('/').filter(|part| !part.is_empty()).collect();
    let file: Vec<&str> = file.split('/').filter(|part| !part.is_empty()).collect();
    segments_match(&glob, &file)
}

fn segments_match(glob: &[&str], file: &[&str]) -> bool {
    match glob.first() {
        None => file.is_empty(),
        Some(&"**") => (0..=file.len()).any(|skip| segments_match(&glob[1..], &file[skip..])),
        Some(part) => !file.is_empty() && segment_matches(part, file[0]) && segments_match(&glob[1..], &file[1..]),
    }
}

fn segment_matches(glob: &str, name: &str) -> bool {
    match glob.split_once('*') {
        None => glob == name,
        Some((before, after)) => name.strip_prefix(before).is_some_and(|rest| {
            // Only where a character starts: `*` may stand for an `ñ`.
            rest.char_indices().map(|(at, _)| at).chain([rest.len()]).any(|at| segment_matches(after, &rest[at..]))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_matches("client/scl/widgets/**", "client/scl/widgets/button.ts"));
        assert!(glob_matches("client/scl/widgets/**", "client/scl/widgets/a/b.ts"));
        assert!(!glob_matches("client/scl/widgets/**", "client/scl/platform/boot.ts"));
        assert!(glob_matches("src/util.ts", "src/util.ts"));
        assert!(glob_matches("src/*.ts", "src/util.ts"));
        assert!(!glob_matches("src/*.ts", "src/a/util.ts"));
        assert!(glob_matches("**/ui*.ts", "client/scl/uikit.ts"));
        assert!(!glob_matches("src/u*l.ts", "src/util.tsx"));
        assert!(glob_matches("src/*.ts", "src/añadir.ts"));
        assert!(!glob_matches("src/*ñ.ts", "src/añadir.ts"));
    }
}
