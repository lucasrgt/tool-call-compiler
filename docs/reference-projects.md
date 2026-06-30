# Reference Projects

These projects were pulled locally with `opensrc` as implementation references.
They are inspiration and contrast points, not source material to copy.

## MCP BatchIt

- Source: https://github.com/ryanjoachim/mcp-batchit
- Local cache: `C:\Users\lucas\.opensrc\repos\github.com\ryanjoachim\mcp-batchit\main`
- Shape: one MCP `batch_execute` tool that aggregates operations for one downstream MCP server.
- Useful lesson: connection caching, `maxConcurrent`, stop-on-error, per-operation timeout, and a beginner-friendly request shape.
- Boundary for us: it explicitly cannot pass data between sub-operations in one batch and is MCP-specific. `tool-call-compiler` should support dependencies, refs, effects, and multiple adapter families.

## LLMCompiler

- Source: https://github.com/SqueezeAILab/LLMCompiler
- Local cache: `C:\Users\lucas\.opensrc\repos\github.com\SqueezeAILab\LLMCompiler\main`
- Shape: planner, task fetching unit, and executor for parallel function calling.
- Useful lesson: task streaming and dependency-aware execution are central to speed, not just batching.
- Boundary for us: it is a research/framework implementation around LLM planning. `tool-call-compiler` should keep the deterministic compiler/runtime as the product surface, with LLM planning as one input path.

## Positioning

The moat is not "batch MCP calls." It is an effect-safe compiler/runtime:

- input can be intent or executable plan;
- tools declare effects, limits, cost, and capabilities;
- optimizer proves dedupe, fusion, cache, and parallel execution are legal;
- runtime executes across adapters and returns one composite feedback object.
