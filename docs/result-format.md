# RunResult Reference

Every execution — CLI `run`, `Runtime::run*`, or the MCP server tools —
returns the same composite `RunResult`.

```json
{
  "status": "failed",
  "outputs": { "summary": { "...": "resolved declared outputs" } },
  "node_outputs": { "fetch_1": { "...": "raw output of every finished node" } },
  "errors": {
    "fetch_2": {
      "message": "timed out after 5000ms",
      "code": "timeout",
      "retryable": true
    }
  },
  "skipped": { "summarize": "failed_dependency" },
  "trace": [
    { "node": "fetch_1", "tool": "fetch", "status": "started", "at_ms": 1751400000123 },
    {
      "node": "fetch_1",
      "tool": "fetch",
      "status": "finished",
      "at_ms": 1751400000180,
      "duration_ms": 57,
      "batch_id": 3
    }
  ],
  "optimization": { "passes": ["dedup", "dce", "batch"], "...": "see below" },
  "metrics": {
    "wall_ms": 213,
    "nodes_total": 3,
    "nodes_succeeded": 1,
    "nodes_failed": 1,
    "nodes_skipped": 1,
    "cache_hits": 0,
    "batch_dispatches": 1,
    "retries": 2
  }
}
```

## Failure semantics

A tool-level failure does **not** discard the run:

- `status` — `success` | `failed` | `cancelled`.
- `errors` — one entry per failed node: `message`, optional machine `code`
  (`timeout`, `blocked_tool`, `invalid_input`, `panic`, adapter codes...),
  optional `retryable` verdict. Keys of the form `outputs.<name>` mark
  declared outputs that could not be resolved.
- `skipped` — nodes that never ran, with why: `condition` (its `when` was
  false), `failed_dependency` (something it references failed/skipped),
  `cancelled`, `not_run` (fail-fast stopped scheduling).

Only infrastructure problems (invalid plan, unknown adapter, broken input
schema) surface as a top-level error instead of a `RunResult`.

## Trace events

One `started` + one terminal event (`finished` | `failed` | `cache_hit` |
`skipped`) per node, plus `retried` events per retry attempt.

- `at_ms` — epoch milliseconds; reconstructs the real timeline.
- `duration_ms` — wall time of the call. Events sharing a `batch_id` were
  one adapter dispatch: their durations are the wall time of the _whole_
  batch, so they overlap rather than add.
- `attempt` — retry ordinal on `retried` events.
- `error` — message on `failed`/`retried` events.

## Result shaping

- `ResultMode::Compact` (CLI `--result-mode compact`, MCP `result_mode`)
  drops `node_outputs` and `trace` entirely: outputs, errors, skips,
  metrics, and the optimization report survive. This is the token-tight
  shape meant for LLM feedback.
- `max_output_bytes` replaces oversized outputs with
  `{"$truncated": true, "bytes": n, "preview": "..."}`.
- A configured `Redactor` (e.g. `KeyRedactor::default_secret_keys()`) masks
  sensitive fields in every exposed output. Reference resolution always sees
  raw values; only the _returned_ payload is redacted.

## Optimization report

- `passes` — pipeline that ran, in order (`dedup`, `dce`, `batch`).
- `deduplicated` — removed duplicates and their canonical survivors.
- `eliminated` — pure nodes pruned because nothing depended on them.
- `batch_groups` — exactly the batch dispatches the runtime performs
  (already split by `batch_size`).
- `summary` — model-visible calls and LLM turns before/after, plus
  `estimated_serial_ms` / `estimated_compiled_ms` /
  `estimated_tokens_before` / `estimated_tokens_after` when tools declare
  `cost` metadata. `estimated_llm_turns_before` assumes the serial-agent
  baseline of one tool call per feedback turn.

## Metrics

`wall_ms` (whole run), node counters (`nodes_total/succeeded/failed/
skipped`), `cache_hits`, `batch_dispatches` (adapter `call_batch` calls,
including `for_each` chunks), and `retries` (attempts beyond first calls).
