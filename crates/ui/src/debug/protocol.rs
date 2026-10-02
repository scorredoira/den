//! The debug protocol a program speaks to be debugged: one JSON object per
//! line over TCP. The program's side is described in sim's `debugger.md`;
//! nothing here knows which language or VM is on the other end.

use serde::Deserialize;
use serde_json::{Map, Value, json};

pub const VERSION: u64 = 1;

/// A frame of a stopped VM, innermost first.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Frame {
    pub function: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: u32,
}

/// A value: its preview is already formatted. `reference` > 0 can be
/// expanded into `count` children.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Var {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(rename = "ref", default)]
    pub reference: u64,
    #[serde(default)]
    pub count: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Exception {
    pub message: String,
    #[serde(default)]
    pub stack: String,
}

/// Everything about a stop, sent with it so it can be shown at once.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Stop {
    pub vm: u64,
    pub reason: String,
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: u32,
    #[serde(default)]
    pub exception: Option<Exception>,
    #[serde(default)]
    pub frames: Vec<Frame>,
    #[serde(default)]
    pub locals: Vec<Var>,
    #[serde(default)]
    pub globals: u64,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Stopped(Box<Stop>),
    Resumed { vm: u64 },
    Output { text: String, file: String, line: u32 },
}

#[derive(Debug, PartialEq)]
pub enum Message {
    Response { id: u64, result: Result<Map<String, Value>, String> },
    Event(Event),
    /// An event this client doesn't know: newer programs may send more.
    Unknown,
}

pub fn parse(line: &str) -> Result<Message, String> {
    let value: Value = serde_json::from_str(line).map_err(|err| format!("invalid message: {err}"))?;
    let Value::Object(mut object) = value else {
        return Err("a message must be an object".into());
    };

    if let Some(Value::String(event)) = object.get("event") {
        let event = event.clone();
        let value = Value::Object(object);
        return Ok(match event.as_str() {
            "stopped" => Message::Event(Event::Stopped(Box::new(decode(value)?))),
            "resumed" => Message::Event(Event::Resumed { vm: value["vm"].as_u64().unwrap_or(0) }),
            "output" => Message::Event(Event::Output {
                text: value["text"].as_str().unwrap_or_default().to_string(),
                file: value["file"].as_str().unwrap_or_default().to_string(),
                line: value["line"].as_u64().unwrap_or(0) as u32,
            }),
            _ => Message::Unknown,
        });
    }

    let id = object
        .get("id")
        .and_then(Value::as_u64)
        .ok_or("a response without id")?;
    let ok = object.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let result = if ok {
        object.remove("id");
        object.remove("ok");
        Ok(object)
    } else {
        Err(object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the request failed")
            .to_string())
    };
    Ok(Message::Response { id, result })
}

/// Reads a typed value out of a response or event.
pub fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|err| format!("invalid message: {err}"))
}

/// A field of a response, typed.
pub fn field<T: for<'de> Deserialize<'de>>(body: &Map<String, Value>, name: &str) -> Result<T, String> {
    decode(body.get(name).cloned().unwrap_or(Value::Null))
}

/// The line of a request: `{"id": id, "cmd": cmd, ...args}`.
pub fn request(id: u64, cmd: &str, args: Value) -> String {
    let mut object = match args {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert("id".into(), json!(id));
    object.insert("cmd".into(), json!(cmd));
    Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_carries_frames_and_locals() {
        let line = r#"{"event":"stopped","vm":3,"reason":"breakpoint","file":"lib/a.ts","line":12,
            "frames":[{"function":"save","file":"lib/a.ts","line":12}],
            "locals":[{"name":"b","value":"{x: 2}","type":"map","ref":7,"count":1}],"globals":8}"#;
        let Message::Event(Event::Stopped(stop)) = parse(&line.replace('\n', "")).unwrap() else {
            panic!("not a stop");
        };
        assert_eq!(stop.vm, 3);
        assert_eq!(stop.frames[0].function, "save");
        assert_eq!(stop.locals[0], Var { name: "b".into(), value: "{x: 2}".into(), kind: "map".into(), reference: 7, count: 1 });
        assert_eq!(stop.globals, 8);
    }

    #[test]
    fn responses_carry_their_fields_or_error() {
        assert_eq!(
            parse(r#"{"id":4,"ok":true,"value":"1"}"#).unwrap(),
            Message::Response { id: 4, result: Ok(json!({"value": "1"}).as_object().unwrap().clone()) }
        );
        assert_eq!(
            parse(r#"{"id":5,"ok":false,"error":"x is not defined"}"#).unwrap(),
            Message::Response { id: 5, result: Err("x is not defined".into()) }
        );
    }

    #[test]
    fn unknown_events_are_not_errors() {
        assert_eq!(parse(r#"{"event":"newThing"}"#).unwrap(), Message::Unknown);
    }
}
