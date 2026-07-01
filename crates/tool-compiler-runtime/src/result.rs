//! Composite run results, traces, metrics, and result shaping.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_compiler_adapter_api::ToolExecutionError;
use tool_compiler_ir::NodeId;
use tool_compiler_optimizer::OptimizationReport;

/// The composite feedback of one compiled run.
///
/// A tool-level failure does **not** discard the run: everything that
/// finished stays in `node_outputs`, the failure lands in `errors`, nodes
/// that never ran land in `skipped`, and `status` summarizes the verdict.
/// Infrastructure problems (unknown adapter, invalid plan, malformed
/// reference) still surface as `RuntimeError` instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    /// Overall verdict of the run.
    pub status: RunStatus,
    /// Declared plan outputs that could be resolved.
    pub outputs: BTreeMap<String, Value>,
    /// Raw output of every executed node (empty in compact mode).
    pub node_outputs: BTreeMap<NodeId, Value>,
    /// Tool-level failures by node.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub errors: BTreeMap<NodeId, ToolExecutionError>,
    /// Nodes that did not execute, with the reason.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skipped: BTreeMap<NodeId, SkipReason>,
    /// Execution trace (empty in compact mode).
    pub trace: Vec<TraceEvent>,
    /// What the optimizer did to the plan.
    pub optimization: OptimizationReport,
    /// Aggregate execution metrics.
    pub metrics: RunMetrics,
}

/// Overall verdict of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// Every scheduled node finished (skips by `when` still count as success).
    Success,
    /// At least one node failed.
    Failed,
    /// The run was cancelled before completion.
    Cancelled,
}

/// Why a node did not execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The node's `when` condition evaluated false.
    Condition,
    /// A node this one references failed or was skipped.
    FailedDependency,
    /// The run was cancelled before the node could start.
    Cancelled,
    /// Fail-fast stopped scheduling after another node failed.
    NotRun,
}

/// Aggregate metrics of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetrics {
    /// Wall-clock duration of the whole run, in milliseconds.
    pub wall_ms: u64,
    /// Nodes in the optimized plan.
    pub nodes_total: usize,
    /// Nodes that finished successfully (including cache hits).
    pub nodes_succeeded: usize,
    /// Nodes that failed.
    pub nodes_failed: usize,
    /// Nodes skipped (any [`SkipReason`]).
    pub nodes_skipped: usize,
    /// Cache hits.
    pub cache_hits: usize,
    /// Batch dispatches issued.
    pub batch_dispatches: usize,
    /// Retry attempts performed beyond first calls.
    pub retries: usize,
}

/// How much of the run detail the result should carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultMode {
    /// Outputs, node outputs, trace, report — everything.
    #[default]
    Full,
    /// Only declared outputs, errors, skips, metrics, and the optimization
    /// report; node outputs and trace are dropped. This is the shape meant
    /// for LLM feedback: compact by construction.
    Compact,
}

/// One execution trace event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    /// Node the event belongs to.
    pub node: NodeId,
    /// Tool the node calls.
    pub tool: String,
    /// Event kind.
    pub status: TraceStatus,
    /// Milliseconds since the Unix epoch when the event was recorded.
    pub at_ms: u64,
    /// Duration of the finished/failed call, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Error message for failed/retried events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Identifier shared by every node dispatched in the same batch call.
    /// A batch event's `duration_ms` is the wall time of the whole dispatch,
    /// so per-node durations within one `batch_id` overlap rather than add.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<u64>,
    /// Attempt number for retried events (1 is the first call).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u8>,
}

impl TraceEvent {
    pub(crate) fn new(node: &str, tool: &str, status: TraceStatus) -> Self {
        Self {
            node: node.to_owned(),
            tool: tool.to_owned(),
            status,
            at_ms: epoch_ms(),
            duration_ms: None,
            error: None,
            batch_id: None,
            attempt: None,
        }
    }

    pub(crate) fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    pub(crate) fn with_error(mut self, error: &ToolExecutionError) -> Self {
        self.error = Some(error.message.clone());
        self
    }

    pub(crate) fn with_batch(mut self, batch_id: Option<u64>) -> Self {
        self.batch_id = batch_id;
        self
    }

    pub(crate) fn with_attempt(mut self, attempt: u8) -> Self {
        self.attempt = Some(attempt);
        self
    }
}

/// Kind of a trace event; failure details live in [`TraceEvent::error`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    /// The node was dispatched.
    Started,
    /// The node finished successfully.
    Finished,
    /// The node's output came from the cache.
    CacheHit,
    /// The node failed.
    Failed,
    /// A failed attempt is being retried.
    Retried,
    /// The node was skipped.
    Skipped,
}

pub(crate) fn epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Redacts sensitive material from result payloads before they leave the
/// runtime (results, node outputs). Applied at result assembly; reference
/// resolution always sees raw values.
pub trait Redactor: Send + Sync {
    /// Returns the value to expose for `node`'s output.
    fn redact_output(&self, node: &str, output: &Value) -> Value;
}

/// [`Redactor`] that masks configured key names (case-insensitive)
/// recursively in objects.
#[derive(Clone, Debug)]
pub struct KeyRedactor {
    keys: Vec<String>,
    replacement: String,
}

impl KeyRedactor {
    /// Masks the given key names with `"[redacted]"`.
    pub fn new(keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            keys: keys.into_iter().map(|key| key.into().to_lowercase()).collect(),
            replacement: "[redacted]".into(),
        }
    }

    /// Common secret-bearing key names.
    pub fn default_secret_keys() -> Self {
        Self::new([
            "token",
            "password",
            "secret",
            "authorization",
            "api_key",
            "apikey",
            "access_key",
        ])
    }

    fn redact_value(&self, value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        if self.keys.iter().any(|needle| key.to_lowercase().contains(needle)) {
                            (key.clone(), Value::String(self.replacement.clone()))
                        } else {
                            (key.clone(), self.redact_value(value))
                        }
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| self.redact_value(item)).collect())
            }
            other => other.clone(),
        }
    }
}

impl Redactor for KeyRedactor {
    fn redact_output(&self, _node: &str, output: &Value) -> Value {
        self.redact_value(output)
    }
}

/// Truncates `value` when its serialized form exceeds `max_bytes`, replacing
/// it with a marker object carrying a preview.
pub(crate) fn truncate_output(value: Value, max_bytes: usize) -> Value {
    let serialized = value.to_string();
    if serialized.len() <= max_bytes {
        return value;
    }
    let preview: String = serialized.chars().take(max_bytes.min(512)).collect();
    json!({
        "$truncated": true,
        "bytes": serialized.len(),
        "preview": preview,
    })
}

pub(crate) fn shape_output(
    node: &str,
    value: Value,
    redactor: Option<&Arc<dyn Redactor>>,
    max_output_bytes: Option<usize>,
) -> Value {
    let value = match redactor {
        Some(redactor) => redactor.redact_output(node, &value),
        None => value,
    };
    match max_output_bytes {
        Some(max_bytes) => truncate_output(value, max_bytes),
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_redactor_masks_matching_keys_recursively() {
        let redactor = KeyRedactor::default_secret_keys();
        let value = json!({
            "user": { "name": "ada", "api_key": "s3cr3t" },
            "items": [{ "Authorization": "Bearer x" }]
        });

        let redacted = redactor.redact_output("n", &value);

        assert_eq!(redacted["user"]["name"], "ada");
        assert_eq!(redacted["user"]["api_key"], "[redacted]");
        assert_eq!(redacted["items"][0]["Authorization"], "[redacted]");
    }

    #[test]
    fn truncates_oversized_outputs_with_a_marker() {
        let value = json!({ "content": "x".repeat(2000) });

        let truncated = truncate_output(value.clone(), 100);

        assert_eq!(truncated["$truncated"], true);
        assert!(truncated["bytes"].as_u64().unwrap() > 100);
        assert!(truncate_output(value, 100_000).get("$truncated").is_none());
    }

    #[test]
    fn trace_status_serializes_flat_with_error_field() {
        let event = TraceEvent::new("n", "tool", TraceStatus::Failed)
            .with_duration(5)
            .with_error(&ToolExecutionError::new("boom"));
        let value = serde_json::to_value(&event).unwrap();

        assert_eq!(value["status"], "failed");
        assert_eq!(value["error"], "boom");
        assert_eq!(value["duration_ms"], 5);
        assert!(value["at_ms"].is_u64());
    }
}
