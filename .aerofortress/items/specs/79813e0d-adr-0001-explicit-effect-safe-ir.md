---
id: 79813e0d-c7d3-41c2-a272-78057f2dc638
slug: specs
type: decision
title: ADR 0001 — Explicit effect-safe IR
tags: adr, ir, effects, compiler
provenance: observado
evidence: C:\Users\lucas\dev\tool-call-compiler\docs\adr\0001-explicit-effect-safe-ir.md
decay: stable
created: 2026-06-30T18:03:52.605034200+00:00
updated: 2026-06-30T18:03:52.605034200+00:00
validated: 2026-06-30T18:03:52.605034200+00:00
links: 
---

ADR 0001 decide que o v0 aceita um IR JSON explicito e exige efeitos declarados antes de aplicar otimizacoes que mudam ordem, batching, cache, retry ou deduplicacao. A decisao privilegia determinismo no core Rust, diagnosticos de compilador e seguranca contra inferencia de semantica por LLM.
