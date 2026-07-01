//! Behavior tests for the dependency-driven runtime.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_compiler_ir::{Effects, Node, Plan, RetryPolicy, ToolLimits, ToolSpec, ValueRef, When};
use tool_compiler_runtime::{
    BatchInput, BatchOutput, ConformanceOptions, ErrorMode, KeyRedactor, MemoryCache, ResultMode,
    RunConfig, RunStatus, Runtime, SkipReason, ToolCapabilities, ToolExecutionError, ToolExecutor,
    ToolRegistry, TraceStatus, check_adapter_conformance_with,
};

#[derive(Default)]
struct Counters {
    calls: AtomicUsize,
    batches: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
}

struct TestExecutor {
    counters: Arc<Counters>,
    sleep_ms: u64,
    fail_first: AtomicUsize,
}

impl TestExecutor {
    fn new(counters: Arc<Counters>) -> Self {
        Self {
            counters,
            sleep_ms: 0,
            fail_first: AtomicUsize::new(0),
        }
    }

    fn with_sleep(mut self, ms: u64) -> Self {
        self.sleep_ms = ms;
        self
    }

    fn failing_first(self, failures: usize) -> Self {
        self.fail_first.store(failures, Ordering::SeqCst);
        self
    }
}

#[async_trait]
impl ToolExecutor for TestExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        let active = self.counters.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.counters.max_active.fetch_max(active, Ordering::SeqCst);
        self.counters.calls.fetch_add(1, Ordering::SeqCst);
        if self.sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        }
        self.counters.active.fetch_sub(1, Ordering::SeqCst);

        if self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(ToolExecutionError::new("transient").with_code("unavailable"));
        }

        match tool {
            "fail" => Err(ToolExecutionError::new("planned failure")),
            "panic" => panic!("adapter exploded"),
            _ => Ok(input),
        }
    }

    async fn call_batch(
        &self,
        _tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        self.counters.batches.fetch_add(1, Ordering::SeqCst);
        Ok(inputs
            .into_iter()
            .map(|input| BatchOutput {
                node: input.node,
                output: input.input,
            })
            .collect())
    }
}

fn runtime_with(counters: Arc<Counters>) -> Runtime {
    Runtime::single_adapter("test", TestExecutor::new(counters))
}

fn pure_tool() -> ToolSpec {
    ToolSpec::new("test").with_effects(Effects::pure())
}

#[tokio::test]
async fn resolves_dependencies_and_outputs() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("user", "echo").with_input(json!({ "id": "u_1" })));
    plan.nodes
        .push(Node::new("message", "echo").with_input(json!({
            "user_id": { "$ref": "user.output.id" }
        })));
    plan.outputs
        .insert("message".into(), ValueRef::output("message"));

    let result = runtime_with(Default::default()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(result.outputs["message"], json!({ "user_id": "u_1" }));
    assert_eq!(result.metrics.nodes_succeeded, 2);
}

#[tokio::test]
async fn failures_keep_partial_results_and_fail_fast_skips_the_rest() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.tools.insert("fail".into(), pure_tool());
    plan.nodes
        .push(Node::new("ok", "echo").with_input(json!({ "v": 1 })));
    plan.nodes.push(Node::new("bad", "fail"));
    let mut dependent = Node::new("child", "echo").with_input(json!({
        "src": { "$ref": "bad.output" }
    }));
    dependent.depends_on = vec!["bad".into()];
    plan.nodes.push(dependent);

    let result = runtime_with(Default::default()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Failed);
    assert!(result.errors.contains_key("bad"));
    assert_eq!(result.skipped["child"], SkipReason::FailedDependency);
    // The successful sibling's output survived.
    assert_eq!(result.node_outputs["ok"], json!({ "v": 1 }));
}

#[tokio::test]
async fn continue_mode_runs_independent_branches_after_a_failure() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.tools.insert("fail".into(), pure_tool());
    plan.nodes.push(Node::new("bad", "fail"));
    let mut after = Node::new("after", "echo").with_input(json!({ "v": 2 }));
    after.depends_on = vec!["bad".into()]; // ordering only, no data ref
    plan.nodes.push(after);

    let result = runtime_with(counters)
        .run_with(plan, RunConfig::new().with_on_error(ErrorMode::Continue))
        .await
        .unwrap();

    assert_eq!(result.status, RunStatus::Failed);
    // Ordering-only dependents still run in continue mode.
    assert_eq!(result.node_outputs["after"], json!({ "v": 2 }));
}

#[tokio::test]
async fn batch_groups_dispatch_through_call_batch() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "echo".into(),
        ToolSpec::new("test").with_effects(Effects {
            batchable: true,
            ..Effects::pure()
        }),
    );
    plan.nodes
        .push(Node::new("a", "echo").with_input(json!({ "id": "a" })));
    plan.nodes
        .push(Node::new("b", "echo").with_input(json!({ "id": "b" })));

    let result = runtime_with(counters.clone()).run(plan).await.unwrap();

    assert_eq!(counters.calls.load(Ordering::SeqCst), 0);
    assert_eq!(counters.batches.load(Ordering::SeqCst), 1);
    assert_eq!(result.metrics.batch_dispatches, 1);
    assert!(result.trace.iter().any(|event| event.batch_id.is_some()));
}

#[tokio::test]
async fn batch_size_limits_split_dispatches() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "echo".into(),
        ToolSpec::new("test")
            .with_effects(Effects {
                batchable: true,
                ..Effects::pure()
            })
            .with_limits(ToolLimits {
                batch_size: Some(2),
                max_concurrency: None,
            }),
    );
    for id in ["a", "b", "c", "d"] {
        plan.nodes
            .push(Node::new(id, "echo").with_input(json!({ "id": id })));
    }

    runtime_with(counters.clone()).run(plan).await.unwrap();

    assert_eq!(counters.batches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn per_tool_limits_do_not_throttle_other_tools() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "limited".into(),
        ToolSpec::new("test")
            .with_effects(Effects::read_only(["api:limited"]))
            .with_limits(ToolLimits {
                max_concurrency: Some(1),
                batch_size: None,
            }),
    );
    plan.tools.insert(
        "free".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["api:free"])),
    );
    for id in ["l1", "l2"] {
        plan.nodes
            .push(Node::new(id, "limited").with_input(json!({ "id": id })));
    }
    for id in ["f1", "f2"] {
        plan.nodes
            .push(Node::new(id, "free").with_input(json!({ "id": id })));
    }

    let runtime =
        Runtime::single_adapter("test", TestExecutor::new(counters.clone()).with_sleep(40));
    runtime.run(plan).await.unwrap();

    // The two "free" calls plus at most one "limited" call may overlap: with
    // a correct per-tool semaphore we must observe at least 3 concurrent.
    assert!(counters.max_active.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn global_limit_caps_all_dispatches() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "echo".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["api:x"])),
    );
    for index in 0..6 {
        plan.nodes
            .push(Node::new(format!("n{index}"), "echo").with_input(json!({ "i": index })));
    }

    let runtime =
        Runtime::single_adapter("test", TestExecutor::new(counters.clone()).with_sleep(20));
    runtime
        .run_with(plan, RunConfig::new().with_max_concurrency(2))
        .await
        .unwrap();

    assert!(counters.max_active.load(Ordering::SeqCst) <= 2);
}

#[tokio::test]
async fn cache_write_invalidation_forces_reexecution() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "read".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["db:x"])),
    );
    plan.tools.insert(
        "write".into(),
        ToolSpec::new("test").with_effects(Effects {
            writes: ["db:x"].into_iter().map(String::from).collect(),
            idempotent: true,
            ..Effects::default()
        }),
    );
    plan.nodes
        .push(Node::new("read1", "read").with_input(json!({ "q": 1 })));
    let mut write = Node::new("mutate", "write");
    write.depends_on = vec!["read1".into()];
    plan.nodes.push(write);
    let mut read2 = Node::new("read2", "read").with_input(json!({ "q": 1 }));
    read2.depends_on = vec!["mutate".into()];
    plan.nodes.push(read2);

    let result = runtime_with(counters.clone()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    // read1, mutate, read2: the second read must NOT be served from cache.
    assert_eq!(counters.calls.load(Ordering::SeqCst), 3);
    assert_eq!(result.metrics.cache_hits, 0);
}

#[tokio::test]
async fn cache_serves_identical_reads_across_runs() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "read".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["db:x"])),
    );
    plan.nodes
        .push(Node::new("read", "read").with_input(json!({ "q": 1 })));

    let runtime = runtime_with(counters.clone());
    runtime.run(plan.clone()).await.unwrap();
    let second = runtime.run(plan.clone()).await.unwrap();

    assert_eq!(counters.calls.load(Ordering::SeqCst), 1);
    assert_eq!(second.metrics.cache_hits, 1);

    runtime.clear_cache().await;
    runtime.run(plan).await.unwrap();
    assert_eq!(counters.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn when_conditions_skip_nodes_and_data_dependents() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("gate", "echo").with_input(json!({ "open": false })));
    plan.nodes.push(
        Node::new("guarded", "echo")
            .with_input(json!({ "v": 1 }))
            .with_when(When::truthy(ValueRef::new("gate", ["open"]))),
    );
    plan.nodes
        .push(Node::new("consumer", "echo").with_input(json!({
            "from": { "$ref": "guarded.output" }
        })));
    let mut ordered = Node::new("ordered", "echo").with_input(json!({ "v": 2 }));
    ordered.depends_on = vec!["guarded".into()];
    plan.nodes.push(ordered);

    let result = runtime_with(Default::default()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(result.skipped["guarded"], SkipReason::Condition);
    assert_eq!(result.skipped["consumer"], SkipReason::FailedDependency);
    // Ordering-only dependents still run.
    assert_eq!(result.node_outputs["ordered"], json!({ "v": 2 }));
}

#[tokio::test]
async fn for_each_expands_arrays_and_batches() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.tools.insert(
        "read".into(),
        ToolSpec::new("test").with_effects(Effects {
            batchable: true,
            ..Effects::pure()
        }),
    );
    plan.nodes
        .push(Node::new("items", "echo").with_input(json!({
            "files": [{ "path": "a.md" }, { "path": "b.md" }, { "path": "c.md" }]
        })));
    plan.nodes.push(
        Node::new("reads", "read")
            .with_input(json!({ "path": { "$item": "path" } }))
            .with_for_each(ValueRef::new("items", ["files"])),
    );
    plan.outputs
        .insert("reads".into(), ValueRef::output("reads"));

    let result = runtime_with(counters.clone()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(
        result.outputs["reads"],
        json!([{ "path": "a.md" }, { "path": "b.md" }, { "path": "c.md" }])
    );
    assert_eq!(counters.batches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retries_follow_declared_policy() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "flaky".into(),
        ToolSpec::new("test").with_effects(Effects {
            retry: Some(RetryPolicy {
                max_attempts: 3,
                retryable_errors: BTreeSet::new(),
                backoff_ms: Some(1),
            }),
            ..Effects::pure()
        }),
    );
    plan.nodes
        .push(Node::new("call", "flaky").with_input(json!({ "v": 1 })));

    let runtime =
        Runtime::single_adapter("test", TestExecutor::new(counters.clone()).failing_first(2));
    let result = runtime.run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Success);
    assert_eq!(result.metrics.retries, 2);
    assert_eq!(counters.calls.load(Ordering::SeqCst), 3);
    assert!(
        result
            .trace
            .iter()
            .any(|event| event.status == TraceStatus::Retried)
    );
}

#[tokio::test]
async fn timeouts_fail_slow_calls() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    let mut effects = Effects::pure();
    effects.timeout_ms = Some(5);
    plan.tools
        .insert("slow".into(), ToolSpec::new("test").with_effects(effects));
    plan.nodes.push(Node::new("call", "slow"));

    let runtime = Runtime::single_adapter("test", TestExecutor::new(counters).with_sleep(200));
    let result = runtime.run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.errors["call"].code.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn blocked_tools_fail_without_dispatch() {
    let counters: Arc<Counters> = Default::default();
    let registry = ToolRegistry::new()
        .with_adapter("test", TestExecutor::new(counters.clone()))
        .with_blocked_tool("recursive");
    let mut plan = Plan::new();
    plan.tools.insert("recursive".into(), pure_tool());
    plan.nodes.push(Node::new("call", "recursive"));

    let result = Runtime::from_registry(registry).run(plan).await.unwrap();

    assert_eq!(result.errors["call"].code.as_deref(), Some("blocked_tool"));
    assert_eq!(counters.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn input_schemas_gate_dispatch() {
    let counters: Arc<Counters> = Default::default();
    let registry = ToolRegistry::new()
        .with_adapter("test", TestExecutor::new(counters.clone()))
        .with_tool_capabilities(
            "typed",
            ToolCapabilities::new().with_input_schema(json!({
                "type": "object",
                "required": ["path"],
                "properties": { "path": { "type": "string" } }
            })),
        );
    let mut plan = Plan::new();
    plan.tools.insert("typed".into(), pure_tool());
    plan.nodes
        .push(Node::new("call", "typed").with_input(json!({ "path": 42 })));

    let result = Runtime::from_registry(registry).run(plan).await.unwrap();

    assert_eq!(result.errors["call"].code.as_deref(), Some("invalid_input"));
    assert_eq!(counters.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn builtin_tools_are_always_available() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.tools.insert("pick".into(), ToolSpec::new("builtin"));
    plan.nodes
        .push(Node::new("data", "echo").with_input(json!({ "user": { "name": "Ada" } })));
    plan.nodes.push(Node::new("name", "pick").with_input(json!({
        "value": { "$ref": "data.output" },
        "path": "user.name"
    })));
    plan.outputs.insert("name".into(), ValueRef::output("name"));

    let result = runtime_with(Default::default()).run(plan).await.unwrap();

    assert_eq!(result.outputs["name"], json!("Ada"));
}

#[tokio::test]
async fn compact_mode_redaction_and_truncation_shape_the_result() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("secret", "echo").with_input(json!({
            "api_key": "s3cr3t",
            "content": "x"
        })));
    plan.nodes
        .push(Node::new("big", "echo").with_input(json!({ "content": "y".repeat(4096) })));
    plan.outputs
        .insert("secret".into(), ValueRef::output("secret"));
    plan.outputs.insert("big".into(), ValueRef::output("big"));

    let result = runtime_with(Default::default())
        .run_with(
            plan,
            RunConfig::new()
                .with_result_mode(ResultMode::Compact)
                .with_max_output_bytes(256)
                .with_redactor(KeyRedactor::default_secret_keys()),
        )
        .await
        .unwrap();

    assert!(result.node_outputs.is_empty());
    assert!(result.trace.is_empty());
    assert_eq!(result.outputs["secret"]["api_key"], "[redacted]");
    assert_eq!(result.outputs["big"]["$truncated"], true);
}

#[tokio::test]
async fn cancellation_returns_partial_results() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "slow".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["api:x"])),
    );
    plan.nodes.push(Node::new("first", "slow"));
    let mut second = Node::new("second", "slow");
    second.depends_on = vec!["first".into()];
    plan.nodes.push(second);

    let cancel = tokio_util::sync::CancellationToken::new();
    let runtime = Runtime::single_adapter("test", TestExecutor::new(counters).with_sleep(30));
    let handle = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel.cancel();
        })
    };

    let result = runtime
        .run_with(plan, RunConfig::new().with_cancel(cancel))
        .await
        .unwrap();
    handle.await.unwrap();

    assert_eq!(result.status, RunStatus::Cancelled);
    assert_eq!(result.skipped["second"], SkipReason::Cancelled);
    // The in-flight first call was drained, not aborted.
    assert!(result.node_outputs.contains_key("first"));
}

#[tokio::test]
async fn adapter_panics_are_attributed_to_the_node() {
    let mut plan = Plan::new();
    plan.tools.insert("panic".into(), pure_tool());
    plan.nodes.push(Node::new("boom", "panic"));

    let result = runtime_with(Default::default()).run(plan).await.unwrap();

    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.errors["boom"].code.as_deref(), Some("panic"));
}

#[tokio::test]
async fn missing_adapters_fail_before_execution() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), ToolSpec::new("missing"));
    plan.nodes.push(Node::new("call", "echo"));

    let error = Runtime::from_registry(ToolRegistry::new())
        .run(plan)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        tool_compiler_runtime::RuntimeError::MissingAdapter { .. }
    ));
}

#[tokio::test]
async fn event_sink_receives_live_trace() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("call", "echo").with_input(json!({ "v": 1 })));

    runtime_with(Default::default())
        .run_with(plan, RunConfig::new().with_events(sender))
        .await
        .unwrap();

    let mut statuses = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        statuses.push(event.status);
    }
    assert!(statuses.contains(&TraceStatus::Started));
    assert!(statuses.contains(&TraceStatus::Finished));
}

#[tokio::test]
async fn single_flight_coalesces_concurrent_identical_calls() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert(
        "read".into(),
        ToolSpec::new("test").with_effects(Effects::read_only(["db:x"])),
    );
    plan.nodes
        .push(Node::new("read", "read").with_input(json!({ "q": 1 })));

    let runtime =
        Runtime::single_adapter("test", TestExecutor::new(counters.clone()).with_sleep(30));
    let (first, second) = tokio::join!(runtime.run(plan.clone()), runtime.run(plan));
    first.unwrap();
    second.unwrap();

    assert_eq!(counters.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn serial_baseline_neither_dedupes_nor_caches_when_disabled() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("a", "echo").with_input(json!({ "q": 1 })));
    plan.nodes
        .push(Node::new("b", "echo").with_input(json!({ "q": 1 })));

    let result = runtime_with(counters.clone())
        .run_serial_with(plan, RunConfig::new().with_cache(false))
        .await
        .unwrap();

    assert_eq!(counters.calls.load(Ordering::SeqCst), 2);
    assert_eq!(result.metrics.cache_hits, 0);
    assert_eq!(result.node_outputs.len(), 2);
}

#[tokio::test]
async fn prepared_plans_run_many_times_without_recompiling() {
    let counters: Arc<Counters> = Default::default();
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("a", "echo").with_input(json!({ "q": 1 })));

    let runtime = runtime_with(counters);
    let prepared = runtime.prepare(plan).unwrap();
    let first = runtime.run_prepared(&prepared).await.unwrap();
    let second = runtime.run_prepared(&prepared).await.unwrap();

    assert_eq!(first.status, RunStatus::Success);
    assert_eq!(second.metrics.cache_hits, 1);
}

#[tokio::test]
async fn conformance_v2_drives_the_executor_directly() {
    let report = check_adapter_conformance_with(
        "test",
        TestExecutor::new(Default::default()),
        ConformanceOptions::new()
            .with_echo_tool("anything")
            .with_failing_tool("fail"),
    )
    .await;

    assert!(report.passed, "{:?}", report.checks);
    assert_eq!(report.checks.len(), 3);
}

#[tokio::test]
async fn custom_cache_backend_is_used() {
    let mut plan = Plan::new();
    plan.tools.insert("echo".into(), pure_tool());
    plan.nodes
        .push(Node::new("a", "echo").with_input(json!({ "q": 1 })));

    let runtime = Runtime::with_cache(
        ToolRegistry::new().with_adapter("test", TestExecutor::new(Default::default())),
        MemoryCache::with_limits(4, Some(Duration::from_secs(60))),
    );
    runtime.run(plan.clone()).await.unwrap();
    let second = runtime.run(plan).await.unwrap();

    assert_eq!(second.metrics.cache_hits, 1);
}
