import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const root = process.cwd();
const includeOptional = process.argv.includes("--include-optional");
const suitePath = process.argv.find((arg) => arg.startsWith("--suite="))?.slice("--suite=".length) ?? "benchmarks/suite.json";
const suite = JSON.parse(readFileSync(path.resolve(root, suitePath), "utf8"));
const outDir = path.resolve(root, suite.out_dir ?? "reports/benchmarks");
mkdirSync(outDir, { recursive: true });

const rows = [];

for (const entry of suite.cases ?? []) {
  if (entry.optional && !includeOptional) {
    rows.push({ id: entry.id, title: entry.title, optional: true, skipped: true, reason: "optional" });
    continue;
  }

  const iterations = entry.iterations ?? suite.iterations ?? 3;
  try {
    const planPath = entry.kind === "recipe" ? compileRecipe(entry, outDir) : path.resolve(root, entry.plan);
    const args = ["run", "--quiet", "-p", "tool-compiler-cli", "--", "bench", planPath, "--iterations", String(iterations)];
    if (entry.runtime_config) {
      args.push("--runtime-config", path.resolve(root, entry.runtime_config));
    }
    const result = runJson("cargo", args);
    const row = summarize(entry, iterations, result);
    rows.push(row);
    writeFileSync(path.join(outDir, `${entry.id}.json`), JSON.stringify({ entry, result }, null, 2));
  } catch (error) {
    if (entry.optional) {
      rows.push({ id: entry.id, title: entry.title, optional: true, skipped: true, reason: String(error.message ?? error) });
      continue;
    }
    throw error;
  }
}

const report = {
  generated_at: new Date().toISOString(),
  include_optional: includeOptional,
  rows,
};
writeFileSync(path.join(outDir, "summary.json"), JSON.stringify(report, null, 2));
writeFileSync(path.join(outDir, "summary.md"), markdown(report));
console.log(markdown(report));

function compileRecipe(entry, outDir) {
  const compiled = runJson("cargo", [
    "run",
    "--quiet",
    "-p",
    "tool-compiler-cli",
    "--",
    "compile-recipe",
    path.resolve(root, entry.recipe),
  ]);
  const planPath = path.join(outDir, `${entry.id}.plan.json`);
  writeFileSync(planPath, JSON.stringify(compiled, null, 2));
  return planPath;
}

function runJson(command, args) {
  const proc = spawnSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (proc.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed\n${proc.stderr || proc.stdout}`);
  }
  const start = proc.stdout.indexOf("{");
  if (start < 0) {
    throw new Error(`command produced no JSON\n${proc.stdout}\n${proc.stderr}`);
  }
  return JSON.parse(proc.stdout.slice(start));
}

function summarize(entry, iterations, result) {
  const beforeCalls = result.baseline.estimated_tool_calls;
  const afterCalls = result.compiled.estimated_tool_calls;
  const beforeTurns = result.baseline.estimated_llm_turns;
  const afterTurns = result.compiled.estimated_llm_turns;
  return {
    id: entry.id,
    title: entry.title,
    description: entry.description,
    iterations,
    baseline_ms: Number(result.baseline.elapsed_ms),
    compiled_ms: Number(result.compiled.elapsed_ms),
    saved_ms: Number(result.saved_ms),
    speedup: Number(result.speedup),
    calls_before: beforeCalls,
    calls_after: afterCalls,
    turns_before: beforeTurns,
    turns_after: afterTurns,
    cache_hits: result.compiled.cache_hits,
    trace_events: result.compiled.trace_events,
  };
}

function markdown(report) {
  const lines = [
    "# Benchmark Suite",
    "",
    `Generated: ${report.generated_at}`,
    "",
    "| Case | Iter | Baseline ms | Compiled ms | Speedup | Calls | Turns | Notes |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |",
  ];
  for (const row of report.rows) {
    if (row.skipped) {
      lines.push(`| ${row.id} | - | - | - | - | - | - | skipped: ${row.reason} |`);
      continue;
    }
    lines.push(
      `| ${row.id} | ${row.iterations} | ${row.baseline_ms} | ${row.compiled_ms} | ${row.speedup.toFixed(2)}x | ${row.calls_before}->${row.calls_after} | ${row.turns_before}->${row.turns_after} | ${row.description ?? ""} |`,
    );
  }
  lines.push("");
  return `${lines.join("\n")}\n`;
}
