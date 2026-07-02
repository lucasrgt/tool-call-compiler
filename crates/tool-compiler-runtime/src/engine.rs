//! Engine construction: units, dependency wiring, executors, validators.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::Value;
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};
use tool_compiler_graph::ExecutionGraph;
use tool_compiler_ir::{Node, NodeId, Plan, ToolName, collect_refs};
use tool_compiler_optimizer::OptimizationReport;

use crate::locks::{LockRequest, LockTable};
use crate::result::{RunMetrics, SkipReason, TraceEvent};
use crate::{RunConfig, Runtime, RuntimeError};

/// One schedulable unit: a single node or an optimizer batch group.
#[derive(Clone, Debug)]
pub(crate) struct Unit {
    /// Member node ids, shared so the scheduler can iterate them while
    /// mutating the engine without cloning the list per dispatch.
    pub(crate) members: Arc<[NodeId]>,
    pub(crate) tool: ToolName,
    pub(crate) adapter: String,
    pub(crate) batch: bool,
    pub(crate) lock: LockRequest,
    pub(crate) cost: u64,
}

pub(crate) struct ContextSeed {
    pub(crate) executors: BTreeMap<String, Arc<dyn ToolExecutor>>,
    pub(crate) cache: Arc<dyn crate::cache::ToolCache>,
    pub(crate) single_flight: Arc<crate::policy::SingleFlight>,
}

/// The dependency-driven execution engine for one run.
pub(crate) struct Engine {
    pub(crate) plan: Arc<Plan>,
    pub(crate) config: RunConfig,
    pub(crate) optimization: OptimizationReport,
    pub(crate) context_seed: ContextSeed,
    pub(crate) graph: Arc<ExecutionGraph>,
    pub(crate) node_index: BTreeMap<NodeId, usize>,
    pub(crate) data_deps: Arc<BTreeMap<NodeId, BTreeSet<NodeId>>>,
    pub(crate) units: Vec<Unit>,
    pub(crate) unit_deps: Vec<BTreeSet<usize>>,
    pub(crate) unit_dependents: Vec<BTreeSet<usize>>,
    pub(crate) serial_position: Option<BTreeMap<usize, usize>>,
    /// Scheduling priority per unit (higher pops first): declared cost, or
    /// inverted serial position when benchmarking serially.
    pub(crate) ready_keys: Vec<u64>,
    pub(crate) blocked_tools: BTreeSet<String>,
    pub(crate) validators: BTreeMap<ToolName, Arc<jsonschema::Validator>>,

    pub(crate) outputs: BTreeMap<NodeId, Arc<Value>>,
    pub(crate) dead: BTreeSet<NodeId>,
    pub(crate) errors: BTreeMap<NodeId, ToolExecutionError>,
    pub(crate) skipped: BTreeMap<NodeId, SkipReason>,
    pub(crate) trace: Vec<TraceEvent>,
    pub(crate) metrics: RunMetrics,
    pub(crate) locks: LockTable,
    pub(crate) tool_in_flight: BTreeMap<ToolName, usize>,
    pub(crate) total_in_flight: usize,
    pub(crate) stop: Option<SkipReason>,
}

/// Data dependencies of each node: input `$ref`s, `for_each` sources, and
/// `when` targets — the references whose failure poisons the node.
pub(crate) fn collect_data_deps(plan: &Plan) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut data_deps = BTreeMap::new();
    for node in &plan.nodes {
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
    data_deps
}

impl Engine {
    pub(crate) fn new(
        runtime: &Runtime,
        plan: Arc<Plan>,
        graph: Arc<ExecutionGraph>,
        data_deps: Arc<BTreeMap<NodeId, BTreeSet<NodeId>>>,
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
                let validator =
                    runtime
                        .registry()
                        .compiled_validator(schema)
                        .map_err(|message| RuntimeError::InvalidInputSchema {
                            tool: tool.clone(),
                            message,
                        })?;
                validators.insert(tool.clone(), validator);
            }
        }

        let node_index: BTreeMap<NodeId, usize> = plan
            .nodes
            .iter()
            .enumerate()
            .map(|(position, node)| (node.id.clone(), position))
            .collect();

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
                    members: group.nodes.clone().into(),
                    tool: group.tool.clone(),
                    adapter: group.adapter.clone(),
                    batch: true,
                    lock,
                });
            }
        }
        for (id, &position) in &node_index {
            if unit_of.contains_key(id) {
                continue;
            }
            let node = &plan.nodes[position];
            let index = units.len();
            let adapter = plan
                .tools
                .get(&node.tool)
                .map(|spec| spec.adapter.clone())
                .unwrap_or_default();
            units.push(Unit {
                members: vec![node.id.clone()].into(),
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

        let serial_position: Option<BTreeMap<usize, usize>> = serial.then(|| {
            graph
                .layers()
                .iter()
                .flatten()
                .enumerate()
                .map(|(position, node)| (unit_of[node], position))
                .collect()
        });
        let ready_keys: Vec<u64> = (0..units.len())
            .map(|unit| match &serial_position {
                // Serial order: earlier positions must pop first, so the
                // priority is the inverted position.
                Some(order) => u64::MAX - order.get(&unit).copied().unwrap_or(usize::MAX) as u64,
                None => units[unit].cost,
            })
            .collect();

        Ok(Self {
            metrics: RunMetrics {
                nodes_total: plan.nodes.len(),
                ..RunMetrics::default()
            },
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
            graph,
            node_index,
            data_deps,
            units,
            unit_deps,
            unit_dependents,
            serial_position,
            ready_keys,
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

    /// The node a settled/settling member id refers to.
    pub(crate) fn node(&self, member: &str) -> &Node {
        &self.plan.nodes[self.node_index[member]]
    }
}

pub(crate) fn unit_cost(plan: &Plan, tool: &str, calls: u64) -> u64 {
    let cost = plan.tools.get(tool).and_then(|spec| spec.cost.as_ref());
    cost.and_then(|cost| cost.fixed_ms).unwrap_or(0)
        + cost
            .and_then(|cost| cost.per_call_ms)
            .unwrap_or(0)
            .saturating_mul(calls)
}
