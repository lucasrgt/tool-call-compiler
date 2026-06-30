---
id: b0af06dd-4a0c-4923-a38d-75fa0621119e
slug: geral
type: scar
title: Dogfood compiler before manual fanout
tags: dogfood, tool-calls, compiler, workflow
provenance: dito
evidence: User correction on 2026-06-30: after seeing `7 tool calls / 2 explore / 5 shell`, asked whether this is exactly the kind of thing we should compile.
decay: stable
created: 2026-06-30T22:37:49.890525400+00:00
updated: 2026-06-30T22:37:49.890525400+00:00
validated: 2026-06-30T22:37:49.890525400+00:00
links: 
---

When working on `tool-call-compiler` or integrating it into AeroFortress, do not default to direct fanout with multiple independent `shell`, `knowledge`, or plugin calls. If the steps are known up front and safe, first try a compiled recipe: native/plugin MCP via `run_compiled_tool_recipe`, or shell/filesystem through the tool-compiler CLI/adapter. `apply_patch` remains explicit because edits must be reviewable.
