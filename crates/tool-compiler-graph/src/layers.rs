//! Layer construction: conservative parallel layers via Kahn's algorithm.
//!
//! Effect-conflict admission is amortized through per-layer aggregates
//! instead of pairwise `can_run_with` comparisons, so building a layer of
//! `k` nodes costs O(k · resources-per-node) rather than O(k²). The
//! decisions — and therefore the emitted layers — are exactly those of the
//! pairwise formulation.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use tool_compiler_ir::NodeId;

use crate::{GraphError, ResolvedEffects};

/// The write claim on one resource within the layer being built.
///
/// Write/write overlap is only admissible within a single commutative tool,
/// so every member writing a resource necessarily shares one tool and
/// commutativity — one claim per resource captures them all.
struct WriteClaim<'a> {
    tool: &'a str,
    commutative: bool,
}

/// Aggregated effect state of the layer being built.
///
/// [`LayerState::admits`] evaluates the same conditions as
/// [`ResolvedEffects::can_run_with`] against every member at once: the
/// pairwise checks decompose into membership tests against the union of
/// member reads and the per-resource write claims (pure members contribute
/// no writes, which is what makes the pure/pure shortcut fall out).
#[derive(Default)]
struct LayerState<'a> {
    members: usize,
    undeclared: bool,
    reads: HashSet<&'a str>,
    writes: HashMap<&'a str, WriteClaim<'a>>,
}

impl<'a> LayerState<'a> {
    fn admits(&self, candidate: &ResolvedEffects) -> bool {
        if self.members == 0 {
            return true;
        }
        if self.undeclared || !candidate.declared {
            return false;
        }
        if candidate
            .reads
            .iter()
            .any(|resource| self.writes.contains_key(resource.as_str()))
        {
            return false;
        }
        if candidate.pure {
            return true;
        }
        candidate.writes.iter().all(|resource| {
            !self.reads.contains(resource.as_str())
                && self.writes.get(resource.as_str()).is_none_or(|claim| {
                    claim.commutative && candidate.commutative && claim.tool == candidate.tool
                })
        })
    }

    fn absorb(&mut self, candidate: &'a ResolvedEffects) {
        self.members += 1;
        if !candidate.declared {
            self.undeclared = true;
            return;
        }
        self.reads
            .extend(candidate.reads.iter().map(String::as_str));
        if !candidate.pure {
            for resource in &candidate.writes {
                self.writes.entry(resource.as_str()).or_insert(WriteClaim {
                    tool: &candidate.tool,
                    commutative: candidate.commutative,
                });
            }
        }
    }
}

/// Builds parallel layers via Kahn's algorithm; the leftover check doubles
/// as cycle detection, so no separate DFS pass is needed (and no recursion
/// that could overflow on deep graphs).
pub(crate) fn safe_layers(
    dependencies: &BTreeMap<NodeId, BTreeSet<NodeId>>,
    resolved_effects: &BTreeMap<NodeId, ResolvedEffects>,
) -> Result<Vec<Vec<NodeId>>, GraphError> {
    let mut pending: BTreeMap<&str, usize> = dependencies
        .iter()
        .map(|(node, deps)| (node.as_str(), deps.len()))
        .collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (node, deps) in dependencies {
        for dep in deps {
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(node.as_str());
        }
    }

    let mut ready: BTreeSet<&str> = pending
        .iter()
        .filter_map(|(node, count)| (*count == 0).then_some(*node))
        .collect();
    let mut layers = Vec::new();
    let mut emitted = 0usize;

    while !ready.is_empty() {
        // An empty layer admits anything, so the first ready node always
        // joins and every pass makes progress.
        let mut layer: Vec<&str> = Vec::new();
        let mut state = LayerState::default();

        for candidate in &ready {
            let effects = &resolved_effects[*candidate];
            if state.admits(effects) {
                layer.push(candidate);
                state.absorb(effects);
            }
        }

        for node in &layer {
            ready.remove(*node);
        }
        emitted += layer.len();
        for node in &layer {
            if let Some(children) = dependents.get(node) {
                for child in children {
                    let count = pending.get_mut(child).expect("child of a known node");
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(child);
                    }
                }
            }
        }
        layers.push(layer.into_iter().map(str::to_owned).collect());
    }

    if emitted != dependencies.len() {
        let missing = pending
            .iter()
            .find(|(_, count)| **count > 0)
            .map(|(node, _)| (*node).to_owned())
            .unwrap_or_default();
        return Err(GraphError::Cycle(missing));
    }

    Ok(layers)
}
