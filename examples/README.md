# Examples

Run from the repository root:

```powershell
cargo run -p tool-compiler-cli -- run examples/sequential-ref.json
cargo run -p tool-compiler-cli -- explain examples/write-conflict.json
cargo run -p tool-compiler-cli -- bench examples/bench-sleep.json --iterations 3
```

All examples use the CLI's deterministic `local` adapter.

