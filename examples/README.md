# Examples

Run from the repository root:

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- compile-intent examples/dogfood-aerofortress-intent.json
cargo run -p tool-compiler-cli -- run-intent examples/dogfood-aerofortress-intent.json
cargo run -p tool-compiler-cli -- compile-recipe examples/recipe-fanout.json
cargo run -p tool-compiler-cli -- run-recipe examples/recipe-fanout.json
cargo run -p tool-compiler-cli -- run-recipe examples/dogfood-aerofortress-knowledge.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-compiled-tool-calls.json --iterations 3
cargo run -p tool-compiler-cli -- run examples/fs-read.json --runtime-config examples/runtime.config.example.json
cargo run -p tool-compiler-cli -- run examples/shell-rustc.json --runtime-config examples/runtime.config.example.json
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --runtime-config examples/runtime.config.example.json --iterations 1
```

Most examples use the CLI's deterministic `local` adapter. The dogfood intent
example models a real AeroFortress-style investigation/fix/verify flow and
compiles it into the executable plan IR before running.

`recipe-fanout.json` demonstrates a high-level recipe: one compact JSON object
expands to several independent nodes. `dogfood-aerofortress-knowledge.json`
mirrors the harness use case where several knowledge lookups should become one
compiled call from the LLM's perspective.

`runtime.config.example.json` registers local fs/shell adapters and the live MCP
filesystem adapter plus a tool capability catalog. `mcp-filesystem-bench.json` launches
`@modelcontextprotocol/server-filesystem` through stdio, then reads `README.md`
and `CONTEXT.md` as one compiled graph.
