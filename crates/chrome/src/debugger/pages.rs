//! The pages and their scripts: Chrome's events, the targets it attaches,
//! the scripts it parses and their source maps.

use super::*;

impl Core {
    pub(super) fn on_cdp(&mut self, event: cdp::Event) -> Result<()> {
        let params = &event.params;
        match event.method.as_str() {
            "Target.attachedToTarget" => return self.on_attached(params),
            "Target.detachedFromTarget" => {
                if let Some(vm) = params
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .and_then(|session| self.sessions.get(session).copied())
                {
                    self.remove_page(vm);
                }
                return Ok(());
            }
            "Target.targetDestroyed" => {
                let target = values::str_of(params, "targetId");
                if let Some(vm) = self.targets.get(target).copied() {
                    self.remove_page(vm);
                }
                // closed, not a session detached (a navigation to another site can do that)
                if self.app_tabs.remove(target) && self.app_tabs.is_empty() {
                    self.app_closed = true;
                }
                return Ok(());
            }
            "Target.targetInfoChanged" => {
                let info = &params["targetInfo"];
                self.note_tab(values::str_of(info, "targetId"), values::str_of(info, "url"));
                if let Some(vm) = self.targets.get(values::str_of(info, "targetId")).copied()
                    && let Some(page) = self.pages.get_mut(&vm)
                {
                    page.url = values::str_of(info, "url").to_string();
                    page.title = values::str_of(info, "title").to_string();
                }
                return Ok(());
            }
            _ => {}
        }
        let Some(vm) = event.session.as_deref().and_then(|session| self.sessions.get(session).copied()) else {
            return Ok(());
        };
        match event.method.as_str() {
            "Runtime.executionContextCreated" => {
                let context = &params["context"];
                let id = context["id"].as_i64().context("a context without id")?;
                let debugged = self.debugged_url(values::str_of(context, "origin"));
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.insert(id, debugged);
                }
            }
            "Runtime.executionContextDestroyed" => {
                let id = params["executionContextId"].as_i64().context("a context without id")?;
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.remove(&id);
                    page.scripts.retain(|_, script| script.context != id);
                    let scripts = &page.scripts;
                    page.placed.retain(|_, placed| scripts.contains_key(&placed.script));
                }
            }
            "Runtime.executionContextsCleared" => {
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.contexts.clear();
                    page.scripts.clear();
                    page.placed.clear();
                }
            }
            "Debugger.scriptParsed" => self.on_script(vm, params)?,
            "Debugger.paused" => self.on_paused(vm, params.clone())?,
            "Debugger.resumed" => {
                if let Some(page) = self.pages.get_mut(&vm) {
                    page.paused = None;
                    if page.stop.take().is_some() {
                        // Chrome resumed on its own: a navigation, a closed tab
                        self.refs.retain(|_, entry| entry.vm != vm);
                        self.out.send(&json!({ "event": "resumed", "vm": vm }));
                    }
                }
            }
            "Overlay.inspectNodeRequested" => self.on_inspect_requested(vm, params)?,
            "Runtime.bindingCalled" if values::str_of(params, "name") == PICK_BINDING => {
                self.on_alt_click(vm)?
            }
            "Runtime.consoleAPICalled" => self.on_console(vm, params),
            "Runtime.exceptionThrown" => self.on_exception_thrown(vm, params),
            _ => {}
        }
        Ok(())
    }

    fn on_attached(&mut self, params: &Value) -> Result<()> {
        let session = values::str_of(params, "sessionId").to_string();
        let info = &params["targetInfo"];
        let waiting = params.get("waitingForDebugger").and_then(Value::as_bool).unwrap_or(false);
        if values::str_of(info, "type") != "page" {
            // workers, frames of other sites: not debugged
            if waiting {
                self.call_in_background(Some(session.clone()), "Runtime.runIfWaitingForDebugger", json!({}));
            }
            self.cdp.call(None, "Target.detachFromTarget", json!({ "sessionId": session }))?;
            return Ok(());
        }
        let target = values::str_of(info, "targetId").to_string();
        self.note_tab(&target, values::str_of(info, "url"));
        let vm = match self.targets.get(&target) {
            Some(vm) => *vm,
            None => {
                let vm = self.next_vm;
                self.next_vm += 1;
                vm
            }
        };
        if let Some(old) = self.pages.get(&vm) {
            self.sessions.remove(&old.session);
        }
        self.targets.insert(target.clone(), vm);
        self.sessions.insert(session.clone(), vm);
        self.pages.insert(
            vm,
            Page {
                target,
                session: session.clone(),
                url: values::str_of(info, "url").to_string(),
                title: values::str_of(info, "title").to_string(),
                contexts: HashMap::new(),
                scripts: HashMap::new(),
                placed: HashMap::new(),
                paused: None,
                stop: None,
                step: None,
                run_to: false,
                pause_request: None,
                last_exception: None,
            },
        );
        // Every page is enabled, whatever it shows: a page decides by the
        // origin of each script's context, which is known before the script
        // runs, so a navigation to a debugged host misses nothing.
        let state = if self.client.is_some() { self.exceptions.state() } else { "none" };
        let setup = [
            ("Runtime.enable", json!({})),
            ("Debugger.enable", json!({})),
            (
                "Debugger.setInstrumentationBreakpoint",
                json!({ "instrumentation": "beforeScriptWithSourceMapExecution" }),
            ),
            ("Debugger.setPauseOnExceptions", json!({ "state": state })),
        ];
        for (method, params) in setup {
            self.cdp.call(Some(&session), method, params).with_context(|| format!("set up vm {vm}"))?;
        }
        // every node keeps the stack that made it, for inspect; on a page held at its start
        // these can wait for it to run, so they go without waiting, before it is let run
        self.call_in_background(Some(session.clone()), "DOM.enable", json!({}));
        self.call_in_background(Some(session.clone()), "DOM.setNodeStackTracesEnabled", json!({ "enable": true }));
        // Alt+click in the page picks the element, as inspect does
        self.call_in_background(Some(session.clone()), "Runtime.addBinding", json!({ "name": PICK_BINDING }));
        // a script for every new document is only kept with the Page domain on
        self.call_in_background(Some(session.clone()), "Page.enable", json!({}));
        self.call_in_background(
            Some(session.clone()),
            "Page.addScriptToEvaluateOnNewDocument",
            json!({ "source": ALT_CLICK_SCRIPT }),
        );
        if !waiting {
            // a page already loaded gets it now
            self.call_in_background(
                Some(session.clone()),
                "Runtime.evaluate",
                json!({ "expression": ALT_CLICK_SCRIPT, "silent": true }),
            );
        }
        if waiting {
            self.call_in_background(Some(session), "Runtime.runIfWaitingForDebugger", json!({}));
        }
        Ok(())
    }

    fn remove_page(&mut self, vm: u64) {
        let Some(page) = self.pages.remove(&vm) else { return };
        self.sessions.remove(&page.session);
        self.targets.remove(&page.target);
        self.refs.retain(|_, entry| entry.vm != vm);
        if page.stop.is_some() {
            self.out.send(&json!({ "event": "resumed", "vm": vm }));
        }
    }

    fn on_script(&mut self, vm: u64, params: &Value) -> Result<()> {
        let id = values::str_of(params, "scriptId").to_string();
        let url = values::str_of(params, "url").to_string();
        let context = params["executionContextId"].as_i64().unwrap_or(0);
        let source_map = values::str_of(params, "sourceMapURL").to_string();
        let debugged = self.pages.get(&vm).is_some_and(|page| page.contexts.get(&context) == Some(&true));
        let mut script = Script {
            url: url.clone(),
            context,
            map: None,
            start_line: params["startLine"].as_u64().unwrap_or(0) as u32,
            start_column: params["startColumn"].as_u64().unwrap_or(0) as u32,
        };
        if debugged && !source_map.is_empty() {
            match self.load_map(&url, &source_map, values::str_of(params, "hash")) {
                Ok(map) => script.map = Some(map),
                Err(err) => self.report(&format!("the source map of {}: {err:#}", fetch::short(&url))),
            }
        }
        let files: Vec<String> = match &script.map {
            Some(map) => map.files().filter(|file| self.breakpoints.contains_key(*file)).cloned().collect(),
            None => Vec::new(),
        };
        if let Some(page) = self.pages.get_mut(&vm) {
            page.scripts.insert(id.clone(), script);
        }
        for file in files {
            for (line, result) in self.place_file(vm, &id, &file) {
                if let Err(error) = result {
                    eprintln!("chrome: breakpoint {file}:{line}: {error}");
                }
            }
        }
        Ok(())
    }

    /// The map of a script, `hash` its content's (CDP's): an inline map is
    /// the same while its URL is, and one from the server while the script
    /// is (a rebuild changes both); each page and reload loads the scripts
    /// again.
    fn load_map(&mut self, script_url: &str, map_url: &str, hash: &str) -> Result<Arc<SourceMap>> {
        let url = match map_url.starts_with("data:") {
            true => map_url.to_string(),
            false => fetch::resolve_url(script_url, map_url),
        };
        // Without the script's hash nothing tells when the map changed.
        let key = (map_url.starts_with("data:") || !hash.is_empty()).then(|| {
            let mut hasher = DefaultHasher::new();
            (&url, hash).hash(&mut hasher);
            hasher.finish()
        });
        if let Some(map) = key.and_then(|key| self.maps.get(key)) {
            return Ok(map);
        }
        let text = fetch::load(&url).with_context(|| format!("load {}", fetch::short(&url)))?;
        let map = Arc::new(SourceMap::parse(&text, &self.settings.root, &mut self.resolved)?);
        if let Some(key) = key {
            self.maps.insert(key, map.clone());
        }
        Ok(map)
    }

    /// An exception's stack with each `url:line:column` of a script with a
    /// map replaced by its original `file:line`.
    pub(super) fn map_stack(&self, vm: u64, stack: &str) -> String {
        let Some(page) = self.pages.get(&vm) else { return stack.to_string() };
        let mut lines = Vec::new();
        for line in stack.lines() {
            let mut mapped = line.to_string();
            for (id, script) in &page.scripts {
                if script.url.is_empty() || script.map.is_none() {
                    continue;
                }
                let pattern = format!("{}:", script.url);
                let Some(at) = line.find(&pattern) else { continue };
                let after = &line[at + pattern.len()..];
                let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
                let rest = &after[digits.len()..];
                let Some(rest) = rest.strip_prefix(':') else { continue };
                let column_digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
                let (Ok(line_number), Ok(column)) = (digits.parse::<u32>(), column_digits.parse::<u32>()) else {
                    continue;
                };
                let location = json!({ "scriptId": id, "lineNumber": line_number.saturating_sub(1), "columnNumber": column.saturating_sub(1) });
                let Some(original) = self.original_of(vm, &location) else { continue };
                let end = at + pattern.len() + digits.len() + 1 + column_digits.len();
                mapped = format!("{}{}:{}{}", &line[..at], original.file, original.line + 1, &line[end..]);
                break;
            }
            lines.push(mapped);
        }
        lines.join("\n")
    }

    fn on_console(&mut self, vm: u64, params: &Value) {
        if self.client.is_none() {
            return;
        }
        let context = params["executionContextId"].as_i64().unwrap_or(0);
        if self.pages.get(&vm).is_none_or(|page| page.contexts.get(&context) != Some(&true)) {
            return;
        }
        let args = params["args"].as_array().cloned().unwrap_or_default();
        let text = values::console_text(&args);
        let (file, line) = match params["stackTrace"]["callFrames"].get(0) {
            Some(frame) => self.location_of(vm, frame, values::str_of(frame, "url")),
            None => (String::new(), 0),
        };
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
    }

    fn on_exception_thrown(&mut self, vm: u64, params: &Value) {
        if self.client.is_none() {
            return;
        }
        let details = &params["exceptionDetails"];
        let context = details["executionContextId"].as_i64().unwrap_or(0);
        let Some(page) = self.pages.get_mut(&vm) else { return };
        if page.contexts.get(&context) != Some(&true) {
            return;
        }
        let description = details["exception"].get("description").and_then(Value::as_str).map(str::to_string);
        // the exception the VM stopped for, which the client has seen
        if description.is_some() && page.last_exception == description {
            page.last_exception = None;
            return;
        }
        let text = match &description {
            Some(description) => format!("Uncaught {}", self.map_stack(vm, description)),
            None => values::str_of(details, "text").to_string(),
        };
        let (file, line) = match details["stackTrace"]["callFrames"].get(0) {
            Some(frame) => self.location_of(vm, frame, values::str_of(frame, "url")),
            None => self.location_of(vm, details, values::str_of(details, "url")),
        };
        self.out.send(&json!({ "event": "output", "text": format!("{text}\n"), "file": file, "line": line }));
    }

    // ---- inspect ----
}

/// Source maps already parsed, by a hash of what identifies them; the
/// oldest go first.
#[derive(Default)]
pub(super) struct Maps {
    by_key: HashMap<u64, Arc<SourceMap>>,
    order: std::collections::VecDeque<u64>,
}

/// Maps kept at most: enough for the bundles of a few apps.
const MAX_MAPS: usize = 64;

impl Maps {
    fn get(&self, key: u64) -> Option<Arc<SourceMap>> {
        self.by_key.get(&key).cloned()
    }

    fn insert(&mut self, key: u64, map: Arc<SourceMap>) {
        if self.by_key.insert(key, map).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_MAPS {
            if let Some(oldest) = self.order.pop_front() {
                self.by_key.remove(&oldest);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_oldest_maps_go_first() {
        let mut maps = Maps::default();
        for key in 0..MAX_MAPS as u64 + 2 {
            maps.insert(key, Arc::default());
        }
        assert!(maps.get(0).is_none() && maps.get(1).is_none());
        assert!(maps.get(2).is_some() && maps.get(MAX_MAPS as u64 + 1).is_some());
        assert_eq!(maps.by_key.len(), MAX_MAPS);
    }
}
