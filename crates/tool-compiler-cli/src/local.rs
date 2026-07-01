//! Deterministic local adapter used by examples and benchmarks.
//!
//! Tools: `const` (returns `input.value`), `echo`/`write` (return the
//! input), `fail` (always errors). Every tool honors an optional `sleep_ms`
//! field to simulate latency. Unknown tool names are errors — a typo in a
//! plan must not "succeed".

use async_trait::async_trait;
use serde_json::Value;
use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};

/// The local example executor.
pub struct LocalExecutor;

#[async_trait]
impl ToolExecutor for LocalExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        if let Some(ms) = input.get("sleep_ms").and_then(Value::as_u64) {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }

        local_output(tool, input)
    }

    /// Ideal native batching: one shared sleep (the slowest item) instead of
    /// the sum — deliberately best-case, since this adapter exists to
    /// demonstrate what batching can save.
    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        let max_sleep_ms = inputs
            .iter()
            .filter_map(|input| input.input.get("sleep_ms").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        if max_sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(max_sleep_ms)).await;
        }

        inputs
            .into_iter()
            .map(|input| {
                Ok(BatchOutput {
                    node: input.node,
                    output: local_output(tool, input.input)?,
                })
            })
            .collect()
    }
}

fn local_output(tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
    match tool {
        "const" => input.get("value").cloned().ok_or_else(|| {
            ToolExecutionError::new("const requires input.value").with_code("invalid_input")
        }),
        "echo" | "write" => Ok(input),
        "fail" => Err(ToolExecutionError::new("local fail tool was called")),
        other => Err(
            ToolExecutionError::new(format!("unknown local tool '{other}'"))
                .with_code("unknown_tool"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn unknown_tools_error_instead_of_echoing() {
        let error = LocalExecutor.call("wrte", json!({})).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("unknown_tool"));
    }

    #[tokio::test]
    async fn const_requires_a_value() {
        let error = LocalExecutor.call("const", json!({})).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("invalid_input"));
        let value = LocalExecutor
            .call("const", json!({ "value": 7 }))
            .await
            .unwrap();
        assert_eq!(value, json!(7));
    }
}
