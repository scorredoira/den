//! Variables and values: a stop's scopes, the children of a value, and
//! evaluating an expression on a frame.

use super::*;

impl Core {
    /// The variables of a frame: its own, its blocks' and its closures', and
    /// a ref to its module's.
    pub(super) fn scope_vars(&mut self, vm: u64, frame: usize) -> Result<(Vec<Value>, u64)> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let call_frame = paused["callFrames"].get(frame).with_context(|| format!("no frame {frame}"))?.clone();
        let script_has_map = page
            .scripts
            .get(values::str_of(&call_frame["location"], "scriptId"))
            .is_some_and(|script| script.map.is_some());

        let mut groups: Vec<Vec<Value>> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut module: Option<Value> = None;
        let mut module_rank = 0;
        let this = &call_frame["this"];
        let this_shown = values::str_of(this, "type") == "object"
            && !matches!(values::str_of(this, "subtype"), "null")
            && values::str_of(this, "className") != "Window";
        for scope in call_frame["scopeChain"].as_array().cloned().unwrap_or_default() {
            let kind = values::str_of(&scope, "type");
            // The bundle's wrapper (code of no source) holds the modules'
            // variables: that is the frame's module, not its locals.
            let wrapper = kind == "closure"
                && script_has_map
                && scope.get("startLocation").is_some_and(|start| self.original_of(vm, start).is_none());
            let rank = match kind {
                "closure" if wrapper => 3,
                "module" => 2,
                "script" => 1,
                _ => 0,
            };
            if rank > 0 {
                if rank > module_rank {
                    module = Some(scope["object"].clone());
                    module_rank = rank;
                }
                continue;
            }
            if !matches!(kind, "local" | "block" | "closure" | "catch" | "with" | "eval") {
                continue;
            }
            let Some(object) = scope["object"].get("objectId").and_then(Value::as_str) else { continue };
            let properties = self.properties(vm, object)?;
            let mut group = Vec::new();
            for (name, value) in properties {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                group.push(self.var(vm, &name, &value));
            }
            groups.push(group);
        }
        // outer scopes were declared first
        let mut locals: Vec<Value> = groups.into_iter().rev().flatten().collect();
        if this_shown {
            let this = this.clone();
            locals.insert(0, self.var(vm, "this", &this));
        }
        let globals = match module {
            Some(object) => self.add_ref(vm, &object),
            None => 0,
        };
        Ok((locals, globals))
    }

    /// The own properties of an object, with their values, in order.
    fn properties(&self, vm: u64, object: &str) -> Result<Vec<(String, Value)>> {
        let result = self.page_call(
            vm,
            "Runtime.getProperties",
            json!({ "objectId": object, "ownProperties": true, "generatePreview": true }),
        )?;
        let mut list = Vec::new();
        for property in result["result"].as_array().cloned().unwrap_or_default() {
            let name = values::str_of(&property, "name").to_string();
            if name == "__proto__" {
                continue;
            }
            match property.get("value") {
                Some(value) => list.push((name, value.clone())),
                // a getter: running it could change something
                None if property.get("get").is_some_and(|get| get.get("objectId").is_some()) => {
                    list.push((name, json!({ "type": "accessor", "description": "(…)" })));
                }
                // a let or const before its declaration runs
                None => list.push((name, json!({ "type": "unavailable", "description": "<value unavailable>" }))),
            }
        }
        for property in result["privateProperties"].as_array().cloned().unwrap_or_default() {
            if let Some(value) = property.get("value") {
                list.push((values::str_of(&property, "name").to_string(), value.clone()));
            }
        }
        for property in result["internalProperties"].as_array().cloned().unwrap_or_default() {
            let name = values::str_of(&property, "name");
            if matches!(name, "[[Entries]]" | "[[PrimitiveValue]]" | "[[Target]]")
                && let Some(value) = property.get("value")
            {
                list.push((name.to_string(), value.clone()));
            }
        }
        Ok(list)
    }

    fn add_ref(&mut self, vm: u64, object: &Value) -> u64 {
        let Some(id) = object.get("objectId").and_then(Value::as_str) else { return 0 };
        let reference = self.next_ref;
        self.next_ref += 1;
        self.refs.insert(
            reference,
            Ref { vm, object: id.to_string(), subtype: values::str_of(object, "subtype").to_string() },
        );
        reference
    }

    fn var(&mut self, vm: u64, name: &str, object: &Value) -> Value {
        let reference = if values::expandable(object) { self.add_ref(vm, object) } else { 0 };
        let mut var = json!({
            "name": name,
            "value": values::preview(object),
            "type": values::type_name(object),
            "ref": reference,
        });
        let count = values::count(object);
        if reference > 0 && count > 0 {
            var["count"] = json!(count);
        }
        var
    }

    pub(super) fn expand(&mut self, reference: u64, start: usize, count: usize) -> Result<Vec<Value>> {
        let entry = self.refs.get(&reference).with_context(|| format!("ref {reference} is no longer valid"))?;
        let (vm, object, subtype) = (entry.vm, entry.object.clone(), entry.subtype.clone());
        let mut children: Vec<(String, Value)> = Vec::new();
        match subtype.as_str() {
            "map" | "set" => {
                // a map's children are its entries, not its properties
                let properties = self.properties(vm, &object)?;
                let entries = properties.iter().find(|(name, _)| name == "[[Entries]]").map(|(_, value)| value.clone());
                if let Some(entries_id) =
                    entries.as_ref().and_then(|entries| entries.get("objectId")).and_then(Value::as_str)
                {
                    for (index, entry) in self.properties(vm, entries_id)? {
                        if index.parse::<usize>().is_err() {
                            continue;
                        }
                        let Some(entry_id) = entry.get("objectId").and_then(Value::as_str) else { continue };
                        let fields = self.properties(vm, entry_id)?;
                        let field =
                            |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
                        let value = field("value").unwrap_or(json!({ "type": "undefined" }));
                        let name = match field("key") {
                            Some(key) => values::preview(&key),
                            None => index,
                        };
                        children.push((name, value));
                    }
                }
            }
            "array" | "typedarray" => {
                children = self
                    .properties(vm, &object)?
                    .into_iter()
                    .filter(|(name, _)| name.parse::<usize>().is_ok())
                    .collect();
            }
            _ => {
                // its own properties, then the accessors of its prototypes (a DOM node's
                // tagName, id…), sorted as Chrome shows them; only those of the page asked
                // for are run
                let own = self.properties(vm, &object)?;
                let accessors = self.accessors(vm, &object, &own)?;
                let total = own.len() + accessors.len();
                let end = if count == 0 { total } else { (start + count).min(total) };
                let mut page = Vec::new();
                for index in start..end {
                    match own.get(index) {
                        Some((name, value)) => page.push((name.clone(), value.clone())),
                        None => {
                            let (name, value) = &accessors[index - own.len()];
                            let value = match value {
                                Some(value) => value.clone(),
                                None => self.accessor_value(vm, &object, name)?,
                            };
                            page.push((name.clone(), value));
                        }
                    }
                }
                return Ok(page.into_iter().map(|(name, value)| self.var(vm, &name, &value)).collect());
            }
        }
        let end = if count == 0 { children.len() } else { (start + count).min(children.len()) };
        let page: Vec<(String, Value)> = children.into_iter().skip(start).take(end.saturating_sub(start)).collect();
        Ok(page.into_iter().map(|(name, value)| self.var(vm, &name, &value)).collect())
    }

    /// The accessors an object gets from its prototypes, sorted by name, but
    /// those of `own`: with their value when Chrome gives it (a DOM node's
    /// tagName, id…), without it for a getter of a script, which has to run.
    fn accessors(&self, vm: u64, object: &str, own: &[(String, Value)]) -> Result<Vec<(String, Option<Value>)>> {
        let result = self.page_call(
            vm,
            "Runtime.getProperties",
            json!({ "objectId": object, "ownProperties": false, "accessorPropertiesOnly": true, "generatePreview": true }),
        )?;
        let mut list: Vec<(String, Option<Value>)> = Vec::new();
        for property in result["result"].as_array().cloned().unwrap_or_default() {
            let name = values::str_of(&property, "name").to_string();
            if name == "__proto__" || name == "constructor" || own.iter().any(|(listed, _)| *listed == name) {
                continue;
            }
            if list.iter().any(|(listed, _)| *listed == name) {
                continue;
            }
            match property.get("value") {
                Some(value) => list.push((name, Some(value.clone()))),
                None if property.get("get").is_some_and(|get| get.get("objectId").is_some()) => list.push((name, None)),
                None => {}
            }
        }
        list.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(list)
    }

    /// An accessor's value, run with no side effects allowed: one that would
    /// have one shows as `(…)`, as Chrome does until it is clicked.
    fn accessor_value(&self, vm: u64, object: &str, name: &str) -> Result<Value> {
        let result = self.page_call(
            vm,
            "Runtime.callFunctionOn",
            json!({
                "functionDeclaration": "function(n) { return this[n] }",
                "objectId": object,
                "arguments": [{ "value": name }],
                "throwOnSideEffect": true,
                "generatePreview": true,
                "silent": true,
            }),
        )?;
        if result.get("exceptionDetails").is_some() {
            return Ok(json!({ "type": "accessor", "description": "(…)" }));
        }
        Ok(result["result"].clone())
    }

    pub(super) fn evaluate_on_frame(&self, vm: u64, frame: usize, expr: &str, no_side_effects: bool) -> Result<Value> {
        let page = self.pages.get(&vm).with_context(|| format!("no vm {vm}"))?;
        let paused = page.paused.as_ref().with_context(|| format!("vm {vm} is not paused"))?;
        let call_frame = values::str_of(&paused["callFrames"][frame], "callFrameId").to_string();
        if call_frame.is_empty() {
            bail!("no frame {frame}");
        }
        let result = self.page_call(
            vm,
            "Debugger.evaluateOnCallFrame",
            json!({
                "callFrameId": call_frame,
                "expression": expr,
                "throwOnSideEffect": no_side_effects,
                "generatePreview": true,
                "silent": true,
            }),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let message = values::exception_text(details);
            if message.contains("Possible side-effect") {
                bail!(SideEffect);
            }
            bail!("{message}");
        }
        Ok(result["result"].clone())
    }

    pub(super) fn eval(&mut self, vm: u64, frame: usize, expr: &str) -> Result<Value> {
        let object = match self.evaluate_on_frame(vm, frame, expr, true) {
            Ok(object) => object,
            Err(err) if err.is::<SideEffect>() && is_assignment(expr) => {
                self.evaluate_on_frame(vm, frame, expr, false)?
            }
            Err(err) => return Err(err),
        };
        Ok(self.var(vm, expr, &object))
    }
}

/// An expression refused because it could change something.
#[derive(Debug)]
struct SideEffect;

impl std::fmt::Display for SideEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the expression may have side effects, so it didn't run; only an assignment does")
    }
}

impl std::error::Error for SideEffect {}

/// `a = x`, `a.b[0] = x`: a name, then fields or indexes, then `=`.
fn is_assignment(expr: &str) -> bool {
    let chars: Vec<char> = expr.trim_start().chars().collect();
    let is_start = |c: char| c.is_alphabetic() || c == '_' || c == '$';
    let is_part = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut index = 0;
    if !chars.first().copied().is_some_and(is_start) {
        return false;
    }
    while index < chars.len() && is_part(chars[index]) {
        index += 1;
    }
    loop {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        match chars.get(index) {
            Some('.') => {
                index += 1;
                while index < chars.len() && chars[index].is_whitespace() {
                    index += 1;
                }
                if !chars.get(index).copied().is_some_and(is_start) {
                    return false;
                }
                while index < chars.len() && is_part(chars[index]) {
                    index += 1;
                }
            }
            Some('[') => {
                let mut depth = 0;
                loop {
                    match chars.get(index) {
                        Some('[') => depth += 1,
                        Some(']') => {
                            depth -= 1;
                            if depth == 0 {
                                index += 1;
                                break;
                            }
                        }
                        Some(_) => {}
                        None => return false,
                    }
                    index += 1;
                }
            }
            Some('=') => return chars.get(index + 1) != Some(&'='),
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments() {
        assert!(is_assignment("a = 1"));
        assert!(is_assignment("order.name = \"Bob\""));
        assert!(is_assignment("a.b[0] = x"));
        assert!(is_assignment("a[b[1]].c=2"));
        assert!(!is_assignment("a == 1"));
        assert!(!is_assignment("a.b()"));
        assert!(!is_assignment("f(a = 1)"));
        assert!(!is_assignment("1 = 2"));
    }
}
