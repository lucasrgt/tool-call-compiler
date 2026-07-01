//! Adapter contract for the tool-call compiler.
//!
//! An adapter is a boundary package that executes tool calls against one
//! external surface (MCP server, filesystem, shell, HTTP endpoint, ...) while
//! exposing that surface through the common [`ToolExecutor`] contract.
//!
//! This crate is intentionally tiny so adapters can implement the contract
//! without depending on the runtime, optimizer, or graph internals.

#![warn(missing_docs)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub mod process;

/// Executes tool calls for one adapter surface.
///
/// Implementations must be safe to call concurrently: the runtime dispatches
/// independent graph nodes in parallel and may issue several `call`s (or a
/// mix of `call` and `call_batch`) against the same executor at once.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Executes a single tool operation and returns its JSON output.
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError>;

    /// Executes an optimizer-selected batch of same-tool operations.
    ///
    /// The default implementation dispatches sequentially through [`ToolExecutor::call`],
    /// so adapters only need to override this when the underlying surface has a
    /// native (pipelined, bulk, or concurrent) batch path. Implementations must
    /// return exactly one [`BatchOutput`] per input, keyed by the input's `node`.
    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        let mut outputs = Vec::with_capacity(inputs.len());
        for input in inputs {
            outputs.push(BatchOutput {
                node: input.node,
                output: self.call(tool, input.input).await?,
            });
        }
        Ok(outputs)
    }
}

/// One entry of a batch dispatch: the node id and its resolved input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchInput {
    /// Graph node id this input belongs to.
    pub node: String,
    /// Resolved JSON input for the call.
    pub input: Value,
}

/// One entry of a batch result: the node id and its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchOutput {
    /// Graph node id this output belongs to.
    pub node: String,
    /// JSON output of the call.
    pub output: Value,
}

/// A structured tool-level execution error.
///
/// `message` is always present and human readable. `code` is an optional
/// machine-readable identifier (for example `"timeout"`, `"not_found"`, or an
/// upstream error code) that retry policies can match on. `retryable` is the
/// adapter's own verdict about whether retrying could succeed; when set it
/// takes precedence over `retryable_errors` matching in retry policies.
#[derive(Clone, Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
#[error("{message}")]
pub struct ToolExecutionError {
    /// Human-readable description of the failure.
    pub message: String,
    /// Optional machine-readable error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Optional adapter verdict on whether a retry could succeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl ToolExecutionError {
    /// Creates an error carrying only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            retryable: None,
        }
    }

    /// Attaches a machine-readable error code.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Marks the error as retryable or not, overriding policy matching.
    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    struct Echo;

    #[async_trait]
    impl ToolExecutor for Echo {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            Ok(input)
        }
    }

    #[tokio::test]
    async fn default_batch_dispatches_sequentially() {
        let outputs = Echo
            .call_batch(
                "echo",
                vec![
                    BatchInput {
                        node: "a".into(),
                        input: json!(1),
                    },
                    BatchInput {
                        node: "b".into(),
                        input: json!(2),
                    },
                ],
            )
            .await
            .unwrap();

        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].node, "a");
        assert_eq!(outputs[1].output, json!(2));
    }

    #[test]
    fn structured_error_round_trips() {
        let error = ToolExecutionError::new("timed out")
            .with_code("timeout")
            .with_retryable(true);
        let value = serde_json::to_value(&error).unwrap();

        assert_eq!(value["message"], "timed out");
        assert_eq!(value["code"], "timeout");
        assert_eq!(value["retryable"], true);
        assert_eq!(
            serde_json::from_value::<ToolExecutionError>(value).unwrap(),
            error
        );
    }

    #[test]
    fn plain_error_omits_optional_fields() {
        let value = serde_json::to_value(ToolExecutionError::new("boom")).unwrap();

        assert_eq!(value, json!({ "message": "boom" }));
    }
}
