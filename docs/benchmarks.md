# Benchmarks

The benchmark CLI compares a serial baseline against the compiled runtime:

```powershell
cargo run -p tool-compiler-cli -- bench examples/bench-compiled-tool-calls.json --iterations 3
```

For the full reproducible suite:

```powershell
npm run bench:suite
```

This reads `benchmarks/suite.json`, runs every deterministic case, and writes:

- `reports/benchmarks/summary.json`
- `reports/benchmarks/summary.md`
- one raw JSON file per case
- compiled temporary plans for recipe cases

Live integration cases are opt-in:

```powershell
npm run bench:suite -- --include-optional
```

The optional MCP filesystem case launches a real MCP stdio server and may use
network/package cache via `npx`, so it is excluded from the default suite.

The reported fields are intentionally host-facing:

- `estimated_tool_calls`: model-visible tool calls after optimization.
- `estimated_llm_turns`: model feedback loops avoided by the composite call.
- `trace_events`: internal execution events for observability.
- `cache_hits`: reused cacheable outputs.
- `speedup`: wall-clock baseline divided by compiled wall-clock.

## Dogfood Targets

Use these before claiming a host integration is fast:

```powershell
npm run bench:suite
cargo run -p tool-compiler-cli -- run-recipe examples/dogfood-aerofortress-knowledge.json
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --runtime-config examples/runtime.config.example.json --iterations 1
```

The first models a harness knowledge fan-out without requiring the harness to be
running. The second exercises a real MCP stdio server while still returning one
compiled result to the caller.

## Interpreting Results

Wall-clock speed is only one metric. The product value is strongest when the
compiled run also reduces model-visible calls and LLM feedback turns. A slower
single run may still be a win if it removes multiple LLM round trips on the hot
path.
