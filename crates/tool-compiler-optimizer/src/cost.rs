//! Cost estimation from declared [`ToolCost`] metadata.

use std::collections::BTreeMap;

use tool_compiler_graph::ExecutionGraph;
use tool_compiler_ir::{NodeId, Plan, ToolCost};

use crate::{OptimizationReport, OptimizationSummary};

pub(crate) fn summarize(
    original_nodes: &[(NodeId, String)],
    plan: &Plan,
    graph: &ExecutionGraph,
    report: &OptimizationReport,
) -> OptimizationSummary {
    let batched_nodes = report
        .batch_groups
        .iter()
        .map(|group| group.nodes.len())
        .sum::<usize>();
    let estimated_tool_calls_after = plan
        .nodes
        .len()
        .saturating_sub(batched_nodes)
        .saturating_add(report.batch_groups.len());

    let costs = CostModel::new(plan);
    let (estimated_serial_ms, estimated_tokens_before) = if costs.declared {
        let serial = original_nodes
            .iter()
            .map(|(_, tool)| costs.call_ms(tool, 1))
            .sum();
        let tokens = original_nodes
            .iter()
            .map(|(_, tool)| costs.tokens(tool))
            .sum();
        (Some(serial), Some(tokens))
    } else {
        (None, None)
    };
    let (estimated_compiled_ms, estimated_tokens_after) = if costs.declared {
        (
            Some(costs.compiled_ms(plan, graph, report)),
            Some(plan.nodes.iter().map(|node| costs.tokens(&node.tool)).sum()),
        )
    } else {
        (None, None)
    };

    OptimizationSummary {
        estimated_tool_calls_before: original_nodes.len(),
        estimated_tool_calls_after,
        estimated_llm_turns_before: original_nodes.len(),
        estimated_llm_turns_after: usize::from(!original_nodes.is_empty()),
        estimated_serial_ms,
        estimated_compiled_ms,
        estimated_tokens_before,
        estimated_tokens_after,
    }
}

struct CostModel<'a> {
    plan: &'a Plan,
    declared: bool,
}

impl<'a> CostModel<'a> {
    fn new(plan: &'a Plan) -> Self {
        let declared = plan.tools.values().any(|spec| spec.cost.is_some());
        Self { plan, declared }
    }

    fn cost(&self, tool: &str) -> ToolCost {
        self.plan
            .tools
            .get(tool)
            .and_then(|spec| spec.cost.clone())
            .unwrap_or_default()
    }

    /// Cost of one dispatch carrying `calls` calls of `tool`.
    fn call_ms(&self, tool: &str, calls: u64) -> u64 {
        let cost = self.cost(tool);
        cost.fixed_ms.unwrap_or(0) + cost.per_call_ms.unwrap_or(0).saturating_mul(calls)
    }

    fn tokens(&self, tool: &str) -> u64 {
        u64::from(self.cost(tool).tokens.unwrap_or(0))
    }

    /// Layered estimate: each layer costs its slowest dispatch (assumes the
    /// runtime can run a whole layer concurrently), and layers add up.
    fn compiled_ms(&self, plan: &Plan, graph: &ExecutionGraph, report: &OptimizationReport) -> u64 {
        let mut batch_cost: BTreeMap<&str, u64> = BTreeMap::new();
        for group in &report.batch_groups {
            let dispatch = self.call_ms(&group.tool, group.nodes.len() as u64);
            for node in &group.nodes {
                batch_cost.insert(node.as_str(), dispatch);
            }
        }

        let tool_of: BTreeMap<&str, &str> = plan
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.tool.as_str()))
            .collect();

        graph
            .layers()
            .iter()
            .map(|layer| {
                layer
                    .iter()
                    .map(|node| {
                        batch_cost.get(node.as_str()).copied().unwrap_or_else(|| {
                            tool_of
                                .get(node.as_str())
                                .map(|tool| self.call_ms(tool, 1))
                                .unwrap_or(0)
                        })
                    })
                    .max()
                    .unwrap_or(0)
            })
            .sum()
    }
}
