//! How a CDP RemoteObject is shown: its preview, its type and how many
//! children it has.

use serde_json::Value;

const MAX_STRING: usize = 500;

/// The preview of a value, already formatted.
pub fn preview(object: &Value) -> String {
    let kind = str_of(object, "type");
    let subtype = str_of(object, "subtype");
    match kind {
        "undefined" => "undefined".into(),
        "string" => quote(object.get("value").and_then(Value::as_str).unwrap_or("")),
        "number" | "boolean" | "bigint" | "symbol" => description(object),
        "function" => function_preview(object),
        "object" if subtype == "null" => "null".into(),
        "object" => match object.get("preview") {
            Some(preview) => object_preview(object, preview),
            None => description(object),
        },
        _ => description(object),
    }
}

/// A value as `console.log` prints it: strings without quotes.
pub fn plain(object: &Value) -> String {
    if str_of(object, "type") == "string" {
        return object.get("value").and_then(Value::as_str).unwrap_or("").to_string();
    }
    preview(object)
}

/// The type shown next to a value; Den colors `string`, `int`, `float`,
/// `bool`, `null`, `undefined` and `error`.
pub fn type_name(object: &Value) -> String {
    let kind = str_of(object, "type");
    match kind {
        "number" => {
            let integer = object.get("value").and_then(Value::as_f64).is_some_and(|value| value.fract() == 0.0);
            if integer { "int".into() } else { "float".into() }
        }
        "boolean" => "bool".into(),
        "object" => match str_of(object, "subtype") {
            "null" => "null".into(),
            "array" | "typedarray" => "array".into(),
            "" => match str_of(object, "className") {
                "Object" | "" => "map".into(),
                class => class.into(),
            },
            subtype => subtype.into(),
        },
        kind => kind.into(),
    }
}

/// Whether the value has children to expand: objects, but not functions.
pub fn expandable(object: &Value) -> bool {
    str_of(object, "type") == "object" && str_of(object, "subtype") != "null" && object.get("objectId").is_some()
}

/// How many children an expandable value has, when known: arrays, maps and
/// sets, whose description carries it (`Array(3)`, `Map(2)`).
pub fn count(object: &Value) -> u64 {
    match str_of(object, "subtype") {
        "array" | "typedarray" | "map" | "set" => {}
        _ => return 0,
    }
    let description = str_of(object, "description");
    let Some(open) = description.find('(') else { return 0 };
    let Some(close) = description[open..].find(')') else { return 0 };
    description[open + 1..open + close].parse().unwrap_or(0)
}

/// The first line of an exception's description: `Error: boom`.
pub fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

fn description(object: &Value) -> String {
    if let Some(description) = object.get("description").and_then(Value::as_str) {
        return description.to_string();
    }
    match object.get("value") {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => str_of(object, "type").to_string(),
    }
}

fn function_preview(object: &Value) -> String {
    let description = description(object);
    let head = description.split('{').next().unwrap_or(&description).trim();
    let head = head.split("=>").next().unwrap_or(head).trim();
    let head: String = head.chars().take(80).collect();
    if head.starts_with("function") || head.starts_with("class") || head.starts_with("async") {
        head
    } else {
        format!("ƒ {head}")
    }
}

fn object_preview(object: &Value, preview: &Value) -> String {
    let subtype = str_of(object, "subtype");
    let overflow = preview.get("overflow").and_then(Value::as_bool).unwrap_or(false);
    let more = if overflow { ", …" } else { "" };
    let properties = preview.get("properties").and_then(Value::as_array).cloned().unwrap_or_default();
    let entries = preview.get("entries").and_then(Value::as_array).cloned().unwrap_or_default();
    match subtype {
        "array" | "typedarray" => {
            let items: Vec<String> = properties.iter().map(property_value).collect();
            format!("[{}{more}]", items.join(", "))
        }
        "map" => {
            let items: Vec<String> = entries
                .iter()
                .map(|entry| {
                    let key = entry.get("key").map(nested_preview).unwrap_or_default();
                    let value = entry.get("value").map(nested_preview).unwrap_or_default();
                    format!("{key} => {value}")
                })
                .collect();
            format!("{} {{{}{more}}}", description(object), items.join(", "))
        }
        "set" => {
            let items: Vec<String> =
                entries.iter().filter_map(|entry| entry.get("value")).map(nested_preview).collect();
            format!("{} {{{}{more}}}", description(object), items.join(", "))
        }
        "" => {
            let items: Vec<String> = properties
                .iter()
                .map(|property| format!("{}: {}", str_of(property, "name"), property_value(property)))
                .collect();
            let body = format!("{{{}{more}}}", items.join(", "));
            match str_of(object, "className") {
                "Object" | "" => body,
                class => format!("{class} {body}"),
            }
        }
        _ => first_line(&description(object)),
    }
}

/// A property of a preview: its value is already a short text.
fn property_value(property: &Value) -> String {
    let value = str_of(property, "value");
    match str_of(property, "type") {
        "string" => quote(value),
        "object" => match str_of(property, "subtype") {
            "null" => "null".into(),
            "" if value == "Object" => "{…}".into(),
            _ => value.into(),
        },
        "function" => "ƒ".into(),
        "undefined" => "undefined".into(),
        _ => value.into(),
    }
}

/// An ObjectPreview inside another (a map's key or value).
fn nested_preview(preview: &Value) -> String {
    match str_of(preview, "type") {
        "string" => quote(str_of(preview, "description")),
        "object" if str_of(preview, "subtype").is_empty() && str_of(preview, "description") == "Object" => "{…}".into(),
        _ => str_of(preview, "description").to_string(),
    }
}

fn quote(text: &str) -> String {
    let cut: String = text.chars().take(MAX_STRING).collect();
    let more = if cut.len() < text.len() { "…" } else { "" };
    format!("{}{more}", Value::String(cut))
}

/// What an evaluation's `exceptionDetails` say: the first line of the
/// exception, else the details' text.
pub fn exception_text(details: &Value) -> String {
    let description = details["exception"].get("description").and_then(Value::as_str).map(first_line);
    description.unwrap_or_else(|| str_of(details, "text").to_string())
}

pub fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// The text of a console call: format specifiers in the first string are
/// replaced by the arguments after it, and the rest are joined by spaces.
pub fn console_text(args: &[Value]) -> String {
    let mut parts = Vec::new();
    let mut rest = args.iter();
    if let Some(first) = args.first()
        && str_of(first, "type") == "string"
    {
        rest.next();
        let format = first.get("value").and_then(Value::as_str).unwrap_or("");
        let mut out = String::new();
        let mut chars = format.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.peek().copied() {
                Some('%') => {
                    chars.next();
                    out.push('%');
                }
                Some(spec @ ('s' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c')) => {
                    chars.next();
                    let Some(arg) = rest.next() else {
                        out.push('%');
                        out.push(spec);
                        continue;
                    };
                    match spec {
                        'c' => {}
                        'd' | 'i' => {
                            let number = arg.get("value").and_then(Value::as_f64);
                            match number {
                                Some(number) => out.push_str(&format!("{}", number.trunc())),
                                None => out.push_str("NaN"),
                            }
                        }
                        _ => out.push_str(&plain(arg)),
                    }
                }
                _ => out.push('%'),
            }
        }
        parts.push(out);
    }
    parts.extend(rest.map(plain));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn previews() {
        assert_eq!(preview(&json!({"type": "string", "value": "Ann"})), "\"Ann\"");
        assert_eq!(preview(&json!({"type": "number", "value": 3, "description": "3"})), "3");
        assert_eq!(type_name(&json!({"type": "number", "value": 3, "description": "3"})), "int");
        assert_eq!(type_name(&json!({"type": "number", "value": 1.5, "description": "1.5"})), "float");
        let object = json!({"type": "object", "className": "Object", "description": "Object", "objectId": "1",
            "preview": {"overflow": false, "properties": [
                {"name": "id", "type": "number", "value": "3"},
                {"name": "name", "type": "string", "value": "Ann"},
                {"name": "items", "type": "object", "subtype": "array", "value": "Array(3)"}]}});
        assert_eq!(preview(&object), "{id: 3, name: \"Ann\", items: Array(3)}");
        assert_eq!(type_name(&object), "map");
        let array = json!({"type": "object", "subtype": "array", "className": "Array", "description": "Array(3)",
            "objectId": "2", "preview": {"overflow": false, "properties": [
                {"name": "0", "type": "number", "value": "1"}, {"name": "1", "type": "number", "value": "2"}]}});
        assert_eq!(preview(&array), "[1, 2]");
        assert_eq!(count(&array), 3);
        assert_eq!(preview(&json!({"type": "object", "subtype": "null", "value": null})), "null");
    }

    #[test]
    fn console_formats() {
        let args = [
            json!({"type": "string", "value": "%s has %d items%c"}),
            json!({"type": "string", "value": "cart"}),
            json!({"type": "number", "value": 2.7, "description": "2.7"}),
            json!({"type": "string", "value": "color: red"}),
            json!({"type": "boolean", "value": true, "description": "true"}),
        ];
        assert_eq!(console_text(&args), "cart has 2 items true");
        let args =
            [json!({"type": "string", "value": "total"}), json!({"type": "number", "value": 6, "description": "6"})];
        assert_eq!(console_text(&args), "total 6");
    }
}
