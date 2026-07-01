import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import Ajv2020 from "ajv/dist/2020.js";

import {
  INTENT_SCHEMA,
  PLAN_SCHEMA,
  RECIPE_SCHEMA,
  collectRefNodes,
  compileIntent,
  compileRecipe,
  compileRecipeWithParams,
  fanOutRef,
  intent,
  literal,
  plan,
  pure,
  readOnly,
  ref,
  tc,
  valueRef,
  write,
  type ConformanceReport,
  type Json,
  type Plan,
  type RunResult,
} from "./index.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..", "..");

function ajv() {
  return new Ajv2020.default({ strict: false, allowUnionTypes: true });
}

test("builds a stable plan json shape", () => {
  const built = plan()
    .name("compose message")
    .tool("fetchUser", readOnly("http", ["api:user"]))
    .tool("format", pure("local"))
    .node("user", "fetchUser", { id: "u_1" })
    .node("message", "format", { user: ref(valueRef("user", ["id"])) })
    .output("message", valueRef("message"))
    .toJSON();

  assert.equal(built.version, "0");
  assert.equal(built.name, "compose message");
  assert.deepEqual(built.nodes[1]?.input, {
    user: { $ref: "user.output.id" },
  });
  assert.equal(built.outputs.message, "message.output");
});

test("built plans validate against the plan schema", () => {
  const validate = ajv().compile(PLAN_SCHEMA as unknown as object);
  const built = plan()
    .tool("read", readOnly("fs", ["file:{path}"]))
    .node("a", "read", { path: "x.md" })
    .node("gate", "read", { path: "flag.md" })
    .node(
      "guarded",
      "read",
      { path: "y.md" },
      { when: { ref: valueRef("gate"), equals: true } },
    )
    .node(
      "each",
      "read",
      { path: { $item: "path" } as unknown as Json },
      { forEach: valueRef("a", ["entries"]) },
    )
    .output("all", valueRef("each"))
    .toJSON();

  assert.equal(validate(built), true, JSON.stringify(validate.errors));
});

test("schema constants match the /schemas source files", () => {
  for (const [constant, file] of [
    [PLAN_SCHEMA, "plan.schema.json"],
    [INTENT_SCHEMA, "intent.schema.json"],
    [RECIPE_SCHEMA, "recipe.schema.json"],
  ] as const) {
    const source = JSON.parse(
      readFileSync(path.join(repoRoot, "schemas", file), "utf8"),
    );
    assert.deepEqual(structuredClone(constant), source, `${file} drifted`);
  }
});

test("parity fixtures compile identically to the Rust planner", () => {
  const dir = path.join(repoRoot, "tests", "parity");
  const fixtures = readdirSync(dir).filter((name) => name.endsWith(".json"));
  assert.ok(fixtures.length >= 5, "expected at least 5 parity fixtures");

  for (const name of fixtures) {
    const fixture = JSON.parse(readFileSync(path.join(dir, name), "utf8"));
    const compiled: Plan = fixture.intent
      ? compileIntent(fixture.intent)
      : compileRecipeWithParams(fixture.recipe, fixture.params ?? {});
    assert.deepEqual(
      JSON.parse(JSON.stringify(compiled)),
      fixture.plan,
      `fixture '${name}' diverged`,
    );
  }
});

test("dynamic fan-out lowers to a for_each node", () => {
  const compiled = compileRecipe(
    tc
      .recipe(fanOutRef("read", valueRef("search", ["hits"]), { inputKey: "doc" }))
      .tool("read", pure("local"))
      .toJSON(),
  );

  assert.equal(compiled.nodes.length, 1);
  assert.equal(compiled.nodes[0]?.for_each, "search.output.hits");
});

test("recipe params are validated", () => {
  const source = tc
    .recipe(tc.fanOut("echo", [{ $param: "query" } as unknown as Json]))
    .param("query")
    .tool("echo", pure("local"))
    .toJSON();

  assert.throws(() => compileRecipe(source), /required/);
  assert.throws(
    () => compileRecipeWithParams(source, { other: 1 }),
    /undeclared/,
  );
  const compiled = compileRecipeWithParams(source, { query: "ok" });
  assert.deepEqual(compiled.nodes[0]?.input, "ok");
});

test("empty fan-out and unknown versions are rejected", () => {
  assert.throws(
    () => compileRecipe(tc.recipe(tc.fanOut("echo", [])).toJSON()),
    /items/,
  );
  const bad = tc.recipe(tc.fanOut("echo", [1])).toJSON();
  (bad as { version: string }).version = "9";
  assert.throws(() => compileRecipe(bad), /version/);
});

test("intent compiler enforces the same rules as Rust", () => {
  const base = () =>
    intent().tool("echo", pure("local")).step("a", "echo", { v: 1 });

  const duplicated = base().step("a", "echo").toJSON();
  assert.throws(() => compileIntent(duplicated), /duplicate step id/);

  const selfRef = intent()
    .tool("echo", pure("local"))
    .step("loop", "echo", { v: ref("loop.output") })
    .toJSON();
  assert.throws(() => compileIntent(selfRef), /references itself/);

  const unknownTool = intent().step("a", "missing").toJSON();
  assert.throws(() => compileIntent(unknownTool), /unknown tool/);

  const dottedId = intent().tool("echo", pure("local")).step("a.b", "echo").toJSON();
  assert.throws(() => compileIntent(dottedId), /must not contain/);
});

test("collectRefNodes throws on malformed refs and skips literals", () => {
  assert.deepEqual(
    collectRefNodes({
      user: { $ref: "user.output" },
      schema: literal({ $ref: "#/definitions/x" }) as unknown as Json,
    }),
    ["user"],
  );
  assert.throws(() => collectRefNodes({ bad: { $ref: 5 } as unknown as Json }));
  assert.throws(() =>
    collectRefNodes({ bad: { $ref: "a.output", extra: 1 } as unknown as Json }),
  );
  assert.throws(() => collectRefNodes({ bad: { $ref: "not-a-ref" } }));
});

test("valueRef escapes dotted path segments", () => {
  assert.equal(valueRef("node", ["a.b", "c~d"]), "node.output.a~1b.c~0d");
  assert.throws(() => valueRef("a.b"), /must not contain/);
});

test("write helper defaults to non-idempotent with an override", () => {
  assert.equal(write("http", ["api:x"]).effects?.idempotent, false);
  assert.equal(
    write("fs", ["file:{path}"], { idempotent: true }).effects?.idempotent,
    true,
  );
});

test("run result v2 type matches composite tool feedback shape", () => {
  const result: RunResult = {
    status: "failed",
    outputs: { answer: "ok" },
    node_outputs: { step: { answer: "ok" } },
    errors: { bad: { message: "boom", code: "timeout", retryable: true } },
    skipped: { child: "failed_dependency" },
    trace: [
      {
        node: "step",
        tool: "echo",
        status: "cache_hit",
        at_ms: 1,
        duration_ms: 0,
      },
    ],
    optimization: {
      passes: ["dedup", "dce", "batch"],
      deduplicated: [],
      eliminated: [],
      batch_groups: [],
      summary: {
        estimated_tool_calls_before: 1,
        estimated_tool_calls_after: 1,
        estimated_llm_turns_before: 1,
        estimated_llm_turns_after: 1,
      },
    },
    metrics: {
      wall_ms: 3,
      nodes_total: 2,
      nodes_succeeded: 1,
      nodes_failed: 1,
      nodes_skipped: 0,
      cache_hits: 1,
      batch_dispatches: 0,
      retries: 0,
    },
  };

  assert.equal(result.errors?.bad.code, "timeout");
});

test("conformance report type carries the adapter", () => {
  const report: ConformanceReport = {
    adapter: "fs.repo",
    passed: true,
    checks: [{ name: "echo_round_trip", passed: true, message: "passed" }],
  };

  assert.equal(report.adapter, "fs.repo");
});
