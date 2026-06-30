# 0001 Explicit Effect-Safe IR

Context: agent frameworks can produce tool plans in many formats, but optimization is only safe when dependencies and side effects are explicit.

Decision: v0 accepts an explicit JSON IR and requires declared tool effects before applying optimizations that change execution order, batching, caching, retry, or deduplication.

Why: this keeps the Rust core deterministic, makes compiler diagnostics possible, and avoids a planner-first design where the LLM guesses semantics the runtime cannot verify.

