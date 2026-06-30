use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinError;
use tool_compiler_graph::GraphError;
use tool_compiler_ir::{Node, NodeId, ValueRef};
use tool_compiler_optimizer::OptimizationReport;

mod conformance;
mod execution;
mod refs;

pub use conformance::{ConformanceCheck, ConformanceReport, check_adapter_conformance};

#[cfg(test)]
use refs::resolve_input;

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError>;

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchInput {
    pub node: NodeId,
    pub input: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchOutput {
    pub node: NodeId,
    pub output: Value,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    adapters: BTreeMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_adapter(
        mut self,
        adapter: impl Into<String>,
        executor: impl ToolExecutor + 'static,
    ) -> Self {
        self.register_adapter(adapter, executor);
        self
    }

    pub fn register_adapter(
        &mut self,
        adapter: impl Into<String>,
        executor: impl ToolExecutor + 'static,
    ) {
        self.adapters.insert(adapter.into(), Arc::new(executor));
    }

    fn executor(&self, adapter: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.adapters.get(adapter).cloned()
    }
}

#[derive(Clone)]
pub struct Runtime {
    registry: ToolRegistry,
    cache: Arc<Mutex<BTreeMap<execution::CacheKey, Value>>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub outputs: BTreeMap<String, Value>,
    pub node_outputs: BTreeMap<NodeId, Value>,
    pub trace: Vec<TraceEvent>,
    pub optimization: OptimizationReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub node: NodeId,
    pub tool: String,
    pub status: TraceStatus,
}

impl TraceEvent {
    fn started(node: &Node) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status: TraceStatus::Started,
        }
    }

    fn finished_with_status(node: &Node, status: TraceStatus) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status,
        }
    }

    fn failed(node: &Node, error: &ToolExecutionError) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status: TraceStatus::Failed(error.message.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceStatus {
    Started,
    Finished,
    CacheHit,
    Failed(String),
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct ToolExecutionError {
    pub message: String,
}

impl ToolExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error("node '{node}' failed: {error}")]
    Tool {
        node: NodeId,
        error: ToolExecutionError,
    },
    #[error("join error: {0}")]
    Join(JoinError),
    #[error("batch call did not return output for node '{node}'")]
    BatchMissingOutput { node: NodeId },
    #[error("no executor registered for adapter '{adapter}'")]
    MissingAdapter { adapter: String },
    #[error("node '{0}' has no output yet")]
    MissingNodeOutput(NodeId),
    #[error("reference '{reference}' missing path segment '{segment}'")]
    MissingPath {
        reference: ValueRef,
        segment: String,
    },
    #[error("invalid reference: {0}")]
    InvalidRef(String),
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, Plan, ToolSpec};

    use super::*;

    struct TestExecutor;

    #[async_trait]
    impl ToolExecutor for TestExecutor {
        async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            match tool {
                "const_user" => Ok(json!({ "id": input["id"], "name": "Ada" })),
                "echo" => Ok(input),
                "fail" => Err(ToolExecutionError::new("planned failure")),
                other => Err(ToolExecutionError::new(format!("unknown tool {other}"))),
            }
        }
    }

    struct BatchExecutor {
        calls: Arc<AtomicUsize>,
        batches: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolExecutor for BatchExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(input)
        }

        async fn call_batch(
            &self,
            _tool: &str,
            inputs: Vec<BatchInput>,
        ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
            self.batches.fetch_add(1, Ordering::SeqCst);
            Ok(inputs
                .into_iter()
                .map(|input| BatchOutput {
                    node: input.node,
                    output: input.input,
                })
                .collect())
        }
    }

    struct CountingExecutor {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolExecutor for CountingExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(input)
        }
    }

    struct FailingBatchExecutor;

    #[async_trait]
    impl ToolExecutor for FailingBatchExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            Ok(input)
        }

        async fn call_batch(
            &self,
            _tool: &str,
            _inputs: Vec<BatchInput>,
        ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
            Err(ToolExecutionError::new("batch failed"))
        }
    }

    struct MissingBatchOutputExecutor;

    #[async_trait]
    impl ToolExecutor for MissingBatchOutputExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            Ok(input)
        }

        async fn call_batch(
            &self,
            _tool: &str,
            _inputs: Vec<BatchInput>,
        ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
            Ok(Vec::new())
        }
    }

    struct WrongExecutor;

    #[async_trait]
    impl ToolExecutor for WrongExecutor {
        async fn call(&self, _tool: &str, _input: Value) -> Result<Value, ToolExecutionError> {
            Ok(json!({ "wrong": true }))
        }
    }

    struct ErrorExecutor;

    #[async_trait]
    impl ToolExecutor for ErrorExecutor {
        async fn call(&self, _tool: &str, _input: Value) -> Result<Value, ToolExecutionError> {
            Err(ToolExecutionError::new("always fails"))
        }
    }

    fn plan() -> Plan {
        let mut plan = Plan::new();
        plan.tools.insert(
            "const_user".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        plan.tools.insert(
            "fail".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        plan
    }

    #[tokio::test]
    async fn executes_layers_and_resolves_outputs() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("user", "const_user").with_input(json!({ "id": "u_1" })));
        plan.nodes
            .push(Node::new("message", "echo").with_input(json!({
                "user_id": { "$ref": "user.output.id" }
            })));
        plan.outputs
            .insert("message".into(), ValueRef::output("message"));

        let result = Runtime::new(TestExecutor).run(plan).await.unwrap();

        assert_eq!(result.outputs["message"], json!({ "user_id": "u_1" }));
        assert_eq!(result.trace.len(), 4);
    }

    #[tokio::test]
    async fn deduplicates_before_execution() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("a", "const_user").with_input(json!({ "id": "u_1" })));
        plan.nodes
            .push(Node::new("b", "const_user").with_input(json!({ "id": "u_1" })));
        plan.outputs.insert("user".into(), ValueRef::output("b"));

        let result = Runtime::new(TestExecutor).run(plan).await.unwrap();

        assert_eq!(
            result.outputs["user"],
            json!({ "id": "u_1", "name": "Ada" })
        );
        assert_eq!(result.node_outputs.len(), 1);
        assert_eq!(result.optimization.deduplicated.len(), 1);
    }

    #[tokio::test]
    async fn serial_baseline_does_not_deduplicate() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("a", "const_user").with_input(json!({ "id": "u_1" })));
        plan.nodes
            .push(Node::new("b", "const_user").with_input(json!({ "id": "u_1" })));
        plan.outputs.insert("user".into(), ValueRef::output("b"));

        let result = Runtime::new(TestExecutor).run_serial(plan).await.unwrap();

        assert_eq!(result.node_outputs.len(), 2);
        assert!(result.optimization.deduplicated.is_empty());
    }

    #[tokio::test]
    async fn batch_groups_use_batch_executor_contract() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));
        plan.outputs.insert("a".into(), ValueRef::output("a"));
        plan.outputs.insert("b".into(), ValueRef::output("b"));
        let calls = Arc::new(AtomicUsize::new(0));
        let batches = Arc::new(AtomicUsize::new(0));

        let result = Runtime::new(BatchExecutor {
            calls: calls.clone(),
            batches: batches.clone(),
        })
        .run(plan)
        .await
        .unwrap();

        assert_eq!(result.outputs["a"], json!({ "id": "a" }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(batches.load(Ordering::SeqCst), 1);
        assert_eq!(result.optimization.batch_groups.len(), 1);
    }

    #[tokio::test]
    async fn cache_survives_between_runtime_runs() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("message", "echo").with_input(json!({ "ok": true })));
        plan.outputs
            .insert("message".into(), ValueRef::output("message"));
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::new(CountingExecutor {
            calls: calls.clone(),
        });

        runtime.run(plan.clone()).await.unwrap();
        let second = runtime.run(plan).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            second
                .trace
                .iter()
                .any(|event| event.status == TraceStatus::CacheHit)
        );
    }

    #[tokio::test]
    async fn clear_cache_removes_cached_outputs() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("message", "echo").with_input(json!({ "ok": true })));
        plan.outputs
            .insert("message".into(), ValueRef::output("message"));
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::new(CountingExecutor {
            calls: calls.clone(),
        });

        runtime.run(plan.clone()).await.unwrap();
        runtime.clear_cache().await;
        runtime.run(plan).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cached_batch_group_returns_without_calling_executor_again() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));
        plan.outputs.insert("a".into(), ValueRef::output("a"));
        let calls = Arc::new(AtomicUsize::new(0));
        let batches = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::new(BatchExecutor {
            calls,
            batches: batches.clone(),
        });

        runtime.run(plan.clone()).await.unwrap();
        let second = runtime.run(plan).await.unwrap();

        assert_eq!(batches.load(Ordering::SeqCst), 1);
        assert_eq!(
            second
                .trace
                .iter()
                .filter(|event| event.status == TraceStatus::CacheHit)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn reports_missing_adapter_for_batch_group() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("missing").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));

        let error = Runtime::from_registry(ToolRegistry::new())
            .run(plan)
            .await
            .unwrap_err();

        assert!(matches!(error, RuntimeError::MissingAdapter { .. }));
    }

    #[tokio::test]
    async fn reports_batch_executor_errors() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));

        let error = Runtime::new(FailingBatchExecutor)
            .run(plan)
            .await
            .unwrap_err();

        assert!(matches!(error, RuntimeError::Tool { .. }));
    }

    #[tokio::test]
    async fn reports_missing_batch_outputs() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));

        let error = Runtime::new(MissingBatchOutputExecutor)
            .run(plan)
            .await
            .unwrap_err();

        assert!(matches!(error, RuntimeError::BatchMissingOutput { .. }));
    }

    #[tokio::test]
    async fn non_cacheable_tools_execute_every_time() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "write".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:item"].into_iter().map(String::from).collect(),
                ..Effects::default()
            }),
        );
        plan.nodes.push(Node::new("write", "write"));
        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = Runtime::new(CountingExecutor {
            calls: calls.clone(),
        });

        runtime.run(plan.clone()).await.unwrap();
        runtime.run(plan).await.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn registry_routes_by_adapter() {
        let mut plan = plan();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("custom").with_effects(Effects::pure()),
        );
        plan.nodes
            .push(Node::new("message", "echo").with_input(json!({ "ok": true })));
        plan.outputs
            .insert("message".into(), ValueRef::output("message"));
        let registry = ToolRegistry::new().with_adapter("custom", TestExecutor);

        let result = Runtime::from_registry(registry).run(plan).await.unwrap();

        assert_eq!(result.outputs["message"], json!({ "ok": true }));
    }

    #[tokio::test]
    async fn reports_missing_adapter() {
        let mut plan = plan();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("missing").with_effects(Effects::pure()),
        );
        plan.nodes.push(Node::new("message", "echo"));

        let error = Runtime::from_registry(ToolRegistry::new())
            .run(plan)
            .await
            .unwrap_err();

        assert!(matches!(error, RuntimeError::MissingAdapter { .. }));
    }

    #[tokio::test]
    async fn serializes_run_result_as_composite_tool_feedback() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("user", "const_user").with_input(json!({ "id": "u_1" })));
        plan.outputs.insert("user".into(), ValueRef::output("user"));

        let result = Runtime::new(TestExecutor).run(plan).await.unwrap();
        let value = serde_json::to_value(&result).unwrap();

        assert_eq!(value["outputs"]["user"]["id"], "u_1");
        assert_eq!(value["trace"][0]["status"], "started");
        assert!(value["optimization"]["deduplicated"].is_array());
    }

    #[tokio::test]
    async fn stops_on_tool_error() {
        let mut plan = plan();
        plan.nodes.push(Node::new("bad", "fail"));

        let error = Runtime::new(TestExecutor).run(plan).await.unwrap_err();

        assert!(matches!(error, RuntimeError::Tool { .. }));
    }

    #[tokio::test]
    async fn resolves_array_path_segments() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("items", "echo").with_input(json!({
                "values": [{ "id": "first" }, { "id": "second" }]
            })));
        plan.nodes.push(Node::new("pick", "echo").with_input(json!({
            "id": { "$ref": "items.output.values.1.id" }
        })));
        plan.outputs.insert("pick".into(), ValueRef::output("pick"));

        let result = Runtime::new(TestExecutor).run(plan).await.unwrap();

        assert_eq!(result.outputs["pick"], json!({ "id": "second" }));
    }

    #[tokio::test]
    async fn reports_missing_reference_paths() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("user", "const_user").with_input(json!({ "id": "u_1" })));
        plan.nodes.push(Node::new("bad", "echo").with_input(json!({
            "missing": { "$ref": "user.output.profile.name" }
        })));

        let error = Runtime::new(TestExecutor).run(plan).await.unwrap_err();

        assert!(matches!(error, RuntimeError::MissingPath { .. }));
    }

    #[tokio::test]
    async fn reports_invalid_runtime_refs() {
        let error =
            resolve_input(&json!({ "$ref": "not-a-runtime-ref" }), &BTreeMap::new()).unwrap_err();

        assert!(matches!(error, RuntimeError::InvalidRef(_)));
    }

    #[tokio::test]
    async fn reports_invalid_array_path_segment() {
        let mut outputs = BTreeMap::new();
        outputs.insert("items".to_owned(), json!(["a"]));
        let error = resolve_input(&json!({ "$ref": "items.output.nope" }), &outputs).unwrap_err();

        assert!(matches!(error, RuntimeError::MissingPath { .. }));
    }

    #[tokio::test]
    async fn reports_out_of_bounds_array_path_segment() {
        let mut outputs = BTreeMap::new();
        outputs.insert("items".to_owned(), json!(["a"]));
        let error = resolve_input(&json!({ "$ref": "items.output.9" }), &outputs).unwrap_err();

        assert!(matches!(error, RuntimeError::MissingPath { .. }));
    }

    #[tokio::test]
    async fn reports_scalar_path_segment() {
        let mut outputs = BTreeMap::new();
        outputs.insert("value".to_owned(), json!("text"));
        let error = resolve_input(&json!({ "$ref": "value.output.name" }), &outputs).unwrap_err();

        assert!(matches!(error, RuntimeError::MissingPath { .. }));
    }

    #[tokio::test]
    async fn conformance_suite_reports_adapter_checks() {
        let report = check_adapter_conformance("test", TestExecutor).await;

        assert!(report.passed);
        assert_eq!(report.checks.len(), 3);
    }

    #[tokio::test]
    async fn conformance_suite_reports_wrong_outputs() {
        let report = check_adapter_conformance("test", WrongExecutor).await;

        assert!(!report.passed);
        assert!(report.checks.iter().any(|check| !check.passed));
    }

    #[tokio::test]
    async fn conformance_suite_reports_executor_errors() {
        let report = check_adapter_conformance("test", ErrorExecutor).await;

        assert!(!report.passed);
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.message.contains("always fails"))
        );
    }
}
