use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::task::JoinError;
use tool_compiler_graph::GraphError;
use tool_compiler_ir::{Node, NodeId, Plan, REF_KEY, ValueRef};
use tool_compiler_optimizer::{OptimizationReport, OptimizedPlan, optimize};

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError>;
}

#[derive(Clone)]
pub struct Runtime {
    executor: Arc<dyn ToolExecutor>,
}

impl Runtime {
    pub fn new(executor: impl ToolExecutor + 'static) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }

    pub async fn run(&self, plan: Plan) -> Result<RunResult, RuntimeError> {
        let optimized = optimize(plan)?;
        self.run_optimized(optimized).await
    }

    async fn run_optimized(&self, optimized: OptimizedPlan) -> Result<RunResult, RuntimeError> {
        let nodes_by_id = index_nodes(optimized.plan());
        let mut node_outputs = BTreeMap::<NodeId, Value>::new();
        let mut trace = Vec::new();

        for layer in optimized.graph().layers() {
            let mut tasks = tokio::task::JoinSet::new();

            for node_id in layer {
                let node = nodes_by_id
                    .get(node_id)
                    .expect("validated graph node should exist")
                    .clone();
                let input = resolve_input(&node.input, &node_outputs)?;
                let executor = Arc::clone(&self.executor);

                tasks.spawn(async move {
                    let started = TraceEvent::started(&node);
                    let result = executor.call(&node.tool, input).await;
                    (node, started, result)
                });
            }

            while let Some(task) = tasks.join_next().await {
                let (node, started, result) = task.map_err(RuntimeError::Join)?;
                trace.push(started);

                match result {
                    Ok(output) => {
                        trace.push(TraceEvent::finished(&node));
                        node_outputs.insert(node.id, output);
                    }
                    Err(error) => {
                        trace.push(TraceEvent::failed(&node, &error));
                        return Err(RuntimeError::Tool {
                            node: node.id,
                            error,
                        });
                    }
                }
            }
        }

        let mut outputs = BTreeMap::new();
        for (name, value_ref) in &optimized.plan().outputs {
            outputs.insert(name.clone(), resolve_value_ref(value_ref, &node_outputs)?);
        }

        Ok(RunResult {
            outputs,
            node_outputs,
            trace,
            optimization: optimized.report().clone(),
        })
    }
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

    fn finished(node: &Node) -> Self {
        Self {
            node: node.id.clone(),
            tool: node.tool.clone(),
            status: TraceStatus::Finished,
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

fn index_nodes(plan: &Plan) -> BTreeMap<NodeId, Node> {
    plan.nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect()
}

fn resolve_input(
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

fn resolve_value_ref(
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

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, ToolSpec};

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
}
