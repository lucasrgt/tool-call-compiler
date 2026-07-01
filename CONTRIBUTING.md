# Contributing

## Layout

Rust workspace (`crates/`), TypeScript SDK (`sdk/node`), public JSON Schemas
(`schemas/` — the single source of truth; the SDK generates from them),
shared cross-language fixtures (`tests/parity/`), runnable examples
(`examples/`), and the quality-gate runner (`crates/xtask`).

## Quality gates

Everything CI runs, locally:

```powershell
cargo run -p xtask -- check   # loc + fmt + clippy + examples + release-check + tests + coverage
npm test                      # SDK: schema generation, parity fixtures, ajv validation
npm run fmt:check             # prettier over TS/JS/JSON/MD/YAML
```

Individual gates: `xtask loc` (≤500 production lines per Rust file; test
code excluded), `xtask clippy`, `xtask examples` (every example validates
against the schemas), `xtask release-check` (workspace/SDK versions and
CHANGELOG agree), `xtask coverage` (≥95% line coverage).

## Rules that bite

- **Effects are the product.** Any change to how effects gate optimization
  needs a test proving the unsafe case stays blocked.
- **Parity is enforced.** If you change the planner's lowering, update the
  TypeScript compiler in `sdk/node/src/index.ts` and the fixtures in
  `tests/parity/` — both toolchains run the same files.
- **Schemas are the source of truth.** Edit `/schemas/*.json`, never
  `sdk/node/src/schemas.gen.ts` (generated) — and keep the Rust serde types
  in sync (they use `deny_unknown_fields`, so drift fails tests).
- **Partial results are sacred.** Tool failures must land in
  `RunResult.errors`, never as a top-level `Err` that discards outputs.
- Every public item carries rustdoc (`missing_docs` is a warning promoted to
  an error by the clippy gate).

## Commits and releases

Conventional commits (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`).
Releasing: bump the workspace version in `Cargo.toml` and
`sdk/node/package.json`, add the `## <version>` CHANGELOG section
(`xtask release-check` enforces both), tag `vX.Y.Z`, push the tag. The
release workflow gates, builds per-platform binaries, creates the GitHub
Release, and publishes crates + npm (crates in dependency order with
index-lag retries; npm with `--provenance`).
