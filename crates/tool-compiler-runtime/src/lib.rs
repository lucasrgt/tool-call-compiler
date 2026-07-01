//! Async runtime for optimized tool-compiler graphs.
//!
//! [`Runtime::run`] takes a [`Plan`], hydrates missing tool metadata from
//! the [`ToolRegistry`], optimizes it, and executes the resulting graph with
//! a dependency-driven scheduler: nodes start the moment their dependencies
//! finish, effect safety is enforced by resource locks, per-tool and global
//! concurrency limits apply, retries and timeouts follow declared policies,
//! and `pure`/`cacheable` outputs are cached with write-aware invalidation.
//!
//! The composite [`RunResult`] is designed for LLM feedback: partial results
//! survive failures (per-node `errors` and `skipped` maps), and
//! [`ResultMode::Compact`] drops internal payloads for token-tight replies.

use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tool_compiler_graph::{ExecutionGraph, GraphError, validate};
use tool_compiler_ir::{NodeId, Plan, ValueRef};
use tool_compiler_optimizer::{OptimizationReport, OptimizedPlan, optimize};

mod builtin;
mod cache;
mod conformance;
mod dispatch;
mod engine;
mod finish;
mod locks;
mod policy;
mod refs;
mod registry;
mod result;
mod scheduler;

pub use builtin::{BUILTIN_TOOLS, BuiltinExecutor};
pub use cache::{CacheKey, MemoryCache, ToolCache};
pub use conformance::{
    ConformanceCheck, ConformanceOptions, ConformanceReport, check_adapter_conformance,
    check_adapter_conformance_with,
};
pub use registry::{BUILTIN_ADAPTER, ToolCapabilities, ToolRegistry};
pub use result::{
    KeyRedactor, Redactor, ResultMode, RunMetrics, RunResult, RunStatus, SkipReason, TraceEvent,
    TraceStatus,
};
pub use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};

use crate::engine::Engine;
use crate::policy::SingleFlight;

/// What to do when a node fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ErrorMode {
    /// Stop scheduling new nodes, drain in-flight work, return partials.
    #[default]
    FailFast,
    /// Keep executing nodes whose dependencies succeeded; only nodes that
    /// (transitively) reference the failed output are skipped.
    Continue,
}

/// Per-run configuration.
#[derive(Clone, Default)]
pub struct RunConfig {
    /// Failure handling mode.
    pub on_error: ErrorMode,
    /// How much detail the result carries.
    pub result_mode: ResultMode,
    /// Whether the tool cache is consulted (defaults to `true` via
    /// [`RunConfig::new`]; `Default::default()` also enables it).
    pub use_cache: bool,
    /// Global ceiling on concurrent dispatches (`None` = unbounded).
    pub max_concurrency: Option<usize>,
    /// Timeout applied to calls whose effects declare none.
    pub default_timeout_ms: Option<u64>,
    /// Per-output byte budget; larger outputs are replaced by a truncation
    /// marker in the result (reference resolution always sees full values).
    pub max_output_bytes: Option<usize>,
    /// Cooperative cancellation: stops scheduling and drains in-flight work
    /// (running calls are never aborted mid-effect).
    pub cancel: Option<CancellationToken>,
    /// Live trace event sink.
    pub events: Option<UnboundedSender<TraceEvent>>,
    /// Redacts node outputs before they leave the runtime.
    pub redactor: Option<Arc<dyn Redactor>>,
}

impl RunConfig {
    /// Default configuration: fail-fast, full result, cache enabled.
    pub fn new() -> Self {
        Self {
            use_cache: true,
            ..Self::default()
        }
    }

    /// Sets the failure handling mode.
    pub fn with_on_error(mut self, mode: ErrorMode) -> Self {
        self.on_error = mode;
        self
    }

    /// Sets the result shaping mode.
    pub fn with_result_mode(mut self, mode: ResultMode) -> Self {
        self.result_mode = mode;
        self
    }

    /// Enables or disables the tool cache for this run.
    pub fn with_cache(mut self, use_cache: bool) -> Self {
        self.use_cache = use_cache;
        self
    }

    /// Sets the global concurrency ceiling.
    pub fn with_max_concurrency(mut self, limit: usize) -> Self {
        self.max_concurrency = Some(limit);
        self
    }

    /// Sets the default timeout for tools that declare none.
    pub fn with_default_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.default_timeout_ms = Some(timeout_ms);
        self
    }

    /// Sets the per-output byte budget.
    pub fn with_max_output_bytes(mut self, max_bytes: usize) -> Self {
        self.max_output_bytes = Some(max_bytes);
        self
    }

    /// Attaches a cancellation token.
    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Attaches a live trace event sink.
    pub fn with_events(mut self, events: UnboundedSender<TraceEvent>) -> Self {
        self.events = Some(events);
        self
    }

    /// Attaches an output redactor.
    pub fn with_redactor(mut self, redactor: impl Redactor + 'static) -> Self {
        self.redactor = Some(Arc::new(redactor));
        self
    }
}

/// The executor of compiled tool graphs.
#[derive(Clone)]
pub struct Runtime {
    registry: ToolRegistry,
    cache: Arc<dyn ToolCache>,
    single_flight: Arc<SingleFlight>,
}

/// A plan compiled once and runnable many times (see [`Runtime::prepare`]).
#[derive(Clone, Debug)]
pub struct PreparedPlan {
    plan: Plan,
    graph: ExecutionGraph,
    report: OptimizationReport,
}

impl PreparedPlan {
    /// The optimized plan.
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The optimization report.
    pub fn report(&self) -> &OptimizationReport {
        &self.report
    }
}

impl Runtime {
    /// Runtime with exactly one adapter registered under `adapter`.
    pub fn single_adapter(
        adapter: impl Into<String>,
        executor: impl ToolExecutor + 'static,
    ) -> Self {
        Self::from_registry(ToolRegistry::new().with_adapter(adapter, executor))
    }

    /// Runtime over a registry with the default in-memory cache.
    pub fn from_registry(registry: ToolRegistry) -> Self {
        Self::with_cache(registry, MemoryCache::default())
    }

    /// Runtime over a registry with a custom cache backend.
    pub fn with_cache(registry: ToolRegistry, cache: impl ToolCache + 'static) -> Self {
        Self {
            registry,
            cache: Arc::new(cache),
            single_flight: Arc::new(SingleFlight::default()),
        }
    }

    /// The adapter/capability registry.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub(crate) fn cache(&self) -> &Arc<dyn ToolCache> {
        &self.cache
    }

    pub(crate) fn single_flight(&self) -> &Arc<SingleFlight> {
        &self.single_flight
    }

    /// Drops every cached tool output.
    pub async fn clear_cache(&self) {
        self.cache.clear().await;
    }

    /// Hydrates and optimizes `plan` once; the result can be executed many
    /// times with [`Runtime::run_prepared`] without re-optimizing.
    pub fn prepare(&self, mut plan: Plan) -> Result<PreparedPlan, RuntimeError> {
        self.registry.apply_capabilities(&mut plan);
        let optimized: OptimizedPlan = optimize(plan)?;
        Ok(PreparedPlan {
            plan: optimized.plan().clone(),
            graph: optimized.graph().clone(),
            report: optimized.report().clone(),
        })
    }

    /// Optimizes and executes `plan` with the default [`RunConfig`].
    pub async fn run(&self, plan: Plan) -> Result<RunResult, RuntimeError> {
        self.run_with(plan, RunConfig::new()).await
    }

    /// Optimizes and executes `plan` with an explicit configuration.
    pub async fn run_with(&self, plan: Plan, config: RunConfig) -> Result<RunResult, RuntimeError> {
        let prepared = self.prepare(plan)?;
        self.run_prepared_with(&prepared, config).await
    }

    /// Executes an already prepared plan with the default configuration.
    pub async fn run_prepared(&self, prepared: &PreparedPlan) -> Result<RunResult, RuntimeError> {
        self.run_prepared_with(prepared, RunConfig::new()).await
    }

    /// Executes an already prepared plan with an explicit configuration.
    pub async fn run_prepared_with(
        &self,
        prepared: &PreparedPlan,
        config: RunConfig,
    ) -> Result<RunResult, RuntimeError> {
        Engine::new(
            self,
            prepared.plan.clone(),
            &prepared.graph,
            prepared.report.clone(),
            config,
            false,
        )?
        .run()
        .await
    }

    /// Validates and executes `plan` with dependency-driven parallelism but
    /// no optimizer passes (no dedup, no elimination, no batching).
    pub async fn run_unoptimized(
        &self,
        mut plan: Plan,
        config: RunConfig,
    ) -> Result<RunResult, RuntimeError> {
        self.registry.apply_capabilities(&mut plan);
        let graph = validate(&plan)?;
        Engine::new(
            self,
            plan,
            &graph,
            OptimizationReport::default(),
            config,
            false,
        )?
        .run()
        .await
    }

    /// Executes `plan` one dispatch at a time in graph order — the serial
    /// baseline used by benchmarks. No optimizer passes apply.
    pub async fn run_serial(&self, plan: Plan) -> Result<RunResult, RuntimeError> {
        self.run_serial_with(plan, RunConfig::new()).await
    }

    /// Serial baseline with an explicit configuration (benchmarks pass
    /// `use_cache: false` so the baseline mirrors a cacheless serial agent).
    pub async fn run_serial_with(
        &self,
        mut plan: Plan,
        config: RunConfig,
    ) -> Result<RunResult, RuntimeError> {
        self.registry.apply_capabilities(&mut plan);
        let graph = validate(&plan)?;
        Engine::new(
            self,
            plan,
            &graph,
            OptimizationReport::default(),
            config,
            true,
        )?
        .run()
        .await
    }
}

/// Infrastructure-level failures.
///
/// Tool-level failures do not surface here: they land inside
/// [`RunResult::errors`] so partial results survive. `RuntimeError` is for
/// problems that make the run impossible or meaningless as a whole.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The plan failed validation.
    #[error(transparent)]
    Graph(#[from] GraphError),
    /// A plan tool names an adapter with no registered executor.
    #[error("no executor registered for adapter '{adapter}'")]
    MissingAdapter {
        /// The missing adapter name.
        adapter: String,
    },
    /// A registered input schema is itself invalid.
    #[error("input schema for tool '{tool}' is invalid: {message}")]
    InvalidInputSchema {
        /// Tool whose schema failed to compile.
        tool: String,
        /// Compiler error detail.
        message: String,
    },
    /// A reference names a node with no recorded output.
    #[error("node '{0}' has no output yet")]
    MissingNodeOutput(NodeId),
    /// A reference path does not exist in the referenced output.
    #[error("reference '{reference}' missing path segment '{segment}'")]
    MissingPath {
        /// The reference being resolved.
        reference: ValueRef,
        /// The missing segment.
        segment: String,
    },
    /// A reference string failed to parse at runtime.
    #[error("invalid reference: {0}")]
    InvalidRef(String),
}

/// Convenience: resolves one reference against a set of node outputs.
/// Exposed for hosts that post-process [`RunResult::node_outputs`].
pub fn resolve_output_ref(
    value_ref: &ValueRef,
    node_outputs: &std::collections::BTreeMap<NodeId, Value>,
) -> Result<Value, RuntimeError> {
    refs::resolve_value_ref(value_ref, node_outputs)
}
