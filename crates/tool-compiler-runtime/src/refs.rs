use std::collections::BTreeMap;

use serde_json::Value;
use tool_compiler_ir::{NodeId, REF_KEY, ValueRef};

use crate::RuntimeError;

pub(crate) fn resolve_input(
    value: &Value,
    node_outputs: &BTreeMap<NodeId, Value>,
) -> Result<Value, RuntimeError> {
    match value {
        Value::Object(map) => {
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

pub(crate) fn resolve_value_ref(
    value_ref: &ValueRef,
    node_outputs: &BTreeMap<NodeId, Value>,
) -> Result<Value, RuntimeError> {
    let mut current = node_outputs
        .get(value_ref.node())
        .ok_or_else(|| RuntimeError::MissingNodeOutput(value_ref.node().to_owned()))?;

    for segment in value_ref.path() {
        current = match current {
            Value::Object(map) => map.get(segment).ok_or_else(|| RuntimeError::MissingPath {
                reference: value_ref.clone(),
                segment: segment.clone(),
            })?,
            Value::Array(items) => {
                let index = segment
                    .parse::<usize>()
                    .map_err(|_| RuntimeError::MissingPath {
                        reference: value_ref.clone(),
                        segment: segment.clone(),
                    })?;
                items.get(index).ok_or_else(|| RuntimeError::MissingPath {
                    reference: value_ref.clone(),
                    segment: segment.clone(),
                })?
            }
            _ => {
                return Err(RuntimeError::MissingPath {
                    reference: value_ref.clone(),
                    segment: segment.clone(),
                });
            }
        };
    }

    Ok(current.clone())
}
