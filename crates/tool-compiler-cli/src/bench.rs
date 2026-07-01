//! Honest benchmarking: warmup, interleaved sides, distribution stats.

use std::time::Instant;

use serde::Serialize;
use tool_compiler_ir::Plan;
use tool_compiler_runtime::{ResultMode, RunConfig, RunResult, Runtime, TraceStatus};

use crate::CliError;

/// Full benchmark output.
#[derive(Serialize)]
pub struct BenchResult {
    /// Measured iterations per side (after warmup).
    pub iterations: u32,
    /// Warmup iterations per side (not measured).
    pub warmup: u32,
    /// Time spent hydrating + optimizing the plan once, in milliseconds.
    pub compile_ms: u64,
    /// Serial baseline stats (no cache, no optimizer passes).
    pub baseline: BenchSide,
    /// Compiled execution stats.
    pub compiled: BenchSide,
    /// Difference of the mean wall times (positive = compiled is faster).
    pub saved_ms: i64,
    /// Baseline mean divided by compiled mean.
    pub speedup: f64,
}

/// Stats for one side of the comparison.
#[derive(Default, Serialize)]
pub struct BenchSide {
    /// Wall time of every measured iteration, in order.
    pub runs_ms: Vec<u64>,
    /// Mean wall time.
    pub mean_ms: f64,
    /// Fastest iteration.
    pub min_ms: u64,
    /// Slowest iteration.
    pub max_ms: u64,
    /// Population standard deviation of the iterations.
    pub stddev_ms: f64,
    /// Model-visible tool calls in the last iteration.
    pub estimated_tool_calls: usize,
    /// LLM feedback turns in the last iteration.
    pub estimated_llm_turns: usize,
    /// Trace events in the last iteration.
    pub trace_events: usize,
    /// Cache hits in the last iteration.
    pub cache_hits: usize,
}

impl BenchSide {
    fn finish(mut self, last: Option<&RunResult>, compiled: bool) -> Self {
        if let Some(result) = last {
            let summary = &result.optimization.summary;
            self.estimated_tool_calls = if compiled {
                summary.estimated_tool_calls_after
            } else {
                result.metrics.nodes_succeeded
            };
            self.estimated_llm_turns = if compiled {
                summary.estimated_llm_turns_after
            } else {
                result.metrics.nodes_succeeded
            };
            self.trace_events = result.trace.len();
            self.cache_hits = result
                .trace
                .iter()
                .filter(|event| event.status == TraceStatus::CacheHit)
                .count();
        }

        if !self.runs_ms.is_empty() {
            let count = self.runs_ms.len() as f64;
            self.mean_ms = self.runs_ms.iter().sum::<u64>() as f64 / count;
            self.min_ms = self.runs_ms.iter().copied().min().unwrap_or(0);
            self.max_ms = self.runs_ms.iter().copied().max().unwrap_or(0);
            let variance = self
                .runs_ms
                .iter()
                .map(|ms| {
                    let delta = *ms as f64 - self.mean_ms;
                    delta * delta
                })
                .sum::<f64>()
                / count;
            self.stddev_ms = variance.sqrt();
        }
        self
    }
}

/// Runs the comparison: `warmup` unmeasured iterations per side, then
/// `iterations` measured ones, interleaving baseline and compiled runs so
/// neither side systematically benefits from warmer OS caches. The runtime
/// cache is cleared before every run, and the baseline runs serially with
/// the cache disabled — a cacheless serial agent.
pub async fn bench(
    runtime: &Runtime,
    plan: &Plan,
    iterations: u32,
    warmup: u32,
) -> Result<BenchResult, CliError> {
    let compile_started = Instant::now();
    let prepared = runtime.prepare(plan.clone())?;
    let compile_ms = elapsed_ms(compile_started);

    let baseline_config = || RunConfig::new().with_cache(false);
    let compiled_config = || RunConfig::new().with_result_mode(ResultMode::Full);

    for _ in 0..warmup {
        runtime.clear_cache().await;
        runtime
            .run_serial_with(plan.clone(), baseline_config())
            .await?;
        runtime.clear_cache().await;
        runtime
            .run_prepared_with(&prepared, compiled_config())
            .await?;
    }

    let mut baseline = BenchSide::default();
    let mut compiled = BenchSide::default();
    let mut last_baseline = None;
    let mut last_compiled = None;

    for _ in 0..iterations.max(1) {
        runtime.clear_cache().await;
        let started = Instant::now();
        let result = runtime
            .run_serial_with(plan.clone(), baseline_config())
            .await?;
        baseline.runs_ms.push(elapsed_ms(started));
        last_baseline = Some(result);

        runtime.clear_cache().await;
        let started = Instant::now();
        let result = runtime
            .run_prepared_with(&prepared, compiled_config())
            .await?;
        compiled.runs_ms.push(elapsed_ms(started));
        last_compiled = Some(result);
    }

    let baseline = baseline.finish(last_baseline.as_ref(), false);
    let compiled = compiled.finish(last_compiled.as_ref(), true);
    let saved_ms = (baseline.mean_ms - compiled.mean_ms).round() as i64;
    let speedup = if compiled.mean_ms > 0.0 {
        baseline.mean_ms / compiled.mean_ms
    } else {
        0.0
    };

    Ok(BenchResult {
        iterations: iterations.max(1),
        warmup,
        compile_ms,
        baseline,
        compiled,
        saved_ms,
        speedup,
    })
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tool_compiler_ir::{Effects, Node, ToolSpec};

    use super::*;
    use crate::local::LocalExecutor;

    #[tokio::test]
    async fn bench_reports_distribution_and_compile_time() {
        let mut plan = Plan::new();
        plan.tools.insert(
            "echo".into(),
            ToolSpec::new("local").with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            }),
        );
        for id in ["a", "b", "c"] {
            plan.nodes
                .push(Node::new(id, "echo").with_input(json!({ "id": id })));
        }

        let runtime = Runtime::single_adapter("local", LocalExecutor);
        let result = bench(&runtime, &plan, 2, 1).await.unwrap();

        assert_eq!(result.iterations, 2);
        assert_eq!(result.baseline.runs_ms.len(), 2);
        assert_eq!(result.compiled.runs_ms.len(), 2);
        assert!(result.compiled.estimated_tool_calls <= 1);
        assert_eq!(result.baseline.estimated_llm_turns, 3);
        assert_eq!(result.compiled.estimated_llm_turns, 1);
        assert!(result.baseline.cache_hits == 0);
    }
}
