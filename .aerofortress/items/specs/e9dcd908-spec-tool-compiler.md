---
id: e9dcd908-f75c-4fa7-9ae6-07786ca1a4b7
slug: specs
type: doc
title: Spec — tool-compiler
tags: tool-call-compiler, spec
provenance: observado
evidence: C:\Users\lucas\dev\tool-call-compiler\.aerofortress\specs\tool-compiler.md
decay: stable
created: 2026-06-30T18:03:36.098827400+00:00
updated: 2026-06-30T19:42:40.612178800+00:00
validated: 2026-06-30T19:42:40.612178800+00:00
links: 
---

Spec viva do tool-call-compiler: compiler agnóstico que transforma múltiplas tool calls dependentes em um único feedback composto para a LLM. A versão atual registra IR de intenção, IR de plano, planner crate, optimizer com summary/fusion groups, runtime com capabilities/cache/limits, adapters HTTP/fs/shell/MCP, schemas públicos, SDK TypeScript, dogfood intent e gates de 500 LOC produtivas + cobertura mínima de 95%.
