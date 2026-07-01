---
id: 6ad863c9-1114-4bae-8a91-0536b8fc3bc7
slug: geral
type: fact
title: Mutirão dos 155 pontos — CONCLUÍDO (v0.2.0 shipped, 155/155)
tags: progresso, 155-pontos, concluido, v0.2.0
provenance: observado
evidence: git log 422907e..ed90cf8; push para github.com/lucasrgt/tool-call-compiler main; /tmp/cov2.txt (95.67%)
decay: seasonal
created: 2026-07-01T21:27:22.506739+00:00
updated: 2026-07-01T22:35:36.623370100+00:00
validated: 2026-07-01T22:35:36.623370100+00:00
links: 
---

Os 155 pontos da auditoria 2f4074bf foram TODOS resolvidos e shipped em 2026-07-01: 11 commits (422907e..ed90cf8) pushados na main do GitHub, workspace 0.2.0.

**Gate final (tudo verde):** cargo fmt --check ✓; clippy -D warnings = 0 (incl. missing_docs) ✓; LOC ≤500/arquivo ✓; 241 testes Rust + 13 SDK, 0 falhas ✓; cobertura de linhas 95,67% (gate 95) ✓; 14 exemplos validados contra schemas ✓; release-check (versões+CHANGELOG) ✓; prettier ✓; paridade TS↔Rust via 5 fixtures compartilhadas ✓; property test run≡run_serial (64 DAGs aleatórios) ✓. CI do GitHub disparado no push (run 28552219684).

**Arquitetura final:** 12 crates (adapter-api novo), runtime com scheduler dependency-driven (engine/scheduler/finish/dispatch/fanout/locks/policy/cache/registry/builtin/refs/result/conformance), planner com recipes.rs/mining.rs, CLI como lib (config/bench/serve/local), schemas /schemas como fonte única → SDK gera schemas.gen.ts, fixtures /tests/parity, xtask com loc/clippy/examples/release-check, CI matriz ubuntu+windows+MSRV+deny+coverage, release com binários+GH Release+publish com retry, LICENSE-MIT+APACHE, docs/effects.md + docs/result-format.md + CONTRIBUTING.

**Decisões notáveis (para futuras sessões):** falha de tool NUNCA é Err — vive em RunResult.errors com skipped tipado; effects vazios = desconhecidos (bloqueiam paralelismo); shell default escreve shell:world; batch = 1 dispatch no max_concurrency; for_each não-batchable roda sequencial dentro do nó; cache invalida por interseção reads×writes; serve-mcp bloqueia os próprios tool names + guard TOOL_COMPILER_DEPTH (max 2); baseline de bench = serial sem cache; version "0" do IR mantida (mudanças aditivas + deny_unknown_fields).

**Pendência pós-loop (não bloqueante):** integração da pleiades-harness (tool_compiler.rs) consome a API 0.1 via re-exports — compatível na maior parte, mas RunResult mudou de shape (status/errors/metrics novos; fused_groups removido): ao atualizar o pin da harness para 0.2, revisar with_metadata/telemetria e os tools MCP expostos (aproveitar partial results e result_mode compact).
