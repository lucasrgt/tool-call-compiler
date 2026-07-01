---
id: 6ad863c9-1114-4bae-8a91-0536b8fc3bc7
slug: geral
type: fact
title: Estado do mutirão dos 155 pontos — T1-T15 shipped, T16 em curso (schemas v2 prontos), T17-T19 restantes
tags: progresso, 155-pontos, continuacao
provenance: observado
evidence: git log + git status em C:\Users\lucas\dev\tool-call-compiler (schemas modificados uncommitted)
decay: volatile
created: 2026-07-01T21:27:22.506739+00:00
updated: 2026-07-01T22:01:13.573168400+00:00
validated: 2026-07-01T22:01:13.573168400+00:00
links: 
---

Execução da correção dos 155 pontos (auditoria 2f4074bf). **T1–T15 completos, commitados na main local** (6 commits, 422907e..HEAD), workspace 0.2.0, `cargo test --workspace` verde (~190 testes). Task list da sessão: #16 in_progress, #17-#19 pending.

**T16 EM CURSO — feito:** /schemas/{plan,intent,recipe}.schema.json REESCRITOS para v2 (UNCOMMITTED): auto-contidos, valueRef pattern `^[^.]+\.output(\.[^.]+)*$`, nodeId sem pontos, pure→writes maxItems 0 via if/then, ToolSpec.version, retry.backoff_ms, Node.for_each/when, intent.name, recipe.params + fan_out(items|items_ref anyOf) + map_reduce + pipeline.

**T16 FALTA (SDK sdk/node):** (a) script scripts/generate-schemas.mjs no sdk que lê ../../schemas/*.json e emite src/schemas.gen.ts (export const PLAN_SCHEMA/INTENT_SCHEMA/RECIPE_SCHEMA as const) — build roda gen antes do tsc; (b) index.ts v2: tipos RunResult{status,outputs,node_outputs,errors?,skipped?,trace,optimization,metrics}, TraceEvent{node,tool,status,at_ms,duration_ms?,error?,batch_id?,attempt?}, TraceStatus união plana (started|finished|cache_hit|failed|retried|skipped), RunMetrics, SkipReason, RunStatus, OptimizationReport{passes,deduplicated,eliminated,batch_groups,summary(+estimated_serial_ms etc)} SEM FusedGroup, Diagnostic{kind:"missing_effects"|"resource_conflict",nodes,resource?,message}, ToolExecutionError{message,code?,retryable?}, ConformanceReport+adapter, Plan+name/description, NodeSpec+for_each/when, ToolSpec+version, Recipe=FanOut|MapReduce|Pipeline, RecipePlan+params, IntentPlan+name; compileIntent/compileRecipe PARIDADE com Rust (depends_on omitido se vazio, sorted; collectRefNodes LANÇA em ref malformada e pula $literal; fan_out items_ref→node "{prefix}each" com for_each e template {"$item":""} (ou {input_key:{"$item":""}}); map_reduce/pipeline como recipes.rs; compileRecipeWithParams com $param); builders +name/forEach/when; runner runPlanViaCli (spawn tool-compiler run - via stdin); (c) fixtures /tests/parity/*.json {name,intent|recipe,params?,plan} lidas por teste Rust novo (crates/tool-compiler-planner/tests/parity.rs, CARGO_MANIFEST_DIR/../../tests/parity) E teste TS (dist/../../../tests/parity); (d) packaging: tsconfig.build.json excluindo *.test.ts, dual CJS (segunda build module commonjs → dist/cjs + package.json type commonjs no dir), engines >=18, sideEffects false, ajv devDep p/ validar builders contra schemas, drift test TS lê ../../../schemas e deep-equal com consts.

**T17 (127-141):** clippy no xtask check+CI; fmt no CI; matriz windows; cobertura: ajustar ignore-filename-regex (cli agora tem lib; ignorar só cli/src/main.rs + xtask); ⚠️ LOC gate: consertar `mod tests;` semicolon (hoje skip_braced_item engole o resto do arquivo) + pular arquivos src/tests.rs e tests/ — mcp usa src/tests.rs; xtask examples valida examples/*.json vs schemas (jsonschema já é dep do xtask); MSRV 1.96 job; cargo-deny + deny.toml; doc-tests no xtask test; release.yml: publish em ordem com retry/espera de index (api→ir→graph→optimizer→runtime→planner→adapters→cli), tag também publica + GH Release via softprops, npm --provenance, binários por plataforma (matrix ubuntu/windows/macos); LICENSE-MIT+LICENSE-APACHE (substituir LICENSE único); prettier root p/ TS/JSON/MD + check no CI; xtask release-check (versões Cargo/package.json/CHANGELOG consistentes).

**T18 (91,142-146):** rodar `cargo check --workspace 2>&1 | grep missing` p/ listar docs faltantes (cli/main ok, restam poucos); CONTRIBUTING.md; docs/effects.md (granularidade+templates); docs/result-format.md; atualizar README/CHANGELOG (mover Unreleased→0.2.0)/getting-started/aerofortress-integration/benchmarks.md/.aerofortress/specs/tool-compiler.md p/ v2.

**T19 (152-153+ship):** proptest em runtime (run≡run_serial outputs p/ DAGs aleatórios de tools puras/read/write determinísticas), stress 1k fan-out, ValueRef roundtrip proptest, collect_refs no-panic; gate final: cargo fmt, clippy -D warnings, xtask loc, cargo test --workspace, cobertura ≥95% (cargo llvm-cov), npm test, xtask examples; commit+PUSH (auto-push autorizado) + confirmar CI GitHub.

**Gotchas:** node no PATH necessário p/ testes mcp; falha de tool NUNCA é Err (RunResult.errors); bench baseline=run_serial_with(use_cache:false); serve-mcp embute schemas via include_str "../../../schemas/..."; SERVED_TOOLS sempre bloqueados no configured_runtime; runtime.config.example.json segue válido; CHANGELOG ainda com Unreleased.
