use std::error::Error;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tool_compiler_adapter_fs::FsExecutor;
use tool_compiler_adapter_mcp::{McpExecutor, McpServerConfig, McpStdioClient, McpTransport};
use tool_compiler_adapter_shell::ShellExecutor;
use tool_compiler_graph::validate;
use tool_compiler_ir::Plan;
use tool_compiler_optimizer::{explain, optimize};
use tool_compiler_planner::{IntentPlan, compile_intent};
use tool_compiler_runtime::{
    BatchInput, BatchOutput, RunResult, Runtime, ToolExecutionError, ToolExecutor, ToolRegistry,
    TraceStatus,
};

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Validate {
        plan: PathBuf,
    },
    Layers {
        plan: PathBuf,
    },
    Optimize {
        plan: PathBuf,
    },
    Explain {
        plan: PathBuf,
    },
    CompileIntent {
        intent: PathBuf,
    },
    RunIntent {
        intent: PathBuf,
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
    Run {
        plan: PathBuf,
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
    Bench {
        plan: PathBuf,
        #[arg(short, long, default_value_t = 3)]
        iterations: u32,
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
    ServeMcp {
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Validate { plan } => {
            let plan = read_plan(plan)?;
            validate(&plan)?;
            println!("valid");
        }
        Command::Layers { plan } => {
            let plan = read_plan(plan)?;
            let graph = validate(&plan)?;
            println!("{}", serde_json::to_string_pretty(graph.layers())?);
        }
        Command::Optimize { plan } => {
            let plan = read_plan(plan)?;
            let optimized = optimize(plan)?;
            println!("{}", serde_json::to_string_pretty(optimized.plan())?);
        }
        Command::Explain { plan } => {
            let plan = read_plan(plan)?;
            println!("{}", serde_json::to_string_pretty(&explain(plan)?)?);
        }
        Command::CompileIntent { intent } => {
            let intent = read_intent(intent)?;
            let plan = compile_intent(intent)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::RunIntent {
            intent,
            runtime_config,
        } => {
            let intent = read_intent(intent)?;
            let plan = compile_intent(intent)?;
            let result = configured_runtime(runtime_config)?.run(plan).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Run {
            plan,
            runtime_config,
        } => {
            let plan = read_plan(plan)?;
            let result = configured_runtime(runtime_config)?.run(plan).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Bench {
            plan,
            iterations,
            runtime_config,
        } => {
            let plan = read_plan(plan)?;
            let iterations = iterations.max(1);
            let baseline = configured_runtime(runtime_config.clone())?;
            let compiled = configured_runtime(runtime_config)?;
            let baseline = bench(&baseline, &plan, iterations, false).await?;
            let compiled = bench(&compiled, &plan, iterations, true).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&BenchResult {
                    iterations,
                    saved_ms: baseline.elapsed_ms as i128 - compiled.elapsed_ms as i128,
                    speedup: speedup(baseline.elapsed_ms, compiled.elapsed_ms),
                    baseline,
                    compiled,
                })?
            );
        }
        Command::ServeMcp { runtime_config } => {
            serve_mcp(configured_runtime(runtime_config)?).await?;
        }
    }

    Ok(())
}

fn read_plan(path: PathBuf) -> Result<Plan, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn read_intent(path: PathBuf) -> Result<IntentPlan, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn configured_runtime(mcp_config: Option<PathBuf>) -> Result<Runtime, Box<dyn Error>> {
    let mut registry = ToolRegistry::new().with_adapter("local", LocalExecutor);

    if let Some(path) = mcp_config {
        let content = fs::read_to_string(path)?;
        let config: RuntimeConfig = serde_json::from_str(&content)?;
        for server in config.mcp {
            let client = McpStdioClient::new(McpServerConfig {
                name: server.name,
                transport: McpTransport::Stdio {
                    command: server.command,
                    args: server.args,
                    env: server.env,
                },
            });
            registry.register_adapter(server.adapter, McpExecutor::new(client));
        }
        for fs in config.fs {
            registry.register_adapter(fs.adapter, FsExecutor::new(fs.root));
        }
        for shell in config.shell {
            let mut executor = ShellExecutor::new();
            if let Some(cwd) = shell.cwd {
                executor = executor.with_cwd(cwd);
            }
            for (key, value) in shell.env {
                executor = executor.with_env(key, value);
            }
            registry.register_adapter(shell.adapter, executor);
        }
    }

    Ok(Runtime::from_registry(registry))
}

async fn bench(
    runtime: &Runtime,
    plan: &Plan,
    iterations: u32,
    compiled: bool,
) -> Result<BenchStats, Box<dyn Error>> {
    let started = Instant::now();
    let mut estimated_tool_calls = 0;
    let mut estimated_llm_turns = 0;
    let mut trace_events = 0;
    let mut cache_hits = 0;

    for _ in 0..iterations {
        runtime.clear_cache().await;
        let result = if compiled {
            runtime.run(plan.clone()).await?
        } else {
            runtime.run_serial(plan.clone()).await?
        };
        let stats = run_stats(&result, compiled);
        estimated_tool_calls += stats.estimated_tool_calls;
        estimated_llm_turns += stats.estimated_llm_turns;
        trace_events += stats.trace_events;
        cache_hits += stats.cache_hits;
    }
    Ok(BenchStats {
        elapsed_ms: started.elapsed().as_millis(),
        estimated_tool_calls,
        estimated_llm_turns,
        trace_events,
        cache_hits,
    })
}

#[derive(Serialize)]
struct BenchResult {
    iterations: u32,
    baseline: BenchStats,
    compiled: BenchStats,
    saved_ms: i128,
    speedup: f64,
}

#[derive(Default, Serialize)]
struct BenchStats {
    elapsed_ms: u128,
    estimated_tool_calls: usize,
    estimated_llm_turns: usize,
    trace_events: usize,
    cache_hits: usize,
}

fn run_stats(result: &RunResult, compiled: bool) -> BenchStats {
    let summary = &result.optimization.summary;
    BenchStats {
        elapsed_ms: 0,
        estimated_tool_calls: if compiled {
            summary.estimated_tool_calls_after
        } else {
            result.node_outputs.len()
        },
        estimated_llm_turns: if compiled {
            summary.estimated_llm_turns_after
        } else {
            result.node_outputs.len()
        },
        trace_events: result.trace.len(),
        cache_hits: result
            .trace
            .iter()
            .filter(|event| event.status == TraceStatus::CacheHit)
            .count(),
    }
}

fn speedup(baseline_ms: u128, compiled_ms: u128) -> f64 {
    if compiled_ms == 0 {
        return 0.0;
    }
    baseline_ms as f64 / compiled_ms as f64
}

#[derive(Deserialize)]
struct RuntimeConfig {
    #[serde(default)]
    mcp: Vec<McpBinding>,
    #[serde(default)]
    fs: Vec<FsBinding>,
    #[serde(default)]
    shell: Vec<ShellBinding>,
}

#[derive(Deserialize)]
struct McpBinding {
    adapter: String,
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct FsBinding {
    adapter: String,
    root: PathBuf,
}

#[derive(Deserialize)]
struct ShellBinding {
    adapter: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

async fn serve_mcp(runtime: Runtime) -> Result<(), Box<dyn Error>> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = handle_mcp_message(&runtime, serde_json::from_str(&line)?).await {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }

    Ok(())
}

async fn handle_mcp_message(runtime: &Runtime, request: Value) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    if id.is_none() {
        return None;
    }

    let id = id.unwrap();
    match method {
        "initialize" => Some(mcp_result(id, initialize_result())),
        "tools/list" => Some(mcp_result(id, tools_list_result())),
        "tools/call" => match call_mcp_tool(runtime, &request).await {
            Ok(result) => Some(mcp_result(id, result)),
            Err(message) => Some(mcp_error(id, -32603, message)),
        },
        _ => Some(mcp_error(id, -32601, format!("unknown method '{method}'"))),
    }
}

async fn call_mcp_tool(runtime: &Runtime, request: &Value) -> Result<Value, String> {
    let params = request
        .get("params")
        .ok_or_else(|| "tools/call missing params".to_owned())?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "tools/call missing name".to_owned())?;
    if name != "run_compiled_tool_graph" {
        return Err(format!("unknown tool '{name}'"));
    }

    let plan = params
        .get("arguments")
        .and_then(|arguments| arguments.get("plan"))
        .cloned()
        .ok_or_else(|| "run_compiled_tool_graph requires arguments.plan".to_owned())?;
    let plan: Plan = serde_json::from_value(plan).map_err(|error| error.to_string())?;
    let result = runtime.run(plan).await.map_err(|error| error.to_string())?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
        }],
        "structuredContent": result,
        "isError": false
    }))
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "tool-call-compiler",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tools_list_result() -> Value {
    json!({
        "tools": [{
            "name": "run_compiled_tool_graph",
            "title": "Run compiled tool graph",
            "description": "Execute a tool-call-compiler Plan as one composite tool call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "plan": { "type": "object" }
                },
                "required": ["plan"]
            }
        }]
    })
}

fn mcp_result(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn mcp_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into()
        }
    })
}

struct LocalExecutor;

#[async_trait]
impl ToolExecutor for LocalExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        if let Some(ms) = input.get("sleep_ms").and_then(Value::as_u64) {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }

        local_output(tool, input)
    }

    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        let max_sleep_ms = inputs
            .iter()
            .filter_map(|input| input.input.get("sleep_ms").and_then(Value::as_u64))
            .max()
            .unwrap_or(0);
        if max_sleep_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(max_sleep_ms)).await;
        }

        inputs
            .into_iter()
            .map(|input| {
                Ok(BatchOutput {
                    node: input.node,
                    output: local_output(tool, input.input)?,
                })
            })
            .collect()
    }
}

fn local_output(tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
    match tool {
        "const" => Ok(input.get("value").cloned().unwrap_or(input)),
        "echo" | "write" => Ok(input),
        "fail" => Err(ToolExecutionError::new("local fail tool was called")),
        other => Ok(json!({ "tool": other, "input": input })),
    }
}
