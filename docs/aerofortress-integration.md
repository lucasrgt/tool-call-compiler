# AeroFortress Integration

AeroFortress should embed the Rust core directly, not call Tool Call Compiler
through an external MCP server by default.

Why:

- The harness already owns MCP routing and session state.
- A compiler-as-MCP wrapper would add one extra model-visible tool hop before it
  fans back into other tools.
- Direct embedding lets the harness call its internal tool dispatcher from a
  native adapter and keep one compact result for the LLM.

## Versioning Strategy

Open source packages remain the product boundary:

1. Publish Rust crates and the TypeScript SDK from this repo.
2. Pin AeroFortress to released crate versions when the crates are public.
3. Use git dependencies only for pre-release dogfood.

This keeps the open source library standalone while letting the harness consume
the same artifact external users get.

## Harness Dogfood Recipes

Good first flows:

- Knowledge fan-out: query/read/store groups that currently appear as several
  consecutive MCP calls.
- CodeGraph fan-out: repeated `search`/`node` lookups collapsed into one recipe.
- Shell/filesystem verification: read many files, run checks, collect compact
  trace.

Name recipes with `RecipePlan.name`. `recipe` is the execution shape; `name` is
the human label the harness should show in transcripts, opportunity reports, and
benchmark tables.

Consecutive manual groups of 3+ compatible tool calls should be surfaced as
compiler opportunities. They are the real-world "before" signal: convert them to
`run_compiled_tool_recipe` when the calls are independent or can be expressed as
a DAG. The detection itself now lives upstream: feed the observed calls to
`tool_compiler_planner::suggest_recipes` (or `tool-compiler suggest calls.json`)
and it returns ready fan-out recipes with the observed inputs as items.

The host adapter should register precise capabilities for each harness tool:
effects, cacheability, batchability, limits, and cost. Plans can then omit most
metadata and let the runtime hydrate it from the registry.
