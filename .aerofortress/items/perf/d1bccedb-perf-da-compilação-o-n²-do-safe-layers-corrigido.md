---
id: d1bccedb-d3b4-48dc-baca-a4b2742d4e4a
slug: perf
type: fact
title: Perf da compilação: O(n²) do safe_layers corrigido (113–161× no caso extremo); teto restante é Amdahl, não CPU
tags: performance, benchmark, compilacao, amdahl, otimizacao
provenance: observado
evidence: commit 1744204 (main); crates/tool-compiler-graph/src/layers.rs; bench %TEMP%\tcc-compile-bench (duas rodadas, mesmo harness); gate xtask check exit 0, cobertura 95,35%
decay: stable
created: 2026-07-01T23:42:26.125323100+00:00
updated: 2026-07-02T00:16:36.253387700+00:00
validated: 2026-07-02T00:16:36.253387700+00:00
links: 
---

**Estado pós-otimização (commit 1744204, 2026-07-01).** A fase de compilação (parse + optimize) custa µs em planos reais e escala ~linear até o caso absurdo. Medição (release, mediana, mesmo harness das duas rodadas):

| shape | n=50 | n=200 | n=1.000 | n=5.000 |
|---|---|---|---|---|
| pure-fanout | 88→54 µs | 965→319 µs | 25,7→1,1 ms | **890→7,9 ms (113×)** |
| write-fanout | 96→45 µs | 1,12 ms→172 µs | 30,9→0,98 ms | **1.158→7,2 ms (161×)** |
| chain | 125→74 µs | 578→299 µs | 3,5→1,7 ms | 31→15 ms |
| dup-heavy | 26→24 µs | 108→90 µs | 787→461 µs | 5,9→3,0 ms |

O que foi feito (mesma semântica — suite completa + property run≡run_serial + paridade SDK + cobertura 95,35% seguraram a reescrita):
- `safe_layers` (graph/src/layers.rs): agregados por camada (união de reads + write-claim por recurso) substituem comparação par-a-par `can_run_with`; contadores de in-degree substituem clone do mapa de deps. Decisões idênticas às do pairwise (documentado no módulo).
- `optimize`: DCE usa `ExecutionGraph::retain` (só membership+layers) em vez de 2ª validação completa; dedup early-return sem duplicatas + HashMap.
- `explain`: bitsets de ancestrais/descendentes + grupos de conflito por recurso (era all-pairs com 3 interseções de BTreeSet por par).
- `PreparedPlan`: plan/graph/data_deps em Arc — prepare sem 3 deep-clones; cada run_prepared sem deep-clone de todos os inputs / re-parse de refs / clone do mapa de effects.
- Runtime: MemoryCache com índice LRU (eviction O(log n), era scan O(n) por insert com cache cheio); SingleFlight com refcount + remoção no último drop (era crescimento ILIMITADO — leak em servidor de longa vida); fanout sem clone dos items; KeyRedactor lowercase 1× por chave; registry lookups sem alocar tupla; canonical JSON sem clonar chaves; segmentos de ref só alocam com escape.

**Continua verdadeiro (Amdahl):** compilação de plano real (≤200 nós) ≤ ~0,3 ms ≈ 0,01–0,1% de um run; multi-thread/GPU/RAM na fase de compile não pagam (GPU: kernel launch+transfer > compilação inteira; já é 100% in-memory; execução já é paralela via tokio+scheduler dependency-driven). Ganhos grandes restantes: COBERTURA (mais do loop do agente pelo compiler — mining, recipes), cache hit rate entre runs, call_batch nativo nos adapters. Não aplicados de propósito (custo/risco > ganho, catalogados): heap no pick_next (mudaria ordem de dispatch em empates), Arc nos node outputs (refactor de API p/ payloads grandes), memo de validators jsonschema no registry (muda contrato prepared×runtime), clone de input por tentativa no retry (API do adapter exige owned).
