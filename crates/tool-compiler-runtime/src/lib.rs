use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::task::JoinError;
use tool_compiler_graph::GraphError;
use tool_compiler_ir::{Effects, Node, NodeId, ToolCost, ToolLimits, ToolSpec, ValueRef};
use tool_compiler_optimizer::OptimizationReport;

mod conformance;
mod execution;
mod refs;

pub use conformance::{ConformanceCheck, ConformanceReport, check_adapter_conformance};
pub use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};

#[cfg(test)]
use refs::resolve_input;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CacheKey {
    pub adapter: String,
    pub tool: String,
    pub input: String,
}

#[async_trait]
pub trait ToolCache: Send + Sync {
    async fn get(&self, key: &CacheKey) -> Option<Value>;
    async fn insert(&self, key: CacheKey, output: Value);
    async fn clear(&self);
}

#[derive(Clone, Default)]
pub struct MemoryCache {
    inner: Arc<Mutex<BTreeMap<CacheKey, Value>>>,
}

#[async_trait]
impl ToolCache for MemoryCache {
    async fn get(&self, key: &CacheKey) -> Option<Value> {
        self.inner.lock().await.get(key).cloned()
    }

    async fn insert(&self, key: CacheKey, output: Value) {
        self.inner.lock().await.insert(key, output);
    }

    async fn clear(&self) {
        self.inner.lock().await.clear();
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    adapters: BTreeMap<String, Arc<dyn ToolExecutor>>,
    capabilities: BTreeMap<String, ToolCapabilities>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ToolLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ToolCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl ToolCapabilities {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_effects(mut self, effects: Effects) -> Self {
        self.effects = Some(effects);
        self
    }

    pub fn with_limits(mut self, limits: ToolLimits) -> Self {
        self.limits = Some(limits);
        self
    }

    pub fn with_cost(mut self, cost: ToolCost) -> Self {
        self.cost = Some(cost);
        self
    }
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

    pub fn with_tool_capabilities(
        mut self,
        tool: impl Into<String>,
        capabilities: ToolCapabilities,
    ) -> Self {
        self.register_tool_capabilities(tool, capabilities);
        self
    }

    pub fn register_tool_capabilities(
        &mut self,
        tool: impl Into<String>,
        capabilities: ToolCapabilities,
    ) {
        self.capabilities.insert(tool.into(), capabilities);
    }

    pub fn capabilities(&self, tool: &str) -> Option<&ToolCapabilities> {
        self.capabilities.get(tool)
    }

    pub(crate) fn apply_capabilities(&self, plan: &mut tool_compiler_ir::Plan) {
        for (tool, spec) in &mut plan.tools {
            if let Some(capabilities) = self.capabilities.get(tool) {
                merge_capabilities(spec, capabilities);
            }
        }
    }

    fn executor(&self, adapter: &str) -> Option<Arc<dyn ToolExecutor>> {
        self.adapters.get(adapter).cloned()
    }
}

fn merge_capabilities(spec: &mut ToolSpec, capabilities: &ToolCapabilities) {
    if spec.effects.is_none() {
        spec.effects = capabilities.effects.clone();
    }
    if spec.limits.is_none() {
        spec.limits = capabilities.limits.clone();
    }
    if spec.cost.is_none() {
        spec.cost = capabilities.cost.clone();
    }
}

#[derive(Clone)]
pub struct Runtime {
    registry: ToolRegistry,
    cache: Arc<dyn ToolCache>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u128>,
}

impl TraceEvent {
    fn started(node: &Node) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status: TraceStatus::Started,
            duration_ms: None,
        }
    }

    fn finished_with_status(node: &Node, status: TraceStatus, duration_ms: u128) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status,
            duration_ms: Some(duration_ms),
        }
    }

    fn failed(node: &Node, error: &ToolExecutionError, duration_ms: u128) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status: TraceStatus::Failed(error.message.clone()),
            duration_ms: Some(duration_ms),
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
    use tool_compiler_ir::{Effects, Node, Plan, ToolLimits, ToolSpec};

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

    struct ConcurrentExecutor {
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolExecutor for ConcurrentExecutor {
        async fn call(&self, _tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(input)
        }
    }

    #[derive(Default)]
    struct CountingCache {
        gets: Arc<AtomicUsize>,
        inserts: Arc<AtomicUsize>,
        inner: MemoryCache,
    }

    #[async_trait]
    impl ToolCache for CountingCache {
        async fn get(&self, key: &CacheKey) -> Option<Value> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            self.inner.get(key).await
        }

        async fn insert(&self, key: CacheKey, output: Value) {
            self.inserts.fetch_add(1, Ordering::SeqCst);
            self.inner.insert(key, output).await;
        }

        async fn clear(&self) {
            self.inner.clear().await;
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
    async fn batch_size_limit_splits_optimizer_groups() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test")
                .with_effects(Effects {
                    batchable: true,
                    ..Effects::pure()
                })
                .with_limits(ToolLimits {
                    batch_size: Some(2),
                    max_concurrency: None,
                }),
        );
        for id in ["a", "b", "c", "d"] {
            plan.nodes
                .push(Node::new(id, "echo").with_input(json!({ "id": id })));
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let batches = Arc::new(AtomicUsize::new(0));

        Runtime::new(BatchExecutor {
            calls,
            batches: batches.clone(),
        })
        .run(plan)
        .await
        .unwrap();

        assert_eq!(batches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn max_concurrency_limit_throttles_layer_execution() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test")
                .with_effects(Effects::read_only(["api:item"]))
                .with_limits(ToolLimits {
                    max_concurrency: Some(1),
                    batch_size: None,
                }),
        );
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));
        let max_active = Arc::new(AtomicUsize::new(0));

        Runtime::new(ConcurrentExecutor {
            active: Arc::new(AtomicUsize::new(0)),
            max_active: max_active.clone(),
        })
        .run(plan)
        .await
        .unwrap();

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
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
    async fn runtime_accepts_custom_cache_backend() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("message", "echo").with_input(json!({ "ok": true })));
        let gets = Arc::new(AtomicUsize::new(0));
        let inserts = Arc::new(AtomicUsize::new(0));
        let cache = CountingCache {
            gets: gets.clone(),
            inserts: inserts.clone(),
            inner: MemoryCache::default(),
        };
        let runtime = Runtime::with_cache(
            ToolRegistry::new().with_adapter("test", TestExecutor),
            cache,
        );

        runtime.run(plan).await.unwrap();

        assert_eq!(gets.load(Ordering::SeqCst), 1);
        assert_eq!(inserts.load(Ordering::SeqCst), 1);
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
    async fn registry_capabilities_hydrate_plan_tools() {
        let mut plan = Plan::new();
        plan.tools.insert("echo".into(), ToolSpec::new("custom"));
        plan.nodes
            .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
        plan.nodes
            .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));
        let registry = ToolRegistry::new()
            .with_adapter(
                "custom",
                BatchExecutor {
                    calls: Arc::new(AtomicUsize::new(0)),
                    batches: Arc::new(AtomicUsize::new(0)),
                },
            )
            .with_tool_capabilities(
                "echo",
                ToolCapabilities::new().with_effects(Effects {
                    batchable: true,
                    ..Effects::pure()
                }),
            );

        let result = Runtime::from_registry(registry).run(plan).await.unwrap();

        assert_eq!(result.optimization.batch_groups.len(), 1);
        assert_eq!(result.optimization.summary.estimated_tool_calls_after, 1);
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
        assert!(value["trace"][1]["duration_ms"].is_number());
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
