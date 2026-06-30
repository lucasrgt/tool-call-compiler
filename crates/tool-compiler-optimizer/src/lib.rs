use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_compiler_graph::{ExecutionGraph, GraphError, validate};
use tool_compiler_ir::{Node, NodeId, Plan, REF_KEY, ToolSpec, ValueRef};

#[derive(Clone, Debug, PartialEq)]
pub struct OptimizedPlan {
    plan: Plan,
    graph: ExecutionGraph,
    report: OptimizationReport,
}

impl OptimizedPlan {
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    pub fn graph(&self) -> &ExecutionGraph {
        &self.graph
    }

    pub fn report(&self) -> &OptimizationReport {
        &self.report
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimizationReport {
    pub deduplicated: Vec<DeduplicatedNode>,
    pub batch_groups: Vec<BatchGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeduplicatedNode {
    pub removed: NodeId,
    pub canonical: NodeId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchGroup {
    pub adapter: String,
    pub tool: String,
    pub nodes: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplainReport {
    pub layers: Vec<Vec<NodeId>>,
    pub optimization: OptimizationReport,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub kind: String,
    pub nodes: Vec<NodeId>,
    pub message: String,
}

pub fn optimize(plan: Plan) -> Result<OptimizedPlan, GraphError> {
    validate(&plan)?;

    let (plan, mut report) = deduplicate_nodes(plan);
    let graph = validate(&plan)?;
    report.batch_groups = find_batch_groups(&plan, &graph);

    Ok(OptimizedPlan {
        plan,
        graph,
        report,
    })
}

pub fn explain(plan: Plan) -> Result<ExplainReport, GraphError> {
    let optimized = optimize(plan)?;
    let diagnostics = explain_parallel_boundaries(optimized.plan(), optimized.graph());

    Ok(ExplainReport {
        layers: optimized.graph().layers().to_vec(),
        optimization: optimized.report().clone(),
        diagnostics,
    })
}

fn deduplicate_nodes(mut plan: Plan) -> (Plan, OptimizationReport) {
    let mut seen = BTreeMap::<DedupKey, NodeId>::new();
    let mut aliases = BTreeMap::<NodeId, NodeId>::new();
    let mut kept = Vec::new();
    let mut report = OptimizationReport::default();
    let nodes = std::mem::take(&mut plan.nodes);

    for mut node in nodes {
        rewrite_node_aliases(&mut node, &aliases);

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
    rewrite_outputs(&mut plan, &aliases);

    (plan, report)
}

fn find_batch_groups(plan: &Plan, graph: &ExecutionGraph) -> Vec<BatchGroup> {
    let mut groups = Vec::new();

    for layer in graph.layers() {
        let mut by_tool = BTreeMap::<String, Vec<NodeId>>::new();
        for node_id in layer {
            let Some(node) = plan.nodes.iter().find(|node| &node.id == node_id) else {
                continue;
            };
            let is_batchable = plan
                .tools
                .get(&node.tool)
                .and_then(|tool| tool.effects.as_ref())
                .is_some_and(|effects| effects.batchable);

            if is_batchable {
                by_tool
                    .entry(node.tool.clone())
                    .or_default()
                    .push(node.id.clone());
            }
        }

        for (tool, nodes) in by_tool {
            if nodes.len() > 1 {
                let adapter = plan
                    .tools
                    .get(&tool)
                    .map(|spec| spec.adapter.clone())
                    .unwrap_or_default();
                groups.push(BatchGroup {
                    adapter,
                    tool,
                    nodes,
                });
            }
        }
    }

    groups
}

fn explain_parallel_boundaries(plan: &Plan, graph: &ExecutionGraph) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let layer_index = layer_index(graph);

    for (left_index, left) in plan.nodes.iter().enumerate() {
        for right in plan.nodes.iter().skip(left_index + 1) {
            if layer_index.get(&left.id) == layer_index.get(&right.id) {
                continue;
            }
            if depends_on(graph, &left.id, &right.id) || depends_on(graph, &right.id, &left.id) {
                continue;
            }
            if let Some(message) =
                parallel_blocker(plan.tools.get(&left.tool), plan.tools.get(&right.tool))
            {
                diagnostics.push(Diagnostic {
                    kind: "not_parallelized".into(),
                    nodes: vec![left.id.clone(), right.id.clone()],
                    message,
                });
            }
        }
    }

    diagnostics
}

fn layer_index(graph: &ExecutionGraph) -> BTreeMap<NodeId, usize> {
    let mut indexes = BTreeMap::new();
    for (index, layer) in graph.layers().iter().enumerate() {
        for node in layer {
            indexes.insert(node.clone(), index);
        }
    }
    indexes
}

fn depends_on(graph: &ExecutionGraph, node: &str, dependency: &str) -> bool {
    graph
        .dependencies()
        .get(node)
        .is_some_and(|dependencies| dependencies.contains(dependency))
}

fn parallel_blocker(left: Option<&ToolSpec>, right: Option<&ToolSpec>) -> Option<String> {
    let left = left?;
    let right = right?;

    match (left.effects.as_ref(), right.effects.as_ref()) {
        (None, _) | (_, None) => Some("missing effects prevent safe parallelization".into()),
        (Some(left), Some(right)) => {
            if !left.writes.is_disjoint(&right.writes) {
                return Some("both tools write the same resource".into());
            }
            if !left.writes.is_disjoint(&right.reads) || !right.writes.is_disjoint(&left.reads) {
                return Some("read/write effects touch the same resource".into());
            }
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DedupKey {
    tool: String,
    input: String,
    depends_on: Vec<NodeId>,
}

fn dedup_key(plan: &Plan, node: &Node) -> Option<DedupKey> {
    let effects = plan.tools.get(&node.tool)?.effects.as_ref()?;
    if !(effects.pure || effects.cacheable) {
        return None;
    }

    Some(DedupKey {
        tool: node.tool.clone(),
        input: serde_json::to_string(&node.input).ok()?,
        depends_on: node.depends_on.clone(),
    })
}

fn rewrite_node_aliases(node: &mut Node, aliases: &BTreeMap<NodeId, NodeId>) {
    node.depends_on = node
        .depends_on
        .iter()
        .map(|dependency| canonical_node(dependency, aliases))
        .collect();
    rewrite_refs_in_value(&mut node.input, aliases);
}

fn rewrite_outputs(plan: &mut Plan, aliases: &BTreeMap<NodeId, NodeId>) {
    for value_ref in plan.outputs.values_mut() {
        *value_ref = rewrite_value_ref(value_ref, aliases);
    }
}

fn rewrite_refs_in_value(value: &mut Value, aliases: &BTreeMap<NodeId, NodeId>) {
    match value {
        Value::Object(map) => {
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

fn rewrite_value_ref(value_ref: &ValueRef, aliases: &BTreeMap<NodeId, NodeId>) -> ValueRef {
    ValueRef::new(
        canonical_node(value_ref.node(), aliases),
        value_ref.path().to_owned(),
    )
}

fn canonical_node(node: &str, aliases: &BTreeMap<NodeId, NodeId>) -> NodeId {
    let mut current = node;
    while let Some(next) = aliases.get(current) {
        current = next;
    }
    current.to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, Plan, ToolSpec, ValueRef};

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
    fn reports_batchable_groups_in_safe_layers() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "lookup".into(),
            ToolSpec::new("test").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        plan.nodes
            .push(Node::new("a", "lookup").with_input(json!({ "q": "a" })));
        plan.nodes
            .push(Node::new("b", "lookup").with_input(json!({ "q": "b" })));

        let optimized = optimize(plan).unwrap();

        assert_eq!(
            optimized.report().batch_groups,
            vec![BatchGroup {
                adapter: "test".into(),
                tool: "lookup".into(),
                nodes: vec!["a".into(), "b".into()],
            }]
        );
    }

    #[test]
    fn explains_parallel_blockers() {
        let mut plan = Plan::new();
        plan.tools.insert("unknown".into(), ToolSpec::new("test"));
        plan.nodes.push(Node::new("a", "unknown"));
        plan.nodes.push(Node::new("b", "unknown"));

        let report = explain(plan).unwrap();

        assert_eq!(
            report.layers,
            vec![vec!["a".to_owned()], vec!["b".to_owned()]]
        );
        assert_eq!(report.diagnostics[0].kind, "not_parallelized");
    }

    #[test]
    fn validates_before_optimizing() {
        let mut plan = Plan::new();
        plan.nodes.push(Node::new("a", "missing"));

        assert!(optimize(plan).is_err());
    }
}
