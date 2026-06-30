# Context

## Tool Call

A request from an agent runtime to an external capability, such as an MCP tool, REST endpoint, shell command, browser action, database operation, or SDK function.

Avoid: using "tool" to mean only MCP.

## Plan

An explicit declarative description of tool nodes, dependencies, inputs, and requested outputs. A plan is input to the compiler; it is not free-form model text.

Avoid: "chain" when the structure can be a graph.

## Compiled Tool Graph

A validated and optimized plan executed as one composite tool call from the LLM's perspective. Internally it may run many operations; externally it returns one compact result/trace for the next LLM turn.

Avoid: saying this eliminates the LLM.

## Effect

A declared semantic property of a tool call that determines whether it is safe to reorder, retry, cache, deduplicate, batch, or run in parallel.

Avoid: assuming calls are safe because they look read-only.

## Adapter

A boundary package that knows how to execute a tool call against one external surface while exposing that surface through the common compiler contract.

Avoid: putting adapter-specific behavior in the IR crate.

## Runtime

The deterministic executor that schedules validated graph layers, resolves references, calls adapters, and records traces.

Avoid: using "runtime" for the LLM planner.

## Batch Group

A same-layer set of compatible nodes that the optimizer selects for one adapter `call_batch` dispatch. It is compiler-selected from declared effects, not a user-written list of unrelated operations.

Avoid: using "batch group" for arbitrary manual MCP aggregation.

## Conformance Suite

A small adapter verification flow that proves the common runtime contract: echo round-trip, batch dispatch shape, and tool error propagation.

Avoid: treating conformance as full integration coverage for a downstream service.

## Tool Limit

A scheduler constraint attached to a tool spec, such as batch size or maximum concurrency. Limits constrain how much execution may happen at once; effects decide whether the execution is legal at all.

Avoid: using limits to describe side-effect safety.

## Tool Cache

The runtime cache boundary used for pure or cacheable tool outputs. It is an adapter-independent contract, with memory as the default backend and external stores as replaceable implementations.

Avoid: treating cache as an optimizer pass.
