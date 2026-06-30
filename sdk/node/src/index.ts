export interface RefValue {
  $ref: string;
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
  input_schema?: Json;
  output_schema?: Json;
}

export interface NodeSpec {
  id: string;
  tool: string;
  input?: Json;
  depends_on?: string[];
}

export interface Plan {
  version: "0";
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
  tools: Record<string, ToolSpec>;
  steps: IntentStep[];
  outputs: Record<string, string>;
}

export interface FanOutRecipe {
  kind: "fan_out";
  tool: string;
  items: Json[];
  node_prefix?: string;
  input_key?: string;
}

export type Recipe = FanOutRecipe;

export interface RecipePlan {
  version: "0";
  tools: Record<string, ToolSpec>;
  recipe: Recipe;
  outputs: Record<string, string>;
}

export interface DeduplicatedNode {
  removed: string;
  canonical: string;
}

export interface OptimizationReport {
  deduplicated: DeduplicatedNode[];
  batch_groups: BatchGroup[];
  fused_groups: FusedGroup[];
  summary: OptimizationSummary;
}

export interface BatchGroup {
  adapter: string;
  tool: string;
  nodes: string[];
}

export interface FusedGroup {
  adapter: string;
  tool: string;
  nodes: string[];
  strategy: string;
}

export interface OptimizationSummary {
  estimated_tool_calls_before: number;
  estimated_tool_calls_after: number;
  estimated_llm_turns_before: number;
  estimated_llm_turns_after: number;
}

export interface Diagnostic {
  kind: string;
  nodes: string[];
  message: string;
}

export interface ExplainReport {
  layers: string[][];
  optimization: OptimizationReport;
  diagnostics: Diagnostic[];
}

export type TraceStatus = "started" | "finished" | "cache_hit" | { failed: string };

export interface TraceEvent {
  node: string;
  tool: string;
  status: TraceStatus;
  duration_ms?: number;
}

export interface RunResult {
  outputs: Record<string, Json>;
  node_outputs: Record<string, Json>;
  trace: TraceEvent[];
  optimization: OptimizationReport;
}

export interface ConformanceCheck {
  name: string;
  passed: boolean;
  message: string;
}

export interface ConformanceReport {
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

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  node(id: string, tool: string, input: Json = {}, dependsOn: string[] = []): this {
    this.value.nodes.push({
      id,
      tool,
      input,
      depends_on: dependsOn,
    });
    return this;
  }

  output(name: string, valueRef: string): this {
    this.value.outputs[name] = valueRef;
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

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  step(id: string, tool: string, input: Json = {}, after: string[] = []): this {
    this.value.steps.push({
      id,
      tool,
      input,
      after,
    });
    return this;
  }

  output(name: string, valueRef: string): this {
    this.value.outputs[name] = valueRef;
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

  tool(name: string, spec: ToolSpec): this {
    this.value.tools[name] = spec;
    return this;
  }

  output(name: string, valueRef: string): this {
    this.value.outputs[name] = valueRef;
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

export function compileRecipe(recipePlan: RecipePlan): Plan {
  if (recipePlan.version !== "0") {
    throw new Error(`unsupported recipe version '${recipePlan.version}'`);
  }

  switch (recipePlan.recipe.kind) {
    case "fan_out": {
      const nodePrefix = recipePlan.recipe.node_prefix ?? "item_";
      return {
        version: "0",
        tools: structuredClone(recipePlan.tools),
        nodes: recipePlan.recipe.items.map((item, index) => ({
          id: `${nodePrefix}${index + 1}`,
          tool: recipePlan.recipe.tool,
          input: recipePlan.recipe.input_key
            ? { [recipePlan.recipe.input_key]: structuredClone(item) }
            : structuredClone(item),
          depends_on: [],
        })),
        outputs: structuredClone(recipePlan.outputs),
      };
    }
  }
}

export function compileIntent(intentPlan: IntentPlan): Plan {
  if (intentPlan.version !== "0") {
    throw new Error(`unsupported intent version '${intentPlan.version}'`);
  }

  return {
    version: "0",
    tools: structuredClone(intentPlan.tools),
    nodes: intentPlan.steps.map((step) => {
      const dependsOn = new Set(step.after ?? []);
      collectRefNodes(step.input ?? {}, dependsOn);
      return {
        id: step.id,
        tool: step.tool,
        input: structuredClone(step.input ?? {}),
        depends_on: [...dependsOn].sort(),
      };
    }),
    outputs: structuredClone(intentPlan.outputs),
  };
}

export function ref(valueRef: string): RefValue {
  return { $ref: valueRef };
}

export function valueRef(node: string, path: string[] = []): string {
  return [node, "output", ...path].join(".");
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

export function write(adapter: string, writes: string[]): ToolSpec {
  return {
    adapter,
    effects: {
      reads: [],
      writes,
      idempotent: false,
      cacheable: false,
    },
  };
}

export function limits(spec: ToolSpec, value: ToolLimits): ToolSpec {
  return {
    ...spec,
    limits: value,
  };
}

export function cost(spec: ToolSpec, value: ToolCost): ToolSpec {
  return {
    ...spec,
    cost: value,
  };
}

export function capabilities(value: ToolCapabilities): ToolCapabilities {
  return structuredClone(value);
}

function collectRefNodes(value: Json, nodes: Set<string>): void {
  if (value === null || typeof value !== "object") {
    return;
  }

  if (Array.isArray(value)) {
    for (const item of value) {
      collectRefNodes(item, nodes);
    }
    return;
  }

  const record = value as Record<string, Json>;
  if (Object.keys(record).length === 1 && typeof record.$ref === "string") {
    const [node, output] = record.$ref.split(".");
    if (node && output === "output") {
      nodes.add(node);
    }
    return;
  }

  for (const item of Object.values(record)) {
    collectRefNodes(item, nodes);
  }
}

export const PLAN_SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://tool-call-compiler.dev/schemas/plan.schema.json",
  title: "Tool Call Compiler Plan",
  type: "object",
  required: ["version", "tools", "nodes", "outputs"],
  additionalProperties: false,
  properties: {
    version: { const: "0" },
    tools: {
      type: "object",
      additionalProperties: { $ref: "#/$defs/toolSpec" },
    },
    nodes: { type: "array", items: { $ref: "#/$defs/node" } },
    outputs: { type: "object", additionalProperties: { type: "string" } },
  },
  $defs: {
    toolSpec: {
      type: "object",
      required: ["adapter"],
      additionalProperties: false,
      properties: {
        adapter: { type: "string", minLength: 1 },
        effects: { $ref: "#/$defs/effects" },
        limits: { $ref: "#/$defs/limits" },
        cost: { $ref: "#/$defs/cost" },
      },
    },
    effects: {
      type: "object",
      additionalProperties: false,
      properties: {
        pure: { type: "boolean" },
        reads: { type: "array", items: { type: "string" } },
        writes: { type: "array", items: { type: "string" } },
        idempotent: { type: "boolean" },
        cacheable: { type: "boolean" },
        batchable: { type: "boolean" },
        commutative: { type: "boolean" },
        timeout_ms: { type: "integer", minimum: 1 },
        retry: { $ref: "#/$defs/retry" },
      },
    },
    cost: {
      type: "object",
      additionalProperties: false,
      properties: {
        fixed_ms: { type: "integer", minimum: 0 },
        per_call_ms: { type: "integer", minimum: 0 },
        tokens: { type: "integer", minimum: 0 },
      },
    },
    limits: {
      type: "object",
      additionalProperties: false,
      properties: {
        max_concurrency: { type: "integer", minimum: 1 },
        batch_size: { type: "integer", minimum: 1 },
      },
    },
    retry: {
      type: "object",
      required: ["max_attempts"],
      additionalProperties: false,
      properties: {
        max_attempts: { type: "integer", minimum: 1, maximum: 255 },
        retryable_errors: { type: "array", items: { type: "string" } },
      },
    },
    node: {
      type: "object",
      required: ["id", "tool"],
      additionalProperties: false,
      properties: {
        id: { type: "string", minLength: 1 },
        tool: { type: "string", minLength: 1 },
        input: true,
        depends_on: { type: "array", items: { type: "string" } },
      },
    },
  },
} as const;

export const INTENT_SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://tool-call-compiler.dev/schemas/intent.schema.json",
  title: "Tool Call Compiler Intent Plan",
  type: "object",
  required: ["version", "tools", "steps", "outputs"],
  additionalProperties: false,
  properties: {
    version: { const: "0" },
    tools: {
      type: "object",
      additionalProperties: { $ref: "plan.schema.json#/$defs/toolSpec" },
    },
    steps: { type: "array", items: { $ref: "#/$defs/step" } },
    outputs: { type: "object", additionalProperties: { type: "string" } },
  },
  $defs: {
    step: {
      type: "object",
      required: ["id", "tool"],
      additionalProperties: false,
      properties: {
        id: { type: "string", minLength: 1 },
        tool: { type: "string", minLength: 1 },
        input: true,
        after: { type: "array", items: { type: "string" } },
      },
    },
  },
} as const;

export const RECIPE_SCHEMA = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $id: "https://tool-call-compiler.dev/schemas/recipe.schema.json",
  title: "Tool Call Compiler Recipe Plan",
  type: "object",
  required: ["version", "tools", "recipe", "outputs"],
  additionalProperties: false,
  properties: {
    version: { const: "0" },
    tools: {
      type: "object",
      additionalProperties: { $ref: "plan.schema.json#/$defs/toolSpec" },
    },
    recipe: { $ref: "#/$defs/recipe" },
    outputs: { type: "object", additionalProperties: { type: "string" } },
  },
  $defs: {
    recipe: {
      oneOf: [{ $ref: "#/$defs/fanOut" }],
    },
    fanOut: {
      type: "object",
      required: ["kind", "tool", "items"],
      additionalProperties: false,
      properties: {
        kind: { const: "fan_out" },
        tool: { type: "string", minLength: 1 },
        items: { type: "array", items: true },
        node_prefix: { type: "string", minLength: 1 },
        input_key: { type: "string", minLength: 1 },
      },
    },
  },
} as const;

export const tc = {
  plan,
  intent,
  recipe,
  fanOut,
  compileIntent,
  compileRecipe,
  ref,
  valueRef,
  effects: {
    pure,
    readOnly,
    write,
  },
  limits,
  cost,
  capabilities,
  PLAN_SCHEMA,
  INTENT_SCHEMA,
  RECIPE_SCHEMA,
};
