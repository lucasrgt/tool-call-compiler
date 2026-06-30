import assert from "node:assert/strict";
import test from "node:test";

import { plan, pure, readOnly, ref, tc, valueRef, type RunResult } from "./index.js";

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

test("run result type matches composite tool feedback shape", () => {
  const result: RunResult = {
    outputs: { answer: "ok" },
    node_outputs: { step: { answer: "ok" } },
    trace: [{ node: "step", tool: "echo", status: "finished" }],
    optimization: { deduplicated: [] },
  };

  assert.equal(result.outputs.answer, "ok");
});
