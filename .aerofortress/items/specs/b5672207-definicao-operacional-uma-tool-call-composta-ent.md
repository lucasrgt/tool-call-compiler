---
id: b5672207-9a02-4d29-9be9-bfc429cfabd0
slug: specs
type: decision
title: Definicao operacional — uma tool call composta entre turnos da LLM
tags: product-definition, tool-compiler, runtime, llm-loop
provenance: dito
evidence: 
decay: stable
created: 2026-06-30T18:07:57.248082+00:00
updated: 2026-06-30T18:07:57.248082+00:00
validated: 2026-06-30T18:07:57.248082+00:00
links: 
---

Confirmado pelo usuário em 2026-06-30: o objetivo central do tool-call-compiler é transformar sequências de tool calls que hoje exigem turnos repetidos da LLM (LLM -> tool -> LLM -> tool -> LLM) em uma única tool call composta do ponto de vista da LLM. Internamente, o runtime ainda executa várias operações em sequência, paralelo, batch, cache ou dedupe; a LLM recebe depois um resultado/trace compacto para dar o feedback ou raciocínio final. O compilador não elimina a LLM: ele remove micro-orquestração de tools quando o trecho pode ser expresso como plano seguro.
