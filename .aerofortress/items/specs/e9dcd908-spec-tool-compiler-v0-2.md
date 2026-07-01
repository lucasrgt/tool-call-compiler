---
id: e9dcd908-f75c-4fa7-9ae6-07786ca1a4b7
slug: specs
type: doc
title: Spec — tool-compiler (v0.2)
tags: spec
provenance: observado
evidence: .aerofortress/specs/tool-compiler.md
decay: stable
created: 2026-06-30T18:03:36.098827400+00:00
updated: 2026-07-01T22:18:09.308558800+00:00
validated: 2026-07-01T22:18:09.308558800+00:00
links: 
---

Spec viva do tool-compiler, atualizada para o v0.2 (reescrita dos 155 pontos da auditoria). Intent inalterado: compilador/runtime otimizante de tool calls de agente — uma tool call composta entre dois turnos do LLM. Mudanças estruturais do v0.2 registradas na spec: IR v2 ($literal, templates de recurso file:{path}, for_each/when, deny_unknown_fields), efeitos vazios tratados como desconhecidos, scheduler dependency-driven com lock table (sem barreira de camadas), resultados parciais (errors/skipped/RunStatus), motores de retry/timeout, cache com invalidação por writes, receitas map_reduce/pipeline/params, mining de oportunidades, serve-mcp com schemas embutidos e guard de recursão, SDK com paridade garantida por fixtures compartilhadas (tests/parity). Gates: 500 LOC/arquivo (teste excluído), cobertura ≥95%, clippy -D warnings, validação exemplos×schemas, release-check.
