---
id: e9dcd908-f75c-4fa7-9ae6-07786ca1a4b7
slug: specs
type: doc
title: Spec — tool-compiler
tags: tool-compiler, spec, rust, typescript, agent-runtime
provenance: observado
evidence: C:\Users\lucas\dev\tool-call-compiler\.aerofortress\specs\tool-compiler.md
decay: seasonal
created: 2026-06-30T18:03:36.098827400+00:00
updated: 2026-06-30T18:23:53.248055+00:00
validated: 2026-06-30T18:23:53.248055+00:00
links: 
---

Spec do tool-call-compiler: biblioteca standalone open source para compilar/otimizar tool calls de agentes. Define a operacao central como uma tool call composta entre dois turnos da LLM: LLM -> compiled tool graph -> compact result/trace -> LLM feedback. Implementacao atual inclui IR JSON explicito, modelo de efeitos, Rust core, SDK TypeScript, runtime com ToolRegistry, execucao serial baseline, CLI validate/layers/optimize/explain/run/bench, exemplos locais, diagnostics de paralelizacao, batch group report, skeleton MCP adapter e gates de 500 LOC produtivas + 95% cobertura.
