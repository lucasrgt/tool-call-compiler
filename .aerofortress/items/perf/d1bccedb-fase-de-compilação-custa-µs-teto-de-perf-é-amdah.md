---
id: d1bccedb-d3b4-48dc-baca-a4b2742d4e4a
slug: perf
type: fact
title: Fase de compilação custa µs — teto de perf é Amdahl, não CPU; hotspot conhecido: safe_layers O(n²)
tags: performance, benchmark, compilacao, amdahl
provenance: observado
evidence: crates/tool-compiler-graph/src/lib.rs:316 (safe_layers); crates/tool-compiler-optimizer/src/lib.rs:131 (optimize); bench: %TEMP%\tcc-compile-bench (pure-fanout n=50 → optimize 88 µs; n=5000 → 890 ms)
decay: stable
created: 2026-07-01T23:42:26.125323100+00:00
updated: 2026-07-01T23:42:26.125323100+00:00
validated: 2026-07-01T23:42:26.125323100+00:00
links: 
---

Medição real (2026-07-01, release build, mediana) da fase de compilação isolada (parse JSON + `optimize()` = dedup → validate/layers → DCE → batch-grouping):

- Plano realista (10–50 nós): **6–125 µs**. Plano grande (200 nós): ≤1,1 ms.
- Caso absurdo fan-out 1.000 nós: ~26–31 ms; 5.000 nós: ~0,9–1,2 s — quadrático confirmado.
- Chain e dup-heavy escalam ~linear (5.000 nós: 31 ms / 6 ms).

Implicação (Amdahl): num run real a compilação é ~0,01–0,1% do tempo total (tool calls = 1–100 ms cada; turno LLM poupado = segundos). **Multi-thread, GPU ou "RAM" na compilação não movem a agulha**: já é 100% in-memory; GPU perde só no custo de kernel launch + transfer (~10 µs > compilação inteira); rayon/threads custam mais em fork-join do que o trabalho em planos <200 nós. A execução já é paralela (tokio rt-multi-thread + scheduler dependency-driven).

Hotspot real SE planos de milhares de nós virarem realidade (ex.: fan_out dinâmico de recipe sobre lista grande): `safe_layers` em crates/tool-compiler-graph/src/lib.rs (~linha 316) compara cada candidato contra todos os já aceitos na camada (par-a-par, O(camada²)) + `ready.clone()` por iteração. Remédio é ALGORÍTMICO, não paralelismo: fast-path para camada toda-pure (pular comparações) e/ou índice recurso→nós (conflitos por bucket hash) → ~O(n·k), ganho 100–1000× no caso absurdo, single-thread.

Ganhos "astronômicos" remanescentes estão na COBERTURA e no RUNTIME, não na fase de compile: mais do loop do agente passando pelo compiler (opportunity mining, recipes), cache hit rate entre runs, call_batch nativo nos adapters.

Método: crate temporário com path-deps (fora do repo, gates intactos), 4 shapes (pure-fanout, write-fanout com file:{path} distintos, chain com $ref, dup-heavy) × n ∈ {10, 50, 200, 1000, 5000}.
