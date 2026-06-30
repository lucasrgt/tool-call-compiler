# Context

## Tool Call

A request from an agent runtime to an external capability, such as an MCP tool, REST endpoint, shell command, browser action, database operation, or SDK function.

Avoid: using "tool" to mean only MCP.

## Plan

An explicit declarative description of tool nodes, dependencies, inputs, and requested outputs. A plan is input to the compiler; it is not free-form model text.

Avoid: "chain" when the structure can be a graph.

## Effect

A declared semantic property of a tool call that determines whether it is safe to reorder, retry, cache, deduplicate, batch, or run in parallel.

Avoid: assuming calls are safe because they look read-only.

## Adapter

A boundary package that knows how to execute a tool call against one external surface while exposing that surface through the common compiler contract.

Avoid: putting adapter-specific behavior in the IR crate.

## Runtime

The deterministic executor that schedules validated graph layers, resolves references, calls adapters, and records traces.

Avoid: using "runtime" for the LLM planner.

