use async_trait::async_trait;
use serde_json::{Value, json};
use tool_compiler_runtime::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};

pub struct LocalExecutor;

#[async_trait]
impl ToolExecutor for LocalExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        if let Some(ms) = input.get("sleep_ms").and_then(Value::as_u64) {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }

        local_output(tool, input)
    }

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
        "const" => Ok(input.get("value").cloned().unwrap_or(input)),
        "echo" | "write" => Ok(input),
        "fail" => Err(ToolExecutionError::new("local fail tool was called")),
        other => Ok(json!({ "tool": other, "input": input })),
    }
}
