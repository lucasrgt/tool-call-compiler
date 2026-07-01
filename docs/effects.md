# Declaring Effects

Effects are the compiler's license to optimize. Every transformation —
parallelization, batching, caching, deduplication, retry — happens only when
the declared effects prove it safe. This guide covers how to declare them
well.

## The declaration

```json
{
  "adapter": "fs.repo",
  "effects": {
    "pure": false,
    "reads": ["file:{path}"],
    "writes": [],
    "idempotent": true,
    "cacheable": true,
    "batchable": true,
    "commutative": false,
    "timeout_ms": 5000,
    "retry": { "max_attempts": 3, "backoff_ms": 100 }
  }
}
```

| Field              | Grants the compiler permission to                         |
| ------------------ | --------------------------------------------------------- |
| `pure`             | treat the call as data: parallelize freely, dedupe, cache |
| `reads` / `writes` | order calls only when their resources actually conflict   |
| `idempotent`       | retry on failure without duplicating external effects     |
| `cacheable`        | memoize the output for identical inputs                   |
| `batchable`        | join same-tool calls into one `call_batch` dispatch       |
| `commutative`      | overlap same-tool writes to the same resource             |
| `timeout_ms`       | kill calls that exceed the deadline                       |
| `retry`            | re-attempt retryable failures with exponential backoff    |

## What blocks optimization

- **Missing effects** (`"effects"` absent): the node runs, but alone — never
  in parallel with anything.
- **Vacuous effects** (`"effects": {}` or only scheduler-ish flags): treated
  exactly like missing effects. An empty object declares _nothing_ about
  side effects, so it grants nothing. To say "this reads and writes nothing",
  say `"pure": true`.
- **Contradictions**: `pure: true` together with non-empty `writes` is a
  validation error.

## Naming resources

Resources are abstract strings; the compiler only intersects them. Pick a
granularity that matches reality:

- Too coarse (`"db"`) serializes calls that could overlap.
- Too fine (or wrong) lets genuinely conflicting calls race — that is a
  soundness bug in _your_ declaration, not something the compiler can catch.

Conventions that work well:

- Prefix with the surface: `file:...`, `api:...`, `db:...`, `mcp:<server>`.
- One resource per independent entity: `api:user`, `api:orders`.
- Writers declare **every** resource they might touch.

## Resource templates (per-node granularity)

A resource may contain `{placeholders}` resolved from each node's input:

```json
"reads":  ["file:{path}"],
"writes": ["file:{path}"]
```

Two `write_file` nodes with different `path` inputs get different resources
and may run concurrently; two nodes with the same path conflict and
serialize. Placeholders resolve dotted paths (`{user.id}`) into scalar input
fields. When a placeholder cannot be resolved — the field is missing,
non-scalar, or a `$ref` only known at runtime — the template stays
unresolved, so every such node shares one conservative resource and
serializes. Unresolvable is always safe, never fast.

## Cache coherence

Cached outputs are invalidated by writes: when a node whose effects write
resource `R` completes, every cached entry whose declared `reads` intersect
`R` is dropped — in the same run and across runs sharing the cache. This
only works if readers declare their reads honestly; a "cacheable" tool with
empty `reads` is promising its output never changes.

`ToolSpec.version` (or the registry capability version) is part of the cache
key: bump it when the tool's behavior changes.

## Defaults shipped by the adapters

- `fs::read_file_tool` — batchable read of the resources you name
  (use `file:{path}` for per-file granularity).
- `fs::write_file_tool` — idempotent write (full-content overwrite).
- `shell::run_tool` — writes `shell:world`: arbitrary commands serialize
  with each other by default. Declare tighter effects yourself when you know
  a command is safe; the default refuses to guess.
- `http::read_endpoint` / `write_endpoint` / `idempotent_write_endpoint` /
  `pure_endpoint` — the REST verbs' semantics.
- `mcp::derive_capabilities` — maps MCP tool annotations (`readOnlyHint`,
  `idempotentHint`) to effects scoped to `mcp:<server>`.
