use std::collections::BTreeMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::process::Command;
use tool_compiler_ir::{Effects, ToolSpec};
use tool_compiler_runtime::{ToolExecutionError, ToolExecutor};

pub const ADAPTER: &str = "shell";

#[derive(Clone, Debug, Default)]
pub struct ShellExecutor {
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
}

impl ShellExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

#[async_trait]
impl ToolExecutor for ShellExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        match tool {
            "run" => run_command(self, input).await.map_err(tool_error),
            other => Err(ToolExecutionError::new(format!(
                "unknown shell tool '{other}'"
            ))),
        }
    }
}

pub fn run_tool() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

async fn run_command(executor: &ShellExecutor, input: Value) -> Result<Value, ShellError> {
    let program = required_str(&input, "program")?;
    let args = input
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| ShellError::InvalidArgs)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();

    let mut command = Command::new(program);
    command.args(args).envs(&executor.env);
    if let Some(cwd) = &executor.cwd {
        command.current_dir(cwd);
    }

    let output = command.output().await?;
    Ok(json!({
        "status": output.status.code(),
        "success": output.status.success(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr)
    }))
}

fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, ShellError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ShellError::MissingField(key.into()))
}

fn tool_error(error: ShellError) -> ToolExecutionError {
    ToolExecutionError::new(error.to_string())
}

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("missing string field '{0}'")]
    MissingField(String),
    #[error("args must be an array of strings")]
    InvalidArgs,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn runs_program_without_shell_string() {
        let output = ShellExecutor::new()
            .call(
                "run",
                json!({
                    "program": "rustc",
                    "args": ["--version"]
                }),
            )
            .await
            .unwrap();

        assert_eq!(output["success"], true);
        assert!(output["stdout"].as_str().unwrap().contains("rustc"));
    }

    #[tokio::test]
    async fn rejects_invalid_args() {
        let error = ShellExecutor::new()
            .call("run", json!({ "program": "rustc", "args": [1] }))
            .await
            .unwrap_err();

        assert!(error.message.contains("args"));
    }

    #[test]
    fn declares_run_tool_effects() {
        let effects = run_tool().effects.unwrap();

        assert!(!effects.cacheable);
    }
}
