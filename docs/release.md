# Release

Release automation lives in `.github/workflows/release.yml`.

## Dry Run

Run the workflow manually with `publish=false`. It executes the full gate,
packages the dependency-root Rust crate, packs the TypeScript SDK, and uploads
artifacts.

Cargo cannot fully package downstream crates before their internal dependencies
exist on crates.io. The first real publish handles that by publishing crates in
dependency order.

## Publish

Run with `publish=true` only when these secrets exist:

- `CARGO_REGISTRY_TOKEN`
- `NPM_TOKEN`

The publish order is dependency-safe: IR, graph, optimizer, runtime, planner,
adapters, then CLI.

## Version Bump Checklist

1. Update workspace version in `Cargo.toml`.
2. Update `sdk/node/package.json`.
3. Update `CHANGELOG.md`.
4. Create tag `vX.Y.Z`.
5. Run release workflow.
