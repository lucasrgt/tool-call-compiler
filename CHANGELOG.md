# Changelog

## Unreleased

- Added effect-safe runtime scheduler limits: `batch_size` and conservative layer `max_concurrency`.
- Added trace `duration_ms` on finish/error/cache events.
- Added pluggable cache via `ToolCache`; `MemoryCache` remains the default.
- Added persistent MCP stdio sessions.
- Added runnable filesystem and shell adapters.
- Added injectable HTTP executor contract.
- Added public Plan JSON Schema and TypeScript SDK schema export.
- Added CLI runtime config for fs/shell/MCP adapters.
