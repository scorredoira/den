//! What Den and a device program say to each other: JSON, one object per line.
//! `<program> list` prints the devices; `<program> serve <id>` serves one, its
//! events on stdout and Den's commands on its stdin, and ends when its stdin
//! closes. The full description is in `docs/device.md`.

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

/// A device a program can serve.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub booted: bool,
}

/// The devices in the output of `list`.
pub fn parse_list(out: &str) -> Result<Vec<Listed>> {
    out.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("not a device: {line}")))
        .collect()
}

/// The screen in its own pixels, and the phone's edge around it in the same
/// pixels: its width and its outer corner radius (0, no edge).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    pub bezel: u32,
    pub radius: u32,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    /// The screen, before its first frame and when it changes.
    Size(Screen),
    /// A new frame is in the IOSurface with this number.
    Frame(u32),
    Error(String),
}

pub fn parse_event(line: &str) -> Result<Event> {
    let value: Value = serde_json::from_str(line).with_context(|| format!("not JSON: {line}"))?;
    let number = |v: &Value| v.as_u64().and_then(|n| u32::try_from(n).ok());
    if let Some(size) = value.get("size") {
        let Some([w, h]) = size.as_array().map(Vec::as_slice) else {
            bail!("a size is [width, height]: {line}");
        };
        let (Some(width), Some(height)) = (number(w), number(h)) else {
            bail!("a size is [width, height]: {line}");
        };
        let edge = |name: &str| match value.get(name) {
            None => Ok(0),
            Some(v) => number(v).with_context(|| format!("{name} is a number of pixels: {line}")),
        };
        return Ok(Event::Size(Screen { width, height, bezel: edge("bezel")?, radius: edge("radius")? }));
    }
    if let Some(frame) = value.get("frame") {
        return number(frame).map(Event::Frame).with_context(|| format!("a frame is a surface number: {line}"));
    }
    if let Some(error) = value.get("error") {
        return error.as_str().map(|e| Event::Error(e.to_string())).with_context(|| format!("an error is text: {line}"));
    }
    bail!("unknown event: {line}")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    Down,
    Move,
    Up,
}

/// A finger at `at`, and a second one at `second` for a pinch: fractions of
/// the screen as shown, from its top left.
pub fn touch(phase: Touch, at: (f32, f32), second: Option<(f32, f32)>) -> String {
    let phase = match phase {
        Touch::Down => "down",
        Touch::Move => "move",
        Touch::Up => "up",
    };
    let mut command = json!({ "touch": phase, "x": at.0, "y": at.1 });
    if let Some((x2, y2)) = second {
        command["x2"] = json!(x2);
        command["y2"] = json!(y2);
    }
    command.to_string()
}

/// The device turned a quarter clockwise (`right`) or the other way; the
/// frames that follow are its screen as held, a new size first.
pub fn rotate(right: bool) -> String {
    json!({ "rotate": if right { "right" } else { "left" } }).to_string()
}

pub fn text(text: &str) -> String {
    json!({ "text": text }).to_string()
}

/// The keys sent by name; any other is a single character.
pub const NAMED_KEYS: [&str; 14] =
    ["enter", "escape", "backspace", "tab", "space", "home", "pageup", "delete", "end", "pagedown", "right", "left", "down", "up"];

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub cmd: bool,
}

/// A key by its name (`NAMED_KEYS`) or its character, with the modifiers held.
pub fn key(name: &str, modifiers: Modifiers) -> String {
    json!({
        "key": name,
        "shift": modifiers.shift,
        "alt": modifiers.alt,
        "ctrl": modifiers.ctrl,
        "cmd": modifiers.cmd,
    })
    .to_string()
}

pub fn home() -> String {
    json!({ "button": "home" }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list() {
        let out = "{\"id\":\"A\",\"name\":\"iPhone 17 Pro (iOS 26.2)\",\"booted\":true}\n\n{\"id\":\"B\",\"name\":\"iPad\"}\n";
        let devices = parse_list(out).unwrap();
        assert_eq!(devices, [
            Listed { id: "A".into(), name: "iPhone 17 Pro (iOS 26.2)".into(), booted: true },
            Listed { id: "B".into(), name: "iPad".into(), booted: false },
        ]);
        assert!(parse_list("{\"name\":\"no id\"}").is_err());
        assert!(parse_list("garbage").is_err());
    }

    #[test]
    fn events() {
        let plain = Screen { width: 1206, height: 2622, bezel: 0, radius: 0 };
        assert_eq!(parse_event("{\"size\":[1206,2622]}").unwrap(), Event::Size(plain));
        let phone = Screen { bezel: 54, radius: 240, ..plain };
        assert_eq!(parse_event("{\"size\":[1206,2622],\"bezel\":54,\"radius\":240}").unwrap(), Event::Size(phone));
        assert!(parse_event("{\"size\":[1206,2622],\"bezel\":\"wide\"}").is_err());
        assert_eq!(parse_event("{\"frame\":118}").unwrap(), Event::Frame(118));
        assert_eq!(parse_event("{\"error\":\"no key f3\"}").unwrap(), Event::Error("no key f3".into()));
        assert!(parse_event("{\"size\":[1206]}").is_err());
        assert!(parse_event("{\"size\":[-1,2]}").is_err());
        assert!(parse_event("{\"frame\":\"x\"}").is_err());
        assert!(parse_event("{\"frame\":5000000000}").is_err());
        assert!(parse_event("{\"other\":1}").is_err());
        assert!(parse_event("nope").is_err());
    }

    #[test]
    fn commands() {
        let parse = |line: String| serde_json::from_str::<Value>(&line).unwrap();
        assert_eq!(parse(touch(Touch::Move, (0.5, 0.25), None)), json!({ "touch": "move", "x": 0.5, "y": 0.25 }));
        assert_eq!(
            parse(touch(Touch::Down, (0.25, 0.5), Some((0.75, 0.5)))),
            json!({ "touch": "down", "x": 0.25, "y": 0.5, "x2": 0.75, "y2": 0.5 })
        );
        assert_eq!(parse(rotate(true)), json!({ "rotate": "right" }));
        assert_eq!(parse(rotate(false)), json!({ "rotate": "left" }));
        assert_eq!(parse(text("Hé")), json!({ "text": "Hé" }));
        assert_eq!(
            parse(key("left", Modifiers { shift: true, ..Default::default() })),
            json!({ "key": "left", "shift": true, "alt": false, "ctrl": false, "cmd": false })
        );
        assert_eq!(parse(home()), json!({ "button": "home" }));
    }
}
