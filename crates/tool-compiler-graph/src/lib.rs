//! Validation and dependency graph construction for tool-compiler plans.
//!
//! [`validate`] checks a [`Plan`] structurally (ids, tools, references,
//! cycles, effect declarations) and produces an [`ExecutionGraph`]: the
//! dependency map, conservative parallel execution layers, and per-node
//! [`ResolvedEffects`] with resource templates substituted from each node's
//! input.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use tool_compiler_ir::{
    CURRENT_VERSION, Effects, Node, NodeId, Plan, Resource, ToolName, collect_refs,
    resolve_resource_template, validate_node_id,
};

/// Validated execution structure of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionGraph {
    dependencies: BTreeMap<NodeId, BTreeSet<NodeId>>,
    layers: Vec<Vec<NodeId>>,
    resolved_effects: BTreeMap<NodeId, ResolvedEffects>,
}

impl ExecutionGraph {
    /// Direct dependencies of each node (explicit `depends_on`, input
    /// references, `when` targets, and `for_each` sources).
    pub fn dependencies(&self) -> &BTreeMap<NodeId, BTreeSet<NodeId>> {
        &self.dependencies
    }

    /// Conservative parallel layers: nodes in one layer have no dependency
    /// or effect conflicts between them.
    pub fn layers(&self) -> &[Vec<NodeId>] {
        &self.layers
    }

    /// Per-node effects with resource templates resolved from node inputs.
    pub fn resolved_effects(&self) -> &BTreeMap<NodeId, ResolvedEffects> {
        &self.resolved_effects
    }
}

/// A node's effects after resolving `{placeholder}` resource templates
/// against its input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedEffects {
    /// Tool the node calls (commutativity only applies within one tool).
    pub tool: ToolName,
    /// Whether the declaration says anything about side effects; vacuous or
    /// missing declarations leave this `false` and block parallelization.
    pub declared: bool,
    /// Declared purity.
    pub pure: bool,
    /// Resolved read resources.
    pub reads: BTreeSet<Resource>,
    /// Resolved write resources.
    pub writes: BTreeSet<Resource>,
    /// Declared commutativity.
    pub commutative: bool,
}

impl ResolvedEffects {
    fn for_node(node: &Node, effects: Option<&Effects>) -> Self {
        let Some(effects) = effects else {
            return Self {
                tool: node.tool.clone(),
                ..Self::default()
            };
        };
        let resolve = |resources: &BTreeSet<Resource>| {
            resources
                .iter()
                .map(|template| resolve_resource_template(template, &node.input))
                .collect()
        };
        Self {
            tool: node.tool.clone(),
            declared: effects.declares_side_effects(),
            pure: effects.pure,
            reads: resolve(&effects.reads),
            writes: resolve(&effects.writes),
            commutative: effects.commutative,
        }
    }

    /// Whether this node may share a layer (run concurrently) with `other`.
    ///
    /// Requires both sides to actually declare side-effect knowledge; then
    /// purity, disjoint read/write sets, or same-tool commutativity (for
    /// write/write overlap only) permit concurrency.
    pub fn can_run_with(&self, other: &Self) -> bool {
        if !self.declared || !other.declared {
            return false;
        }
        if self.pure && other.pure {
            return true;
        }

        let write_read_race =
            !self.writes.is_disjoint(&other.reads) || !other.writes.is_disjoint(&self.reads);
        if write_read_race {
            return false;
        }
        if self.writes.is_disjoint(&other.writes) {
            return true;
        }

        self.tool == other.tool && self.commutative && other.commutative
    }
}

/// Validates a plan and builds its execution graph.
pub fn validate(plan: &Plan) -> Result<ExecutionGraph, GraphError> {
    if plan.version != CURRENT_VERSION {
        return Err(GraphError::UnsupportedVersion(plan.version.clone()));
    }

    let nodes = index_nodes(&plan.nodes)?;
    ensure_tools_exist(plan, &nodes)?;
    ensure_effects_are_coherent(plan)?;

    let dependencies = collect_dependencies(&nodes)?;
    ensure_outputs_exist(plan, &nodes)?;

    let resolved_effects = nodes
        .iter()
        .map(|(id, node)| {
            let effects = plan
                .tools
                .get(&node.tool)
                .and_then(|tool| tool.effects.as_ref());
            ((*id).to_owned(), ResolvedEffects::for_node(node, effects))
        })
        .collect::<BTreeMap<_, _>>();

    let layers = safe_layers(&nodes, &dependencies, &resolved_effects)?;

    Ok(ExecutionGraph {
        dependencies,
        layers,
        resolved_effects,
    })
}

/// Errors detected while validating a plan.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GraphError {
    /// The plan declares a version this crate does not support.
    #[error("unsupported plan version '{0}'")]
    UnsupportedVersion(String),
    /// Two nodes share the same id.
    #[error("duplicate node id '{0}'")]
    DuplicateNode(NodeId),
    /// A node id fails [`validate_node_id`].
    #[error("invalid node id: {0}")]
    InvalidNodeId(String),
    /// A node names a tool the plan does not declare.
    #[error("node '{node}' uses unknown tool '{tool}'")]
    UnknownTool {
        /// Offending node id.
        node: NodeId,
        /// Missing tool name.
        tool: String,
    },
    /// A node depends on (or references) a node that does not exist.
    #[error("node '{node}' depends on unknown node '{dependency}'")]
    UnknownDependency {
        /// Offending node id.
        node: NodeId,
        /// Missing dependency id.
        dependency: NodeId,
    },
    /// A node depends on or references itself.
    #[error("node '{0}' references itself")]
    SelfReference(NodeId),
    /// A named output references a node that does not exist.
    #[error("output '{output}' references unknown node '{node}'")]
    UnknownOutputReference {
        /// Output name.
        output: String,
        /// Missing node id.
        node: NodeId,
    },
    /// A node input carries a malformed reference.
    #[error("invalid reference in node '{node}': {message}")]
    InvalidNodeReference {
        /// Offending node id.
        node: NodeId,
        /// Parse failure description.
        message: String,
    },
    /// A tool declares contradictory effects.
    #[error("tool '{tool}' declares contradictory effects: {message}")]
    InvalidEffects {
        /// Offending tool name.
        tool: String,
        /// Contradiction description.
        message: String,
    },
    /// The dependency graph contains a cycle.
    #[error("cycle detected involving node '{0}'")]
    Cycle(NodeId),
}

fn index_nodes(nodes: &[Node]) -> Result<BTreeMap<&str, &Node>, GraphError> {
    let mut indexed = BTreeMap::new();

    for node in nodes {
        validate_node_id(&node.id).map_err(|error| GraphError::InvalidNodeId(error.to_string()))?;
        if indexed.insert(node.id.as_str(), node).is_some() {
            return Err(GraphError::DuplicateNode(node.id.clone()));
        }
    }

    Ok(indexed)
}

fn ensure_tools_exist(plan: &Plan, nodes: &BTreeMap<&str, &Node>) -> Result<(), GraphError> {
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

fn ensure_effects_are_coherent(plan: &Plan) -> Result<(), GraphError> {
    for (tool, spec) in &plan.tools {
        if let Some(effects) = &spec.effects
            && effects.is_contradictory()
        {
            return Err(GraphError::InvalidEffects {
                tool: tool.clone(),
                message: "a pure tool cannot declare writes".into(),
            });
        }
    }

    Ok(())
}

fn collect_dependencies(
    nodes: &BTreeMap<&str, &Node>,
) -> Result<BTreeMap<NodeId, BTreeSet<NodeId>>, GraphError> {
    let mut dependencies = BTreeMap::new();

    for node in nodes.values() {
        let mut deps = BTreeSet::new();

        for dependency in &node.depends_on {
            insert_dependency(&node.id, dependency, nodes, &mut deps)?;
        }

        let refs = collect_refs(&node.input).map_err(|error| GraphError::InvalidNodeReference {
            node: node.id.clone(),
            message: error.to_string(),
        })?;
        for value_ref in &refs {
            insert_dependency(&node.id, value_ref.node(), nodes, &mut deps)?;
        }

        if let Some(items) = &node.for_each {
            insert_dependency(&node.id, items.node(), nodes, &mut deps)?;
        }
        if let Some(when) = &node.when {
            insert_dependency(&node.id, when.target.node(), nodes, &mut deps)?;
        }

        dependencies.insert(node.id.clone(), deps);
    }

    Ok(dependencies)
}

fn insert_dependency(
    node_id: &str,
    dependency: &str,
    nodes: &BTreeMap<&str, &Node>,
    deps: &mut BTreeSet<NodeId>,
) -> Result<(), GraphError> {
    if dependency == node_id {
        return Err(GraphError::SelfReference(node_id.to_owned()));
    }
    if !nodes.contains_key(dependency) {
        return Err(GraphError::UnknownDependency {
            node: node_id.to_owned(),
            dependency: dependency.to_owned(),
        });
    }

    deps.insert(dependency.to_owned());
    Ok(())
}

fn ensure_outputs_exist(plan: &Plan, nodes: &BTreeMap<&str, &Node>) -> Result<(), GraphError> {
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

/// Builds parallel layers via Kahn's algorithm; the leftover check doubles
/// as cycle detection, so no separate DFS pass is needed (and no recursion
/// that could overflow on deep graphs).
fn safe_layers(
    nodes: &BTreeMap<&str, &Node>,
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    resolved_effects: &BTreeMap<NodeId, ResolvedEffects>,
) -> Result<Vec<Vec<NodeId>>, GraphError> {
    let mut remaining_dependencies = dependencies.clone();
    let dependents = reverse_dependencies(dependencies);
    let mut ready = initial_ready(&remaining_dependencies);
    let mut layers = Vec::new();
    let mut emitted = BTreeSet::new();

    while !ready.is_empty() {
        let mut layer: Vec<NodeId> = Vec::new();

        for candidate in ready.clone() {
            let candidate_effects = &resolved_effects[&candidate];
            let joins = layer
                .iter()
                .all(|existing| resolved_effects[existing].can_run_with(candidate_effects));
            if joins {
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
            .find(|node| !emitted.contains(**node))
            .map(|node| (*node).to_owned())
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, Plan, ToolSpec, ValueRef, When};

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
    fn vacuous_effects_are_not_parallelized() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "a".into(),
            ToolSpec::new("test").with_effects(Effects {
                idempotent: true,
                ..Effects::default()
            }),
        );
        plan.nodes.push(Node::new("one", "a"));
        plan.nodes.push(Node::new("two", "a"));

        let graph = validate(&plan).unwrap();

        assert_eq!(graph.layers().len(), 2);
    }

    #[test]
    fn commutative_same_tool_writes_share_a_layer() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "increment".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:counter"].into_iter().map(String::from).collect(),
                commutative: true,
                idempotent: false,
                ..Effects::default()
            }),
        );
        plan.nodes.push(Node::new("one", "increment"));
        plan.nodes.push(Node::new("two", "increment"));

        let graph = validate(&plan).unwrap();

        assert_eq!(graph.layers().len(), 1);
    }

    #[test]
    fn resource_templates_resolve_per_node() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "write_file".into(),
            ToolSpec::new("fs").with_effects(Effects {
                writes: ["file:{path}"].into_iter().map(String::from).collect(),
                idempotent: true,
                ..Effects::default()
            }),
        );
        plan.nodes
            .push(Node::new("a", "write_file").with_input(json!({ "path": "a.txt" })));
        plan.nodes
            .push(Node::new("b", "write_file").with_input(json!({ "path": "b.txt" })));
        plan.nodes
            .push(Node::new("c", "write_file").with_input(json!({ "path": "a.txt" })));

        let graph = validate(&plan).unwrap();

        assert!(graph.resolved_effects()["a"].writes.contains("file:a.txt"));
        // a and b write different files and may share a layer; c conflicts with a.
        assert_eq!(graph.layers()[0], vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(graph.layers()[1], vec!["c".to_owned()]);
    }

    #[test]
    fn rejects_pure_tools_that_write() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "broken".into(),
            ToolSpec::new("test").with_effects(Effects {
                writes: ["db:x"].into_iter().map(String::from).collect(),
                ..Effects::pure()
            }),
        );
        plan.nodes.push(Node::new("a", "broken"));

        assert!(matches!(
            validate(&plan),
            Err(GraphError::InvalidEffects { .. })
        ));
    }

    #[test]
    fn rejects_self_references() {
        let mut plan = plan_with_tools();
        plan.nodes
            .push(Node::new("loop", "read_user").with_input(json!({
                "value": { "$ref": "loop.output" }
            })));

        assert_eq!(
            validate(&plan),
            Err(GraphError::SelfReference("loop".into()))
        );
    }

    #[test]
    fn rejects_self_dependencies() {
        let mut plan = plan_with_tools();
        let mut node = Node::new("loop", "read_user");
        node.depends_on = vec!["loop".into()];
        plan.nodes.push(node);

        assert_eq!(
            validate(&plan),
            Err(GraphError::SelfReference("loop".into()))
        );
    }

    #[test]
    fn rejects_invalid_node_ids() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("a.b", "read_user"));

        assert!(matches!(validate(&plan), Err(GraphError::InvalidNodeId(_))));
    }

    #[test]
    fn when_and_for_each_create_dependencies() {
        let mut plan = plan_with_tools();
        plan.nodes.push(Node::new("gate", "read_user"));
        plan.nodes.push(Node::new("items", "read_orders"));
        plan.nodes.push(
            Node::new("send", "read_orders")
                .with_for_each(ValueRef::new("items", ["list"]))
                .with_when(When::truthy(ValueRef::output("gate"))),
        );

        let graph = validate(&plan).unwrap();

        assert_eq!(
            graph.dependencies()["send"],
            BTreeSet::from(["gate".to_owned(), "items".to_owned()])
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
        let mut a = Node::new("a", "read_user");
        a.depends_on = vec!["b".into()];
        let mut b = Node::new("b", "read_user");
        b.depends_on = vec!["a".into()];
        plan.nodes.push(a);
        plan.nodes.push(b);

        assert!(matches!(validate(&plan), Err(GraphError::Cycle(_))));
    }

    #[test]
    fn deep_chains_do_not_overflow_the_stack() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("test").with_effects(Effects::pure()),
        );
        for index in 0..50_000 {
            let mut node = Node::new(format!("n{index}"), "echo");
            if index > 0 {
                node.depends_on = vec![format!("n{}", index - 1)];
            }
            plan.nodes.push(node);
        }

        let graph = validate(&plan).unwrap();

        assert_eq!(graph.layers().len(), 50_000);
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
        let mut node = Node::new("bad", "read_user");
        node.depends_on = vec!["missing".into()];
        plan.nodes.push(node);

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
