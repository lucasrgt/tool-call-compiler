//! Runtime reference resolution: `$ref`, `$literal`, and `$item`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tool_compiler_ir::{ITEM_KEY, LITERAL_KEY, NodeId, REF_KEY, ValueRef};

use crate::RuntimeError;

/// Resolves every `$ref` in `value` against `node_outputs`, unwraps
/// `$literal` escapes, and leaves `$item` markers in place (they are
/// substituted per element during `for_each` expansion).
pub(crate) fn resolve_input(
    value: &Value,
    node_outputs: &BTreeMap<NodeId, Arc<Value>>,
) -> Result<Value, RuntimeError> {
    match value {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(literal) = map.get(LITERAL_KEY)
            {
                return Ok(literal.clone());
            }
            if map.len() == 1
                && let Some(Value::String(raw_ref)) = map.get(REF_KEY)
            {
                let value_ref = raw_ref
                    .parse::<ValueRef>()
                    .map_err(|error| RuntimeError::InvalidRef(error.to_string()))?;
                return resolve_value_ref(&value_ref, node_outputs);
            }

            let mut resolved = serde_json::Map::new();
            for (key, value) in map {
                resolved.insert(key.clone(), resolve_input(value, node_outputs)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(items) => items
            .iter()
            .map(|item| resolve_input(item, node_outputs))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(value.clone()),
    }
}

/// Resolves one reference against the recorded node outputs (shared or
/// plain values).
pub(crate) fn resolve_value_ref<V: std::borrow::Borrow<Value>>(
    value_ref: &ValueRef,
    node_outputs: &BTreeMap<NodeId, V>,
) -> Result<Value, RuntimeError> {
    let mut current: &Value = node_outputs
        .get(value_ref.node())
        .ok_or_else(|| RuntimeError::MissingNodeOutput(value_ref.node().to_owned()))?
        .borrow();

    for segment in value_ref.path() {
        current = step_into(current, segment).ok_or_else(|| RuntimeError::MissingPath {
            reference: value_ref.clone(),
            segment: segment.clone(),
        })?;
    }

    Ok(current.clone())
}

fn step_into<'a>(current: &'a Value, segment: &str) -> Option<&'a Value> {
    match current {
        Value::Object(map) => map.get(segment),
        Value::Array(items) => items.get(segment.parse::<usize>().ok()?),
        _ => None,
    }
}

/// Substitutes `{"$item": <path>}` markers in a `for_each` input template
/// with the corresponding parts of `item`.
pub(crate) fn substitute_item(template: &Value, item: &Value) -> Result<Value, RuntimeError> {
    match template {
        Value::Object(map) => {
            if map.len() == 1
                && let Some(marker) = map.get(ITEM_KEY)
            {
                let Value::String(path) = marker else {
                    return Err(RuntimeError::InvalidRef(format!(
                        "'$item' path must be a string, found {marker}"
                    )));
                };
                return item_at_path(item, path).ok_or_else(|| {
                    RuntimeError::InvalidRef(format!("fan-out item has no path '{path}'"))
                });
            }
            if map.len() == 1 && map.contains_key(LITERAL_KEY) {
                return Ok(template.clone());
            }

            let mut resolved = serde_json::Map::new();
            for (key, value) in map {
                resolved.insert(key.clone(), substitute_item(value, item)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(items) => items
            .iter()
            .map(|value| substitute_item(value, item))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        _ => Ok(template.clone()),
    }
}

fn item_at_path(item: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(item.clone());
    }
    let mut current = item;
    for segment in path.split('.') {
        current = step_into(current, segment)?;
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn unwraps_literals() {
        let value = json!({ "schema": { "$literal": { "$ref": "not-a-ref" } } });

        let resolved = resolve_input(&value, &BTreeMap::new()).unwrap();

        assert_eq!(resolved, json!({ "schema": { "$ref": "not-a-ref" } }));
    }

    #[test]
    fn substitutes_whole_items_and_paths() {
        let template = json!({ "path": { "$item": "file" }, "all": { "$item": "" } });
        let item = json!({ "file": "a.txt" });

        let resolved = substitute_item(&template, &item).unwrap();

        assert_eq!(resolved["path"], "a.txt");
        assert_eq!(resolved["all"], item);
    }

    #[test]
    fn missing_item_paths_error() {
        let template = json!({ "x": { "$item": "nope" } });

        assert!(substitute_item(&template, &json!({})).is_err());
    }

    #[test]
    fn non_string_item_markers_error() {
        let template = json!({ "x": { "$item": 5 } });

        assert!(substitute_item(&template, &json!({})).is_err());
    }

    #[test]
    fn item_substitution_preserves_literals_and_arrays() {
        let template = json!({
            "keep": { "$literal": { "$item": "raw" } },
            "list": [{ "$item": "id" }, 7]
        });
        let item = json!({ "id": 3 });

        let resolved = substitute_item(&template, &item).unwrap();

        assert_eq!(resolved["keep"], json!({ "$literal": { "$item": "raw" } }));
        assert_eq!(resolved["list"], json!([3, 7]));
    }

    #[test]
    fn resolve_input_walks_arrays_and_scalars() {
        let mut outputs = BTreeMap::new();
        outputs.insert("a".to_owned(), Arc::new(json!({ "items": ["x", "y"] })));

        let resolved = resolve_input(
            &json!([{ "$ref": "a.output.items.1" }, 5, "text"]),
            &outputs,
        )
        .unwrap();

        assert_eq!(resolved, json!(["y", 5, "text"]));
    }

    #[test]
    fn resolve_value_ref_reports_missing_indexes_and_scalars() {
        let mut outputs = BTreeMap::new();
        outputs.insert("a".to_owned(), Arc::new(json!({ "items": ["x"], "n": 5 })));

        assert!(resolve_value_ref(&ValueRef::new("a", ["items", "9"]), &outputs).is_err());
        assert!(resolve_value_ref(&ValueRef::new("a", ["items", "x"]), &outputs).is_err());
        assert!(resolve_value_ref(&ValueRef::new("a", ["n", "deep"]), &outputs).is_err());
        assert!(resolve_value_ref(&ValueRef::output("missing"), &outputs).is_err());
    }

    #[test]
    fn resolve_input_rejects_malformed_refs() {
        let error = resolve_input(&json!({ "$ref": "not-a-ref" }), &BTreeMap::new()).unwrap_err();

        assert!(matches!(error, RuntimeError::InvalidRef(_)));
    }
}
