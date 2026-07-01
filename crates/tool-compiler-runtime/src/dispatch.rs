//! Unit dispatch: cache lookups, single-flight, batching, fan-out expansion.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tool_compiler_adapter_api::{BatchInput, ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{NodeId, Plan, Resource, ToolName, canonical_json_string};

use crate::cache::{CacheKey, ToolCache};
use crate::policy::{CallPolicy, SingleFlight, call_batch_with_policy, call_with_policy};
use crate::result::{TraceEvent, TraceStatus};
use crate::scheduler::next_batch_id;

/// Static description of the unit being dispatched.
#[derive(Clone, Debug)]
pub(crate) struct UnitSpec {
    pub tool: ToolName,
    pub adapter: String,
    pub batch: bool,
}

/// One node prepared for dispatch: resolved input, policy, cache metadata.
pub(crate) struct PreparedMember {
    pub node: NodeId,
    pub input: Value,
    /// Resolved fan-out items for `for_each` nodes.
    pub items: Option<Vec<Value>>,
    pub policy: CallPolicy,
    pub cacheable: bool,
    pub version: Option<String>,
    pub reads: BTreeSet<Resource>,
    pub batchable: bool,
    pub batch_size: usize,
}

impl PreparedMember {
    pub(crate) fn key(&self, spec: &UnitSpec, input: &Value) -> Option<CacheKey> {
        self.cacheable.then(|| CacheKey {
            adapter: spec.adapter.clone(),
            tool: spec.tool.clone(),
            version: self.version.clone(),
            input: canonical_json_string(input),
        })
    }
}

/// What a dispatched unit reports back to the engine.
pub(crate) struct DispatchOutcome {
    pub results: Vec<(NodeId, Result<Value, ToolExecutionError>)>,
    pub events: Vec<TraceEvent>,
    pub cache_hits: usize,
    pub retries: usize,
    pub batch_dispatches: usize,
}

impl DispatchOutcome {
    fn new() -> Self {
        Self {
            results: Vec::new(),
            events: Vec::new(),
            cache_hits: 0,
            retries: 0,
            batch_dispatches: 0,
        }
    }
}

pub(crate) struct DispatchContext {
    pub executor: Arc<dyn ToolExecutor>,
    pub cache: Arc<dyn ToolCache>,
    pub single_flight: Arc<SingleFlight>,
    pub use_cache: bool,
}

pub(crate) async fn dispatch_unit(
    context: DispatchContext,
    spec: UnitSpec,
    members: Vec<PreparedMember>,
) -> DispatchOutcome {
    let mut outcome = DispatchOutcome::new();

    if spec.batch {
        dispatch_batch(&context, &spec, members, &mut outcome).await;
        return outcome;
    }

    for member in members {
        if member.items.is_some() {
            dispatch_fan_out(&context, &spec, member, &mut outcome).await;
        } else {
            dispatch_single(&context, &spec, member, &mut outcome).await;
        }
    }
    outcome
}

async fn dispatch_single(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: PreparedMember,
    outcome: &mut DispatchOutcome,
) {
    let key = context
        .use_cache
        .then(|| member.key(spec, &member.input))
        .flatten();

    // Single-flight: the first caller of a key executes; concurrent callers
    // wait and then re-check the cache.
    let _guard = match &key {
        Some(key) => Some(context.single_flight.acquire(key).await),
        None => None,
    };

    if let Some(key) = &key
        && let Some(cached) = context.cache.get(key).await
    {
        outcome.cache_hits += 1;
        outcome.events.push(TraceEvent::new(
            &member.node,
            &spec.tool,
            TraceStatus::CacheHit,
        ));
        outcome
            .results
            .push((member.node, cached.as_ref().clone()).into_ok());
        return;
    }

    outcome.events.push(TraceEvent::new(
        &member.node,
        &spec.tool,
        TraceStatus::Started,
    ));
    let started = Instant::now();
    let result =
        call_with_policy(&context.executor, &spec.tool, &member.input, &member.policy).await;
    let duration = elapsed_ms(started);

    record_retries(&member.node, &spec.tool, &result.retries, outcome);
    match result.result {
        Ok(output) => {
            if let Some(key) = key {
                context
                    .cache
                    .insert(key, Arc::new(output.clone()), member.reads.clone())
                    .await;
            }
            outcome.events.push(
                TraceEvent::new(&member.node, &spec.tool, TraceStatus::Finished)
                    .with_duration(duration),
            );
            outcome.results.push((member.node, Ok(output)));
        }
        Err(error) => {
            outcome.events.push(
                TraceEvent::new(&member.node, &spec.tool, TraceStatus::Failed)
                    .with_duration(duration)
                    .with_error(&error),
            );
            outcome.results.push((member.node, Err(error)));
        }
    }
}

async fn dispatch_batch(
    context: &DispatchContext,
    spec: &UnitSpec,
    members: Vec<PreparedMember>,
    outcome: &mut DispatchOutcome,
) {
    let mut pending = Vec::new();
    for member in members {
        let key = context
            .use_cache
            .then(|| member.key(spec, &member.input))
            .flatten();
        if let Some(key) = &key
            && let Some(cached) = context.cache.get(key).await
        {
            outcome.cache_hits += 1;
            outcome.events.push(TraceEvent::new(
                &member.node,
                &spec.tool,
                TraceStatus::CacheHit,
            ));
            outcome
                .results
                .push((member.node, cached.as_ref().clone()).into_ok());
            continue;
        }
        pending.push((member, key));
    }

    if pending.is_empty() {
        return;
    }

    let batch_id = next_batch_id();
    outcome.batch_dispatches += 1;
    for (member, _) in &pending {
        outcome.events.push(
            TraceEvent::new(&member.node, &spec.tool, TraceStatus::Started)
                .with_batch(Some(batch_id)),
        );
    }

    let inputs: Vec<BatchInput> = pending
        .iter()
        .map(|(member, _)| BatchInput {
            node: member.node.clone(),
            input: member.input.clone(),
        })
        .collect();
    let policy = pending[0].0.policy.clone();
    let started = Instant::now();
    let result = call_batch_with_policy(&context.executor, &spec.tool, &inputs, &policy).await;
    let duration = elapsed_ms(started);
    outcome.retries += result.retries.len();

    match result.result {
        Ok(outputs) => {
            let mut by_node: BTreeMap<NodeId, Value> = outputs
                .into_iter()
                .map(|output| (output.node, output.output))
                .collect();
            for (member, key) in pending {
                match by_node.remove(&member.node) {
                    Some(output) => {
                        if let Some(key) = key {
                            context
                                .cache
                                .insert(key, Arc::new(output.clone()), member.reads.clone())
                                .await;
                        }
                        outcome.events.push(
                            TraceEvent::new(&member.node, &spec.tool, TraceStatus::Finished)
                                .with_duration(duration)
                                .with_batch(Some(batch_id)),
                        );
                        outcome.results.push((member.node, Ok(output)));
                    }
                    None => {
                        let error = ToolExecutionError::new(format!(
                            "batch call did not return output for node '{}'",
                            member.node
                        ))
                        .with_code("batch_missing_output");
                        outcome.events.push(
                            TraceEvent::new(&member.node, &spec.tool, TraceStatus::Failed)
                                .with_duration(duration)
                                .with_batch(Some(batch_id))
                                .with_error(&error),
                        );
                        outcome.results.push((member.node, Err(error)));
                    }
                }
            }
        }
        Err(error) => {
            for (member, _) in pending {
                outcome.events.push(
                    TraceEvent::new(&member.node, &spec.tool, TraceStatus::Failed)
                        .with_duration(duration)
                        .with_batch(Some(batch_id))
                        .with_error(&error),
                );
                outcome.results.push((member.node, Err(error.clone())));
            }
        }
    }
}

/// Executes a `for_each` node: one call per item, batched through
/// `call_batch` in `batch_size` chunks when the tool is batchable, otherwise
/// sequential. The node output is the array of per-item outputs in order.
async fn dispatch_fan_out(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: PreparedMember,
    outcome: &mut DispatchOutcome,
) {
    let items = member.items.clone().unwrap_or_default();
    outcome.events.push(TraceEvent::new(
        &member.node,
        &spec.tool,
        TraceStatus::Started,
    ));
    let started = Instant::now();

    let mut item_inputs = Vec::with_capacity(items.len());
    for item in &items {
        match crate::refs::substitute_item(&member.input, item) {
            Ok(input) => item_inputs.push(input),
            Err(error) => {
                let error = ToolExecutionError::new(error.to_string()).with_code("invalid_item");
                fail_fan_out(&member, spec, error, started, outcome);
                return;
            }
        }
    }

    let mut results: Vec<Option<Value>> = vec![None; item_inputs.len()];
    let mut pending: Vec<usize> = Vec::new();
    if context.use_cache {
        for (index, input) in item_inputs.iter().enumerate() {
            match member.key(spec, input) {
                Some(key) => match context.cache.get(&key).await {
                    Some(cached) => {
                        outcome.cache_hits += 1;
                        results[index] = Some(cached.as_ref().clone());
                    }
                    None => pending.push(index),
                },
                None => pending.push(index),
            }
        }
    } else {
        pending = (0..item_inputs.len()).collect();
    }

    let run = if member.batchable && pending.len() > 1 {
        run_fan_out_batched(
            context,
            spec,
            &member,
            &item_inputs,
            &pending,
            &mut results,
            outcome,
        )
        .await
    } else {
        run_fan_out_sequential(
            context,
            spec,
            &member,
            &item_inputs,
            &pending,
            &mut results,
            outcome,
        )
        .await
    };
    if let Err(error) = run {
        fail_fan_out(&member, spec, error, started, outcome);
        return;
    }

    let output = Value::Array(results.into_iter().map(Option::unwrap_or_default).collect());
    outcome.events.push(
        TraceEvent::new(&member.node, &spec.tool, TraceStatus::Finished)
            .with_duration(elapsed_ms(started)),
    );
    outcome.results.push((member.node, Ok(output)));
}

async fn run_fan_out_batched(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    item_inputs: &[Value],
    pending: &[usize],
    results: &mut [Option<Value>],
    outcome: &mut DispatchOutcome,
) -> Result<(), ToolExecutionError> {
    for chunk in pending.chunks(member.batch_size.max(1)) {
        let inputs: Vec<BatchInput> = chunk
            .iter()
            .map(|index| BatchInput {
                node: index.to_string(),
                input: item_inputs[*index].clone(),
            })
            .collect();
        outcome.batch_dispatches += 1;
        let result =
            call_batch_with_policy(&context.executor, &spec.tool, &inputs, &member.policy).await;
        outcome.retries += result.retries.len();
        let outputs = result.result?;
        let mut by_index: BTreeMap<usize, Value> = outputs
            .into_iter()
            .filter_map(|output| Some((output.node.parse::<usize>().ok()?, output.output)))
            .collect();
        for index in chunk {
            let output = by_index.remove(index).ok_or_else(|| {
                ToolExecutionError::new(format!(
                    "batch call did not return output for item {index}"
                ))
                .with_code("batch_missing_output")
            })?;
            insert_item_cache(context, spec, member, &item_inputs[*index], &output).await;
            results[*index] = Some(output);
        }
    }
    Ok(())
}

async fn run_fan_out_sequential(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    item_inputs: &[Value],
    pending: &[usize],
    results: &mut [Option<Value>],
    outcome: &mut DispatchOutcome,
) -> Result<(), ToolExecutionError> {
    for index in pending {
        let result = call_with_policy(
            &context.executor,
            &spec.tool,
            &item_inputs[*index],
            &member.policy,
        )
        .await;
        outcome.retries += result.retries.len();
        let output = result.result.map_err(|error| {
            ToolExecutionError::new(format!("item {index}: {}", error.message))
                .with_code(error.code.unwrap_or_else(|| "item_failed".into()))
        })?;
        insert_item_cache(context, spec, member, &item_inputs[*index], &output).await;
        results[*index] = Some(output);
    }
    Ok(())
}

async fn insert_item_cache(
    context: &DispatchContext,
    spec: &UnitSpec,
    member: &PreparedMember,
    input: &Value,
    output: &Value,
) {
    if context.use_cache
        && let Some(key) = member.key(spec, input)
    {
        context
            .cache
            .insert(key, Arc::new(output.clone()), member.reads.clone())
            .await;
    }
}

fn fail_fan_out(
    member: &PreparedMember,
    spec: &UnitSpec,
    error: ToolExecutionError,
    started: Instant,
    outcome: &mut DispatchOutcome,
) {
    outcome.events.push(
        TraceEvent::new(&member.node, &spec.tool, TraceStatus::Failed)
            .with_duration(elapsed_ms(started))
            .with_error(&error),
    );
    outcome.results.push((member.node.clone(), Err(error)));
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn record_retries(
    node: &str,
    tool: &str,
    retries: &[ToolExecutionError],
    outcome: &mut DispatchOutcome,
) {
    outcome.retries += retries.len();
    for (attempt, error) in retries.iter().enumerate() {
        outcome.events.push(
            TraceEvent::new(node, tool, TraceStatus::Retried)
                .with_error(error)
                .with_attempt((attempt + 1).min(u8::MAX as usize) as u8),
        );
    }
}

trait IntoOk {
    type Out;
    fn into_ok(self) -> Self::Out;
}

impl IntoOk for (NodeId, Value) {
    type Out = (NodeId, Result<Value, ToolExecutionError>);
    fn into_ok(self) -> Self::Out {
        (self.0, Ok(self.1))
    }
}

/// Builds prepared members' cache/batch metadata from the plan.
pub(crate) fn member_from_plan(
    node: &tool_compiler_ir::Node,
    plan: &Plan,
    input: Value,
    items: Option<Vec<Value>>,
    policy: CallPolicy,
    reads: BTreeSet<Resource>,
) -> PreparedMember {
    let spec = plan.tools.get(&node.tool);
    let effects = spec.and_then(|spec| spec.effects.as_ref());
    PreparedMember {
        node: node.id.clone(),
        input,
        items,
        policy,
        cacheable: effects.is_some_and(|effects| effects.pure || effects.cacheable),
        version: spec.and_then(|spec| spec.version.clone()),
        reads,
        batchable: effects.is_some_and(|effects| effects.batchable),
        batch_size: spec
            .and_then(|spec| spec.limits.as_ref())
            .and_then(|limits| limits.batch_size)
            .unwrap_or(usize::MAX),
    }
}
