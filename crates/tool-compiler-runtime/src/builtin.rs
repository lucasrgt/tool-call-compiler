//! Built-in pure data tools available on every runtime.
//!
//! Registered under the [`crate::registry::BUILTIN_ADAPTER`] adapter name so
//! plans always have an aggregation/transform point without inventing a
//! host-specific adapter:
//!
//! - `collect`: returns its (resolved) input verbatim — the aggregation node.
//! - `pick`: `{ "value": ..., "path": "a.b.0" }` extracts a path.
//! - `merge`: `{ "objects": [{...}, {...}] }` shallow-merges left to right.
//! - `template`: `{ "template": "Hi {name}", "values": {...} }` interpolates
//!   `{key.path}` placeholders with scalar values.

use serde_json::Value;

use async_trait::async_trait;
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{Effects, resolve_resource_template};

use crate::registry::{BUILTIN_ADAPTER, ToolCapabilities, ToolRegistry};

/// Executor of the built-in pure data tools.
pub struct BuiltinExecutor;

/// The tool names served by [`BuiltinExecutor`].
pub const BUILTIN_TOOLS: [&str; 4] = ["collect", "pick", "merge", "template"];

#[async_trait]
impl ToolExecutor for BuiltinExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        match tool {
            "collect" => Ok(input),
            "pick" => pick(&input),
            "merge" => merge(&input),
            "template" => template(&input),
            other => Err(
                ToolExecutionError::new(format!("unknown builtin tool '{other}'"))
                    .with_code("unknown_tool"),
            ),
        }
    }
}

pub(crate) fn register_builtin_capabilities(registry: &mut ToolRegistry) {
    for tool in BUILTIN_TOOLS {
        registry.register_adapter_tool_capabilities(
            BUILTIN_ADAPTER,
            tool,
            ToolCapabilities::new().with_effects(Effects::pure()),
        );
    }
}

fn pick(input: &Value) -> Result<Value, ToolExecutionError> {
    let value = input
        .get("value")
        .ok_or_else(|| missing_field("pick", "value"))?;
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| missing_field("pick", "path"))?;

    let mut current = value;
    for segment in path.split('.').filter(|segment| !segment.is_empty()) {
        current = match current {
            Value::Object(map) => map.get(segment),
            Value::Array(items) => segment
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index)),
            _ => None,
        }
        .ok_or_else(|| {
            ToolExecutionError::new(format!("pick: path segment '{segment}' not found"))
                .with_code("missing_path")
        })?;
    }
    Ok(current.clone())
}

fn merge(input: &Value) -> Result<Value, ToolExecutionError> {
    let objects = input
        .get("objects")
        .and_then(Value::as_array)
        .ok_or_else(|| missing_field("merge", "objects"))?;

    let mut merged = serde_json::Map::new();
    for object in objects {
        let Value::Object(map) = object else {
            return Err(ToolExecutionError::new(
                "merge: every element of 'objects' must be an object",
            )
            .with_code("invalid_input"));
        };
        for (key, value) in map {
            merged.insert(key.clone(), value.clone());
        }
    }
    Ok(Value::Object(merged))
}

fn template(input: &Value) -> Result<Value, ToolExecutionError> {
    let template = input
        .get("template")
        .and_then(Value::as_str)
        .ok_or_else(|| missing_field("template", "template"))?;
    let values = input
        .get("values")
        .ok_or_else(|| missing_field("template", "values"))?;

    let rendered = resolve_resource_template(template, values);
    if rendered == template && template.contains('{') && template.contains('}') {
        return Err(ToolExecutionError::new(
            "template: placeholders could not be resolved from 'values'",
        )
        .with_code("invalid_input"));
    }
    Ok(Value::String(rendered))
}

fn missing_field(tool: &str, field: &str) -> ToolExecutionError {
    ToolExecutionError::new(format!("{tool}: missing field '{field}'")).with_code("invalid_input")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn collect_echoes_resolved_input() {
        let output = BuiltinExecutor
            .call("collect", json!({ "a": 1, "b": [2] }))
            .await
            .unwrap();

        assert_eq!(output, json!({ "a": 1, "b": [2] }));
    }

    #[tokio::test]
    async fn pick_extracts_paths_across_objects_and_arrays() {
        let output = BuiltinExecutor
            .call(
                "pick",
                json!({ "value": { "items": [{ "id": 7 }] }, "path": "items.0.id" }),
            )
            .await
            .unwrap();

        assert_eq!(output, json!(7));
    }

    #[tokio::test]
    async fn merge_shallow_merges_left_to_right() {
        let output = BuiltinExecutor
            .call(
                "merge",
                json!({ "objects": [{ "a": 1, "b": 1 }, { "b": 2 }] }),
            )
            .await
            .unwrap();

        assert_eq!(output, json!({ "a": 1, "b": 2 }));
    }

    #[tokio::test]
    async fn template_interpolates_scalars() {
        let output = BuiltinExecutor
            .call(
                "template",
                json!({ "template": "Hi {user.name}", "values": { "user": { "name": "Ada" } } }),
            )
            .await
            .unwrap();

        assert_eq!(output, json!("Hi Ada"));
    }

    #[tokio::test]
    async fn unknown_builtin_tool_errors() {
        let error = BuiltinExecutor.call("nope", json!({})).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("unknown_tool"));
    }
}
