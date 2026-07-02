---
id: d1bccedb-d3b4-48dc-baca-a4b2742d4e4a
slug: perf
type: fact
title: Pass de otimização algorítmica COMPLETO (2 lotes): compile O(n²)→linear (113–161×) + runtime sem deep-clones; teto restante é Amdahl
tags: performance, benchmark, compilacao, amdahl, otimizacao
provenance: observado
evidence: commits 1744204/4bd921e/9df9e90/96c9a98 (main); bench %TEMP%\tcc-compile-bench; xtask check verde (cobertura 95,29%); harness v1.6.5 commit 5a649fa
decay: stable
created: 2026-07-01T23:42:26.125323100+00:00
updated: 2026-07-02T02:31:39.375356900+00:00
validated: 2026-07-02T02:31:39.375356900+00:00
links: 
---

**Estado final (commits 1744204 + 4bd921e + 9df9e90 + 96c9a98, 2026-07-02).** Catálogo de ~30 achados da varredura de 2026-07-01 TODO aplicado (nenhum rejeitado ficou de fora — o usuário pediu tudo).

**Lote 1 — fase de compile** (números medidos, mediana release): safe_layers com agregados por camada (union de reads + write-claim por recurso) em vez de par-a-par → fan-out 5.000 nós: 890 ms→7,9 ms (113×) puro, 1.158→7,2 ms (161×) com writes; 1.000 nós: ~26–31→~1 ms; planos reais (≤200 nós): ≤0,32 ms. DCE via ExecutionGraph::retain (sem 2ª validação/re-parse); dedup early-return + hash tables; explain com bitsets de ancestrais/descendentes + conflitos por recurso; canonical JSON sem clonar chaves; cost sem clone de ToolCost; registry aninhado sem alocar tupla.

**Lote 2 — runtime**: outputs como Arc<Value> ponta a ponta (cache hit/insert e node_outputs full sem deep-copy; try_unwrap no finish); ready queue como BinaryHeap (pop O(log n), tie-break determinístico por menor índice — antes scan O(n) por pick, O(k²) por rodada); validators jsonschema memoizados no ToolRegistry por schema canônico (compartilhado entre clones, semântica intacta); retry move o input na última tentativa possível (0 clones no caso comum de 1 tentativa) e batch/fan-out movem inputs com mem::take (keys de cache pré-computadas); members como Arc<[NodeId]>; in_flight e LockTable em HashMap; SingleFlight com refcount+cleanup (fix de crescimento ilimitado); MemoryCache com índice LRU (eviction O(log n)).

Tudo semânticamente idêntico: suite completa + property run≡run_serial + paridade SDK + cobertura ≥95% verdes em cada lote; CI ubuntu+windows+MSRV+deny verde. Lições de CI: clippy stable do CI é mais novo que o local (filter_map_bool_then pegou o que o local não pegou); o gate LOC ≤500/arquivo aperta — panicked_outcome migrou pro dispatch.rs.

**Consumidor:** a aerofortress-harness pina os 4 crates por git rev no src-tauri/Cargo.toml — otimizações chegam por bump manual do rev (v1.6.5 = rev 9df9e90; API-compatível, zero mudanças de código na harness).

**Continua verdadeiro (Amdahl):** compile de plano real ≈ 0,01–0,1% de um run; multi-thread/GPU/RAM na fase de compile não pagam. Ganhos grandes restantes: cobertura (mais do loop do agente pelo compiler), cache hit rate entre runs, call_batch nativo nos adapters.
