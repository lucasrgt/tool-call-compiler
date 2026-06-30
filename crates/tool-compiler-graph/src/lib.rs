use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tool_compiler_ir::{CURRENT_VERSION, Node, NodeId, Plan, ToolSpec, ValueRef, collect_refs};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionGraph {
    dependencies: BTreeMap<NodeId, BTreeSet<NodeId>>,
    layers: Vec<Vec<NodeId>>,
}

impl ExecutionGraph {
    pub fn dependencies(&self) -> &BTreeMap<NodeId, BTreeSet<NodeId>> {
        &self.dependencies
    }

    pub fn layers(&self) -> &[Vec<NodeId>] {
        &self.layers
    }
}

pub fn validate(plan: &Plan) -> Result<ExecutionGraph, GraphError> {
    if plan.version != CURRENT_VERSION {
        return Err(GraphError::UnsupportedVersion(plan.version.clone()));
    }

    let nodes = index_nodes(&plan.nodes)?;
    ensure_tools_exist(plan, &nodes)?;

    let dependencies = collect_dependencies(plan, &nodes)?;
    ensure_outputs_exist(plan, &nodes)?;
    let layers = safe_layers(plan, &nodes, &dependencies)?;

    Ok(ExecutionGraph {
        dependencies,
        layers,
    })
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    #[error("unsupported plan version '{0}'")]
    UnsupportedVersion(String),
    #[error("duplicate node id '{0}'")]
    DuplicateNode(NodeId),
    #[error("node '{node}' uses unknown tool '{tool}'")]
    UnknownTool { node: NodeId, tool: String },
    #[error("node '{node}' depends on unknown node '{dependency}'")]
    UnknownDependency { node: NodeId, dependency: NodeId },
    #[error("output '{output}' references unknown node '{node}'")]
    UnknownOutputReference { output: String, node: NodeId },
    #[error("invalid reference in node '{node}': {message}")]
    InvalidNodeReference { node: NodeId, message: String },
    #[error("cycle detected involving node '{0}'")]
    Cycle(NodeId),
}

fn index_nodes(nodes: &[Node]) -> Result<BTreeMap<NodeId, Node>, GraphError> {
    let mut indexed = BTreeMap::new();

    for node in nodes {
        if indexed.insert(node.id.clone(), node.clone()).is_some() {
            return Err(GraphError::DuplicateNode(node.id.clone()));
        }
    }

    Ok(indexed)
}

fn ensure_tools_exist(plan: &Plan, nodes: &BTreeMap<NodeId, Node>) -> Result<(), GraphError> {
    for node in nodes.values() {
        if !plan.tools.contains_key(&node.tool) {
            return Err(GraphError::UnknownTool {
                node: node.id.clone(),
                tool: node.tool.clone(),
            });
        }
    }

    Ok(())
}

fn collect_dependencies(
    plan: &Plan,
    nodes: &BTreeMap<NodeId, Node>,
) -> Result<BTreeMap<NodeId, BTreeSet<NodeId>>, GraphError> {
    let mut dependencies = BTreeMap::new();

    for node in nodes.values() {
        let mut deps = BTreeSet::new();

        for dependency in &node.depends_on {
            if !nodes.contains_key(dependency) {
                return Err(GraphError::UnknownDependency {
                    node: node.id.clone(),
                    dependency: dependency.clone(),
                });
            }
            deps.insert(dependency.clone());
        }

        let refs = collect_refs(&node.input).map_err(|error| GraphError::InvalidNodeReference {
            node: node.id.clone(),
            message: error.to_string(),
        })?;

        for value_ref in refs {
            insert_ref_dependency(&node.id, &value_ref, nodes, &mut deps)?;
        }

        dependencies.insert(node.id.clone(), deps);
    }

    ensure_no_cycles(nodes, &dependencies)?;
    ensure_no_unknown_tools_in_dependencies(plan, nodes)?;

    Ok(dependencies)
}

fn insert_ref_dependency(
    node_id: &str,
    value_ref: &ValueRef,
    nodes: &BTreeMap<NodeId, Node>,
    deps: &mut BTreeSet<NodeId>,
) -> Result<(), GraphError> {
    let dependency = value_ref.node();
    if !nodes.contains_key(dependency) {
        return Err(GraphError::UnknownDependency {
            node: node_id.to_owned(),
            dependency: dependency.to_owned(),
        });
    }

    if dependency != node_id {
        deps.insert(dependency.to_owned());
    }

    Ok(())
}

fn ensure_no_unknown_tools_in_dependencies(
    plan: &Plan,
    nodes: &BTreeMap<NodeId, Node>,
) -> Result<(), GraphError> {
    for node in nodes.values() {
        if !plan.tools.contains_key(&node.tool) {
            return Err(GraphError::UnknownTool {
                node: node.id.clone(),
                tool: node.tool.clone(),
            });
        }
    }

    Ok(())
}

fn ensure_outputs_exist(plan: &Plan, nodes: &BTreeMap<NodeId, Node>) -> Result<(), GraphError> {
    for (output, value_ref) in &plan.outputs {
        if !nodes.contains_key(value_ref.node()) {
            return Err(GraphError::UnknownOutputReference {
                output: output.clone(),
                node: value_ref.node().to_owned(),
            });
        }
    }

    Ok(())
}

fn ensure_no_cycles(
    nodes: &BTreeMap<NodeId, Node>,
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<(), GraphError> {
    let mut temporary = BTreeSet::new();
    let mut permanent = BTreeSet::new();

    for node in nodes.keys() {
        visit_for_cycle(node, dependencies, &mut temporary, &mut permanent)?;
    }

    Ok(())
}

fn visit_for_cycle(
    node: &str,
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    temporary: &mut BTreeSet<NodeId>,
    permanent: &mut BTreeSet<NodeId>,
) -> Result<(), GraphError> {
    if permanent.contains(node) {
        return Ok(());
    }

    if !temporary.insert(node.to_owned()) {
        return Err(GraphError::Cycle(node.to_owned()));
    }

    if let Some(deps) = dependencies.get(node) {
        for dependency in deps {
            visit_for_cycle(dependency, dependencies, temporary, permanent)?;
        }
    }

    temporary.remove(node);
    permanent.insert(node.to_owned());
    Ok(())
}

fn safe_layers(
    plan: &Plan,
    nodes: &BTreeMap<NodeId, Node>,
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> Result<Vec<Vec<NodeId>>, GraphError> {
    let mut remaining_dependencies = dependencies.clone();
    let dependents = reverse_dependencies(dependencies);
    let mut ready = initial_ready(&remaining_dependencies);
    let mut layers = Vec::new();
    let mut emitted = BTreeSet::new();

    while !ready.is_empty() {
        let mut layer = Vec::new();

        for candidate in ready.clone() {
            if can_join_layer(plan, nodes, &candidate, &layer) {
                ready.remove(&candidate);
                layer.push(candidate);
            }
        }

        if layer.is_empty()
            && let Some(candidate) = ready.pop_first()
        {
            layer.push(candidate);
        }

        for node in &layer {
            emitted.insert(node.clone());
            if let Some(children) = dependents.get(node) {
                for child in children {
                    if let Some(deps) = remaining_dependencies.get_mut(child) {
                        deps.remove(node);
                        if deps.is_empty() && !emitted.contains(child) {
                            ready.insert(child.clone());
                        }
                    }
                }
            }
        }

        layers.push(layer);
    }

    if emitted.len() != nodes.len() {
        let missing = nodes
            .keys()
            .find(|node| !emitted.contains(*node))
            .cloned()
            .unwrap_or_default();
        return Err(GraphError::Cycle(missing));
    }

    Ok(layers)
}

fn reverse_dependencies(
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut dependents = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();

    for (node, deps) in dependencies {
        for dep in deps {
            dependents
                .entry(dep.clone())
                .or_default()
                .insert(node.clone());
        }
    }

    dependents
}

fn initial_ready(dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>) -> BTreeSet<NodeId> {
    dependencies
        .iter()
        .filter_map(|(node, deps)| deps.is_empty().then_some(node.clone()))
        .collect()
}

fn can_join_layer(
    plan: &Plan,
    nodes: &BTreeMap<NodeId, Node>,
    candidate: &str,
    layer: &[NodeId],
) -> bool {
    layer.iter().all(|existing| {
        let existing_node = nodes.get(existing).expect("validated node should exist");
        let candidate_node = nodes.get(candidate).expect("validated node should exist");
        effects_can_run_together(
            plan.tools.get(&existing_node.tool),
            plan.tools.get(&candidate_node.tool),
        )
    })
}

fn effects_can_run_together(left: Option<&ToolSpec>, right: Option<&ToolSpec>) -> bool {
    let Some(left) = left.and_then(|tool| tool.effects.as_ref()) else {
        return false;
    };
    let Some(right) = right.and_then(|tool| tool.effects.as_ref()) else {
        return false;
    };

    if left.pure && right.pure {
        return true;
    }

    if left.writes.is_empty() && right.writes.is_empty() {
        return true;
    }

    left.writes.is_disjoint(&right.writes)
        && left.writes.is_disjoint(&right.reads)
        && right.writes.is_disjoint(&left.reads)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, Plan, ToolSpec, ValueRef};

    use super::*;

    fn plan_with_tools() -> Plan {
        let mut plan = Plan::new();
        plan.tools.insert(
            "read_user".into(),
            ToolSpec::new("test").with_effects(Effects::read_only(["api:user"])),
        );
        plan.tools.insert(
            "read_orders".into(),
            ToolSpec::new("test").with_effects(Effects::read_only(["api:orders"])),
        );
        plan.tools.insert(
            "write_user".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["api:user"].into_iter().map(String::from).collect(),
                idempotent: true,
                ..Effects::default()
            }),
        );
        plan
    }

    #[test]
    fn builds_dependencies_from_refs() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("user", "read_user"));
        plan.nodes
            .push(Node::new("orders", "read_orders").with_input(json!({
                "user_id": { "$ref": "user.output.id" }
            })));

        let graph = validate(&plan).unwrap();

        assert_eq!(
            graph.dependencies().get("orders").unwrap(),
            &BTreeSet::from(["user".to_owned()])
        );
        assert_eq!(
            graph.layers(),
            &[vec!["user".to_owned()], vec!["orders".to_owned()]]
        );
    }

    #[test]
    fn independent_read_only_nodes_share_a_layer() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("user", "read_user"));
        plan.nodes.push(Node::new("orders", "read_orders"));

        let graph = validate(&plan).unwrap();

        assert_eq!(
            graph.layers(),
            &[vec!["orders".to_owned(), "user".to_owned()]]
        );
    }

    #[test]
    fn conflicting_writes_do_not_share_a_layer() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("read", "read_user"));
        plan.nodes.push(Node::new("write", "write_user"));

        let graph = validate(&plan).unwrap();

        assert_eq!(
            graph.layers(),
            &[vec!["read".to_owned()], vec!["write".to_owned()]]
        );
    }

    #[test]
    fn unknown_effects_are_not_parallelized() {
        let mut plan = Plan::new();
        plan.tools.insert("a".into(), ToolSpec::new("test"));
        plan.nodes.push(Node::new("one", "a"));
        plan.nodes.push(Node::new("two", "a"));

        let graph = validate(&plan).unwrap();

        assert_eq!(
            graph.layers(),
            &[vec!["one".to_owned()], vec!["two".to_owned()]]
        );
    }

    #[test]
    fn rejects_unknown_output_ref() {
        let mut plan = plan_with_tools();
        plan.outputs
            .insert("result".into(), ValueRef::output("missing"));

        assert!(matches!(
            validate(&plan),
            Err(GraphError::UnknownOutputReference { .. })
        ));
    }

    #[test]
    fn rejects_cycles() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node {
            id: "a".into(),
            tool: "read_user".into(),
            input: json!({}),
            depends_on: vec!["b".into()],
        });
        plan.nodes.push(Node {
            id: "b".into(),
            tool: "read_user".into(),
            input: json!({}),
            depends_on: vec!["a".into()],
        });

        assert!(matches!(validate(&plan), Err(GraphError::Cycle(_))));
    }

    #[test]
    fn rejects_unsupported_versions() {
        let mut plan = plan_with_tools();
        plan.version = "999".into();

        assert!(matches!(
            validate(&plan),
            Err(GraphError::UnsupportedVersion(version)) if version == "999"
        ));
    }

    #[test]
    fn rejects_duplicate_nodes() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("same", "read_user"));
        plan.nodes.push(Node::new("same", "read_user"));

        assert!(matches!(validate(&plan), Err(GraphError::DuplicateNode(id)) if id == "same"));
    }

    #[test]
    fn rejects_unknown_tools() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("bad", "missing"));

        assert!(matches!(
            validate(&plan),
            Err(GraphError::UnknownTool { .. })
        ));
    }

    #[test]
    fn rejects_unknown_explicit_dependencies() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node {
            id: "bad".into(),
            tool: "read_user".into(),
            input: json!({}),
            depends_on: vec!["missing".into()],
        });

        assert!(matches!(
            validate(&plan),
            Err(GraphError::UnknownDependency { .. })
        ));
    }

    #[test]
    fn rejects_invalid_refs_in_inputs() {
        let mut plan = plan_with_tools();
        plan.nodes
            .push(Node::new("bad", "read_user").with_input(json!({
                "value": { "$ref": "bad-ref" }
            })));

        assert!(matches!(
            validate(&plan),
            Err(GraphError::InvalidNodeReference { .. })
        ));
    }
}
