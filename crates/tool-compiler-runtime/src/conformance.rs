//! Adapter conformance checks.
//!
//! The suite exercises the [`ToolExecutor`] contract directly — no runtime,
//! no optimizer — so what it verifies is the adapter itself: echo round-trip,
//! the `call_batch` node-mapping contract, and tool error propagation. Tool
//! names are configurable because real adapters rarely expose a literal
//! `echo` tool; point the suite at whatever cheap read-only tool the adapter
//! has.

use serde_json::json;
use tool_compiler_adapter_api::{BatchInput, ToolExecutor};

use serde::{Deserialize, Serialize};

/// Names of the tools the suite drives.
#[derive(Clone, Debug)]
pub struct ConformanceOptions {
    /// A tool that returns its input verbatim.
    pub echo_tool: String,
    /// A tool that always fails; skip the error check when `None`.
    pub failing_tool: Option<String>,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        Self {
            echo_tool: "echo".into(),
            failing_tool: None,
        }
    }
}

impl ConformanceOptions {
    /// Default options (an `echo` tool, no failing tool).
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses `tool` as the echo probe.
    pub fn with_echo_tool(mut self, tool: impl Into<String>) -> Self {
        self.echo_tool = tool.into();
        self
    }

    /// Enables the error-propagation check against `tool`.
    pub fn with_failing_tool(mut self, tool: impl Into<String>) -> Self {
        self.failing_tool = Some(tool.into());
        self
    }
}

/// Outcome of the conformance suite for one adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceReport {
    /// Adapter label the suite ran against.
    pub adapter: String,
    /// Whether every check passed.
    pub passed: bool,
    /// Individual check outcomes.
    pub checks: Vec<ConformanceCheck>,
}

/// One conformance check outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceCheck {
    /// Check name.
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Human-readable detail.
    pub message: String,
}

/// Runs the suite with default options (see [`ConformanceOptions`]).
pub async fn check_adapter_conformance(
    adapter: impl Into<String>,
    executor: impl ToolExecutor + 'static,
) -> ConformanceReport {
    check_adapter_conformance_with(adapter, executor, ConformanceOptions::default()).await
}

/// Runs the suite against `executor` with explicit options.
pub async fn check_adapter_conformance_with(
    adapter: impl Into<String>,
    executor: impl ToolExecutor + 'static,
    options: ConformanceOptions,
) -> ConformanceReport {
    let mut checks = vec![
        check_echo_round_trip(&executor, &options.echo_tool).await,
        check_batch_contract(&executor, &options.echo_tool).await,
    ];
    if let Some(failing_tool) = &options.failing_tool {
        checks.push(check_error_propagation(&executor, failing_tool).await);
    }

    ConformanceReport {
        adapter: adapter.into(),
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}

async fn check_echo_round_trip(executor: &impl ToolExecutor, echo_tool: &str) -> ConformanceCheck {
    let input = json!({ "ok": true });
    match executor.call(echo_tool, input.clone()).await {
        Ok(output) if output == input => pass("echo_round_trip"),
        Ok(output) => fail(
            "echo_round_trip",
            format!("echo output did not match input: {output}"),
        ),
        Err(error) => fail("echo_round_trip", error.to_string()),
    }
}

async fn check_batch_contract(executor: &impl ToolExecutor, echo_tool: &str) -> ConformanceCheck {
    let inputs = vec![
        BatchInput {
            node: "a".into(),
            input: json!({ "id": "a" }),
        },
        BatchInput {
            node: "b".into(),
            input: json!({ "id": "b" }),
        },
    ];
    match executor.call_batch(echo_tool, inputs).await {
        Ok(outputs) => {
            let mapped = outputs.len() == 2
                && outputs.iter().any(|output| {
                    output.node == "a" && output.output == json!({ "id": "a" })
                })
                && outputs.iter().any(|output| {
                    output.node == "b" && output.output == json!({ "id": "b" })
                });
            if mapped {
                pass("batch_contract")
            } else {
                fail(
                    "batch_contract",
                    "batch outputs were not mapped back to their node ids",
                )
            }
        }
        Err(error) => fail("batch_contract", error.to_string()),
    }
}

async fn check_error_propagation(
    executor: &impl ToolExecutor,
    failing_tool: &str,
) -> ConformanceCheck {
    match executor.call(failing_tool, json!({})).await {
        Err(_) => pass("error_propagation"),
        Ok(_) => fail("error_propagation", "failing tool returned success"),
    }
}

fn pass(name: &str) -> ConformanceCheck {
    ConformanceCheck {
        name: name.to_owned(),
        passed: true,
        message: "passed".into(),
    }
}

fn fail(name: &str, message: impl Into<String>) -> ConformanceCheck {
    ConformanceCheck {
        name: name.to_owned(),
        passed: false,
        message: message.into(),
    }
}
