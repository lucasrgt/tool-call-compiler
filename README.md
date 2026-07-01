# Tool Call Compiler

An effect-aware optimizing compiler/runtime for agent tool calls.

The project turns explicit tool plans into validated execution graphs,
applies safe optimization passes, executes the graph through adapters with a
dependency-driven scheduler, and returns one compact structured result plus
a trace.

Operationally, the product is one composite tool call between two LLM turns:

```text
LLM -> compiled tool graph -> compact result/trace -> LLM feedback
```

From the LLM's perspective this replaces repeated loops like:

```text
LLM -> tool -> LLM -> tool -> LLM
```

Internally the runtime still executes many operations — in parallel, in
batches, cached, deduplicated, retried — but the model sees one call and one
compact result. The compiler removes tool micro-orchestration when the
segment can be expressed as a safe plan; it does not remove the LLM from
decisions that require judgment.

## Why

Agent systems become slow because a model loop drives many tiny, serial,
context-heavy tool calls. This project moves the safe execution decisions
into a deterministic runtime:

```text
intent/recipe IR -> plan IR -> validation -> optimization -> DAG execution -> result + trace
```

Batching and parallelism are table stakes. The core is **effect-safe
compilation**: knowing when a call may be reordered, batched, cached,
retried, deduplicated — or must be left alone. Declarations are per-tool
with per-node resource templates (`file:{path}`), missing or vacuous
declarations block optimization instead of permitting it, and failures
return partial results instead of discarding the run. See
[docs/effects.md](docs/effects.md) and
[docs/result-format.md](docs/result-format.md).

## Workspace

- `crates/tool-compiler-ir` — plan IR: nodes, effects, `$ref`/`$literal`,
  resource templates, `for_each`, `when`.
- `crates/tool-compiler-graph` — validation, per-node resolved effects,
  conservative parallel layers.
- `crates/tool-compiler-optimizer` — dedup/CSE, dead-node elimination,
  batch grouping, cost estimates, explain diagnostics.
- `crates/tool-compiler-planner` — intents, recipes (`fan_out`,
  `map_reduce`, `pipeline`, params), opportunity mining.
- `crates/tool-compiler-runtime` — dependency-driven scheduler, resource
  locks, retry/timeout policies, write-aware cache, partial results,
  built-in data tools.
- `crates/tool-compiler-adapter-api` — the executor contract adapters
  implement.
- `crates/tool-compiler-adapter-{fs,shell,http,mcp}` — built-in adapters.
- `crates/tool-compiler-cli` — CLI + the compiler as an MCP server.
- `sdk/node` — TypeScript builder SDK, parity-tested against the Rust
  planner.
- `schemas/` — public JSON Schemas (single source of truth).

## Try it

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- run-intent examples/dogfood-aerofortress-intent.json
cargo run -p tool-compiler-cli -- run-recipe examples/recipe-fanout.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-compiled-tool-calls.json --iterations 3
cargo run -p tool-compiler-cli -- serve-mcp
```

The CLI ships a deterministic `local` adapter for examples (`const`, `echo`,
`write`, `fail`; unknown tools are errors) plus the always-available
`builtin` adapter (`collect`, `pick`, `merge`, `template`) for pure data
reshaping between calls.

Useful flags on the run commands: `--result-mode compact` (token-tight
results), `--on-error continue`, `--timeout-ms`, `--max-output-bytes`,
`--param key=value` (recipes), and `-` to read the plan from stdin.

## MCP server

`serve-mcp` exposes the compiler itself as MCP stdio tools:

- `run_compiled_tool_graph` — execute a Plan.
- `run_compiled_tool_intent` — compile + execute an intent.
- `run_compiled_tool_recipe` — compile + execute a recipe (with `params`).
- `explain_tool_plan` — validate and optimize without executing.

`tools/list` embeds the full JSON Schemas, plan failures come back as tool
results with `isError` and the partial `RunResult` in `structuredContent`,
malformed input never kills the server, and nested compiler composition is
refused beyond depth 2 (`TOOL_COMPILER_DEPTH`).

For a live MCP filesystem benchmark:

```powershell
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --runtime-config examples/runtime.config.example.json --iterations 1
```

## Embedding

```rust
use tool_compiler_runtime::{Runtime, RunConfig, ToolRegistry};

let runtime = Runtime::from_registry(
    ToolRegistry::new().with_adapter("native", my_executor),
);
let prepared = runtime.prepare(plan)?;          // compile once
let result = runtime.run_prepared(&prepared).await?; // run many times
```

`RunConfig` controls failure mode (fail-fast vs continue), result shaping,
caching, global concurrency, default timeouts, output budgets, redaction,
cancellation, and a live trace event sink. See
[docs/getting-started.md](docs/getting-started.md).

## Adapter contract

Adapters implement `call(tool, input)` and optionally a native
`call_batch(tool, inputs)` (`tool-compiler-adapter-api`). The runtime
provides caching (write-aware invalidation, single-flight), retry/timeout
policies, per-tool and global concurrency limits, and conformance checks
(`check_adapter_conformance_with`) that drive the executor directly.

## Quality gates

```powershell
cargo run -p xtask -- check   # loc, fmt, clippy, examples, release-check, tests, coverage (>=95%)
npm test                      # SDK: schema generation, cross-language parity, ajv validation
```

## License

MIT OR Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
