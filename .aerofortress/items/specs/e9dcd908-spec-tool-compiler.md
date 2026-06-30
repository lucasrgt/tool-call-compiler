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
updated: 2026-06-30T18:56:19.513765800+00:00
validated: 2026-06-30T18:56:19.513765800+00:00
links: 
---

Spec do tool-call-compiler: biblioteca standalone open source para compilar/otimizar tool calls de agentes. Define a operação central como uma tool call composta entre dois turnos da LLM: LLM -> compiled tool graph -> compact result/trace -> LLM feedback. Implementação atual inclui IR JSON explícito, modelo de efeitos, Rust core, SDK TypeScript, runtime com ToolRegistry, execução serial baseline, batch executor contract (`call_batch`), cache de ferramentas pure/cacheable, CLI validate/layers/optimize/explain/run/bench/serve-mcp, MCP stdio client/server, conformance suite de adapters, exemplos locais e benchmark MCP filesystem, diagnostics de paralelização e gates de 500 LOC produtivas + 95% cobertura.
