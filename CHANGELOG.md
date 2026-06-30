# Changelog

## Unreleased

- Added GitHub CI for Rust gates, coverage, LOC, and TypeScript SDK tests.
- Added release workflow for crate/npm packaging and guarded publishing.
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
