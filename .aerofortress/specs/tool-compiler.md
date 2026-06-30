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
-> typed IR
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

The optimizer must refuse transformations when effects are missing or ambiguous.

## Quality Gates

- Maximum 500 production LOC per Rust file.
- Lines inside `#[cfg(test)] mod tests` do not count as production LOC.
- Minimum line coverage: 95%.
- Coverage command: `cargo llvm-cov --workspace --lib --tests --fail-under-lines 95`.
