//! Diagnostics explaining why safe parallelization was blocked.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tool_compiler_graph::ExecutionGraph;
use tool_compiler_ir::{NodeId, Plan, Resource};

/// Machine-readable category of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    /// Nodes could not be parallelized because their tools declare no usable
    /// side-effect knowledge (missing or vacuous effects).
    MissingEffects,
    /// Nodes could not be parallelized because their declared effects touch
    /// the same resource.
    ResourceConflict,
}

/// One optimizer diagnostic: which nodes were held back and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Diagnostic category.
    pub kind: DiagnosticKind,
    /// Nodes involved, sorted.
    pub nodes: Vec<NodeId>,
    /// Conflicting resource, for [`DiagnosticKind::ResourceConflict`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<Resource>,
    /// Human-readable explanation.
    pub message: String,
}

/// Reports why node pairs that are not ordered by (transitive) dependencies
/// still ended up in different layers.
///
/// Pairs that are transitively ordered are never reported: their layering is
/// a consequence of the dependency graph, not of effect conservatism.
/// Findings are grouped — one diagnostic per conflicting resource, and one
/// for the whole set of nodes with missing/vacuous effects — instead of one
/// entry per node pair.
pub fn explain_parallel_boundaries(plan: &Plan, graph: &ExecutionGraph) -> Vec<Diagnostic> {
    let layer_index = layer_index(graph);
    let ancestors = transitive_ancestors(graph);
    let effects = graph.resolved_effects();

    let mut undeclared = BTreeSet::<NodeId>::new();
    let mut conflicts = BTreeMap::<Resource, BTreeSet<NodeId>>::new();

    for (left_index, left) in plan.nodes.iter().enumerate() {
        for right in plan.nodes.iter().skip(left_index + 1) {
            if layer_index.get(&left.id) == layer_index.get(&right.id) {
                continue;
            }
            if is_ordered(&ancestors, &left.id, &right.id) {
                continue;
            }

            let (Some(left_fx), Some(right_fx)) = (effects.get(&left.id), effects.get(&right.id))
            else {
                continue;
            };

            if !left_fx.declared || !right_fx.declared {
                if !left_fx.declared {
                    undeclared.insert(left.id.clone());
                }
                if !right_fx.declared {
                    undeclared.insert(right.id.clone());
                }
                continue;
            }

            for resource in left_fx.writes.intersection(&right_fx.writes) {
                let entry = conflicts.entry(resource.clone()).or_default();
                entry.insert(left.id.clone());
                entry.insert(right.id.clone());
            }
            for resource in left_fx.writes.intersection(&right_fx.reads) {
                let entry = conflicts.entry(resource.clone()).or_default();
                entry.insert(left.id.clone());
                entry.insert(right.id.clone());
            }
            for resource in right_fx.writes.intersection(&left_fx.reads) {
                let entry = conflicts.entry(resource.clone()).or_default();
                entry.insert(left.id.clone());
                entry.insert(right.id.clone());
            }
        }
    }

    let mut diagnostics = Vec::new();
    if !undeclared.is_empty() {
        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::MissingEffects,
            nodes: undeclared.into_iter().collect(),
            resource: None,
            message: "missing or vacuous tool effects prevent safe parallelization".into(),
        });
    }
    for (resource, nodes) in conflicts {
        diagnostics.push(Diagnostic {
            kind: DiagnosticKind::ResourceConflict,
            nodes: nodes.into_iter().collect(),
            resource: Some(resource.clone()),
            message: format!("effects touch the same resource '{resource}'"),
        });
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

fn is_ordered(ancestors: &BTreeMap<NodeId, BTreeSet<NodeId>>, left: &str, right: &str) -> bool {
    ancestors.get(left).is_some_and(|set| set.contains(right))
        || ancestors.get(right).is_some_and(|set| set.contains(left))
}

/// Full transitive ancestor sets, computed iteratively in topological order
/// (the layer order is already topological).
fn transitive_ancestors(graph: &ExecutionGraph) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut ancestors = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();

    for layer in graph.layers() {
        for node in layer {
            let mut set = BTreeSet::new();
            if let Some(deps) = graph.dependencies().get(node) {
                for dep in deps {
                    set.insert(dep.clone());
                    if let Some(dep_ancestors) = ancestors.get(dep) {
                        set.extend(dep_ancestors.iter().cloned());
                    }
                }
            }
            ancestors.insert(node.clone(), set);
        }
    }

    ancestors
}
