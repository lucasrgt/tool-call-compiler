# Examples

Run from the repository root:

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-sleep.json --iterations 3
cargo run -p tool-compiler-cli -- run examples/fs-read.json --runtime-config examples/runtime.config.example.json
cargo run -p tool-compiler-cli -- run examples/shell-rustc.json --runtime-config examples/runtime.config.example.json
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --runtime-config examples/runtime.config.example.json --iterations 1
```

Most examples use the CLI's deterministic `local` adapter.

`runtime.config.example.json` registers local fs/shell adapters and the live MCP
filesystem adapter. `mcp-filesystem-bench.json` launches
`@modelcontextprotocol/server-filesystem` through stdio, then reads `README.md`
and `CONTEXT.md` as one compiled graph.
