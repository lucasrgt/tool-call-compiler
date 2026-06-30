# Tool Compiler

Working name: `tool-compiler`.

## Intent

`tool-compiler` is an optimizing compiler/runtime for agent tool calls.

It takes a declared tool plan, validates it as a typed intermediate representation, applies safe optimization passes, executes the resulting graph, and returns compact structured results plus a trace.

The core product is not an MCP batcher. MCP is one adapter. The library should also support REST, shell, browser, database, SDK-hosted functions, and future framework adapters.

## Operational Definition

The product is one composite tool call between two LLM turns:

```text
LLM -> compiled tool graph -> compact result/trace -> LLM feedback
```

It turns tool micro-orchestration like this:

```text
LLM -> tool -> LLM -> tool -> LLM
```

into a single tool call from the LLM's point of view when the segment can be expressed as a safe plan.

The runtime still executes multiple internal operations. The compiler does not remove the LLM; it removes repeated LLM turns that only exist to drive deterministic tool orchestration.

## Product Thesis

Agent systems are slow not only because model inference is slow, but because execution often degenerates into many small, serial, context-heavy tool calls.

The compiler should move work from the LLM loop into deterministic runtime execution:

```text
agent intent or plan
-> intent IR
-> typed plan IR
-> validation
-> optimization passes
-> DAG execution
-> compact result + trace
```

The LLM may produce a plan, but the Rust core must validate and optimize it deterministically.

## v0 Boundary

The recommended v0 input is an explicit JSON IR.

TypeScript should make that JSON ergonomic, but the Rust core owns validation, graph construction, optimization, execution, and tracing.

## Safety Model

Optimization is legal only when tool semantics make it legal.

Every tool capability should declare:

- `pure`: no external side effects.
- `reads`: abstract resources read by the call.
- `writes`: abstract resources written by the call.
- `idempotent`: retry does not duplicate external effects.
- `cacheable`: result can be memoized for a cache key.
- `batchable`: multiple calls of the same operation can be joined.
- `commutative`: order does not matter across compatible operations.
- `timeout`: default execution deadline.
- `retry_policy`: retryable errors and attempt limits.
- `cost`: fixed latency, per-call latency, and token estimates for planning/benchmarking.

Scheduler limits are separate from effects:

- `batch_size`: maximum nodes joined into one adapter batch call.
- `max_concurrency`: conservative ceiling for concurrent execution in a graph layer.

The optimizer must refuse transformations when effects are missing or ambiguous.

## Quality Gates

- Maximum 500 production LOC per Rust file.
- Lines inside `#[cfg(test)] mod tests` do not count as production LOC.
- Minimum line coverage: 95%.
- Coverage command: `cargo llvm-cov --workspace --lib --tests --fail-under-lines 95`.

## Implemented Slice

- Runtime registry maps adapter names to executors.
- CLI supports `validate`, `layers`, `optimize`, `explain`, `compile-intent`, `run-intent`, `run`, `bench`, and `serve-mcp`.
- `run` executes a compiled tool graph and returns JSON outputs, node outputs, trace, and optimization report.
- `bench` compares serial baseline vs compiled execution, clears runtime cache between iterations, and reports elapsed time, estimated tool calls, estimated LLM turns, trace events, cache hits, saved milliseconds, and speedup.
- Planner crate compiles an agent intent IR into executable `Plan` JSON by deriving dependencies from refs plus explicit `after` ordering.
- Optimizer reports deduplicated nodes, batchable groups, fused groups, and summary counts for tool calls and LLM turns before/after compilation.
- Runtime executes optimizer-selected `batch_groups` through the `call_batch` adapter contract.
- Runtime registry can hydrate tool effects, limits, and cost from registered capabilities before optimization.
- Runtime caches `pure`/`cacheable` tool outputs through a pluggable `ToolCache` trait and reports cache hits in traces.
- Trace finish/error/cache events include `duration_ms`.
- Runtime exports an adapter conformance suite covering echo round-trip, batch contract, and tool error propagation.
- Explain diagnostics report why safe parallelization was blocked.
- MCP adapter includes typed config, MCP executor, and persistent stdio JSON-RPC sessions for `initialize` + `tools/call`.
- Filesystem and shell adapters are runnable from CLI runtime config; HTTP adapter has an injectable client executor.
- CLI can load runtime adapters from config and can expose the compiler itself as one MCP stdio tool.
- Examples include a live MCP filesystem benchmark config.
- Public plan JSON Schema lives in `schemas/plan.schema.json`; public intent JSON Schema lives in `schemas/intent.schema.json`; both are mirrored by the TypeScript SDK.
