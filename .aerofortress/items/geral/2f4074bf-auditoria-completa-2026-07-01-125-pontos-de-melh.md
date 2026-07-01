---
id: 2f4074bf-925c-4949-8d12-5d40028d78fe
slug: geral
type: fact
title: Auditoria completa 2026-07-01 — ~125 pontos de melhoria mapeados
tags: auditoria, qualidade, roadmap
provenance: observado
evidence: C:\Users\lucas\dev\tool-call-compiler (crates/*, sdk/, schemas/, .github/), harness: pleiades-harness/src-tauri/src/tool_compiler.rs
decay: seasonal
created: 2026-07-01T20:19:54.215306300+00:00
updated: 2026-07-01T20:19:54.215306300+00:00
validated: 2026-07-01T20:19:54.215306300+00:00
links: 
---

Auditoria de código completa do repo em 2026-07-01 (v0.1.1, HEAD 4e7391e). Os achados críticos, confirmados por trace de código:

1. **Dedup quebra refs forward** (optimizer/lib.rs `deduplicate_nodes`): nó que referencia um nó duplicado que aparece DEPOIS dele no vec fica com ref órfã — o 2º `validate()` falha e o plano válido inteiro erra. Falta passada final de rewrite sobre os nodes mantidos.
2. **Cache fura ordenação de writes no MESMO run** (execution.rs `cache_key`): read cacheable → write no recurso → read igual depois = cache hit com valor pré-write. Chave é só adapter+tool+input; não há invalidação por `writes`.
3. **`Effects` vazio = "seguro"**: `effects: {}` (e o `run_tool()` do shell) tem writes vazio → paraleliza livremente. Ausência de effects bloqueia, mas objeto vazio declara segurança sem saber nada. Dois `shell.run` arbitrários rodam em paralelo por default.
4. **Falha descarta tudo**: RuntimeError::Tool perde RunResult, trace e outputs parciais; drop do JoinSet ABORTA tasks efeitosas em voo. Sem continue-on-error/partial results.
5. **MCP adapter**: notificação JSON-RPC sem id (logging/progress de servidores reais) mata a chamada (read_response exige id); `isError: true` de tools/call é tratado como sucesso; sessão morta nunca respawna.
6. **retry/timeout/commutative/idempotent/cost declarados no IR e NUNCA aplicados** — spec promete, runtime ignora.
7. serve-mcp: JSON malformado derruba o processo; tools/list sem schema real; sem tool de intent.
8. Release: cli/Cargo.toml pinna adapters em 0.1.0 com workspace 0.1.1 (publish quebra); CI sem fmt/clippy/Windows; serve_mcp/bench excluídos de cobertura.
9. `node_outputs` + trace completos SEMPRE no resultado — a promessa de "resultado compacto" não é aplicada no shape.
10. explain() emite diagnósticos falso-positivos para pares transitivamente ordenados (checa só dependência direta).

Lista completa (~125 itens, por área, com file:line) entregue na sessão de 2026-07-01 no workspace dev. Nada foi alterado no repo — auditoria somente-leitura.
