# Getting Started

Tool Call Compiler gives a host application one composite tool surface while
the runtime executes the internal DAG deterministically. There are three
entry points, from tightest to loosest integration.

## 1. Embed the Rust runtime

```rust
use tool_compiler_ir::{Effects, ToolLimits};
use tool_compiler_runtime::{
    ErrorMode, KeyRedactor, ResultMode, RunConfig, Runtime, ToolCapabilities, ToolRegistry,
};

// Register adapters and per-tool capabilities once.
let mut registry = ToolRegistry::new().with_adapter("native", my_executor);
registry.register_adapter_tool_capabilities(
    "native",
    "search_docs",
    ToolCapabilities::new()
        .with_effects(Effects {
            batchable: true,
            ..Effects::read_only(["docs:{corpus}"])
        })
        .with_limits(ToolLimits {
            batch_size: Some(16),
            max_concurrency: Some(8),
        })
        .with_version("v1"),
);
let runtime = Runtime::from_registry(registry);

// Compile once, run many times.
let prepared = runtime.prepare(plan)?;
let result = runtime
    .run_prepared_with(
        &prepared,
        RunConfig::new()
            .with_on_error(ErrorMode::Continue)
            .with_result_mode(ResultMode::Compact)
            .with_default_timeout_ms(30_000)
            .with_max_output_bytes(16 * 1024)
            .with_redactor(KeyRedactor::default_secret_keys()),
    )
    .await?;
```

Plans can omit effects/limits/cost/version for tools the registry knows —
the runtime hydrates them before optimizing. Failures return a partial
`RunResult` (see [result-format.md](result-format.md)); declare effects per
[effects.md](effects.md).

## 2. TypeScript SDK

```ts
import { tc } from "@tool-compiler/sdk-node";

const recipe = tc
  .recipe(
    tc.fanOutRef("read_file", tc.valueRef("search", ["hits"]), { inputKey: "path" }),
  )
  .name("read every hit")
  .tool("read_file", tc.effects.readOnly("fs.repo", ["file:{path}"]))
  .toJSON();

const plan = tc.compileRecipe(recipe); // identical output to the Rust planner
```

The SDK builds and compiles plans/intents/recipes with exact Rust parity
(shared golden fixtures under `tests/parity/`), exposes the generated JSON
Schemas, and can execute plans through an installed CLI via
`tc.runPlanViaCli(plan)`.

## 3. MCP server wrapper

```powershell
cargo run -p tool-compiler-cli -- serve-mcp --runtime-config examples/runtime.config.example.json
```

Exposes `run_compiled_tool_graph`, `run_compiled_tool_intent`,
`run_compiled_tool_recipe`, and `explain_tool_plan`. Use MCP when the host
cannot embed Rust directly; prefer direct embedding to avoid an extra
model-visible hop.

The runtime config file binds adapters (`mcp` with optional
`hydrate: true` to derive capabilities from the server's `tools/list`,
`fs`, `shell`, `http`) and registers capabilities, optionally
adapter-scoped. See `examples/runtime.config.example.json`.

## Quality gates

```powershell
cargo run -p xtask -- check
npm test
```
