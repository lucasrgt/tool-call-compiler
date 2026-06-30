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

## Quality Gates

```powershell
cargo run -p xtask -- check
npm test
```

The Rust gate enforces a maximum of 500 production LOC per Rust file and 95% minimum line coverage.
