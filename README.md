# Tool Call Compiler

An effect-aware optimizing compiler/runtime for agent tool calls.

The project turns explicit tool plans into validated execution graphs, applies safe optimization passes, executes the graph through adapters, and returns compact structured results plus traces.

Operationally, the product is one composite tool call between two LLM turns:

```text
LLM -> compiled tool graph -> compact result/trace -> LLM feedback
```

From the LLM's perspective this replaces repeated loops like:

```text
LLM -> tool -> LLM -> tool -> LLM
```

Internally, the runtime still executes multiple operations in sequence, parallel, batch, cache, or dedupe. The compiler removes tool micro-orchestration when the segment can be expressed as a safe plan; it does not remove the LLM from decisions that still require model judgment.

## Why

Agent systems often become slow because a model loop drives many tiny, serial, context-heavy tool calls. This project moves safe execution decisions into a deterministic runtime.

```text
intent IR -> plan IR -> validation -> optimization -> DAG execution -> result + trace
```

## Moat

Batching and parallel execution are table stakes. The core moat is effect-safe compilation: knowing when a call can be reordered, batched, cached, retried, deduplicated, or must be left alone.

## Workspace

- `crates/tool-compiler-ir`: stable plan IR types and tool effect declarations.
- `crates/tool-compiler-graph`: graph validation, dependency extraction, safe execution layers.
- `crates/tool-compiler-optimizer`: pure graph transforms.
- `crates/tool-compiler-planner`: agent intent IR compiled into executable plans.
- `crates/tool-compiler-runtime`: async execution over validated plans.
- `crates/tool-compiler-adapter-*`: adapter packages.
- `sdk/node`: TypeScript builder SDK.
- `schemas/plan.schema.json`: public JSON Schema for executable plans.
- `schemas/intent.schema.json`: public JSON Schema for agent intent plans.
- `schemas/recipe.schema.json`: public JSON Schema for high-level recipes.
- `docs/reference-projects.md`: notes from comparable open source projects pulled with `opensrc`.
- `docs/getting-started.md`: embedding, SDK, and MCP wrapper entry points.
- `docs/aerofortress-integration.md`: direct harness integration strategy.
- `docs/benchmarks.md`: benchmark commands and interpretation.
- `crates/xtask`: local quality gates.

## Try It

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- compile-intent examples/dogfood-aerofortress-intent.json
cargo run -p tool-compiler-cli -- run-intent examples/dogfood-aerofortress-intent.json
cargo run -p tool-compiler-cli -- compile-recipe examples/recipe-fanout.json
cargo run -p tool-compiler-cli -- run-recipe examples/recipe-fanout.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-compiled-tool-calls.json --iterations 3
cargo run -p tool-compiler-cli -- serve-mcp
cargo run -p tool-compiler-cli -- run examples/fs-read.json --runtime-config examples/runtime.config.example.json
cargo run -p tool-compiler-cli -- run examples/shell-rustc.json --runtime-config examples/runtime.config.example.json
```

The CLI ships with a deterministic `local` adapter for examples:

- `const`: returns `input.value`, after optional `sleep_ms`.
- `echo`: returns the input.
- `write`: returns the input, but its declared effects can block parallelization.
- `fail`: returns a tool error.

`compile-intent` lowers agent intent into the executable plan IR. `run-intent`
does that compile step and immediately returns the same composite feedback as
`run`.

`compile-recipe` lowers supported high-level recipes into the executable plan IR.
The first recipe is `fan_out`, for cases where one compact intent should become
many independent calls.

`explain` reports optimization output, execution layers, fused batch groups, summary counts, and diagnostics such as missing effects or read/write conflicts.

For a live MCP filesystem smoke benchmark:

```powershell
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --runtime-config examples/runtime.config.example.json --iterations 1
```

The MCP config launches `@modelcontextprotocol/server-filesystem` over stdio and
registers it as adapter `mcp.filesystem`.

## MCP Server

`serve-mcp` exposes the compiler itself as one MCP stdio tool:

- `run_compiled_tool_graph`: accepts `{ "plan": <Plan> }`.
- `run_compiled_tool_recipe`: accepts `{ "recipe": <RecipePlan> }`.
- Returns MCP `structuredContent` containing the same `RunResult` as the CLI.

This is the product loop directly: one model-visible tool call, many internal tool
operations, one compact result back to the model.

## Adapter Contracts

Runtime adapters implement:

- `call(tool, input)` for a single operation.
- `call_batch(tool, inputs)` for optimizer-selected batch groups. The default
  implementation is sequential, so adapters can opt into native batching later.

The runtime also keeps an in-memory cache for `pure`/`cacheable` tools and reports
cache hits in the trace as `cache_hit`.

Cache is pluggable through the `ToolCache` trait; `MemoryCache` is the default.
Scheduler limits live on `ToolSpec.limits` and currently enforce `batch_size`
and a conservative layer-level `max_concurrency` ceiling.
Tool cost metadata lives on `ToolSpec.cost` and is included in the public schema
for planning and benchmark reporting.

`ToolRegistry` can also carry tool capabilities. When a plan only names the
adapter, the runtime can hydrate effects, limits, and cost from the registry
before optimization.

Trace finish/error/cache events include `duration_ms`.

`tool-compiler-runtime` exports `check_adapter_conformance(...)` for adapter smoke
tests: echo round-trip, batch contract, and tool error propagation.

Built-in adapters:

- `tool-compiler-adapter-mcp`: persistent stdio MCP sessions.
- `tool-compiler-adapter-fs`: root-contained filesystem reads/writes/listing.
- `tool-compiler-adapter-shell`: direct process execution via `program` + `args`.
- `tool-compiler-adapter-http`: generic executor over an injected HTTP client.

## Quality Gates

```powershell
cargo run -p xtask -- check
npm test
```

The Rust gate enforces a maximum of 500 production LOC per Rust file and 95% minimum line coverage.
