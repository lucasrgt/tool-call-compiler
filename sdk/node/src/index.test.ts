import assert from "node:assert/strict";
import test from "node:test";

import {
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

test("run result type matches composite tool feedback shape", () => {
  const result: RunResult = {
    outputs: { answer: "ok" },
    node_outputs: { step: { answer: "ok" } },
    trace: [{ node: "step", tool: "echo", status: "cache_hit", duration_ms: 0 }],
    optimization: { deduplicated: [], batch_groups: [] },
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
