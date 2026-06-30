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
plan IR -> validation -> optimization -> DAG execution -> result + trace
```

## Moat

Batching and parallel execution are table stakes. The core moat is effect-safe compilation: knowing when a call can be reordered, batched, cached, retried, deduplicated, or must be left alone.

## Workspace

- `crates/tool-compiler-ir`: stable IR types and tool effect declarations.
- `crates/tool-compiler-graph`: graph validation, dependency extraction, safe execution layers.
- `crates/tool-compiler-optimizer`: pure graph transforms.
- `crates/tool-compiler-runtime`: async execution over validated plans.
- `crates/tool-compiler-adapter-*`: adapter packages.
- `sdk/node`: TypeScript builder SDK.
- `crates/xtask`: local quality gates.

## Try It

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-sleep.json --iterations 3
cargo run -p tool-compiler-cli -- serve-mcp
```

The CLI ships with a deterministic `local` adapter for examples:

- `const`: returns `input.value`, after optional `sleep_ms`.
- `echo`: returns the input.
- `write`: returns the input, but its declared effects can block parallelization.
- `fail`: returns a tool error.

`explain` reports optimization output, execution layers, batchable groups, and diagnostics such as missing effects or read/write conflicts.

For a live MCP filesystem smoke benchmark:

```powershell
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --mcp-config examples/mcp-filesystem.config.example.json --iterations 1
```

The MCP config launches `@modelcontextprotocol/server-filesystem` over stdio and
registers it as adapter `mcp.filesystem`.

## MCP Server

`serve-mcp` exposes the compiler itself as one MCP stdio tool:

- `run_compiled_tool_graph`: accepts `{ "plan": <Plan> }`.
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

`tool-compiler-runtime` exports `check_adapter_conformance(...)` for adapter smoke
tests: echo round-trip, batch contract, and tool error propagation.

## Quality Gates

```powershell
cargo run -p xtask -- check
npm test
```

The Rust gate enforces a maximum of 500 production LOC per Rust file and 95% minimum line coverage.
