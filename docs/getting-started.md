# Getting Started

Tool Call Compiler gives a host application one composite tool surface while the
runtime executes the internal DAG deterministically.

## Rust Runtime

```rust
use tool_compiler_runtime::{Runtime, ToolRegistry};

let runtime = Runtime::from_registry(
    ToolRegistry::new().with_adapter("local", my_executor)
);
let result = runtime.run(plan).await?;
```

Use this path when the host already owns tool execution. It avoids adding another
MCP hop and lets the host register native adapters, capabilities, cache, and
tracing.

## TypeScript SDK

```ts
import { tc } from "@tool-compiler/sdk-node";

const recipe = tc
  .recipe(tc.fanOut("read_file", ["README.md", "Cargo.toml"], { inputKey: "path" }))
  .tool("read_file", tc.effects.readOnly("fs.repo", ["fs:repo"]))
  .output("readme", tc.valueRef("item_1"))
  .toJSON();
```

The SDK builds stable JSON for plan, intent, and recipe inputs. A host can send
that JSON to a Rust runtime, an MCP server wrapper, or a language-specific
adapter.

## MCP Server Wrapper

```powershell
cargo run -p tool-compiler-cli -- serve-mcp --runtime-config examples/runtime.config.example.json
```

This exposes:

- `run_compiled_tool_graph`
- `run_compiled_tool_recipe`

Use MCP when the host cannot embed Rust directly. If the host can embed the core
runtime, prefer direct integration to avoid turning one composite call into an
extra external tool hop.

## Quality Gates

```powershell
cargo test --workspace
cargo run -p xtask -- loc
cargo run -p xtask -- coverage
npm test
```

The Rust LOC gate counts production lines only; inline tests do not count toward
the 500 LOC ceiling.

