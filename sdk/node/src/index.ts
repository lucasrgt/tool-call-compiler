/**
 * TypeScript builder SDK for tool-compiler plans, intents, and recipes.
 *
 * The compilers in this module (`compileIntent`, `compileRecipe`) mirror the
 * Rust planner exactly — same node ids, same dependency derivation, same
 * error conditions — and that parity is enforced by shared golden fixtures
 * under `tests/parity/` executed by both toolchains.
 *
 * The JSON Schemas are generated from `/schemas/*.json` (the single source
 * of truth) into `schemas.gen.ts` at build time.
 */

export {
  INTENT_SCHEMA,
  PLAN_SCHEMA,
  RECIPE_SCHEMA,
} from "./schemas.gen.js";
import {
  INTENT_SCHEMA as PLAN_INTENT_SCHEMA,
  PLAN_SCHEMA as PLAN_PLAN_SCHEMA,
  RECIPE_SCHEMA as PLAN_RECIPE_SCHEMA,
} from "./schemas.gen.js";

export interface RefValue {
  $ref: string;
}

export interface LiteralValue {
  $literal: Json;
}

export type Json =
  | null
  | boolean
  | number
  | string
  | RefValue
  | Json[]
  | { [key: string]: Json };

export interface RetryPolicy {
  max_attempts: number;
  retryable_errors?: string[];
  /** Base backoff in ms; attempt n waits backoff_ms * 2^(n-1) plus jitter. */
  backoff_ms?: number;
}

export interface Effects {
  pure?: boolean;
  reads?: string[];
  writes?: string[];
  idempotent?: boolean;
  cacheable?: boolean;
  batchable?: boolean;
  commutative?: boolean;
  timeout_ms?: number;
  retry?: RetryPolicy;
}

export interface ToolSpec {
  adapter: string;
  effects?: Effects;
  limits?: ToolLimits;
  cost?: ToolCost;
  /** Tool behavior version; changing it invalidates cached outputs. */
  version?: string;
}

export interface ToolLimits {
  max_concurrency?: number;
  batch_size?: number;
}

export interface ToolCost {
  fixed_ms?: number;
  per_call_ms?: number;
  tokens?: number;
}

export interface ToolCapabilities {
  effects?: Effects;
  limits?: ToolLimits;
  cost?: ToolCost;
  version?: string;
  input_schema?: Json;
  output_schema?: Json;
}

/** Runtime condition gating a node. */
export interface When {
  ref: string;
  equals?: Json;
  not?: boolean;
}

export interface NodeSpec {
  id: string;
  tool: string;
  input?: Json;
  depends_on?: string[];
  /** Expands at runtime into one call per element of the referenced array. */
  for_each?: string;
  when?: When;
}

export interface Plan {
  version: "0";
  name?: string;
  description?: string;
  tools: Record<string, ToolSpec>;
  nodes: NodeSpec[];
  outputs: Record<string, string>;
}

export interface IntentStep {
  id: string;
  tool: string;
  input?: Json;
  after?: string[];
}

export interface IntentPlan {
  version: "0";
  name?: string;
  tools: Record<string, ToolSpec>;
  steps: IntentStep[];
  outputs: Record<string, string>;
}

export interface FanOutRecipe {
  kind: "fan_out";
  tool: string;
  items?: Json[];
  /** Runtime source array; lowers to a for_each node. */
  items_ref?: string;
  node_prefix?: string;
  input_key?: string;
}

export interface MapReduceRecipe {
  kind: "map_reduce";
  tool: string;
  items?: Json[];
  items_ref?: string;
  node_prefix?: string;
  input_key?: string;
  reduce_tool: string;
  reduce_input_key?: string;
}

export interface PipelineStep {
  tool: string;
  /** `{"$prev": "<path>"}` markers resolve to the previous step's output. */
  input?: Json;
}

export interface PipelineRecipe {
  kind: "pipeline";
  steps: PipelineStep[];
  node_prefix?: string;
}

export type Recipe = FanOutRecipe | MapReduceRecipe | PipelineRecipe;

export interface RecipePlan {
  version: "0";
  name?: string;
  /** Declared parameters with defaults; null marks a parameter required. */
  params?: Record<string, Json>;
  tools: Record<string, ToolSpec>;
  recipe: Recipe;
  outputs: Record<string, string>;
}

export interface DeduplicatedNode {
  removed: string;
  canonical: string;
}

export interface OptimizationReport {
  passes: string[];
  deduplicated: DeduplicatedNode[];
  eliminated: string[];
  batch_groups: BatchGroup[];
  summary: OptimizationSummary;
}

export interface BatchGroup {
  adapter: string;
  tool: string;
  nodes: string[];
}

export interface OptimizationSummary {
  estimated_tool_calls_before: number;
  estimated_tool_calls_after: number;
  estimated_llm_turns_before: number;
  estimated_llm_turns_after: number;
  estimated_serial_ms?: number;
  estimated_compiled_ms?: number;
  estimated_tokens_before?: number;
  estimated_tokens_after?: number;
}

export type DiagnosticKind = "missing_effects" | "resource_conflict";

export interface Diagnostic {
  kind: DiagnosticKind;
  nodes: string[];
  resource?: string;
  message: string;
}

export interface ExplainReport {
  layers: string[][];
  optimization: OptimizationReport;
  diagnostics: Diagnostic[];
}

export type TraceStatus =
  | "started"
  | "finished"
  | "cache_hit"
  | "failed"
  | "retried"
  | "skipped";

export interface TraceEvent {
  node: string;
  tool: string;
  status: TraceStatus;
  at_ms: number;
  duration_ms?: number;
  error?: string;
  batch_id?: number;
  attempt?: number;
}

export type RunStatus = "success" | "failed" | "cancelled";

export type SkipReason =
  | "condition"
  | "failed_dependency"
  | "cancelled"
  | "not_run";

export interface ToolExecutionError {
  message: string;
  code?: string;
  retryable?: boolean;
}

export interface RunMetrics {
  wall_ms: number;
  nodes_total: number;
  nodes_succeeded: number;
  nodes_failed: number;
  nodes_skipped: number;
  cache_hits: number;
  batch_dispatches: number;
  retries: number;
}

export interface RunResult {
  status: RunStatus;
  outputs: Record<string, Json>;
  node_outputs: Record<string, Json>;
  errors?: Record<string, ToolExecutionError>;
  skipped?: Record<string, SkipReason>;
  trace: TraceEvent[];
  optimization: OptimizationReport;
  metrics: RunMetrics;
}

export interface ConformanceCheck {
  name: string;
  passed: boolean;
  message: string;
}

export interface ConformanceReport {
  adapter: string;
  passed: boolean;
  checks: ConformanceCheck[];
}

export class PlanBuilder {
  private readonly value: Plan = {
    version: "0",
    tools: {},
    nodes: [],
    outputs: {},
  };

  name(name: string): this {
    this.value.name = name;
    return this;
  }

  description(description: string): this {
    this.value.description = description;
    return this;
  }

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  node(
    id: string,
    tool: string,
    input: Json = {},
    options: { dependsOn?: string[]; forEach?: string; when?: When } = {},
  ): this {
    const node: NodeSpec = { id, tool, input };
    if (options.dependsOn?.length) {
      node.depends_on = options.dependsOn;
    }
    if (options.forEach) {
      node.for_each = options.forEach;
    }
    if (options.when) {
      node.when = options.when;
    }
    this.value.nodes.push(node);
    return this;
  }

  output(name: string, ref: string): this {
    this.value.outputs[name] = ref;
    return this;
  }

  toJSON(): Plan {
    return structuredClone(this.value);
  }
}

export class IntentBuilder {
  private readonly value: IntentPlan = {
    version: "0",
    tools: {},
    steps: [],
    outputs: {},
  };

  name(name: string): this {
    this.value.name = name;
    return this;
  }

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  step(id: string, tool: string, input: Json = {}, after: string[] = []): this {
    const step: IntentStep = { id, tool, input };
    if (after.length) {
      step.after = after;
    }
    this.value.steps.push(step);
    return this;
  }

  output(name: string, ref: string): this {
    this.value.outputs[name] = ref;
    return this;
  }

  toJSON(): IntentPlan {
    return structuredClone(this.value);
  }
}

export class RecipeBuilder {
  private readonly value: RecipePlan;

  constructor(recipeValue: Recipe) {
    this.value = {
      version: "0",
      tools: {},
      recipe: structuredClone(recipeValue),
      outputs: {},
    };
  }

  name(name: string): this {
    this.value.name = name;
    return this;
  }

  param(name: string, defaultValue: Json = null): this {
    this.value.params = { ...this.value.params, [name]: defaultValue };
    return this;
  }

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  output(name: string, ref: string): this {
    this.value.outputs[name] = ref;
    return this;
  }

  toJSON(): RecipePlan {
    return structuredClone(this.value);
  }
}

export function plan(): PlanBuilder {
  return new PlanBuilder();
}

export function intent(): IntentBuilder {
  return new IntentBuilder();
}

export function recipe(recipeValue: Recipe): RecipeBuilder {
  return new RecipeBuilder(recipeValue);
}

export function fanOut(
  tool: string,
  items: Json[],
  options: { nodePrefix?: string; inputKey?: string } = {},
): FanOutRecipe {
  return {
    kind: "fan_out",
    tool,
    items,
    ...(options.nodePrefix ? { node_prefix: options.nodePrefix } : {}),
    ...(options.inputKey ? { input_key: options.inputKey } : {}),
  };
}

export function fanOutRef(
  tool: string,
  itemsRef: string,
  options: { nodePrefix?: string; inputKey?: string } = {},
): FanOutRecipe {
  return {
    kind: "fan_out",
    tool,
    items_ref: itemsRef,
    ...(options.nodePrefix ? { node_prefix: options.nodePrefix } : {}),
    ...(options.inputKey ? { input_key: options.inputKey } : {}),
  };
}

const DEFAULT_NODE_PREFIX = "item_";
const DEFAULT_REDUCE_KEY = "items";

/** Compiles a recipe using only its declared parameter defaults. */
export function compileRecipe(recipePlan: RecipePlan): Plan {
  return compileRecipeWithParams(recipePlan, {});
}

/**
 * Compiles a recipe, substituting `{"$param": name}` placeholders from
 * `values` merged over the declared defaults. Mirrors the Rust planner.
 */
export function compileRecipeWithParams(
  recipePlan: RecipePlan,
  values: Record<string, Json>,
): Plan {
  if (recipePlan.version !== "0") {
    throw new Error(`unsupported recipe version '${recipePlan.version}'`);
  }

  const params: Record<string, Json> = { ...(recipePlan.params ?? {}) };
  for (const [name, value] of Object.entries(values)) {
    if (!(name in params)) {
      throw new Error(`recipe references undeclared parameter '${name}'`);
    }
    params[name] = value;
  }
  for (const [name, value] of Object.entries(params)) {
    if (value === null) {
      throw new Error(`recipe parameter '${name}' is required but was not supplied`);
    }
  }

  const substituted =
    Object.keys(params).length > 0
      ? (substituteParams(
          recipePlan.recipe as unknown as Json,
          params,
        ) as unknown as Recipe)
      : recipePlan.recipe;

  const compiled: Plan = {
    version: "0",
    tools: structuredClone(recipePlan.tools),
    nodes: compileRecipeNodes(substituted),
    outputs: structuredClone(recipePlan.outputs),
  };
  if (recipePlan.name !== undefined) {
    compiled.name = recipePlan.name;
  }
  return orderPlanKeys(compiled);
}

function compileRecipeNodes(recipeValue: Recipe): NodeSpec[] {
  switch (recipeValue.kind) {
    case "fan_out":
      return compileFanOut(recipeValue);
    case "map_reduce": {
      const nodes = compileFanOut(recipeValue);
      const prefix = recipeValue.node_prefix ?? DEFAULT_NODE_PREFIX;
      const reduceKey = recipeValue.reduce_input_key ?? DEFAULT_REDUCE_KEY;
      const reduceInput =
        nodes.length === 1 && nodes[0]?.for_each
          ? { [reduceKey]: { $ref: `${nodes[0].id}.output` } }
          : {
              [reduceKey]: nodes.map((node) => ({
                $ref: `${node.id}.output`,
              })),
            };
      nodes.push({
        id: `${prefix}reduce`,
        tool: recipeValue.reduce_tool,
        input: reduceInput as Json,
      });
      return nodes;
    }
    case "pipeline": {
      if (recipeValue.steps.length === 0) {
        throw new Error("pipeline recipe needs at least one step");
      }
      const prefix = recipeValue.node_prefix ?? DEFAULT_NODE_PREFIX;
      return recipeValue.steps.map((step, index) => {
        const input =
          index === 0
            ? structuredClone(step.input ?? {})
            : rewritePrevMarkers(
                structuredClone(step.input ?? {}),
                `${prefix}${index}`,
              );
        return { id: `${prefix}${index + 1}`, tool: step.tool, input };
      });
    }
  }
}

function compileFanOut(recipeValue: FanOutRecipe | MapReduceRecipe): NodeSpec[] {
  const prefix = recipeValue.node_prefix ?? DEFAULT_NODE_PREFIX;
  if (recipeValue.items_ref) {
    const template: Json = recipeValue.input_key
      ? { [recipeValue.input_key]: { $item: "" } as unknown as Json }
      : ({ $item: "" } as unknown as Json);
    return [
      {
        id: `${prefix}each`,
        tool: recipeValue.tool,
        input: template,
        for_each: recipeValue.items_ref,
      },
    ];
  }
  const items = recipeValue.items ?? [];
  if (items.length === 0) {
    throw new Error("fan_out recipe needs a non-empty 'items' array or an 'items_ref'");
  }
  return items.map((item, index) => ({
    id: `${prefix}${index + 1}`,
    tool: recipeValue.tool,
    input: recipeValue.input_key
      ? { [recipeValue.input_key]: structuredClone(item) }
      : structuredClone(item),
  }));
}

function rewritePrevMarkers(value: Json, previousId: string): Json {
  if (value === null || typeof value !== "object") {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => rewritePrevMarkers(item, previousId));
  }
  const record = value as Record<string, Json>;
  const keys = Object.keys(record);
  if (keys.length === 1 && typeof record.$prev === "string") {
    const path = record.$prev;
    const reference =
      path === "" ? `${previousId}.output` : `${previousId}.output.${path}`;
    return { $ref: reference };
  }
  const out: Record<string, Json> = {};
  for (const [key, item] of Object.entries(record)) {
    out[key] = rewritePrevMarkers(item, previousId);
  }
  return out;
}

function substituteParams(value: Json, params: Record<string, Json>): Json {
  if (value === null || typeof value !== "object") {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => substituteParams(item, params));
  }
  const record = value as Record<string, Json>;
  const keys = Object.keys(record);
  if (keys.length === 1 && typeof record.$param === "string") {
    const name = record.$param;
    if (!(name in params)) {
      throw new Error(`recipe references undeclared parameter '${name}'`);
    }
    return structuredClone(params[name] ?? null);
  }
  const out: Record<string, Json> = {};
  for (const [key, item] of Object.entries(record)) {
    out[key] = substituteParams(item, params);
  }
  return out;
}

/** Compiles an intent into an executable plan; mirrors the Rust planner. */
export function compileIntent(intentPlan: IntentPlan): Plan {
  if (intentPlan.version !== "0") {
    throw new Error(`unsupported intent version '${intentPlan.version}'`);
  }

  const seen = new Set<string>();
  for (const step of intentPlan.steps) {
    validateNodeId(step.id);
    if (seen.has(step.id)) {
      throw new Error(`duplicate step id '${step.id}'`);
    }
    seen.add(step.id);
    if (!(step.tool in intentPlan.tools)) {
      throw new Error(`step '${step.id}' uses unknown tool '${step.tool}'`);
    }
  }

  const compiled: Plan = {
    version: "0",
    tools: structuredClone(intentPlan.tools),
    nodes: intentPlan.steps.map((step) => {
      const dependsOn = new Set<string>();
      for (const dependency of step.after ?? []) {
        if (dependency === step.id) {
          throw new Error(`step '${step.id}' references itself`);
        }
        dependsOn.add(dependency);
      }
      for (const node of collectRefNodes(step.input ?? {})) {
        if (node === step.id) {
          throw new Error(`step '${step.id}' references itself`);
        }
        dependsOn.add(node);
      }
      const node: NodeSpec = {
        id: step.id,
        tool: step.tool,
        input: structuredClone(step.input ?? {}),
      };
      if (dependsOn.size > 0) {
        node.depends_on = [...dependsOn].sort();
      }
      return node;
    }),
    outputs: structuredClone(intentPlan.outputs),
  };
  if (intentPlan.name !== undefined) {
    compiled.name = intentPlan.name;
  }
  return orderPlanKeys(compiled);
}

/** Reorders plan keys to the canonical serialization order used by Rust. */
function orderPlanKeys(compiled: Plan): Plan {
  const ordered: Plan = { version: compiled.version } as Plan;
  if (compiled.name !== undefined) {
    ordered.name = compiled.name;
  }
  if (compiled.description !== undefined) {
    ordered.description = compiled.description;
  }
  ordered.tools = compiled.tools;
  ordered.nodes = compiled.nodes;
  ordered.outputs = compiled.outputs;
  return ordered;
}

/** Builds a `{"$ref": ...}` reference object. */
export function ref(reference: string): RefValue {
  parseRef(reference);
  return { $ref: reference };
}

/** Wraps data so it is never interpreted as a reference. */
export function literal(value: Json): LiteralValue {
  return { $literal: structuredClone(value) };
}

/**
 * Builds a `<node>.output[.<path>]` reference string. Path segments escape
 * `.` as `~1` and `~` as `~0`, matching the Rust IR.
 */
export function valueRef(node: string, path: string[] = []): string {
  validateNodeId(node);
  const segments = path.map((segment) =>
    segment.replaceAll("~", "~0").replaceAll(".", "~1"),
  );
  return [node, "output", ...segments].join(".");
}

function validateNodeId(id: string): void {
  if (id.length === 0) {
    throw new Error("node id must not be empty");
  }
  if (id.includes(".")) {
    throw new Error(`node id '${id}' must not contain '.'`);
  }
}

function parseRef(reference: string): string {
  const parts = reference.split(".");
  const node = parts[0];
  if (!node || parts[1] !== "output") {
    throw new Error(
      `invalid value reference '${reference}', expected '<node>.output[.<path>...]'`,
    );
  }
  for (const segment of parts.slice(2)) {
    if (segment === "") {
      throw new Error(`value reference '${reference}' contains an empty path segment`);
    }
  }
  return node;
}

/**
 * Collects referenced node ids. Malformed `$ref` shapes throw — matching
 * the Rust validator — and `{"$literal": ...}` subtrees are skipped.
 */
export function collectRefNodes(value: Json): string[] {
  const nodes = new Set<string>();
  collectInto(value, nodes);
  return [...nodes];
}

function collectInto(value: Json, nodes: Set<string>): void {
  if (value === null || typeof value !== "object") {
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) {
      collectInto(item, nodes);
    }
    return;
  }

  const record = value as Record<string, Json>;
  const keys = Object.keys(record);
  if (keys.length === 1 && keys[0] === "$literal") {
    return;
  }
  if ("$ref" in record) {
    if (keys.length > 1) {
      const others = keys.filter((key) => key !== "$ref").join(", ");
      throw new Error(
        `object mixes '$ref' with other keys (${others}); wrap literal data in {"$literal": ...}`,
      );
    }
    const reference = record.$ref;
    if (typeof reference !== "string") {
      throw new Error(`'$ref' value must be a string, found ${JSON.stringify(reference)}`);
    }
    nodes.add(parseRef(reference));
    return;
  }

  for (const item of Object.values(record)) {
    collectInto(item, nodes);
  }
}

export function pure(adapter: string): ToolSpec {
  return {
    adapter,
    effects: {
      pure: true,
      idempotent: true,
      cacheable: true,
      commutative: true,
    },
  };
}

export function readOnly(adapter: string, reads: string[]): ToolSpec {
  return {
    adapter,
    effects: {
      reads,
      writes: [],
      idempotent: true,
      cacheable: true,
    },
  };
}

/**
 * Generic write effects. Defaults to non-idempotent (POST-like) because a
 * generic write may duplicate effects on retry; pass `{ idempotent: true }`
 * for full-overwrite semantics (PUT-like, matching the Rust fs adapter's
 * `write_file_tool`).
 */
export function write(
  adapter: string,
  writes: string[],
  options: { idempotent?: boolean } = {},
): ToolSpec {
  return {
    adapter,
    effects: {
      reads: [],
      writes,
      idempotent: options.idempotent ?? false,
      cacheable: false,
    },
  };
}

export function limits(spec: ToolSpec, value: ToolLimits): ToolSpec {
  return { ...spec, limits: value };
}

export function cost(spec: ToolSpec, value: ToolCost): ToolSpec {
  return { ...spec, cost: value };
}

export function capabilities(value: ToolCapabilities): ToolCapabilities {
  return structuredClone(value);
}

export interface RunViaCliOptions {
  /** CLI binary to spawn (default: `tool-compiler` from PATH). */
  bin?: string;
  /** Extra arguments, e.g. `["--runtime-config", "runtime.json"]`. */
  args?: string[];
  /** Milliseconds before the CLI is killed (default 120000). */
  timeoutMs?: number;
}

/**
 * Executes a plan by spawning the `tool-compiler` CLI (`run -` over stdin)
 * and returns the parsed [`RunResult`]. Requires the CLI to be installed.
 */
export async function runPlanViaCli(
  planValue: Plan,
  options: RunViaCliOptions = {},
): Promise<RunResult> {
  const { execFile } = await import("node:child_process");
  const bin = options.bin ?? "tool-compiler";
  const args = ["run", "-", ...(options.args ?? [])];

  return new Promise<RunResult>((resolve, reject) => {
    const child = execFile(
      bin,
      args,
      { timeout: options.timeoutMs ?? 120_000, maxBuffer: 64 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (error) {
          reject(new Error(`${bin} failed: ${error.message}\n${stderr}`));
          return;
        }
        try {
          resolve(JSON.parse(stdout) as RunResult);
        } catch (parseError) {
          reject(
            new Error(
              `could not parse RunResult from ${bin} output: ${String(parseError)}`,
            ),
          );
        }
      },
    );
    child.stdin?.write(JSON.stringify(planValue));
    child.stdin?.end();
  });
}

export const tc = {
  plan,
  intent,
  recipe,
  fanOut,
  fanOutRef,
  compileIntent,
  compileRecipe,
  compileRecipeWithParams,
  ref,
  literal,
  valueRef,
  collectRefNodes,
  runPlanViaCli,
  effects: {
    pure,
    readOnly,
    write,
  },
  limits,
  cost,
  capabilities,
  PLAN_SCHEMA: PLAN_PLAN_SCHEMA,
  INTENT_SCHEMA: PLAN_INTENT_SCHEMA,
  RECIPE_SCHEMA: PLAN_RECIPE_SCHEMA,
};
