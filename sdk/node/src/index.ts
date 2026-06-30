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

export interface DeduplicatedNode {
  removed: string;
  canonical: string;
}

export interface OptimizationReport {
  deduplicated: DeduplicatedNode[];
  batch_groups: BatchGroup[];
}

export interface BatchGroup {
  adapter: string;
  tool: string;
  nodes: string[];
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

export function plan(): PlanBuilder {
  return new PlanBuilder();
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

export const tc = {
  plan,
  ref,
  valueRef,
  effects: {
    pure,
    readOnly,
    write,
  },
};
