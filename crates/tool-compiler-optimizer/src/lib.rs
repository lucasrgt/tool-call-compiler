//! Safe optimization passes for effect-aware tool plans.
//!
//! [`optimize`] runs a fixed pass pipeline — deduplication (with pure-node
//! common-subexpression elimination), dead-node elimination, and batch
//! grouping — and returns the optimized plan, its execution graph, and an
//! [`OptimizationReport`] describing every transformation.
//!
//! Every pass is effect-gated: a transformation only happens when the
//! declared tool effects prove it safe.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_compiler_graph::{ExecutionGraph, GraphError, validate};
use tool_compiler_ir::{LITERAL_KEY, Node, NodeId, Plan, REF_KEY, ValueRef, canonical_json_string};

mod cost;
mod explain;

use cost::summarize;
pub use explain::{Diagnostic, DiagnosticKind, explain_parallel_boundaries};

/// A validated, optimized plan with its graph and report.
#[derive(Clone, Debug, PartialEq)]
pub struct OptimizedPlan {
    plan: Plan,
    graph: ExecutionGraph,
    report: OptimizationReport,
}

impl OptimizedPlan {
    /// The optimized plan.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The execution graph of the optimized plan.
    pub fn graph(&self) -> &ExecutionGraph {
        &self.graph
    }

    /// What the optimizer did.
    pub fn report(&self) -> &OptimizationReport {
        &self.report
    }

    /// Decomposes into `(plan, graph, report)` without cloning.
    pub fn into_parts(self) -> (Plan, ExecutionGraph, OptimizationReport) {
        (self.plan, self.graph, self.report)
    }
}

/// Report of the transformations applied by [`optimize`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationReport {
    /// Names of the passes that ran, in order.
    pub passes: Vec<String>,
    /// Duplicate nodes removed, with their canonical survivor.
    pub deduplicated: Vec<DeduplicatedNode>,
    /// Pure nodes eliminated because nothing (outputs or effectful nodes)
    /// depends on them. Only runs when the plan declares outputs.
    pub eliminated: Vec<NodeId>,
    /// Same-layer, same-tool groups selected for one `call_batch` dispatch.
    /// Groups are already split by the tool's `batch_size` limit, so this is
    /// exactly what the runtime dispatches.
    pub batch_groups: Vec<BatchGroup>,
    /// Aggregate estimates before/after compilation.
    pub summary: OptimizationSummary,
}

/// A node removed by deduplication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeduplicatedNode {
    /// The removed node id.
    pub removed: NodeId,
    /// The surviving node whose output replaces it.
    pub canonical: NodeId,
}

/// A compiler-selected batch: same layer, same tool, batchable effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchGroup {
    /// Adapter that executes the batch.
    pub adapter: String,
    /// Tool shared by every node in the batch.
    pub tool: String,
    /// Member node ids.
    pub nodes: Vec<NodeId>,
}

/// Aggregate before/after estimates.
///
/// `estimated_llm_turns_before` assumes the serial-agent baseline: one
/// model-visible tool call (and therefore one feedback turn) per node. The
/// compiled plan is one composite call, so `estimated_llm_turns_after` is 1
/// for any non-empty plan. Cost estimates are only present when at least one
/// tool declares [`ToolCost`] metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationSummary {
    /// Tool calls a serial agent would make (original node count).
    pub estimated_tool_calls_before: usize,
    /// Model-visible dispatches after dedup, elimination, and batching.
    pub estimated_tool_calls_after: usize,
    /// LLM feedback turns for the serial baseline.
    pub estimated_llm_turns_before: usize,
    /// LLM feedback turns for the compiled plan (1 when non-empty).
    pub estimated_llm_turns_after: usize,
    /// Estimated serial wall time from declared costs, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_serial_ms: Option<u64>,
    /// Estimated compiled wall time from declared costs, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_compiled_ms: Option<u64>,
    /// Estimated result tokens before optimization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_before: Option<u64>,
    /// Estimated result tokens after optimization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
}

/// Explain output: layers, optimization report, and blocked-parallelism
/// diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainReport {
    /// Execution layers of the optimized plan.
    pub layers: Vec<Vec<NodeId>>,
    /// The optimization report.
    pub optimization: OptimizationReport,
    /// Why unordered nodes ended up serialized.
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs the optimization pipeline on `plan`.
pub fn optimize(plan: Plan) -> Result<OptimizedPlan, GraphError> {
    let original_tools: Vec<String> = plan.nodes.iter().map(|node| node.tool.clone()).collect();

    let mut report = OptimizationReport::default();

    report.passes.push("dedup".into());
    let plan = deduplicate_nodes(plan, &mut report);
    let mut graph = validate(&plan)?;

    report.passes.push("dce".into());
    let plan = eliminate_dead_nodes(plan, &graph, &mut report);
    if !report.eliminated.is_empty() {
        // Elimination only removes nodes; the survivors' dependencies and
        // effects are untouched, so rebuilding the layers over the kept set
        // replaces a full (input-reparsing) re-validation.
        let kept: BTreeSet<NodeId> = plan.nodes.iter().map(|node| node.id.clone()).collect();
        graph = graph.retain(&kept)?;
    }

    report.passes.push("batch".into());
    report.batch_groups = find_batch_groups(&plan, &graph);

    report.summary = summarize(&original_tools, &plan, &graph, &report);

    Ok(OptimizedPlan {
        plan,
        graph,
        report,
    })
}

/// Optimizes `plan` and explains its parallel boundaries.
pub fn explain(plan: Plan) -> Result<ExplainReport, GraphError> {
    let optimized = optimize(plan)?;
    let diagnostics = explain_parallel_boundaries(optimized.plan(), optimized.graph());

    Ok(ExplainReport {
        layers: optimized.graph().layers().to_vec(),
        optimization: optimized.report().clone(),
        diagnostics,
    })
}

fn deduplicate_nodes(mut plan: Plan, report: &mut OptimizationReport) -> Plan {
    let mut seen = HashMap::<String, NodeId>::new();
    let mut aliases = HashMap::<NodeId, NodeId>::new();
    let mut kept = Vec::new();
    let nodes = std::mem::take(&mut plan.nodes);

    for node in nodes {
        if let Some(key) = dedup_key(&plan, &node) {
            if let Some(canonical) = seen.get(&key) {
                aliases.insert(node.id.clone(), canonical.clone());
                report.deduplicated.push(DeduplicatedNode {
                    removed: node.id,
                    canonical: canonical.clone(),
                });
                continue;
            }

            seen.insert(key, node.id.clone());
        }

        kept.push(node);
    }

    plan.nodes = kept;
    if aliases.is_empty() {
        return plan;
    }

    // Rewrite AFTER the alias map is complete so references to duplicates
    // that appear later in the node list are also redirected.
    for node in &mut plan.nodes {
        rewrite_node_aliases(node, &aliases);
    }
    for value_ref in plan.outputs.values_mut() {
        *value_ref = rewrite_value_ref(value_ref, &aliases);
    }

    plan
}

fn eliminate_dead_nodes(
    mut plan: Plan,
    graph: &ExecutionGraph,
    report: &mut OptimizationReport,
) -> Plan {
    if plan.outputs.is_empty() {
        return plan;
    }

    // Roots and the reachability walk borrow node ids from the graph (which
    // indexes every node), so no ids are cloned while marking.
    let mut stack: Vec<&str> = Vec::new();
    for value_ref in plan.outputs.values() {
        if let Some((node, _)) = graph.dependencies().get_key_value(value_ref.node()) {
            stack.push(node.as_str());
        }
    }
    for (node, effects) in graph.resolved_effects() {
        if !effects.pure {
            stack.push(node.as_str());
        }
    }

    let mut kept = HashSet::<&str>::new();
    while let Some(node) = stack.pop() {
        if !kept.insert(node) {
            continue;
        }
        if let Some(deps) = graph.dependencies().get(node) {
            stack.extend(deps.iter().map(String::as_str));
        }
    }

    report.eliminated = plan
        .nodes
        .iter()
        .filter(|node| !kept.contains(node.id.as_str()))
        .map(|node| node.id.clone())
        .collect();
    if !report.eliminated.is_empty() {
        plan.nodes.retain(|node| kept.contains(node.id.as_str()));
    }
    plan
}

fn find_batch_groups(plan: &Plan, graph: &ExecutionGraph) -> Vec<BatchGroup> {
    let nodes_by_id: BTreeMap<&str, &Node> = plan
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut groups = Vec::new();

    for layer in graph.layers() {
        let mut by_tool = BTreeMap::<String, Vec<NodeId>>::new();
        for node_id in layer {
            let Some(node) = nodes_by_id.get(node_id.as_str()) else {
                continue;
            };
            let is_batchable = plan
                .tools
                .get(&node.tool)
                .and_then(|tool| tool.effects.as_ref())
                .is_some_and(|effects| effects.batchable);

            // Dynamic fan-out nodes expand at runtime; they are dispatched
            // individually so the expansion controls its own batching.
            if is_batchable && node.for_each.is_none() {
                by_tool
                    .entry(node.tool.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        for (tool, nodes) in by_tool {
            let spec = plan.tools.get(&tool);
            let adapter = spec.map(|spec| spec.adapter.clone()).unwrap_or_default();
            let batch_size = spec
                .and_then(|spec| spec.limits.as_ref())
                .and_then(|limits| limits.batch_size)
                .unwrap_or(nodes.len())
                .max(1);

            for chunk in nodes.chunks(batch_size) {
                if chunk.len() > 1 {
                    groups.push(BatchGroup {
                        adapter: adapter.clone(),
                        tool: tool.clone(),
                        nodes: chunk.to_vec(),
                    });
                }
            }
        }
    }

    groups
}

fn dedup_key(plan: &Plan, node: &Node) -> Option<String> {
    let effects = plan.tools.get(&node.tool)?.effects.as_ref()?;
    if !(effects.pure || effects.cacheable) {
        return None;
    }

    let mut key = format!(
        "{}\u{0}{}\u{0}{:?}\u{0}{:?}",
        node.tool,
        canonical_json_string(&node.input),
        node.for_each,
        node.when,
    );

    // Pure outputs do not depend on execution order, so explicit ordering
    // edges are ignored and identical pure calls merge across the graph
    // (common-subexpression elimination). Cacheable-but-impure tools keep
    // their ordering context in the key.
    if !effects.pure {
        key.push('\u{0}');
        key.push_str(&node.depends_on.join(","));
    }

    Some(key)
}

fn rewrite_node_aliases(node: &mut Node, aliases: &HashMap<NodeId, NodeId>) {
    node.depends_on = node
        .depends_on
        .iter()
        .map(|dependency| canonical_node(dependency, aliases))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if let Some(items) = &node.for_each {
        node.for_each = Some(rewrite_value_ref(items, aliases));
    }
    if let Some(when) = &mut node.when {
        when.target = rewrite_value_ref(&when.target, aliases);
    }
    rewrite_refs_in_value(&mut node.input, aliases);
}

fn rewrite_refs_in_value(value: &mut Value, aliases: &HashMap<NodeId, NodeId>) {
    match value {
        Value::Object(map) => {
            if map.len() == 1 && map.contains_key(LITERAL_KEY) {
                return;
            }
            if map.len() == 1
                && let Some(Value::String(raw_ref)) = map.get(REF_KEY)
                && let Ok(value_ref) = raw_ref.parse::<ValueRef>()
            {
                map.insert(
                    REF_KEY.to_owned(),
                    rewrite_value_ref(&value_ref, aliases).to_string().into(),
                );
                return;
            }

            for value in map.values_mut() {
                rewrite_refs_in_value(value, aliases);
            }
        }
        Value::Array(items) => {
            for item in items {
                rewrite_refs_in_value(item, aliases);
            }
        }
        _ => {}
    }
}

fn rewrite_value_ref(value_ref: &ValueRef, aliases: &HashMap<NodeId, NodeId>) -> ValueRef {
    ValueRef::new(
        canonical_node(value_ref.node(), aliases),
        value_ref.path().to_owned(),
    )
}

fn canonical_node(node: &str, aliases: &HashMap<NodeId, NodeId>) -> NodeId {
    let mut current = node;
    while let Some(next) = aliases.get(current) {
        current = next;
    }
    current.to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, Plan, ToolCost, ToolLimits, ToolSpec, ValueRef};

    use super::*;

    fn plan() -> Plan {
        let mut plan = Plan::new();
        plan.tools.insert(
            "lookup".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        plan.tools.insert(
            "write".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:item"].into_iter().map(String::from).collect(),
                ..Effects::default()
            }),
        );
        plan
    }

    #[test]
    fn deduplicates_identical_pure_nodes_and_rewrites_refs() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("a", "lookup").with_input(json!({ "q": "x" })));
        plan.nodes
            .push(Node::new("b", "lookup").with_input(json!({ "q": "x" })));
        plan.nodes.push(Node::new("c", "lookup").with_input(json!({
            "source": { "$ref": "b.output.id" }
        })));
        plan.outputs
            .insert("result".into(), ValueRef::new("b", ["id"]));
        plan.outputs.insert("chain".into(), ValueRef::output("c"));

        let optimized = optimize(plan).unwrap();

        assert_eq!(
            optimized.report().deduplicated,
            vec![DeduplicatedNode {
                removed: "b".into(),
                canonical: "a".into(),
            }]
        );
        assert_eq!(optimized.plan().nodes.len(), 2);
        assert_eq!(
            optimized.plan().outputs["result"],
            ValueRef::new("a", ["id"])
        );
        assert_eq!(
            optimized.plan().nodes[1].input,
            json!({ "source": { "$ref": "a.output.id" } })
        );
    }

    #[test]
    fn rewrites_forward_references_to_deduplicated_nodes() {
        let mut plan = plan();
        // "x" references "item2", which appears LATER and deduplicates into
        // "item1". The rewrite must still redirect x's reference.
        plan.nodes.push(Node::new("x", "lookup").with_input(json!({
            "source": { "$ref": "item2.output" }
        })));
        plan.nodes
            .push(Node::new("item1", "lookup").with_input(json!({ "q": "same" })));
        plan.nodes
            .push(Node::new("item2", "lookup").with_input(json!({ "q": "same" })));
        plan.outputs.insert("x".into(), ValueRef::output("x"));

        let optimized = optimize(plan).unwrap();

        assert_eq!(optimized.report().deduplicated.len(), 1);
        assert_eq!(
            optimized.plan().nodes[0].input,
            json!({ "source": { "$ref": "item1.output" } })
        );
    }

    #[test]
    fn pure_nodes_merge_across_ordering_edges() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("first", "lookup").with_input(json!({ "q": "x" })));
        let mut later = Node::new("later", "lookup").with_input(json!({ "q": "x" }));
        later.depends_on = vec!["gate".into()];
        plan.nodes.push(Node::new("gate", "lookup"));
        plan.nodes.push(later);
        plan.outputs
            .insert("value".into(), ValueRef::output("later"));

        let optimized = optimize(plan).unwrap();

        assert_eq!(optimized.report().deduplicated.len(), 1);
        assert_eq!(optimized.plan().outputs["value"], ValueRef::output("first"));
    }

    #[test]
    fn cacheable_impure_nodes_keep_ordering_in_the_key() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "read".into(),
            ToolSpec::new("test").with_effects(Effects::read_only(["db:x"])),
        );
        plan.tools.insert(
            "write".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:x"].into_iter().map(String::from).collect(),
                ..Effects::default()
            }),
        );
        plan.nodes
            .push(Node::new("before", "read").with_input(json!({ "q": 1 })));
        plan.nodes.push(Node::new("mutate", "write"));
        let mut after = Node::new("after", "read").with_input(json!({ "q": 1 }));
        after.depends_on = vec!["mutate".into()];
        plan.nodes.push(after);

        let optimized = optimize(plan).unwrap();

        assert!(optimized.report().deduplicated.is_empty());
        assert_eq!(optimized.plan().nodes.len(), 3);
    }

    #[test]
    fn does_not_deduplicate_effectful_nodes() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("a", "write").with_input(json!({ "q": "x" })));
        plan.nodes
            .push(Node::new("b", "write").with_input(json!({ "q": "x" })));

        let optimized = optimize(plan).unwrap();

        assert!(optimized.report().deduplicated.is_empty());
        assert_eq!(optimized.plan().nodes.len(), 2);
    }

    #[test]
    fn eliminates_unused_pure_nodes_when_outputs_are_declared() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("used", "lookup").with_input(json!({ "q": 1 })));
        plan.nodes
            .push(Node::new("orphan", "lookup").with_input(json!({ "q": 2 })));
        plan.nodes.push(Node::new("effect", "write"));
        plan.outputs.insert("out".into(), ValueRef::output("used"));

        let optimized = optimize(plan).unwrap();

        assert_eq!(optimized.report().eliminated, vec!["orphan".to_owned()]);
        assert_eq!(optimized.plan().nodes.len(), 2);
    }

    #[test]
    fn keeps_all_nodes_when_no_outputs_are_declared() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("orphan", "lookup").with_input(json!({ "q": 2 })));

        let optimized = optimize(plan).unwrap();

        assert!(optimized.report().eliminated.is_empty());
        assert_eq!(optimized.plan().nodes.len(), 1);
    }

    #[test]
    fn reports_batchable_groups_split_by_batch_size() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "lookup".into(),
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
        for id in ["a", "b", "c", "d", "e"] {
            plan.nodes
                .push(Node::new(id, "lookup").with_input(json!({ "q": id })));
        }

        let optimized = optimize(plan).unwrap();

        // Five nodes with batch_size 2: two pairs; the remainder runs alone.
        assert_eq!(optimized.report().batch_groups.len(), 2);
        assert_eq!(
            optimized.report().summary.estimated_tool_calls_after,
            3 // two batch dispatches + one single call
        );
        assert_eq!(optimized.report().summary.estimated_llm_turns_after, 1);
    }

    #[test]
    fn summary_estimates_cost_from_declared_metadata() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "lookup".into(),
            ToolSpec::new("test")
                .with_effects(Effects {
                    batchable: true,
                    ..Effects::pure()
                })
                .with_cost(ToolCost {
                    fixed_ms: Some(10),
                    per_call_ms: Some(5),
                    tokens: Some(100),
                }),
        );
        plan.nodes
            .push(Node::new("a", "lookup").with_input(json!({ "q": "a" })));
        plan.nodes
            .push(Node::new("b", "lookup").with_input(json!({ "q": "b" })));

        let summary = optimize(plan).unwrap().report().summary.clone();

        // Serial: two dispatches of (10 + 5). Compiled: one batch of two.
        assert_eq!(summary.estimated_serial_ms, Some(30));
        assert_eq!(summary.estimated_compiled_ms, Some(20));
        assert_eq!(summary.estimated_tokens_before, Some(200));
        assert_eq!(summary.estimated_tokens_after, Some(200));
    }

    #[test]
    fn summary_omits_costs_without_metadata() {
        let mut plan = plan();
        plan.nodes.push(Node::new("a", "lookup"));

        let summary = optimize(plan).unwrap().report().summary.clone();

        assert_eq!(summary.estimated_serial_ms, None);
        assert_eq!(summary.estimated_tokens_after, None);
    }

    #[test]
    fn records_the_pass_pipeline() {
        let optimized = optimize(plan()).unwrap();

        assert_eq!(optimized.report().passes, vec!["dedup", "dce", "batch"]);
    }

    #[test]
    fn does_not_rewrite_refs_inside_literals() {
        let mut plan = plan();
        plan.nodes
            .push(Node::new("a", "lookup").with_input(json!({ "q": "x" })));
        plan.nodes
            .push(Node::new("b", "lookup").with_input(json!({ "q": "x" })));
        plan.nodes.push(Node::new("c", "lookup").with_input(json!({
            "schema": { "$literal": { "$ref": "b.output" } }
        })));
        plan.outputs.insert("c".into(), ValueRef::output("c"));

        let optimized = optimize(plan).unwrap();

        let c = optimized
            .plan()
            .nodes
            .iter()
            .find(|node| node.id == "c")
            .unwrap();
        assert_eq!(
            c.input,
            json!({ "schema": { "$literal": { "$ref": "b.output" } } })
        );
    }

    #[test]
    fn explains_missing_effects_as_one_grouped_diagnostic() {
        let mut plan = Plan::new();
        plan.tools.insert("unknown".into(), ToolSpec::new("test"));
        plan.nodes.push(Node::new("a", "unknown"));
        plan.nodes.push(Node::new("b", "unknown"));
        plan.nodes.push(Node::new("c", "unknown"));

        let report = explain(plan).unwrap();

        assert_eq!(report.layers.len(), 3);
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].kind, DiagnosticKind::MissingEffects);
        assert_eq!(report.diagnostics[0].nodes, vec!["a", "b", "c"]);
    }

    #[test]
    fn explains_resource_conflicts_grouped_by_resource() {
        let mut plan = plan();
        plan.nodes.push(Node::new("w1", "write"));
        plan.nodes.push(Node::new("w2", "write"));

        let report = explain(plan).unwrap();

        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].kind, DiagnosticKind::ResourceConflict);
        assert_eq!(report.diagnostics[0].resource.as_deref(), Some("db:item"));
        assert_eq!(report.diagnostics[0].nodes, vec!["w1", "w2"]);
    }

    #[test]
    fn explains_read_write_conflicts() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "read".into(),
            ToolSpec::new("test").with_effects(Effects::read_only(["db:item"])),
        );
        plan.tools.insert(
            "write".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:item"].into_iter().map(String::from).collect(),
                ..Effects::default()
            }),
        );
        plan.nodes.push(Node::new("r", "read"));
        plan.nodes.push(Node::new("w", "write"));

        let report = explain(plan).unwrap();

        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].kind, DiagnosticKind::ResourceConflict);
        assert_eq!(report.diagnostics[0].nodes, vec!["r", "w"]);
    }

    #[test]
    fn does_not_flag_transitively_ordered_pairs() {
        let mut plan = plan();
        // w1 -> gate -> w2: same-resource writers, but ordered through gate.
        plan.nodes.push(Node::new("w1", "write"));
        let mut gate = Node::new("gate", "lookup");
        gate.depends_on = vec!["w1".into()];
        plan.nodes.push(gate);
        let mut w2 = Node::new("w2", "write");
        w2.depends_on = vec!["gate".into()];
        plan.nodes.push(w2);

        let report = explain(plan).unwrap();

        assert!(
            report
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.kind != DiagnosticKind::ResourceConflict)
        );
    }

    #[test]
    fn validates_before_optimizing() {
        let mut plan = Plan::new();
        plan.nodes.push(Node::new("a", "missing"));

        assert!(optimize(plan).is_err());
    }
}
