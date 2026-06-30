# Benchmarks

The benchmark CLI compares a serial baseline against the compiled runtime:

```powershell
cargo run -p tool-compiler-cli -- bench examples/bench-compiled-tool-calls.json --iterations 3
```

The reported fields are intentionally host-facing:

- `estimated_tool_calls`: model-visible tool calls after optimization.
- `estimated_llm_turns`: model feedback loops avoided by the composite call.
- `trace_events`: internal execution events for observability.
- `cache_hits`: reused cacheable outputs.
- `speedup`: wall-clock baseline divided by compiled wall-clock.

## Dogfood Targets

Use these before claiming a host integration is fast:

```powershell
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

