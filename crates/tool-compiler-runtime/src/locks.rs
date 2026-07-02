//! Synchronous resource-lock accounting for the scheduler.
//!
//! The scheduler loop owns this table exclusively, so no async
//! synchronization is needed: units try to acquire before being spawned and
//! release when their task completes. Lock modes mirror the effect rules in
//! `tool-compiler-graph`: reads share, writes exclude, and same-tool
//! commutative writes overlap each other.

use std::collections::{BTreeSet, HashMap};

use tool_compiler_graph::ResolvedEffects;
use tool_compiler_ir::Resource;

#[derive(Debug, Default)]
pub(crate) struct LockTable {
    resources: HashMap<Resource, LockState>,
    /// Number of running units; used by undeclared-effects units, which
    /// require global exclusivity.
    running: usize,
    /// Whether the currently running unit demands global exclusivity.
    exclusive: bool,
}

#[derive(Debug)]
enum LockState {
    Read(usize),
    Write,
    Commutative { tool: String, count: usize },
}

/// The lock demand of one schedulable unit, derived from resolved effects.
#[derive(Clone, Debug, Default)]
pub(crate) struct LockRequest {
    reads: BTreeSet<Resource>,
    writes: BTreeSet<Resource>,
    commutative_tool: Option<String>,
    /// Undeclared effects: the unit must run alone.
    world: bool,
}

impl LockRequest {
    pub(crate) fn for_effects(effects: &ResolvedEffects) -> Self {
        if !effects.declared {
            return Self {
                world: true,
                ..Self::default()
            };
        }
        Self {
            reads: effects.reads.clone(),
            writes: effects.writes.clone(),
            commutative_tool: effects.commutative.then(|| effects.tool.clone()),
            world: false,
        }
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.reads.extend(other.reads.iter().cloned());
        self.writes.extend(other.writes.iter().cloned());
        self.world |= other.world;
        if self.commutative_tool != other.commutative_tool {
            // Mixed commutativity in one unit falls back to exclusive writes.
            self.commutative_tool = None;
        }
    }
}

impl LockTable {
    /// Attempts to acquire every resource in `request` atomically.
    pub(crate) fn try_acquire(&mut self, request: &LockRequest) -> bool {
        if self.exclusive {
            return false;
        }
        if request.world {
            if self.running > 0 {
                return false;
            }
            self.exclusive = true;
            self.running = 1;
            return true;
        }

        if !self.is_compatible(request) {
            return false;
        }

        for resource in &request.reads {
            match self.resources.get_mut(resource) {
                Some(LockState::Read(count)) => *count += 1,
                None => {
                    self.resources.insert(resource.clone(), LockState::Read(1));
                }
                Some(_) => unreachable!("compatibility checked above"),
            }
        }
        for resource in &request.writes {
            match (&request.commutative_tool, self.resources.get_mut(resource)) {
                (Some(_), Some(LockState::Commutative { count, .. })) => *count += 1,
                (Some(tool), None) => {
                    self.resources.insert(
                        resource.clone(),
                        LockState::Commutative {
                            tool: tool.clone(),
                            count: 1,
                        },
                    );
                }
                (None, None) => {
                    self.resources.insert(resource.clone(), LockState::Write);
                }
                _ => unreachable!("compatibility checked above"),
            }
        }
        self.running += 1;
        true
    }

    fn is_compatible(&self, request: &LockRequest) -> bool {
        for resource in &request.reads {
            match self.resources.get(resource) {
                None | Some(LockState::Read(_)) => {}
                Some(_) => return false,
            }
        }
        for resource in &request.writes {
            match (self.resources.get(resource), &request.commutative_tool) {
                (None, _) => {}
                (Some(LockState::Commutative { tool, .. }), Some(mine)) if tool == mine => {}
                _ => return false,
            }
        }
        true
    }

    /// Releases a previously acquired request.
    pub(crate) fn release(&mut self, request: &LockRequest) {
        self.running = self.running.saturating_sub(1);
        if request.world {
            self.exclusive = false;
            return;
        }

        for resource in &request.reads {
            if let Some(LockState::Read(count)) = self.resources.get_mut(resource) {
                *count -= 1;
                if *count == 0 {
                    self.resources.remove(resource);
                }
            }
        }
        for resource in &request.writes {
            match self.resources.get_mut(resource) {
                Some(LockState::Commutative { count, .. }) => {
                    *count -= 1;
                    if *count == 0 {
                        self.resources.remove(resource);
                    }
                }
                Some(LockState::Write) => {
                    self.resources.remove(resource);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects(reads: &[&str], writes: &[&str], declared: bool) -> ResolvedEffects {
        ResolvedEffects {
            tool: "t".into(),
            declared,
            pure: false,
            reads: reads.iter().map(|s| s.to_string()).collect(),
            writes: writes.iter().map(|s| s.to_string()).collect(),
            commutative: false,
        }
    }

    #[test]
    fn readers_share_and_writers_exclude() {
        let mut table = LockTable::default();
        let read = LockRequest::for_effects(&effects(&["r"], &[], true));
        let write = LockRequest::for_effects(&effects(&[], &["r"], true));

        assert!(table.try_acquire(&read));
        assert!(table.try_acquire(&read));
        assert!(!table.try_acquire(&write));
        table.release(&read);
        table.release(&read);
        assert!(table.try_acquire(&write));
        assert!(!table.try_acquire(&read));
    }

    #[test]
    fn commutative_same_tool_writers_overlap() {
        let mut table = LockTable::default();
        let mut fx = effects(&[], &["counter"], true);
        fx.commutative = true;
        let request = LockRequest::for_effects(&fx);
        let mut other_tool = fx.clone();
        other_tool.tool = "other".into();
        let other = LockRequest::for_effects(&other_tool);

        assert!(table.try_acquire(&request));
        assert!(table.try_acquire(&request));
        assert!(!table.try_acquire(&other));
        table.release(&request);
        table.release(&request);
        assert!(table.try_acquire(&other));
    }

    #[test]
    fn undeclared_effects_run_alone() {
        let mut table = LockTable::default();
        let declared = LockRequest::for_effects(&effects(&["r"], &[], true));
        let unknown = LockRequest::for_effects(&effects(&[], &[], false));

        assert!(table.try_acquire(&declared));
        assert!(!table.try_acquire(&unknown));
        table.release(&declared);
        assert!(table.try_acquire(&unknown));
        assert!(!table.try_acquire(&declared));
        table.release(&unknown);
        assert!(table.try_acquire(&declared));
    }
}
