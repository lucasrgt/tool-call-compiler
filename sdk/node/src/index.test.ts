import assert from "node:assert/strict";
import test from "node:test";

import {
  capabilities,
  compileRecipe,
  compileIntent,
  cost,
  fanOut,
  intent,
  plan,
  PLAN_SCHEMA,
  pure,
  readOnly,
  ref,
  tc,
  valueRef,
  type ConformanceReport,
  type RunResult,
} from "./index.js";

test("builds a stable plan json shape", () => {
  const built = plan()
    .tool("fetchUser", readOnly("http", ["api:user"]))
    .tool("format", pure("local"))
    .node("user", "fetchUser", { id: "u_1" })
    .node("message", "format", { user: ref(valueRef("user", ["id"])) })
    .output("message", valueRef("message"))
    .toJSON();

  assert.equal(built.version, "0");
  assert.deepEqual(built.nodes[1]?.input, {
    user: { $ref: "user.output.id" },
  });
  assert.equal(built.outputs.message, "message.output");
});

test("tc namespace exposes the same builder helpers", () => {
  const built = tc
    .plan()
    .tool("fetch", tc.effects.readOnly("http", ["api:item"]))
    .node("item", "fetch")
    .output("item", tc.valueRef("item"))
    .toJSON();

  assert.equal(built.tools.fetch.adapter, "http");
  assert.equal(built.outputs.item, "item.output");
});

test("tool limits can be layered onto specs", () => {
  const built = plan()
    .tool("fetch", tc.limits(pure("http"), { batch_size: 10, max_concurrency: 2 }))
    .node("a", "fetch")
    .toJSON();

  assert.equal(built.tools.fetch.limits?.batch_size, 10);
  assert.equal(PLAN_SCHEMA.properties.version.const, "0");
});

test("tool cost can be layered onto specs", () => {
  const spec = cost(pure("local"), { fixed_ms: 50, tokens: 80 });

  assert.equal(spec.cost?.fixed_ms, 50);
  assert.equal(spec.cost?.tokens, 80);
});

test("capabilities clone adapter metadata", () => {
  const caps = capabilities({
    effects: { batchable: true, cacheable: true },
    limits: { batch_size: 4 },
  });

  assert.equal(caps.effects?.batchable, true);
  assert.equal(caps.limits?.batch_size, 4);
});

test("intent compiles refs and explicit ordering into plan dependencies", () => {
  const source = intent()
    .tool("echo", pure("local"))
    .step("user", "echo", { id: "u_1" })
    .step("profile", "echo", { user: ref(valueRef("user", ["id"])) }, ["audit"])
    .output("profile", valueRef("profile"))
    .toJSON();

  const compiled = compileIntent(source);

  assert.deepEqual(compiled.nodes[1]?.depends_on, ["audit", "user"]);
  assert.equal(compiled.outputs.profile, "profile.output");
});

test("recipe compiles fan-out into independent plan nodes", () => {
  const source = tc
    .recipe(fanOut("read", ["a.md", "b.md"], { nodePrefix: "file_", inputKey: "path" }))
    .name("read docs")
    .tool("read", pure("fs.repo"))
    .output("first", valueRef("file_1"))
    .toJSON();

  const compiled = compileRecipe(source);

  assert.equal(source.name, "read docs");
  assert.equal(compiled.nodes.length, 2);
  assert.equal(compiled.nodes[0]?.id, "file_1");
  assert.deepEqual(compiled.nodes[0]?.input, { path: "a.md" });
  assert.equal(compiled.outputs.first, "file_1.output");
  assert.equal(tc.RECIPE_SCHEMA.properties.version.const, "0");
});

test("run result type matches composite tool feedback shape", () => {
  const result: RunResult = {
    outputs: { answer: "ok" },
    node_outputs: { step: { answer: "ok" } },
    trace: [{ node: "step", tool: "echo", status: "cache_hit", duration_ms: 0 }],
    optimization: {
      deduplicated: [],
      batch_groups: [],
      fused_groups: [],
      summary: {
        estimated_tool_calls_before: 1,
        estimated_tool_calls_after: 1,
        estimated_llm_turns_before: 1,
        estimated_llm_turns_after: 1,
      },
    },
  };

  assert.equal(result.outputs.answer, "ok");
});

test("conformance report type is exported", () => {
  const report: ConformanceReport = {
    passed: true,
    checks: [{ name: "echo_round_trip", passed: true, message: "passed" }],
  };

  assert.equal(report.checks[0]?.name, "echo_round_trip");
});
