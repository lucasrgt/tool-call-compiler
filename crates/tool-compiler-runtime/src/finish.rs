//! Run finalization: settling leftovers, resolving declared outputs, and
//! shaping the composite result.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::Value;
use tool_compiler_ir::NodeId;

use crate::engine::Engine;
use crate::refs::resolve_value_ref;
use crate::result::{RunResult, RunStatus, SkipReason, shape_output};
use crate::scheduler::resolution_error;
use crate::{ResultMode, RuntimeError};

impl Engine {
    pub(crate) fn finish(mut self, started: Instant) -> Result<RunResult, RuntimeError> {
        // Safety net: account for nodes that never reached any queue.
        let unsettled: Vec<NodeId> = self
            .node_index
            .keys()
            .filter(|node| !self.is_settled(node))
            .cloned()
            .collect();
        for node in unsettled {
            let reason = if self
                .data_deps
                .get(&node)
                .is_some_and(|deps| deps.iter().any(|dep| self.dead.contains(dep)))
            {
                SkipReason::FailedDependency
            } else {
                self.stop.unwrap_or(SkipReason::NotRun)
            };
            self.complete_skip(&node, reason);
        }

        let mut outputs = BTreeMap::new();
        for (name, value_ref) in &self.plan.outputs {
            match resolve_value_ref(value_ref, &self.outputs) {
                Ok(value) => {
                    outputs.insert(name.clone(), value);
                }
                Err(error) => {
                    if !self.dead.contains(value_ref.node()) {
                        self.errors.insert(
                            format!("outputs.{name}"),
                            resolution_error(error).with_code("unresolved_output"),
                        );
                    }
                }
            }
        }

        let redactor = self.config.redactor.clone();
        let max_bytes = self.config.max_output_bytes;
        let outputs = outputs
            .into_iter()
            .map(|(name, value)| {
                let shaped = shape_output(&name, value, redactor.as_ref(), max_bytes);
                (name, shaped)
            })
            .collect();
        let node_outputs: BTreeMap<NodeId, Value> = match self.config.result_mode {
            ResultMode::Compact => BTreeMap::new(),
            ResultMode::Full => self
                .outputs
                .into_iter()
                .map(|(node, value)| {
                    // The engine usually holds the only reference by now, so
                    // this unwraps in place instead of deep-copying.
                    let value = std::sync::Arc::try_unwrap(value)
                        .unwrap_or_else(|shared| (*shared).clone());
                    let shaped = shape_output(&node, value, redactor.as_ref(), max_bytes);
                    (node, shaped)
                })
                .collect(),
        };
        let trace = match self.config.result_mode {
            ResultMode::Compact => Vec::new(),
            ResultMode::Full => self.trace,
        };

        self.metrics.wall_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let status = if self.stop == Some(SkipReason::Cancelled) {
            RunStatus::Cancelled
        } else if self.errors.is_empty() {
            RunStatus::Success
        } else {
            RunStatus::Failed
        };

        Ok(RunResult {
            status,
            outputs,
            node_outputs,
            errors: self.errors,
            skipped: self.skipped,
            trace,
            optimization: self.optimization,
            metrics: self.metrics,
        })
    }
}
