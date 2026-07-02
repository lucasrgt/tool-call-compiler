//! Dependency-driven scheduling loop.
//!
//! Nodes start as soon as their dependencies finish — there is no layer
//! barrier. Effect safety is enforced at runtime by a resource lock table
//! (reads share, writes exclude, same-tool commutative writes overlap,
//! undeclared effects run alone), which preserves the guarantees of the
//! layered model while letting independent branches overlap.
//!
//! Failures never abort in-flight work: the engine stops scheduling (in
//! fail-fast mode), drains what is running, and returns a partial result
//! with per-node errors and skips.

use std::cmp::Reverse;
use std::collections::{BTreeSet, BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::Value;
use tokio::task::JoinSet;
use tool_compiler_adapter_api::ToolExecutionError;
use tool_compiler_ir::{Node, When};

use crate::dispatch::{
    DispatchContext, DispatchOutcome, PreparedMember, UnitSpec, dispatch_unit, member_from_plan,
    panicked_outcome,
};
use crate::engine::Engine;
use crate::refs::{resolve_input, resolve_value_ref};
use crate::result::{RunResult, SkipReason, TraceEvent, TraceStatus};
use crate::{ErrorMode, RuntimeError};

static BATCH_IDS: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_batch_id() -> u64 {
    BATCH_IDS.fetch_add(1, Ordering::Relaxed)
}

/// Ready units ordered by scheduling priority (see `Engine::ready_keys`),
/// ties broken by the lower unit index. Pops are O(log n) where the previous
/// per-pick scan was O(n).
type ReadyQueue = BinaryHeap<(u64, Reverse<usize>)>;

impl Engine {
    pub(crate) async fn run(mut self) -> Result<RunResult, RuntimeError> {
        let started = Instant::now();
        let span = tracing::debug_span!("tool_compiler_run", plan = self.plan.name.as_deref());
        let _entered = span.enter();

        let mut remaining: Vec<usize> = self.unit_deps.iter().map(BTreeSet::len).collect();
        let mut ready: ReadyQueue = remaining
            .iter()
            .enumerate()
            .filter(|(_, count)| **count == 0)
            .map(|(unit, _)| (self.ready_keys[unit], Reverse(unit)))
            .collect();
        let mut blocked: Vec<usize> = Vec::new();
        let mut running: JoinSet<DispatchOutcome> = JoinSet::new();
        let mut in_flight: HashMap<tokio::task::Id, usize> = HashMap::new();

        loop {
            self.schedule(
                &mut ready,
                &mut blocked,
                &mut remaining,
                &mut running,
                &mut in_flight,
            );

            if running.is_empty() {
                if ready.is_empty() && blocked.is_empty() {
                    break;
                }
                // Nothing running: all locks and permits are free, so a
                // blocked retry must make progress. If it does not, settle
                // the leftovers instead of spinning.
                self.requeue(&mut blocked, &mut ready);
                let before_running = running.len();
                self.schedule(
                    &mut ready,
                    &mut blocked,
                    &mut remaining,
                    &mut running,
                    &mut in_flight,
                );
                if running.len() == before_running && running.is_empty() {
                    let leftovers: Vec<usize> = blocked
                        .drain(..)
                        .chain(ready.drain().map(|(_, Reverse(unit))| unit))
                        .collect();
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
                    let unit = in_flight.remove(&join_error.id()).expect("task registered");
                    (unit, panicked_outcome(&self.units[unit], &join_error))
                }
            };

            self.release(unit);
            self.absorb(outcome).await;
            unblock(
                &self.unit_dependents,
                &self.ready_keys,
                unit,
                &mut remaining,
                &mut ready,
            );
            self.requeue(&mut blocked, &mut ready);
        }

        self.finish(started)
    }

    /// Puts blocked units back in the ready queue for another admission try.
    fn requeue(&self, blocked: &mut Vec<usize>, ready: &mut ReadyQueue) {
        for unit in blocked.drain(..) {
            ready.push((self.ready_keys[unit], Reverse(unit)));
        }
    }

    fn schedule(
        &mut self,
        ready: &mut ReadyQueue,
        blocked: &mut Vec<usize>,
        remaining: &mut [usize],
        running: &mut JoinSet<DispatchOutcome>,
        in_flight: &mut HashMap<tokio::task::Id, usize>,
    ) {
        while let Some((_, Reverse(unit))) = ready.pop() {
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

    /// Resolves inputs and gates (`when`, blocked tools, input schemas).
    /// Members that fail or skip are settled inline; returns `None` when no
    /// member survived to dispatch.
    fn prepare_members(
        &mut self,
        unit: usize,
        remaining: &mut [usize],
        ready: &mut ReadyQueue,
    ) -> Option<Vec<PreparedMember>> {
        let members = Arc::clone(&self.units[unit].members);
        let mut prepared = Vec::new();
        let plan = Arc::clone(&self.plan);

        for member in members.iter() {
            let node = &plan.nodes[self.node_index[member]];

            if self
                .data_deps
                .get(member)
                .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
            {
                self.complete_skip(member, SkipReason::FailedDependency);
                continue;
            }

            if let Some(when) = &node.when {
                match self.evaluate_when(when) {
                    Ok(true) => {}
                    Ok(false) => {
                        self.complete_skip(member, SkipReason::Condition);
                        continue;
                    }
                    Err(error) => {
                        self.complete_error(member, error);
                        continue;
                    }
                }
            }

            if self.blocked_tools.contains(&node.tool) {
                self.complete_error(
                    member,
                    ToolExecutionError::new(format!(
                        "tool '{}' is blocked in this runtime",
                        node.tool
                    ))
                    .with_code("blocked_tool"),
                );
                continue;
            }

            match self.prepare_one(node) {
                Ok(member_dispatch) => prepared.push(member_dispatch),
                Err(error) => self.complete_error(member, error),
            }
        }

        if prepared.is_empty() {
            unblock(
                &self.unit_dependents,
                &self.ready_keys,
                unit,
                remaining,
                ready,
            );
            return None;
        }
        Some(prepared)
    }

    fn prepare_one(&self, node: &Node) -> Result<PreparedMember, ToolExecutionError> {
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
        let policy =
            crate::policy::CallPolicy::from_effects(effects, self.config.default_timeout_ms);
        let reads = self
            .graph
            .resolved_effects()
            .get(&node.id)
            .map(|effects| effects.reads.clone())
            .unwrap_or_default();
        Ok(member_from_plan(
            node, &self.plan, input, items, policy, reads,
        ))
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
                    if let Some(effects) = self.graph.resolved_effects().get(&node) {
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
        let tool = self.node(node).tool.clone();
        self.push_event(
            TraceEvent::new(node, &tool, TraceStatus::Failed)
                .with_duration(0)
                .with_error(&error),
        );
        self.record_failure(node, error);
    }

    pub(crate) fn complete_skip(&mut self, node: &str, reason: SkipReason) {
        let tool = self.node(node).tool.clone();
        self.push_event(TraceEvent::new(node, &tool, TraceStatus::Skipped));
        self.metrics.nodes_skipped += 1;
        self.dead.insert(node.to_owned());
        self.skipped.insert(node.to_owned(), reason);
    }

    fn settle_unit(
        &mut self,
        unit: usize,
        fallback: SkipReason,
        remaining: &mut [usize],
        ready: &mut ReadyQueue,
    ) {
        let members = Arc::clone(&self.units[unit].members);
        for member in members.iter() {
            if !self.is_settled(member) {
                // Prefer the precise reason: a member whose data dependency
                // already failed/skipped is a FailedDependency even when the
                // engine is settling everything else as NotRun/Cancelled.
                let reason = if self
                    .data_deps
                    .get(member)
                    .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
                {
                    SkipReason::FailedDependency
                } else {
                    fallback
                };
                self.complete_skip(member, reason);
            }
        }
        unblock(
            &self.unit_dependents,
            &self.ready_keys,
            unit,
            remaining,
            ready,
        );
    }

    pub(crate) fn is_settled(&self, node: &str) -> bool {
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
}

fn unblock(
    unit_dependents: &[BTreeSet<usize>],
    ready_keys: &[u64],
    unit: usize,
    remaining: &mut [usize],
    ready: &mut ReadyQueue,
) {
    for dependent in &unit_dependents[unit] {
        remaining[*dependent] = remaining[*dependent].saturating_sub(1);
        if remaining[*dependent] == 0 {
            ready.push((ready_keys[*dependent], Reverse(*dependent)));
        }
    }
}

pub(crate) fn resolution_error(error: RuntimeError) -> ToolExecutionError {
    let code = match &error {
        RuntimeError::MissingNodeOutput(_) => "missing_node_output",
        RuntimeError::MissingPath { .. } => "missing_path",
        _ => "invalid_ref",
    };
    ToolExecutionError::new(error.to_string()).with_code(code)
}
