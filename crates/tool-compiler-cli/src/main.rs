use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use tool_compiler_graph::validate;
use tool_compiler_ir::Plan;
use tool_compiler_optimizer::{explain, optimize};
use tool_compiler_runtime::{Runtime, ToolExecutionError, ToolExecutor, ToolRegistry};

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
    Run {
        plan: PathBuf,
    },
    Bench {
        plan: PathBuf,
        #[arg(short, long, default_value_t = 3)]
        iterations: u32,
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
        Command::Run { plan } => {
            let plan = read_plan(plan)?;
            let result = local_runtime().run(plan).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Bench { plan, iterations } => {
            let plan = read_plan(plan)?;
            let runtime = local_runtime();
            let iterations = iterations.max(1);
            let baseline_ms = bench(&runtime, &plan, iterations, false).await?;
            let compiled_ms = bench(&runtime, &plan, iterations, true).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&BenchResult {
                    iterations,
                    baseline_ms,
                    compiled_ms,
                })?
            );
        }
    }

    Ok(())
}

fn read_plan(path: PathBuf) -> Result<Plan, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn local_runtime() -> Runtime {
    Runtime::from_registry(ToolRegistry::new().with_adapter("local", LocalExecutor))
}

async fn bench(
    runtime: &Runtime,
    plan: &Plan,
    iterations: u32,
    compiled: bool,
) -> Result<u128, Box<dyn Error>> {
    let started = Instant::now();
    for _ in 0..iterations {
        if compiled {
            runtime.run(plan.clone()).await?;
        } else {
            runtime.run_serial(plan.clone()).await?;
        }
    }
    Ok(started.elapsed().as_millis())
}

#[derive(Serialize)]
struct BenchResult {
    iterations: u32,
    baseline_ms: u128,
    compiled_ms: u128,
}

struct LocalExecutor;

#[async_trait]
impl ToolExecutor for LocalExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        if let Some(ms) = input.get("sleep_ms").and_then(Value::as_u64) {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }

        match tool {
            "const" => Ok(input.get("value").cloned().unwrap_or(input)),
            "echo" | "write" => Ok(input),
            "fail" => Err(ToolExecutionError::new("local fail tool was called")),
            other => Ok(json!({ "tool": other, "input": input })),
        }
    }
}
