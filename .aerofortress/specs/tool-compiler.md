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

- Maximum 500 production LOC per Rust file (test code excluded: `#[cfg(test)]`
  items, `tests.rs` files, and `tests/` directories).
- Minimum line coverage: 95% (`cargo run -p xtask -- coverage`).
- `cargo run -p xtask -- check` = loc + fmt + clippy (-D warnings) +
  example/schema validation + release consistency + tests (incl. doc-tests)
  + coverage.
- SDK parity with the Rust planner is enforced by shared fixtures under
  `tests/parity/`.

## Implemented Slice (v0.2)

- IR v2: strict `$ref` shapes with `$literal` escapes, path escaping,
  `deny_unknown_fields`, plan `name`/`description`, `ToolSpec.version`,
  `Node.for_each` (dynamic fan-out) and `Node.when` (conditionals), and
  per-node resource templates (`file:{path}`).
- Graph validation resolves effects per node, treats vacuous declarations as
  unknown, rejects contradictions and self-references, and allows same-tool
  commutative write overlap.
- Optimizer pipeline (dedup/CSE, dead-node elimination, batch grouping split
  by `batch_size`) with cost estimates and grouped, transitively-aware
  explain diagnostics.
- Runtime: dependency-driven scheduler with a resource lock table, per-tool
  and global concurrency limits, retry (backoff + jitter, idempotency-gated)
  and timeout engines, write-aware cache invalidation with single-flight and
  LRU/TTL, partial results (`errors`/`skipped`/`RunStatus`), trace v2
  (timestamps, batch ids, attempts), compact result mode, redaction,
  truncation budgets, cancellation, live event sink, `PreparedPlan`,
  built-in pure data tools, capability input-schema validation, and a
  blocked-tool recursion guard.
- Adapters: fs (symlink containment, caps, transactional `write_files`,
  native batch), shell (conservative `shell:world` default, env hygiene,
  timeouts), http (headers, fixed response contract, reqwest client), mcp
  (demultiplexed sessions, respawn, `isError` mapping, capability
  derivation from annotations, HTTP transport, depth propagation).
- Planner: step-attributed errors, recipes (`fan_out` static/dynamic,
  `map_reduce`, `pipeline`, `params`), and opportunity mining from observed
  call transcripts.
- CLI: library-shaped commands (`validate`, `layers`, `optimize`,
  `explain`, `compile-*`, `run-*`, `bench` with warmup/interleave/stddev,
  `conformance`, `suggest`, `serve-mcp`), stdin plans, result shaping
  flags; the MCP server embeds the schemas, survives malformed input, and
  returns partial results as `isError` tool results.
- Schemas v2 are the single source of truth; the TypeScript SDK generates
  from them and mirrors the Rust planner exactly (shared parity fixtures in
  `tests/parity/` run by both toolchains).
