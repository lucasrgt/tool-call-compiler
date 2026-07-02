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
/// entry per node pair. Order relations are held as ancestor/descendant
/// bitsets and conflicts are found per resource, so cost tracks actual
/// conflicts instead of every node pair.
pub fn explain_parallel_boundaries(plan: &Plan, graph: &ExecutionGraph) -> Vec<Diagnostic> {
    let order = OrderIndex::new(graph);
    let effects = graph.resolved_effects();

    // A node with missing/vacuous effects is reported when at least one
    // other node is unordered with it across layers. Ancestors and
    // descendants always sit in other layers, so the count of unordered
    // cross-layer partners falls out arithmetically.
    let mut undeclared = BTreeSet::<NodeId>::new();
    for node in &plan.nodes {
        let Some(fx) = effects.get(&node.id) else {
            continue;
        };
        if fx.declared {
            continue;
        }
        let Some(index) = order.index_of(&node.id) else {
            continue;
        };
        let ordered = order.ancestor_count(index) + order.descendant_count(index);
        let unordered_cross_layer = order.total() - 1 - ordered - (order.layer_size(index) - 1);
        if unordered_cross_layer > 0 {
            undeclared.insert(node.id.clone());
        }
    }

    // Group writers/readers per resource; a node joins a resource's conflict
    // set when it is unordered across layers with a writer (or, when itself
    // a writer, with any toucher of the resource).
    let mut writers = BTreeMap::<&Resource, Vec<usize>>::new();
    let mut readers = BTreeMap::<&Resource, Vec<usize>>::new();
    for node in &plan.nodes {
        let (Some(fx), Some(index)) = (effects.get(&node.id), order.index_of(&node.id)) else {
            continue;
        };
        if !fx.declared {
            continue;
        }
        for resource in &fx.writes {
            writers.entry(resource).or_default().push(index);
        }
        for resource in &fx.reads {
            readers.entry(resource).or_default().push(index);
        }
    }

    let mut conflicts = BTreeMap::<Resource, BTreeSet<NodeId>>::new();
    for (resource, writing) in &writers {
        let reading = readers.get(*resource).map(Vec::as_slice).unwrap_or(&[]);
        let touchers: Vec<usize> = writing.iter().chain(reading).copied().collect();
        for &node in &touchers {
            // Writers conflict with every other toucher; readers only with
            // writers (read/read sharing is always safe).
            let partners: &[usize] = if writing.contains(&node) {
                &touchers
            } else {
                writing
            };
            let conflicting = partners
                .iter()
                .any(|&other| other != node && order.unordered_cross_layer(node, other));
            if conflicting {
                conflicts
                    .entry((*resource).clone())
                    .or_default()
                    .insert(order.node_id(node).to_owned());
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

/// Node order relations: layer membership plus transitive ancestor and
/// descendant sets as bitsets (one row of `n/64` words per node).
struct OrderIndex<'a> {
    ids: Vec<&'a str>,
    index: BTreeMap<&'a str, usize>,
    layer_of: Vec<usize>,
    layer_sizes: Vec<usize>,
    words: usize,
    ancestors: Vec<u64>,
    descendants: Vec<u64>,
}

impl<'a> OrderIndex<'a> {
    fn new(graph: &'a ExecutionGraph) -> Self {
        let ids: Vec<&str> = graph
            .layers()
            .iter()
            .flat_map(|layer| layer.iter().map(String::as_str))
            .collect();
        let index: BTreeMap<&str, usize> = ids
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position))
            .collect();
        let mut layer_of = vec![0; ids.len()];
        let mut layer_sizes = Vec::with_capacity(graph.layers().len());
        for (layer_index, layer) in graph.layers().iter().enumerate() {
            layer_sizes.push(layer.len());
            for node in layer {
                layer_of[index[node.as_str()]] = layer_index;
            }
        }

        let total = ids.len();
        let words = total.div_ceil(64);
        // The flattened layer order is topological, so one forward pass
        // accumulates ancestors and one backward pass descendants.
        let mut ancestors = vec![0u64; total * words];
        for (position, id) in ids.iter().enumerate() {
            if let Some(deps) = graph.dependencies().get(*id) {
                for dep in deps {
                    let dep_position = index[dep.as_str()];
                    or_row(&mut ancestors, words, dep_position, position);
                    set_bit(&mut ancestors, words, position, dep_position);
                }
            }
        }
        let mut descendants = vec![0u64; total * words];
        for (position, id) in ids.iter().enumerate().rev() {
            if let Some(deps) = graph.dependencies().get(*id) {
                for dep in deps {
                    let dep_position = index[dep.as_str()];
                    or_row(&mut descendants, words, position, dep_position);
                    set_bit(&mut descendants, words, dep_position, position);
                }
            }
        }

        Self {
            ids,
            index,
            layer_of,
            layer_sizes,
            words,
            ancestors,
            descendants,
        }
    }

    fn total(&self) -> usize {
        self.ids.len()
    }

    fn index_of(&self, node: &str) -> Option<usize> {
        self.index.get(node).copied()
    }

    fn node_id(&self, index: usize) -> &str {
        self.ids[index]
    }

    fn layer_size(&self, index: usize) -> usize {
        self.layer_sizes[self.layer_of[index]]
    }

    fn ancestor_count(&self, index: usize) -> usize {
        row(&self.ancestors, self.words, index)
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    fn descendant_count(&self, index: usize) -> usize {
        row(&self.descendants, self.words, index)
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    fn unordered_cross_layer(&self, left: usize, right: usize) -> bool {
        self.layer_of[left] != self.layer_of[right]
            && !get_bit(&self.ancestors, self.words, left, right)
            && !get_bit(&self.ancestors, self.words, right, left)
    }
}

fn row(bits: &[u64], words: usize, index: usize) -> &[u64] {
    &bits[index * words..(index + 1) * words]
}

fn set_bit(bits: &mut [u64], words: usize, row: usize, column: usize) {
    bits[row * words + column / 64] |= 1 << (column % 64);
}

fn get_bit(bits: &[u64], words: usize, row: usize, column: usize) -> bool {
    bits[row * words + column / 64] & (1 << (column % 64)) != 0
}

/// `bits[dst] |= bits[src]` for two row indexes of one matrix.
fn or_row(bits: &mut [u64], words: usize, src: usize, dst: usize) {
    for word in 0..words {
        let value = bits[src * words + word];
        bits[dst * words + word] |= value;
    }
}
