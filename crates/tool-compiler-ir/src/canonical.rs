//! Canonical JSON serialization for stable keys.

use std::fmt::Write;

use serde_json::Value;

/// Serializes `value` with recursively sorted object keys.
///
/// `serde_json`'s default map already sorts keys, but that is a feature-flag
/// accident: enabling `preserve_order` anywhere in the dependency graph would
/// silently change serialization order and weaken deduplication and cache
/// keys. This function guarantees a canonical form regardless of features.
pub fn canonical_json_string(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::String(text) => write_json_string(text, out),
        leaf => {
            let _ = write!(out, "{leaf}");
        }
    }
}

/// Writes `text` as a JSON string literal with `serde_json`-compatible
/// escaping, without allocating.
fn write_json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sorts_keys_recursively() {
        let value = json!({ "b": { "d": 1, "c": [2, { "z": 1, "a": 2 }] }, "a": null });

        assert_eq!(
            canonical_json_string(&value),
            r#"{"a":null,"b":{"c":[2,{"a":2,"z":1}],"d":1}}"#
        );
    }

    #[test]
    fn escapes_strings_like_json() {
        let value = json!({ "quote\"key": "line\nbreak" });

        assert_eq!(
            canonical_json_string(&value),
            r#"{"quote\"key":"line\nbreak"}"#
        );
    }

    #[test]
    fn identical_values_share_a_canonical_form() {
        let left: serde_json::Value = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
        let right: serde_json::Value = serde_json::from_str(r#"{"y":2,"x":1}"#).unwrap();

        assert_eq!(canonical_json_string(&left), canonical_json_string(&right));
    }
}
