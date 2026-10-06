//! What makes several programs look like one to the debugger: their VM and
//! ref numbers made distinct, and the answers to a request every program
//! gets merged into one.

use serde_json::{Map, Value, json};

/// Programs a join can hold: a combined number keeps which one in its low
/// four bits.
pub const MAX_PROGRAMS: usize = 16;

/// The combined number of program `child`'s `n`. 0 (no VM, nothing to
/// expand) stays 0.
pub fn encode(n: u64, child: usize) -> Result<u64, String> {
    if n == 0 {
        return Ok(0);
    }
    n.checked_mul(MAX_PROGRAMS as u64)
        .and_then(|n| n.checked_add(child as u64))
        .ok_or_else(|| format!("{n}: too large to tell the programs apart"))
}

/// The program a combined number belongs to, of `count`, and its own number.
pub fn decode(n: u64, count: usize) -> Option<(usize, u64)> {
    let child = (n % MAX_PROGRAMS as u64) as usize;
    let own = n / MAX_PROGRAMS as u64;
    (own > 0 && child < count).then_some((child, own))
}

/// Rewrites every `vm`, `ref` and `globals` of a message of program `child`
/// to its combined number, and the numbers of a `stopped` list (`threads`).
pub fn namespace(value: &mut Value, child: usize) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match (key.as_str(), value) {
                    ("vm" | "ref" | "globals", Value::Number(number)) => {
                        let n = number.as_u64().ok_or_else(|| format!("{key}: {number} is not a positive integer"))?;
                        let combined = encode(n, child)?;
                        *number = combined.into();
                    }
                    ("stopped", Value::Array(items)) => {
                        for item in items {
                            match item.as_u64() {
                                Some(vm) => {
                                    let combined = encode(vm, child)?;
                                    *item = combined.into();
                                }
                                None => namespace(item, child)?,
                            }
                        }
                    }
                    (_, value) => namespace(value, child)?,
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                namespace(item, child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// One program's answer to a request every program got: its fields, or its
/// error.
pub type Answer = Result<Map<String, Value>, String>;

/// The one answer to `cmd` from the programs' answers, in the programs'
/// order, each with the program's name. `page` keeps the programs' page in
/// `hello`.
pub fn merge(cmd: &str, answers: &[(String, Answer)], page: bool) -> Answer {
    let oks: Vec<&Map<String, Value>> = answers.iter().filter_map(|(_, answer)| answer.as_ref().ok()).collect();
    let first_error = || {
        answers
            .iter()
            .find_map(|(name, answer)| answer.as_ref().err().map(|error| format!("{name}: {error}")))
            .unwrap_or_else(|| "no program answered".to_string())
    };
    match cmd {
        "hello" => {
            if oks.len() < answers.len() || oks.is_empty() {
                return Err(first_error());
            }
            let cwd = oks[0].get("cwd").cloned().unwrap_or(Value::Null);
            if let Some((name, other)) = answers.iter().find_map(|(name, answer)| {
                let other = answer.as_ref().ok()?.get("cwd").cloned().unwrap_or(Value::Null);
                (other != cwd).then_some((name, other))
            }) {
                return Err(format!("the programs run in different folders: {} in {cwd}, {name} in {other}", answers[0].0));
            }
            let mut out = Map::new();
            out.insert("version".into(), oks[0].get("version").cloned().unwrap_or(Value::Null));
            out.insert("cwd".into(), cwd);
            out.insert("waiting".into(), json!(oks.iter().any(|ok| ok.get("waiting").and_then(Value::as_bool) == Some(true))));
            out.insert("running".into(), json!(running(&oks)));
            out.insert("stopped".into(), Value::Array(stopped(&oks)));
            if page && let Some(page) = oks.iter().find_map(|ok| ok.get("page")) {
                out.insert("page".into(), page.clone());
            }
            Ok(out)
        }
        "threads" => {
            if oks.is_empty() {
                return Err(first_error());
            }
            let mut out = Map::new();
            out.insert("running".into(), json!(running(&oks)));
            out.insert("stopped".into(), Value::Array(stopped(&oks)));
            Ok(out)
        }
        // A program keeps the breakpoints of files it doesn't load as
        // pending: a line is where any program could put it.
        "setBreakpoints" => {
            if oks.is_empty() {
                return Err(first_error());
            }
            let lists: Vec<&Vec<Value>> =
                oks.iter().filter_map(|ok| ok.get("breakpoints").and_then(Value::as_array)).collect();
            let len = lists.iter().map(|list| list.len()).max().unwrap_or(0);
            let merged: Vec<Value> = (0..len)
                .filter_map(|ix| {
                    let at: Vec<&Value> = lists.iter().filter_map(|list| list.get(ix)).collect();
                    at.iter().find(|bp| bp.get("error").is_none()).or(at.first()).map(|bp| (*bp).clone())
                })
                .collect();
            let mut out = Map::new();
            out.insert("breakpoints".into(), Value::Array(merged));
            Ok(out)
        }
        // What every program must do: any that couldn't is the answer.
        "run" | "setExceptions" | "pause" => {
            if oks.len() < answers.len() || oks.is_empty() {
                return Err(first_error());
            }
            Ok(Map::new())
        }
        // A command only some programs have (`inspect`): the one that does.
        _ => match oks.first() {
            Some(ok) => Ok((*ok).clone()),
            None => Err(first_error()),
        },
    }
}

fn running(oks: &[&Map<String, Value>]) -> u64 {
    oks.iter().filter_map(|ok| ok.get("running").and_then(Value::as_u64)).sum()
}

fn stopped(oks: &[&Map<String, Value>]) -> Vec<Value> {
    oks.iter()
        .filter_map(|ok| ok.get("stopped").and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(value: Value) -> Answer {
        match value {
            Value::Object(map) => Ok(map),
            _ => Err("not an object".into()),
        }
    }

    fn answers(list: Vec<Answer>) -> Vec<(String, Answer)> {
        list.into_iter().enumerate().map(|(ix, answer)| (format!("p{ix}"), answer)).collect()
    }

    #[test]
    fn numbers_keep_their_program() {
        assert_eq!(encode(0, 3), Ok(0));
        assert_eq!(encode(5, 3), Ok(83));
        assert_eq!(decode(83, 4), Some((3, 5)));
        assert_eq!(decode(83, 3), None, "a program that isn't there");
        assert_eq!(decode(3, 4), None, "0 of its program");
        assert!(encode(u64::MAX / 2, 1).is_err());
    }

    #[test]
    fn a_message_is_namespaced_all_through() {
        let mut stop = json!({
            "event": "stopped", "vm": 2, "frames": [{"function": "f", "file": "a.ts", "line": 3}],
            "locals": [{"name": "a", "value": "{}", "ref": 7}, {"name": "b", "value": "1", "ref": 0}],
            "globals": 8
        });
        namespace(&mut stop, 1).unwrap();
        assert_eq!(stop["vm"], 33);
        assert_eq!(stop["locals"][0]["ref"], 113);
        assert_eq!(stop["locals"][1]["ref"], 0);
        assert_eq!(stop["globals"], 129);
        assert_eq!(stop["frames"][0]["line"], 3);

        let mut threads = json!({"running": 2, "stopped": [1, 4]});
        namespace(&mut threads, 2).unwrap();
        assert_eq!(threads, json!({"running": 2, "stopped": [18, 66]}));

        let mut bad = json!({"vm": -1});
        assert!(namespace(&mut bad, 0).is_err());
    }

    #[test]
    fn hellos_merge() {
        let list = answers(vec![
            ok(json!({"version": 1, "cwd": "/w", "waiting": false, "running": 1, "stopped": [{"vm": 16}]})),
            ok(json!({"version": 1, "cwd": "/w", "waiting": true, "running": 2, "stopped": [{"vm": 33}], "page": "http://localhost:1/"})),
        ]);
        let hello = merge("hello", &list, true).unwrap();
        assert_eq!(
            Value::Object(hello),
            json!({"version": 1, "cwd": "/w", "waiting": true, "running": 3, "stopped": [{"vm": 16}, {"vm": 33}], "page": "http://localhost:1/"})
        );
        assert!(merge("hello", &list, false).unwrap().get("page").is_none(), "--no-page");

        let other = answers(vec![ok(json!({"cwd": "/w"})), ok(json!({"cwd": "/x"}))]);
        assert!(merge("hello", &other, true).unwrap_err().contains("different folders"));
        let refused = answers(vec![ok(json!({"cwd": "/w"})), Err("protocol version 2".into())]);
        assert_eq!(merge("hello", &refused, true).unwrap_err(), "p1: protocol version 2");
    }

    #[test]
    fn a_breakpoint_goes_where_any_program_puts_it() {
        let list = answers(vec![
            ok(json!({"breakpoints": [{"line": 3, "error": "not loaded"}, {"line": 9}]})),
            ok(json!({"breakpoints": [{"line": 4}, {"line": 9, "error": "no code"}]})),
        ]);
        let merged = merge("setBreakpoints", &list, true).unwrap();
        assert_eq!(merged["breakpoints"], json!([{"line": 4}, {"line": 9}]));
        let none = answers(vec![ok(json!({"breakpoints": [{"line": 3, "error": "a"}]})), ok(json!({"breakpoints": [{"line": 3, "error": "b"}]}))]);
        assert_eq!(merge("setBreakpoints", &none, true).unwrap()["breakpoints"], json!([{"line": 3, "error": "a"}]));
    }

    #[test]
    fn threads_add_up_and_others_take_the_first_that_can() {
        let list = answers(vec![ok(json!({"running": 1, "stopped": [16]})), ok(json!({"running": 0, "stopped": [33, 49]}))]);
        assert_eq!(Value::Object(merge("threads", &list, true).unwrap()), json!({"running": 1, "stopped": [16, 33, 49]}));

        let inspect = answers(vec![Err("unknown command inspect".into()), ok(json!({"x": 1}))]);
        assert_eq!(Value::Object(merge("inspect", &inspect, true).unwrap()), json!({"x": 1}));
        let nobody = answers(vec![Err("unknown command".into()), Err("no".into())]);
        assert_eq!(merge("inspect", &nobody, true).unwrap_err(), "p0: unknown command");

        let run = answers(vec![ok(json!({})), Err("can't".into())]);
        assert_eq!(merge("run", &run, true).unwrap_err(), "p1: can't");
        assert!(merge("run", &answers(vec![ok(json!({})), ok(json!({}))]), true).is_ok());
    }
}
