//! JSON ↔ dynamic messages (proto3 JSON mapping): parsing with errors that name the field, pretty
//! output, and skeletons of messages for the editor.

use prost_reflect::{DeserializeOptions, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, ReflectMessage, SerializeOptions};
use serde_json::{json, Map, Value};

/// How deep nested messages are expanded in a skeleton.
const EXAMPLE_DEPTH: usize = 3;

pub(super) fn serialize_options() -> SerializeOptions {
    SerializeOptions::new().skip_default_fields(false)
}

/// A message as pretty JSON (every field, defaults included).
pub(super) fn to_json(m: &DynamicMessage) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b"  "));
    match m.serialize_with_options(&mut ser, &serialize_options()) {
        Ok(()) => String::from_utf8(buf).unwrap_or_default(),
        Err(e) => format!("\"<can't show {}: {e}>\"", m.descriptor().full_name()),
    }
}

/// A JSON text as a message of type `desc`. Errors name the field (`address.zip: expected int32, got "abc"`).
pub(super) fn parse(desc: &MessageDescriptor, text: &str) -> Result<DynamicMessage, String> {
    let text = if text.trim().is_empty() { "{}" } else { text };
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    if let Some(e) = check_message(desc, &value, "") {
        return Err(e);
    }
    DynamicMessage::deserialize_with_options(desc.clone(), value, &DeserializeOptions::new()).map_err(|e| format!("{}: {e}", desc.full_name()))
}

/// The first problem of `v` as a `desc`, with its path; `None` when it looks fine (prost-reflect has the last word).
fn check_message(desc: &MessageDescriptor, v: &Value, path: &str) -> Option<String> {
    if desc.full_name().starts_with("google.protobuf.") {
        // Well-known types have their own JSON forms: prost-reflect checks them, the path is added here.
        if path.is_empty() || v.is_null() {
            return None;
        }
        return DynamicMessage::deserialize(desc.clone(), v.clone()).err().map(|e| format!("{}{e} ({})", at(path), desc.full_name()));
    }
    let obj = match v {
        Value::Object(o) => o,
        Value::Null if !path.is_empty() => return None,
        other => return Some(format!("{}expected an object ({}), got {}", at(path), desc.full_name(), show(other))),
    };
    let mut oneofs: Vec<(String, String)> = Vec::new();
    for (key, val) in obj {
        let p = join(path, key);
        let Some(field) = desc.get_field_by_json_name(key).or_else(|| desc.get_field_by_name(key)) else {
            let names: Vec<String> = desc.fields().map(|f| f.json_name().to_string()).collect();
            return Some(format!("unknown field `{p}` in {} (fields: {})", desc.full_name(), if names.is_empty() { "none".into() } else { names.join(", ") }));
        };
        if val.is_null() {
            continue;
        }
        if let Some(o) = field.containing_oneof().filter(|o| !o.is_synthetic()) {
            if let Some((_, other)) = oneofs.iter().find(|(n, _)| n == o.name()) {
                return Some(format!("`{other}` and `{p}` are both set, but only one of oneof `{}` can be", o.name()));
            }
            oneofs.push((o.name().to_string(), p.clone()));
        }
        if let Some(e) = check_field(&field, val, &p) {
            return Some(e);
        }
    }
    None
}

fn check_field(field: &FieldDescriptor, v: &Value, path: &str) -> Option<String> {
    if field.is_map() {
        let Kind::Message(entry) = field.kind() else { return None };
        let Value::Object(o) = v else { return Some(format!("{}expected an object (map), got {}", at(path), show(v))) };
        let value_field = entry.map_entry_value_field();
        return o.iter().find_map(|(k, val)| check_value(&value_field.kind(), val, &format!("{path}[{k:?}]")));
    }
    if field.is_list() {
        let Value::Array(a) = v else { return Some(format!("{}expected a list, got {}", at(path), show(v))) };
        return a.iter().enumerate().find_map(|(i, val)| check_value(&field.kind(), val, &format!("{path}[{i}]")));
    }
    check_value(&field.kind(), v, path)
}

fn check_value(kind: &Kind, v: &Value, path: &str) -> Option<String> {
    let bad = |expected: &str| Some(format!("{}expected {expected}, got {}", at(path), show(v)));
    match kind {
        Kind::Message(m) => check_message(m, v, path),
        Kind::Enum(e) => match v {
            Value::String(s) if e.get_value_by_name(s).is_some() => None,
            Value::Number(n) if n.as_i64().is_some_and(|n| i32::try_from(n).is_ok()) => None,
            Value::Null => None,
            Value::String(s) => {
                let values: Vec<String> = e.values().map(|v| v.name().to_string()).collect();
                Some(format!("{}unknown value {s:?} of enum {} (values: {})", at(path), e.full_name(), values.join(", ")))
            }
            _ => bad(&format!("a value of enum {}", e.full_name())),
        },
        Kind::Bool => {
            if v.is_boolean() {
                None
            } else {
                bad("true or false")
            }
        }
        Kind::String => {
            if v.is_string() {
                None
            } else {
                bad("a string")
            }
        }
        Kind::Bytes => {
            if v.is_string() {
                None
            } else {
                bad("a base64 string (bytes)")
            }
        }
        Kind::Float | Kind::Double => match v {
            Value::Number(_) => None,
            Value::String(s) if matches!(s.as_str(), "NaN" | "Infinity" | "-Infinity") || s.trim().parse::<f64>().is_ok() => None,
            _ => bad(if matches!(kind, Kind::Float) { "a number (float)" } else { "a number (double)" }),
        },
        int => {
            let (name, min, max): (&str, i128, i128) = match int {
                Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => ("int32", i32::MIN.into(), i32::MAX.into()),
                Kind::Uint32 | Kind::Fixed32 => ("uint32", 0, u32::MAX.into()),
                Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => ("int64", i64::MIN.into(), i64::MAX.into()),
                _ => ("uint64", 0, u64::MAX.into()),
            };
            let n: Option<i128> = match v {
                Value::Number(n) => {
                    n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)).or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i128))
                }
                Value::String(s) => s.trim().parse::<i128>().ok(),
                _ => None,
            };
            match n {
                Some(n) if n >= min && n <= max => None,
                Some(_) => bad(&format!("an integer within {name} range")),
                None => bad(&format!("an integer ({name})")),
            }
        }
    }
}

fn at(path: &str) -> String {
    if path.is_empty() {
        String::new()
    } else {
        format!("`{path}`: ")
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// A JSON value in an error message: strings quoted (cut), others by kind.
fn show(v: &Value) -> String {
    match v {
        Value::String(s) if s.chars().count() > 40 => format!("{:?}…", s.chars().take(40).collect::<String>()),
        Value::String(s) => format!("{s:?}"),
        Value::Number(n) => format!("the number {n}"),
        Value::Bool(b) => format!("{b}"),
        Value::Null => "null".into(),
        Value::Array(_) => "a list".into(),
        Value::Object(_) => "an object".into(),
    }
}

/// A skeleton of `desc`: every field at its default; the first field of each oneof only.
pub(super) fn example(desc: &MessageDescriptor) -> Value {
    example_message(desc, &mut Vec::new())
}

fn example_message(desc: &MessageDescriptor, stack: &mut Vec<String>) -> Value {
    if let Some(v) = well_known(desc.full_name()) {
        return v;
    }
    if stack.len() >= EXAMPLE_DEPTH || stack.iter().any(|n| n == desc.full_name()) {
        return json!({});
    }
    stack.push(desc.full_name().to_string());
    let mut obj = Map::new();
    let mut oneofs_seen: Vec<String> = Vec::new();
    for field in desc.fields() {
        if let Some(o) = field.containing_oneof().filter(|o| !o.is_synthetic()) {
            if oneofs_seen.iter().any(|n| n == o.name()) {
                continue;
            }
            oneofs_seen.push(o.name().to_string());
        }
        let value = if field.is_map() {
            match field.kind() {
                Kind::Message(entry) => {
                    let key = match entry.map_entry_key_field().kind() {
                        Kind::String => "key".to_string(),
                        Kind::Bool => "false".to_string(),
                        _ => "0".to_string(),
                    };
                    let mut m = Map::new();
                    m.insert(key, example_value(&entry.map_entry_value_field().kind(), stack));
                    Value::Object(m)
                }
                _ => json!({}),
            }
        } else if field.is_list() {
            json!([example_value(&field.kind(), stack)])
        } else {
            example_value(&field.kind(), stack)
        };
        obj.insert(field.json_name().to_string(), value);
    }
    stack.pop();
    Value::Object(obj)
}

fn example_value(kind: &Kind, stack: &mut Vec<String>) -> Value {
    match kind {
        Kind::Double | Kind::Float => json!(0.0),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 | Kind::Uint32 | Kind::Fixed32 => json!(0),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 | Kind::Uint64 | Kind::Fixed64 => json!("0"),
        Kind::Bool => json!(false),
        Kind::String | Kind::Bytes => json!(""),
        Kind::Enum(e) => e.values().next().map(|v| json!(v.name())).unwrap_or(json!(0)),
        Kind::Message(m) => example_message(m, stack),
    }
}

/// Defaults of the well-known types, in their JSON forms.
fn well_known(name: &str) -> Option<Value> {
    Some(match name {
        "google.protobuf.Timestamp" => json!("1970-01-01T00:00:00Z"),
        "google.protobuf.Duration" => json!("0s"),
        "google.protobuf.FieldMask" => json!(""),
        "google.protobuf.Struct" | "google.protobuf.Empty" => json!({}),
        "google.protobuf.ListValue" => json!([]),
        "google.protobuf.Value" | "google.protobuf.Any" => Value::Null,
        "google.protobuf.DoubleValue" | "google.protobuf.FloatValue" => json!(0.0),
        "google.protobuf.Int32Value" | "google.protobuf.UInt32Value" => json!(0),
        "google.protobuf.Int64Value" | "google.protobuf.UInt64Value" => json!("0"),
        "google.protobuf.BoolValue" => json!(false),
        "google.protobuf.StringValue" | "google.protobuf.BytesValue" => json!(""),
        _ => return None,
    })
}
