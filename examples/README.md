# Examples

Run from the repository root:

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-sleep.json --iterations 3
cargo run -p tool-compiler-cli -- bench examples/mcp-filesystem-bench.json --mcp-config examples/mcp-filesystem.config.example.json --iterations 1
```

Most examples use the CLI's deterministic `local` adapter.

`mcp-filesystem-bench.json` is the live MCP adapter smoke benchmark. It launches
`@modelcontextprotocol/server-filesystem` through stdio using the companion config
file, then reads `README.md` and `CONTEXT.md` as one compiled graph.
