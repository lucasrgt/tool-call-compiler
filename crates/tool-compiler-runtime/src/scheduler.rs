//! Dependency-driven scheduler.
//!
//! Nodes start as soon as their dependencies finish — there is no layer
//! barrier. Effect safety is enforced at runtime by a resource lock table
//! (reads share, writes exclude, same-tool commutative writes overlap,
//! undeclared effects run alone), which preserves the guarantees of the
//! layered model while letting independent branches overlap.
//!
//! Failures never abort in-flight work: the engine stops scheduling (in
//! fail-fast mode), drains what is running, and returns a partial
//! [`RunResult`] with per-node errors and skips.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::Value;
use tokio::task::JoinSet;
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};
use tool_compiler_graph::{ExecutionGraph, ResolvedEffects};
use tool_compiler_ir::{Node, NodeId, Plan, ToolName, When, collect_refs};
use tool_compiler_optimizer::OptimizationReport;

use crate::dispatch::{
    DispatchContext, DispatchOutcome, PreparedMember, UnitSpec, dispatch_unit, member_from_plan,
};
use crate::locks::{LockRequest, LockTable};
use crate::policy::CallPolicy;
use crate::refs::{resolve_input, resolve_value_ref};
use crate::result::{RunMetrics, RunResult, RunStatus, SkipReason, TraceEvent, TraceStatus};
use crate::{ErrorMode, ResultMode, RunConfig, Runtime, RuntimeError, result::shape_output};

static BATCH_IDS: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_batch_id() -> u64 {
    BATCH_IDS.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug)]
struct Unit {
    members: Vec<NodeId>,
    tool: ToolName,
    adapter: String,
    batch: bool,
    lock: LockRequest,
    cost: u64,
}

pub(crate) struct Engine {
    plan: Plan,
    config: RunConfig,
    optimization: OptimizationReport,
    context_seed: ContextSeed,
    nodes: BTreeMap<NodeId, Arc<Node>>,
    effects: BTreeMap<NodeId, ResolvedEffects>,
    data_deps: BTreeMap<NodeId, BTreeSet<NodeId>>,
    units: Vec<Unit>,
    unit_deps: Vec<BTreeSet<usize>>,
    unit_dependents: Vec<BTreeSet<usize>>,
    serial_position: Option<BTreeMap<usize, usize>>,
    blocked_tools: BTreeSet<String>,
    validators: BTreeMap<ToolName, Arc<jsonschema::Validator>>,

    outputs: BTreeMap<NodeId, Value>,
    dead: BTreeSet<NodeId>,
    errors: BTreeMap<NodeId, ToolExecutionError>,
    skipped: BTreeMap<NodeId, SkipReason>,
    trace: Vec<TraceEvent>,
    metrics: RunMetrics,
    locks: LockTable,
    tool_in_flight: BTreeMap<ToolName, usize>,
    total_in_flight: usize,
    stop: Option<SkipReason>,
}

struct ContextSeed {
    executors: BTreeMap<String, Arc<dyn ToolExecutor>>,
    cache: Arc<dyn crate::cache::ToolCache>,
    single_flight: Arc<crate::policy::SingleFlight>,
}

impl Engine {
    pub(crate) fn new(
        runtime: &Runtime,
        plan: Plan,
        graph: &ExecutionGraph,
        optimization: OptimizationReport,
        config: RunConfig,
        serial: bool,
    ) -> Result<Self, RuntimeError> {
        let mut executors = BTreeMap::new();
        for spec in plan.tools.values() {
            if !executors.contains_key(&spec.adapter) {
                let executor = runtime.registry().executor(&spec.adapter).ok_or_else(|| {
                    RuntimeError::MissingAdapter {
                        adapter: spec.adapter.clone(),
                    }
                })?;
                executors.insert(spec.adapter.clone(), executor);
            }
        }

        let mut validators = BTreeMap::new();
        for (tool, spec) in &plan.tools {
            if let Some(schema) = runtime
                .registry()
                .capabilities_for(&spec.adapter, tool)
                .and_then(|capabilities| capabilities.input_schema.as_ref())
            {
                let validator = jsonschema::validator_for(schema).map_err(|error| {
                    RuntimeError::InvalidInputSchema {
                        tool: tool.clone(),
                        message: error.to_string(),
                    }
                })?;
                validators.insert(tool.clone(), Arc::new(validator));
            }
        }

        let nodes: BTreeMap<NodeId, Arc<Node>> = plan
            .nodes
            .iter()
            .map(|node| (node.id.clone(), Arc::new(node.clone())))
            .collect();

        let mut data_deps = BTreeMap::new();
        for node in nodes.values() {
            let mut deps = BTreeSet::new();
            if let Ok(refs) = collect_refs(&node.input) {
                deps.extend(refs.into_iter().map(|r| r.node().to_owned()));
            }
            if let Some(items) = &node.for_each {
                deps.insert(items.node().to_owned());
            }
            if let Some(when) = &node.when {
                deps.insert(when.target.node().to_owned());
            }
            data_deps.insert(node.id.clone(), deps);
        }

        let mut units: Vec<Unit> = Vec::new();
        let mut unit_of = BTreeMap::<NodeId, usize>::new();
        if !serial {
            for group in &optimization.batch_groups {
                let index = units.len();
                let mut lock = LockRequest::default();
                for member in &group.nodes {
                    lock.merge(&LockRequest::for_effects(&graph.resolved_effects()[member]));
                    unit_of.insert(member.clone(), index);
                }
                units.push(Unit {
                    cost: unit_cost(&plan, &group.tool, group.nodes.len() as u64),
                    members: group.nodes.clone(),
                    tool: group.tool.clone(),
                    adapter: group.adapter.clone(),
                    batch: true,
                    lock,
                });
            }
        }
        for node in nodes.values() {
            if unit_of.contains_key(&node.id) {
                continue;
            }
            let index = units.len();
            let adapter = plan
                .tools
                .get(&node.tool)
                .map(|spec| spec.adapter.clone())
                .unwrap_or_default();
            units.push(Unit {
                members: vec![node.id.clone()],
                tool: node.tool.clone(),
                adapter,
                batch: false,
                lock: LockRequest::for_effects(&graph.resolved_effects()[&node.id]),
                cost: unit_cost(&plan, &node.tool, 1),
            });
            unit_of.insert(node.id.clone(), index);
        }

        let mut unit_deps = vec![BTreeSet::new(); units.len()];
        let mut unit_dependents = vec![BTreeSet::new(); units.len()];
        for (node, deps) in graph.dependencies() {
            let unit = unit_of[node];
            for dep in deps {
                let dep_unit = unit_of[dep];
                if dep_unit != unit {
                    unit_deps[unit].insert(dep_unit);
                    unit_dependents[dep_unit].insert(unit);
                }
            }
        }

        let serial_position = serial.then(|| {
            graph
                .layers()
                .iter()
                .flatten()
                .enumerate()
                .map(|(position, node)| (unit_of[node], position))
                .collect()
        });

        Ok(Self {
            metrics: RunMetrics {
                nodes_total: plan.nodes.len(),
                ..RunMetrics::default()
            },
            effects: graph.resolved_effects().clone(),
            context_seed: ContextSeed {
                executors,
                cache: runtime.cache().clone(),
                single_flight: runtime.single_flight().clone(),
            },
            blocked_tools: plan
                .tools
                .keys()
                .filter(|tool| runtime.registry().is_blocked(tool))
                .cloned()
                .collect(),
            validators,
            plan,
            config,
            optimization,
            nodes,
            data_deps,
            units,
            unit_deps,
            unit_dependents,
            serial_position,
            outputs: BTreeMap::new(),
            dead: BTreeSet::new(),
            errors: BTreeMap::new(),
            skipped: BTreeMap::new(),
            trace: Vec::new(),
            locks: LockTable::default(),
            tool_in_flight: BTreeMap::new(),
            total_in_flight: 0,
            stop: None,
        })
    }

    pub(crate) async fn run(mut self) -> Result<RunResult, RuntimeError> {
        let started = Instant::now();
        let span = tracing::debug_span!("tool_compiler_run", plan = self.plan.name.as_deref());
        let _entered = span.enter();

        let mut remaining: Vec<usize> = self.unit_deps.iter().map(BTreeSet::len).collect();
        let mut ready: Vec<usize> = remaining
            .iter()
            .enumerate()
            .filter_map(|(unit, count)| (*count == 0).then_some(unit))
            .collect();
        let mut blocked: Vec<usize> = Vec::new();
        let mut running: JoinSet<DispatchOutcome> = JoinSet::new();
        let mut in_flight: BTreeMap<tokio::task::Id, usize> = BTreeMap::new();

        loop {
            self.schedule(&mut ready, &mut blocked, &mut remaining, &mut running, &mut in_flight);

            if running.is_empty() {
                if ready.is_empty() && blocked.is_empty() {
                    break;
                }
                // Nothing running: all locks and permits are free, so a
                // blocked retry must make progress. If it does not, settle
                // the leftovers instead of spinning.
                ready.append(&mut blocked);
                let before_running = running.len();
                self.schedule(
                    &mut ready,
                    &mut blocked,
                    &mut remaining,
                    &mut running,
                    &mut in_flight,
                );
                if running.len() == before_running && running.is_empty() {
                    let leftovers: Vec<usize> =
                        blocked.drain(..).chain(ready.drain(..)).collect();
                    for unit in leftovers {
                        self.settle_unit(unit, SkipReason::NotRun, &mut remaining, &mut ready);
                    }
                    if ready.is_empty() {
                        break;
                    }
                }
                continue;
            }

            let joined = if let Some(cancel) = self.config.cancel.clone() {
                tokio::select! {
                    joined = running.join_next_with_id() => joined,
                    _ = cancel.cancelled(), if self.stop.is_none() => {
                        self.stop = Some(SkipReason::Cancelled);
                        continue;
                    }
                }
            } else {
                running.join_next_with_id().await
            };
            let Some(joined) = joined else { continue };

            let (unit, outcome) = match joined {
                Ok((task_id, outcome)) => (
                    in_flight.remove(&task_id).expect("task registered"),
                    outcome,
                ),
                Err(join_error) => {
                    let unit = in_flight
                        .remove(&join_error.id())
                        .expect("task registered");
                    (unit, panicked_outcome(&self.units[unit], &join_error))
                }
            };

            self.release(unit);
            self.absorb(outcome).await;
            unblock(&self.unit_dependents, unit, &mut remaining, &mut ready);
            ready.append(&mut blocked);
        }

        self.finish(started)
    }

    fn schedule(
        &mut self,
        ready: &mut Vec<usize>,
        blocked: &mut Vec<usize>,
        remaining: &mut Vec<usize>,
        running: &mut JoinSet<DispatchOutcome>,
        in_flight: &mut BTreeMap<tokio::task::Id, usize>,
    ) {
        while let Some(unit) = self.pick_next(ready) {
            if let Some(reason) = self.stop {
                self.settle_unit(unit, reason, remaining, ready);
                continue;
            }

            let Some(members) = self.prepare_members(unit, remaining, ready) else {
                continue;
            };

            if !self.try_admit(unit) {
                blocked.push(unit);
                continue;
            }

            let context = DispatchContext {
                executor: self.context_seed.executors[&self.units[unit].adapter].clone(),
                cache: self.context_seed.cache.clone(),
                single_flight: self.context_seed.single_flight.clone(),
                use_cache: self.config.use_cache,
            };
            let spec = UnitSpec {
                tool: self.units[unit].tool.clone(),
                adapter: self.units[unit].adapter.clone(),
                batch: self.units[unit].batch,
            };
            let handle = running.spawn(dispatch_unit(context, spec, members));
            in_flight.insert(handle.id(), unit);
        }
    }

    /// Picks the next ready unit: serial order when benchmarking serially,
    /// otherwise highest estimated cost first (longest work starts earliest).
    fn pick_next(&self, ready: &mut Vec<usize>) -> Option<usize> {
        if ready.is_empty() {
            return None;
        }
        let index = match &self.serial_position {
            Some(order) => ready
                .iter()
                .enumerate()
                .min_by_key(|(_, unit)| order.get(*unit).copied().unwrap_or(usize::MAX))
                .map(|(index, _)| index)?,
            None => ready
                .iter()
                .enumerate()
                .max_by_key(|(_, unit)| self.units[**unit].cost)
                .map(|(index, _)| index)?,
        };
        Some(ready.swap_remove(index))
    }

    /// Resolves inputs and gates (`when`, blocked tools, input schemas).
    /// Members that fail or skip are settled inline; returns `None` when no
    /// member survived to dispatch.
    fn prepare_members(
        &mut self,
        unit: usize,
        remaining: &mut Vec<usize>,
        ready: &mut Vec<usize>,
    ) -> Option<Vec<PreparedMember>> {
        let members = self.units[unit].members.clone();
        let mut prepared = Vec::new();

        for member in members {
            let node = self.nodes[&member].clone();

            if self
                .data_deps
                .get(&member)
                .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
            {
                self.complete_skip(&member, SkipReason::FailedDependency);
                continue;
            }

            if let Some(when) = &node.when {
                match self.evaluate_when(when) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.complete_skip(&member, SkipReason::Condition);
                        continue;
                    }
                    Err(error) => {
                        self.complete_error(&member, error);
                        continue;
                    }
                }
            }

            if self.blocked_tools.contains(&node.tool) {
                self.complete_error(
                    &member,
                    ToolExecutionError::new(format!(
                        "tool '{}' is blocked in this runtime",
                        node.tool
                    ))
                    .with_code("blocked_tool"),
                );
                continue;
            }

            match self.prepare_one(&node) {
                Ok(member_dispatch) => prepared.push(member_dispatch),
                Err(error) => self.complete_error(&member, error),
            }
        }

        if prepared.is_empty() {
            unblock(&self.unit_dependents, unit, remaining, ready);
            return None;
        }
        Some(prepared)
    }

    fn prepare_one(&self, node: &Arc<Node>) -> Result<PreparedMember, ToolExecutionError> {
        let input = resolve_input(&node.input, &self.outputs).map_err(resolution_error)?;

        let items = match &node.for_each {
            Some(items_ref) => {
                let resolved =
                    resolve_value_ref(items_ref, &self.outputs).map_err(resolution_error)?;
                let Value::Array(items) = resolved else {
                    return Err(ToolExecutionError::new(format!(
                        "for_each of node '{}' must resolve to an array",
                        node.id
                    ))
                    .with_code("invalid_for_each"));
                };
                Some(items)
            }
            None => None,
        };

        if items.is_none()
            && let Some(validator) = self.validators.get(&node.tool)
            && let Err(error) = validator.validate(&input)
        {
            return Err(ToolExecutionError::new(format!(
                "input of node '{}' does not match the '{}' input schema: {error}",
                node.id, node.tool
            ))
            .with_code("invalid_input"));
        }

        let effects = self
            .plan
            .tools
            .get(&node.tool)
            .and_then(|spec| spec.effects.as_ref());
        let policy = CallPolicy::from_effects(effects, self.config.default_timeout_ms);
        let reads = self
            .effects
            .get(&node.id)
            .map(|effects| effects.reads.clone())
            .unwrap_or_default();
        Ok(member_from_plan(node, &self.plan, input, items, policy, reads))
    }

    fn evaluate_when(&self, when: &When) -> Result<bool, ToolExecutionError> {
        let value = resolve_value_ref(&when.target, &self.outputs).map_err(resolution_error)?;
        let verdict = match &when.equals {
            Some(expected) => &value == expected,
            None => !matches!(value, Value::Null | Value::Bool(false)),
        };
        Ok(verdict != when.not)
    }

    fn try_admit(&mut self, unit: usize) -> bool {
        let spec = &self.units[unit];
        let global = if self.serial_position.is_some() {
            Some(1)
        } else {
            self.config.max_concurrency
        };
        if let Some(global) = global
            && self.total_in_flight >= global
        {
            return false;
        }
        let tool_limit = self
            .plan
            .tools
            .get(&spec.tool)
            .and_then(|tool| tool.limits.as_ref())
            .and_then(|limits| limits.max_concurrency);
        if let Some(limit) = tool_limit
            && self.tool_in_flight.get(&spec.tool).copied().unwrap_or(0) >= limit
        {
            return false;
        }
        if !self.locks.try_acquire(&spec.lock) {
            return false;
        }
        *self.tool_in_flight.entry(spec.tool.clone()).or_default() += 1;
        self.total_in_flight += 1;
        true
    }

    fn release(&mut self, unit: usize) {
        let spec = &self.units[unit];
        self.locks.release(&spec.lock);
        if let Some(count) = self.tool_in_flight.get_mut(&spec.tool) {
            *count = count.saturating_sub(1);
        }
        self.total_in_flight = self.total_in_flight.saturating_sub(1);
    }

    async fn absorb(&mut self, outcome: DispatchOutcome) {
        self.metrics.cache_hits += outcome.cache_hits;
        self.metrics.retries += outcome.retries;
        self.metrics.batch_dispatches += outcome.batch_dispatches;
        for event in outcome.events {
            self.push_event(event);
        }

        let mut written = BTreeSet::new();
        for (node, result) in outcome.results {
            match result {
                Ok(output) => {
                    if let Some(effects) = self.effects.get(&node) {
                        written.extend(effects.writes.iter().cloned());
                    }
                    self.metrics.nodes_succeeded += 1;
                    self.outputs.insert(node, output);
                }
                Err(error) => {
                    self.record_failure(&node, error);
                }
            }
        }

        // Invalidate cached reads overlapping this unit's writes BEFORE any
        // dependent is scheduled, so later reads re-execute instead of
        // observing pre-write values.
        if !written.is_empty() {
            self.context_seed.cache.invalidate_reads(&written).await;
        }
    }

    fn record_failure(&mut self, node: &str, error: ToolExecutionError) {
        tracing::debug!(node, error = %error, "node failed");
        self.metrics.nodes_failed += 1;
        self.dead.insert(node.to_owned());
        self.errors.insert(node.to_owned(), error);
        if self.stop.is_none() && self.config.on_error == ErrorMode::FailFast {
            self.stop = Some(SkipReason::NotRun);
        }
    }

    fn complete_error(&mut self, node: &str, error: ToolExecutionError) {
        let tool = self.nodes[node].tool.clone();
        self.push_event(
            TraceEvent::new(node, &tool, TraceStatus::Failed)
                .with_duration(0)
                .with_error(&error),
        );
        self.record_failure(node, error);
    }

    fn complete_skip(&mut self, node: &str, reason: SkipReason) {
        let tool = self.nodes[node].tool.clone();
        self.push_event(TraceEvent::new(node, &tool, TraceStatus::Skipped));
        self.metrics.nodes_skipped += 1;
        self.dead.insert(node.to_owned());
        self.skipped.insert(node.to_owned(), reason);
    }

    fn settle_unit(
        &mut self,
        unit: usize,
        fallback: SkipReason,
        remaining: &mut Vec<usize>,
        ready: &mut Vec<usize>,
    ) {
        for member in self.units[unit].members.clone() {
            if !self.is_settled(&member) {
                // Prefer the precise reason: a member whose data dependency
                // already failed/skipped is a FailedDependency even when the
                // engine is settling everything else as NotRun/Cancelled.
                let reason = if self
                    .data_deps
                    .get(&member)
                    .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
                {
                    SkipReason::FailedDependency
                } else {
                    fallback
                };
                self.complete_skip(&member, reason);
            }
        }
        unblock(&self.unit_dependents, unit, remaining, ready);
    }

    fn is_settled(&self, node: &str) -> bool {
        self.outputs.contains_key(node)
            || self.errors.contains_key(node)
            || self.skipped.contains_key(node)
    }

    fn push_event(&mut self, event: TraceEvent) {
        if let Some(sink) = &self.config.events {
            let _ = sink.send(event.clone());
        }
        self.trace.push(event);
    }

    fn finish(mut self, started: Instant) -> Result<RunResult, RuntimeError> {
        let unsettled: Vec<NodeId> = self
            .nodes
            .keys()
            .filter(|node| !self.is_settled(node))
            .cloned()
            .collect();
        for node in unsettled {
            let reason = if self
                .data_deps
                .get(&node)
                .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
            {
                SkipReason::FailedDependency
            } else {
                self.stop.unwrap_or(SkipReason::NotRun)
            };
            self.complete_skip(&node, reason);
        }

        let mut outputs = BTreeMap::new();
        for (name, value_ref) in &self.plan.outputs {
            match resolve_value_ref(value_ref, &self.outputs) {
                Ok(value) => {
                    outputs.insert(name.clone(), value);
                }
                Err(error) => {
                    if !self.dead.contains(value_ref.node()) {
                        self.errors.insert(
                            format!("outputs.{name}"),
                            resolution_error(error).with_code("unresolved_output"),
                        );
                    }
                }
            }
        }

        let redactor = self.config.redactor.clone();
        let max_bytes = self.config.max_output_bytes;
        let outputs = outputs
            .into_iter()
            .map(|(name, value)| {
                let shaped = shape_output(&name, value, redactor.as_ref(), max_bytes);
                (name, shaped)
            })
            .collect();
        let node_outputs: BTreeMap<NodeId, Value> = match self.config.result_mode {
            ResultMode::Compact => BTreeMap::new(),
            ResultMode::Full => self
                .outputs
                .into_iter()
                .map(|(node, value)| {
                    let shaped = shape_output(&node, value, redactor.as_ref(), max_bytes);
                    (node, shaped)
                })
                .collect(),
        };
        let trace = match self.config.result_mode {
            ResultMode::Compact => Vec::new(),
            ResultMode::Full => self.trace,
        };

        self.metrics.wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = if self.stop == Some(SkipReason::Cancelled) {
            RunStatus::Cancelled
        } else if self.errors.is_empty() {
            RunStatus::Success
        } else {
            RunStatus::Failed
        };

        Ok(RunResult {
            status,
            outputs,
            node_outputs,
            errors: self.errors,
            skipped: self.skipped,
            trace,
            optimization: self.optimization,
            metrics: self.metrics,
        })
    }
}

fn unblock(
    unit_dependents: &[BTreeSet<usize>],
    unit: usize,
    remaining: &mut Vec<usize>,
    ready: &mut Vec<usize>,
) {
    for dependent in &unit_dependents[unit] {
        remaining[*dependent] = remaining[*dependent].saturating_sub(1);
        if remaining[*dependent] == 0 {
            ready.push(*dependent);
        }
    }
}

fn unit_cost(plan: &Plan, tool: &str, calls: u64) -> u64 {
    let cost = plan
        .tools
        .get(tool)
        .and_then(|spec| spec.cost.clone())
        .unwrap_or_default();
    cost.fixed_ms.unwrap_or(0) + cost.per_call_ms.unwrap_or(0).saturating_mul(calls)
}

fn resolution_error(error: RuntimeError) -> ToolExecutionError {
    let code = match &error {
        RuntimeError::MissingNodeOutput(_) => "missing_node_output",
        RuntimeError::MissingPath { .. } => "missing_path",
        _ => "invalid_ref",
    };
    ToolExecutionError::new(error.to_string()).with_code(code)
}

fn panicked_outcome(unit: &Unit, join_error: &tokio::task::JoinError) -> DispatchOutcome {
    let error = ToolExecutionError::new(format!("adapter panicked: {join_error}"))
        .with_code("panic")
        .with_retryable(false);
    DispatchOutcome {
        results: unit
            .members
            .iter()
            .map(|member| (member.clone(), Err(error.clone())))
            .collect(),
        events: unit
            .members
            .iter()
            .map(|member| {
                TraceEvent::new(member, &unit.tool, TraceStatus::Failed).with_error(&error)
            })
            .collect(),
        cache_hits: 0,
        retries: 0,
        batch_dispatches: 0,
    }
}
