# Changelog

## 0.2.0

Breaking, correctness-focused rewrite of the whole pipeline. Highlights, by
layer:

### IR and validation

- Strict `$ref` shapes: non-string values or refs mixed with other keys are
  parse errors; `{"$literal": ...}` escapes ref-shaped data.
- Reference paths escape `.` as `~1` and `~` as `~0`; node ids may not
  contain dots; empty path segments are rejected.
- `deny_unknown_fields` on every IR type: typos in LLM-authored plans fail
  loudly instead of being ignored.
- Plans carry optional `name`/`description`; tools carry an optional
  `version` (part of the cache key); retry policies gain `backoff_ms`.
- Nodes gain `for_each` (runtime fan-out over an upstream array) and `when`
  (conditional execution with transitive skips).
- Resource templates: effect resources such as `file:{path}` resolve
  per-node from the input, giving per-call effect granularity.
- Contradictory effects (`pure` with writes), self-references, and vacuous
  effect declarations (`effects: {}`) are rejected or treated as unknown.
- Same-tool `commutative` writes may share a layer; cycle detection is a
  single iterative pass (no recursion).

### Optimizer

- Deduplication rewrites references after the alias map completes, fixing
  plans that referenced a duplicate declared later in the node list.
- Pure nodes merge across ordering edges (CSE); cacheable-but-impure tools
  keep ordering context in a canonical dedup key.
- Dead-node elimination prunes unused pure nodes when outputs are declared.
- Batch groups are split by `batch_size` at report time; `FusedGroup` was
  removed; the pass pipeline is recorded in `report.passes`.
- Cost model: estimated serial/compiled milliseconds and token totals.
- `explain` groups diagnostics by resource with typed kinds and no longer
  flags transitively ordered pairs.

### Runtime

- Dependency-driven scheduler: nodes start when their dependencies finish;
  effect safety enforced by a resource lock table; per-tool and global
  concurrency limits; cost-ordered dispatch.
- Partial results: tool failures land in `RunResult.errors` with typed
  `skipped` reasons and a `RunStatus`; in-flight work is drained, never
  aborted. `ErrorMode::Continue` keeps independent branches running.
- Retry engine (exponential backoff + jitter, gated on idempotent/pure,
  adapter `retryable` verdicts win) and per-call timeouts with a plan-level
  default.
- Cache v2: `Arc`'d values, LRU + TTL, tool-version keys, write-aware
  invalidation (fixes stale reads after writes), single-flight coalescing.
- Trace v2: epoch timestamps, batch ids, retry attempts, flat status with a
  separate `error` field; run metrics; live event sink; cooperative
  cancellation; `ResultMode::Compact`; `KeyRedactor`; per-output truncation.
- Built-in pure data adapter (`collect`, `pick`, `merge`, `template`);
  capability input schemas validated before dispatch; adapter-scoped
  capabilities; blocked-tool guard; `PreparedPlan` compile-once API.
- `ToolExecutionError` is structured (`message`, `code`, `retryable`) and
  lives in the new `tool-compiler-adapter-api` crate together with the
  executor contract.

### Adapters

- fs: symlink-escape containment, read byte caps, typed/recursive/glob
  `list_dir`, native concurrent batch, transactional `write_files`.
- shell: conservative default effects (`shell:world`), `env_clear` with an
  inherit allowlist, per-call cwd/stdin/timeout with kill, output caps,
  Windows launcher-shim resolution.
- http: request headers, fixed `{status, ok, headers, body}` response
  contract, idempotent write helpers, feature-gated reqwest client.
- mcp: demultiplexed responses (concurrent in-flight requests),
  notifications no longer corrupt calls, `isError` maps to tool errors,
  dead sessions respawn, request timeouts, stderr tails on transport
  errors, protocol version validation, graceful shutdown, `tools/list` +
  `derive_capabilities` (annotations → effects), streamable-HTTP transport,
  `TOOL_COMPILER_DEPTH` propagation.

### Planner and CLI

- Intent errors carry step context; `IntentPlan.name` flows into plans.
- Recipes: dynamic fan-out (`items_ref`), `map_reduce`, `pipeline` with
  `$prev` markers, declared `params` with `{"$param": ...}` placeholders.
- Opportunity mining: `suggest_recipes` turns runs of consecutive same-tool
  calls into ready fan-out recipes (`tool-compiler suggest`).
- CLI logic moved into a testable library; `serve-mcp` survives malformed
  JSON, handles requests concurrently, supports `ping`, embeds the full
  JSON Schemas in `tools/list`, exposes intent/explain tools, returns
  partial results with `isError`, and refuses runaway recursive
  composition.
- `bench` gained warmup, interleaved sides, distribution stats, and a
  compile-time split over a cacheless serial baseline; new `conformance`
  command; `-` reads plans from stdin.

### Schemas, SDK, and tooling

- Self-contained v2 JSON Schemas are the single source of truth; the SDK's
  schema constants are generated from them and drift-tested.
- SDK compilers mirror the Rust planner exactly, enforced by shared golden
  fixtures under `tests/parity/` executed by both toolchains; malformed
  refs now throw; dual ESM/CJS build; `engines >= 18`.
- xtask: clippy gate, string-aware LOC counting (test files excluded),
  example-vs-schema validation, release consistency checks, doc-tests.

## 0.1.1

- Added GitHub CI for Rust gates, coverage, LOC, and TypeScript SDK tests.
- Added release workflow for crate/npm packaging and guarded publishing.
- Added declarative benchmark suite runner with JSON/Markdown reports and optional live MCP cases.
- Added public Recipe JSON Schema, `fan_out` recipe, CLI compile/run commands, and TypeScript SDK recipe builder/compiler.
- Added `run_compiled_tool_recipe` to the MCP server wrapper.
- Added runtime config capability catalog support.
- Added adoption, benchmark, release, and AeroFortress integration docs.
- Corrected repository metadata for the public GitHub repository.
- Added planner crate and `compile-intent`/`run-intent` CLI flow for agent-authored intent plans.
- Added optimizer summary and fusion reporting for estimated tool calls and LLM turns before/after compilation.
- Added registry tool capabilities that hydrate effects, limits, and cost before optimization.
- Added public Intent JSON Schema and TypeScript SDK intent builder/compiler.
- Added benchmark example for compiled batch calls plus richer bench metrics.
- Added effect-safe runtime scheduler limits: `batch_size` and conservative layer `max_concurrency`.
- Added trace `duration_ms` on finish/error/cache events.
- Added pluggable cache via `ToolCache`; `MemoryCache` remains the default.
- Added persistent MCP stdio sessions.
- Added runnable filesystem and shell adapters.
- Added injectable HTTP executor contract.
- Added public Plan JSON Schema and TypeScript SDK schema export.
- Added CLI runtime config for fs/shell/MCP adapters.
