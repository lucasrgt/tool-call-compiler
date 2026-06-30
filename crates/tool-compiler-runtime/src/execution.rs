use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tokio::sync::Semaphore;
use tool_compiler_graph::validate;
use tool_compiler_ir::{Effects, Node, NodeId, Plan, ToolLimits};
use tool_compiler_optimizer::{BatchGroup, OptimizationReport, OptimizedPlan, optimize};

use crate::refs::{resolve_input, resolve_value_ref};
use crate::{BatchInput, CacheKey, MemoryCache, RunResult, Runtime, RuntimeError, ToolCache};
use crate::{ToolExecutor, ToolRegistry, TraceEvent, TraceStatus};

#[derive(Clone, Debug)]
struct PreparedCall {
    node: Node,
    input: Value,
    cache_key: Option<CacheKey>,
}

#[derive(Clone, Debug)]
struct NodeExecution {
    node: Node,
    started: Option<TraceEvent>,
    status: TraceStatus,
    duration_ms: u128,
    result: Result<Value, crate::ToolExecutionError>,
}

impl Runtime {
    pub fn new(executor: impl ToolExecutor + 'static) -> Self {
        Self::from_registry(ToolRegistry::new().with_adapter("test", executor))
    }

    pub fn from_registry(registry: ToolRegistry) -> Self {
        Self::with_cache(registry, MemoryCache::default())
    }

    pub fn with_cache(registry: ToolRegistry, cache: impl ToolCache + 'static) -> Self {
        Self {
            registry,
            cache: Arc::new(cache),
        }
    }

    pub async fn run(&self, plan: Plan) -> Result<RunResult, RuntimeError> {
        let optimized = optimize(plan)?;
        self.run_optimized(optimized).await
    }

    pub async fn run_serial(&self, plan: Plan) -> Result<RunResult, RuntimeError> {
        let graph = validate(&plan)?;
        let layers = graph
            .layers()
            .iter()
            .flat_map(|layer| layer.iter().map(|node| vec![node.clone()]))
            .collect();
        self.run_layers(plan, layers, OptimizationReport::default())
            .await
    }

    pub async fn clear_cache(&self) {
        self.cache.clear().await;
    }

    async fn run_optimized(&self, optimized: OptimizedPlan) -> Result<RunResult, RuntimeError> {
        self.run_layers(
            optimized.plan().clone(),
            optimized.graph().layers().to_vec(),
            optimized.report().clone(),
        )
        .await
    }

    async fn run_layers(
        &self,
        plan: Plan,
        layers: Vec<Vec<NodeId>>,
        optimization: OptimizationReport,
    ) -> Result<RunResult, RuntimeError> {
        let nodes_by_id = index_nodes(&plan);
        let mut node_outputs = BTreeMap::<NodeId, Value>::new();
        let mut trace = Vec::new();

        for layer in &layers {
            let mut tasks = tokio::task::JoinSet::new();
            let mut claimed = BTreeSet::new();
            let permits = Arc::new(Semaphore::new(
                layer_max_concurrency(&plan, layer)
                    .unwrap_or(layer.len())
                    .max(1),
            ));

            for group in layer_batch_groups(&plan, &optimization, layer) {
                let adapter = group.adapter.clone();
                let executor = self.registry.executor(&adapter).ok_or_else(|| {
                    RuntimeError::MissingAdapter {
                        adapter: adapter.clone(),
                    }
                })?;
                let calls = prepare_calls(&plan, &nodes_by_id, &node_outputs, &group.nodes)?;
                claimed.extend(group.nodes.iter().cloned());
                let cache = self.cache.clone();
                let permits = permits.clone();
                tasks.spawn(async move {
                    let _permit = permits.acquire_owned().await.expect("semaphore is open");
                    execute_batch(cache, group.tool, executor, calls).await
                });
            }

            for node_id in layer.iter().filter(|node_id| !claimed.contains(*node_id)) {
                let node = nodes_by_id
                    .get(node_id)
                    .expect("validated graph node should exist")
                    .clone();
                let input = resolve_input(&node.input, &node_outputs)?;
                let adapter = plan
                    .tools
                    .get(&node.tool)
                    .expect("validated tool should exist")
                    .adapter
                    .clone();
                let executor = self.registry.executor(&adapter).ok_or_else(|| {
                    RuntimeError::MissingAdapter {
                        adapter: adapter.clone(),
                    }
                })?;
                let call = PreparedCall {
                    cache_key: cache_key(&plan, &node, &adapter, &input),
                    node,
                    input,
                };
                let cache = self.cache.clone();
                let permits = permits.clone();

                tasks.spawn(async move {
                    let _permit = permits.acquire_owned().await.expect("semaphore is open");
                    execute_single(cache, executor, call).await
                });
            }

            while let Some(task) = tasks.join_next().await {
                record_events(
                    task.map_err(RuntimeError::Join)??,
                    &mut trace,
                    &mut node_outputs,
                )?;
            }
        }

        let mut outputs = BTreeMap::new();
        for (name, value_ref) in &plan.outputs {
            outputs.insert(name.clone(), resolve_value_ref(value_ref, &node_outputs)?);
        }

        Ok(RunResult {
            outputs,
            node_outputs,
            trace,
            optimization,
        })
    }
}

async fn execute_single(
    cache: Arc<dyn ToolCache>,
    executor: Arc<dyn ToolExecutor>,
    call: PreparedCall,
) -> Result<Vec<NodeExecution>, RuntimeError> {
    Ok(vec![execute_prepared_call(cache, executor, call).await])
}

async fn execute_batch(
    cache: Arc<dyn ToolCache>,
    tool: String,
    executor: Arc<dyn ToolExecutor>,
    calls: Vec<PreparedCall>,
) -> Result<Vec<NodeExecution>, RuntimeError> {
    let (mut events, pending) = split_cached_calls(&cache, calls).await;
    if pending.is_empty() {
        return Ok(events);
    }

    let batch_inputs = pending
        .iter()
        .map(|call| BatchInput {
            node: call.node.id.clone(),
            input: call.input.clone(),
        })
        .collect();

    let started = Instant::now();
    match executor.call_batch(&tool, batch_inputs).await {
        Ok(outputs) => {
            push_batch_outputs(
                &cache,
                &mut events,
                pending,
                outputs,
                started.elapsed().as_millis(),
            )
            .await?
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis();
            events.extend(pending.into_iter().map(|call| NodeExecution {
                started: Some(TraceEvent::started(&call.node)),
                node: call.node,
                status: TraceStatus::Finished,
                duration_ms,
                result: Err(error.clone()),
            }));
        }
    }

    Ok(events)
}

async fn split_cached_calls(
    cache: &Arc<dyn ToolCache>,
    calls: Vec<PreparedCall>,
) -> (Vec<NodeExecution>, Vec<PreparedCall>) {
    let mut cached = Vec::new();
    let mut pending = Vec::new();

    for call in calls {
        if let Some(output) = cached_output(cache, &call.cache_key).await {
            cached.push(cache_hit(call.node, output));
        } else {
            pending.push(call);
        }
    }

    (cached, pending)
}

async fn push_batch_outputs(
    cache: &Arc<dyn ToolCache>,
    events: &mut Vec<NodeExecution>,
    pending: Vec<PreparedCall>,
    outputs: Vec<crate::BatchOutput>,
    duration_ms: u128,
) -> Result<(), RuntimeError> {
    let mut by_node = outputs
        .into_iter()
        .map(|output| (output.node, output.output))
        .collect::<BTreeMap<_, _>>();

    for call in pending {
        let output =
            by_node
                .remove(&call.node.id)
                .ok_or_else(|| RuntimeError::BatchMissingOutput {
                    node: call.node.id.clone(),
                })?;
        insert_cache(cache, &call.cache_key, &output).await;
        events.push(NodeExecution {
            started: Some(TraceEvent::started(&call.node)),
            node: call.node,
            status: TraceStatus::Finished,
            duration_ms,
            result: Ok(output),
        });
    }

    Ok(())
}

async fn execute_prepared_call(
    cache: Arc<dyn ToolCache>,
    executor: Arc<dyn ToolExecutor>,
    call: PreparedCall,
) -> NodeExecution {
    if let Some(output) = cached_output(&cache, &call.cache_key).await {
        return cache_hit(call.node, output);
    }

    let started = Instant::now();
    let result = executor.call(&call.node.tool, call.input).await;
    let duration_ms = started.elapsed().as_millis();
    if let Ok(output) = &result {
        insert_cache(&cache, &call.cache_key, output).await;
    }

    NodeExecution {
        started: Some(TraceEvent::started(&call.node)),
        node: call.node,
        status: TraceStatus::Finished,
        duration_ms,
        result,
    }
}

fn record_events(
    events: Vec<NodeExecution>,
    trace: &mut Vec<TraceEvent>,
    node_outputs: &mut BTreeMap<NodeId, Value>,
) -> Result<(), RuntimeError> {
    for event in events {
        if let Some(started) = event.started {
            trace.push(started);
        }

        match event.result {
            Ok(output) => {
                trace.push(TraceEvent::finished_with_status(
                    &event.node,
                    event.status,
                    event.duration_ms,
                ));
                node_outputs.insert(event.node.id, output);
            }
            Err(error) => {
                trace.push(TraceEvent::failed(&event.node, &error, event.duration_ms));
                return Err(RuntimeError::Tool {
                    node: event.node.id,
                    error,
                });
            }
        }
    }
    Ok(())
}

async fn cached_output(cache: &Arc<dyn ToolCache>, key: &Option<CacheKey>) -> Option<Value> {
    let key = key.as_ref()?;
    cache.get(key).await
}

async fn insert_cache(cache: &Arc<dyn ToolCache>, key: &Option<CacheKey>, output: &Value) {
    if let Some(key) = key {
        cache.insert(key.clone(), output.clone()).await;
    }
}

fn cache_hit(node: Node, output: Value) -> NodeExecution {
    NodeExecution {
        node,
        started: None,
        status: TraceStatus::CacheHit,
        duration_ms: 0,
        result: Ok(output),
    }
}

fn prepare_calls(
    plan: &Plan,
    nodes_by_id: &BTreeMap<NodeId, Node>,
    node_outputs: &BTreeMap<NodeId, Value>,
    node_ids: &[NodeId],
) -> Result<Vec<PreparedCall>, RuntimeError> {
    node_ids
        .iter()
        .map(|node_id| {
            let node = nodes_by_id
                .get(node_id)
                .expect("validated graph node should exist")
                .clone();
            let input = resolve_input(&node.input, node_outputs)?;
            let adapter = plan
                .tools
                .get(&node.tool)
                .expect("validated tool should exist")
                .adapter
                .clone();
            Ok(PreparedCall {
                cache_key: cache_key(plan, &node, &adapter, &input),
                node,
                input,
            })
        })
        .collect()
}

fn layer_batch_groups(
    plan: &Plan,
    report: &OptimizationReport,
    layer: &[NodeId],
) -> Vec<BatchGroup> {
    let layer = layer.iter().collect::<BTreeSet<_>>();
    report
        .batch_groups
        .iter()
        .filter(|group| group.nodes.iter().all(|node| layer.contains(node)))
        .flat_map(|group| split_batch_group(group.clone(), tool_limits(plan, &group.tool)))
        .collect()
}

fn split_batch_group(group: BatchGroup, limits: Option<&ToolLimits>) -> Vec<BatchGroup> {
    let batch_size = limits
        .and_then(|limits| limits.batch_size)
        .unwrap_or(group.nodes.len());
    let batch_size = batch_size.max(1);
    group
        .nodes
        .chunks(batch_size)
        .map(|nodes| BatchGroup {
            adapter: group.adapter.clone(),
            tool: group.tool.clone(),
            nodes: nodes.to_vec(),
        })
        .filter(|group| group.nodes.len() > 1)
        .collect()
}

fn tool_limits<'a>(plan: &'a Plan, tool: &str) -> Option<&'a ToolLimits> {
    plan.tools.get(tool).and_then(|spec| spec.limits.as_ref())
}

fn layer_max_concurrency(plan: &Plan, layer: &[NodeId]) -> Option<usize> {
    layer
        .iter()
        .filter_map(|node_id| {
            plan.nodes
                .iter()
                .find(|node| &node.id == node_id)
                .and_then(|node| tool_limits(plan, &node.tool))
                .and_then(|limits| limits.max_concurrency)
        })
        .min()
}

fn cache_key(plan: &Plan, node: &Node, adapter: &str, input: &Value) -> Option<CacheKey> {
    let effects = plan.tools.get(&node.tool)?.effects.as_ref()?;
    if !cacheable(effects) {
        return None;
    }

    Some(CacheKey {
        adapter: adapter.to_owned(),
        tool: node.tool.clone(),
        input: serde_json::to_string(input).ok()?,
    })
}

fn cacheable(effects: &Effects) -> bool {
    effects.pure || effects.cacheable
}

fn index_nodes(plan: &Plan) -> BTreeMap<NodeId, Node> {
    plan.nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect()
}
