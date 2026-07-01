//! Command implementations for the `tool-compiler` binary.
//!
//! Everything lives in this library (the binary is a two-line shim) so the
//! command surface — including the MCP server and the benchmark math — is
//! unit-testable and counted by coverage.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;
use thiserror::Error;
use tool_compiler_graph::validate;
use tool_compiler_ir::Plan;
use tool_compiler_optimizer::{explain, optimize};
use tool_compiler_planner::{
    DEFAULT_MIN_OCCURRENCES, IntentPlan, ObservedCall, RecipePlan, compile_intent,
    compile_recipe_with_params, suggest_recipes,
};
use tool_compiler_runtime::{
    ConformanceOptions, ErrorMode, ResultMode, RunConfig, check_adapter_conformance_with,
};

mod bench;
mod config;
mod local;
mod serve;

pub use bench::{BenchResult, BenchSide, bench};
pub use config::{configured_runtime, parse_params};
pub use local::LocalExecutor;
pub use serve::{MAX_DEPTH, SERVED_TOOLS, ServeState, handle_line, handle_message, serve};

/// Errors surfaced by CLI commands.
#[derive(Debug, Error)]
pub enum CliError {
    /// Reading an input file failed.
    #[error("failed to read '{path}': {source}")]
    Io {
        /// The offending path.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Parsing an input file failed.
    #[error("failed to parse '{path}': {message}")]
    Parse {
        /// The offending path.
        path: String,
        /// The parser message.
        message: String,
    },
    /// A configuration file problem.
    #[error("runtime config '{path}': {message}")]
    Config {
        /// The offending path.
        path: String,
        /// The failure detail.
        message: String,
    },
    /// A malformed command-line input.
    #[error("{0}")]
    Input(String),
    /// Plan validation failed.
    #[error(transparent)]
    Graph(#[from] tool_compiler_graph::GraphError),
    /// Intent/recipe lowering failed.
    #[error(transparent)]
    Planner(#[from] tool_compiler_planner::PlannerError),
    /// Execution failed at the infrastructure level.
    #[error(transparent)]
    Runtime(#[from] tool_compiler_runtime::RuntimeError),
    /// Serializing an output failed.
    #[error("failed to serialize output: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl CliError {
    pub(crate) fn config(path: &Path, message: impl Into<String>) -> Self {
        Self::Config {
            path: path.display().to_string(),
            message: message.into(),
        }
    }
}

/// Reads a file, or stdin when the path is `-`.
pub fn read_input(path: &Path) -> Result<String, CliError> {
    if path.as_os_str() == "-" {
        let mut content = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut content).map_err(|source| {
            CliError::Io {
                path: "<stdin>".into(),
                source,
            }
        })?;
        return Ok(content);
    }
    std::fs::read_to_string(path).map_err(|source| CliError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let content = read_input(path)?;
    serde_json::from_str(&content).map_err(|error| CliError::Parse {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

/// The `tool-compiler` command line.
#[derive(Debug, Parser)]
#[command(author, version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum ResultModeArg {
    Full,
    Compact,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum OnErrorArg {
    FailFast,
    Continue,
}

#[derive(Debug, clap::Args)]
struct RunArgs {
    #[arg(long = "runtime-config", alias = "mcp-config")]
    runtime_config: Option<PathBuf>,
    /// How much detail the result carries.
    #[arg(long, value_enum, default_value = "full")]
    result_mode: ResultModeArg,
    /// Failure handling mode.
    #[arg(long, value_enum, default_value = "fail-fast")]
    on_error: OnErrorArg,
    /// Timeout applied to calls whose effects declare none, in milliseconds.
    #[arg(long)]
    timeout_ms: Option<u64>,
    /// Per-output byte budget; larger outputs are truncated with a marker.
    #[arg(long)]
    max_output_bytes: Option<usize>,
}

impl RunArgs {
    fn config(&self) -> RunConfig {
        let mut config = RunConfig::new();
        if matches!(self.result_mode, ResultModeArg::Compact) {
            config = config.with_result_mode(ResultMode::Compact);
        }
        if matches!(self.on_error, OnErrorArg::Continue) {
            config = config.with_on_error(ErrorMode::Continue);
        }
        if let Some(timeout) = self.timeout_ms {
            config = config.with_default_timeout_ms(timeout);
        }
        if let Some(max_bytes) = self.max_output_bytes {
            config = config.with_max_output_bytes(max_bytes);
        }
        config
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate a plan file (use '-' to read stdin).
    Validate { plan: PathBuf },
    /// Print a plan's parallel execution layers.
    Layers { plan: PathBuf },
    /// Print the optimized plan.
    Optimize { plan: PathBuf },
    /// Print the optimization report and parallelism diagnostics.
    Explain { plan: PathBuf },
    /// Compile an intent into an executable plan.
    CompileIntent { intent: PathBuf },
    /// Compile a recipe into an executable plan.
    CompileRecipe {
        recipe: PathBuf,
        /// Recipe parameter values as key=value (values parse as JSON).
        #[arg(long = "param")]
        params: Vec<String>,
    },
    /// Compile and execute an intent.
    RunIntent {
        intent: PathBuf,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Compile and execute a recipe.
    RunRecipe {
        recipe: PathBuf,
        /// Recipe parameter values as key=value (values parse as JSON).
        #[arg(long = "param")]
        params: Vec<String>,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Execute a plan.
    Run {
        plan: PathBuf,
        #[command(flatten)]
        run: RunArgs,
    },
    /// Compare the serial baseline against compiled execution.
    Bench {
        plan: PathBuf,
        #[arg(short, long, default_value_t = 3)]
        iterations: u32,
        /// Unmeasured warmup iterations per side.
        #[arg(long, default_value_t = 1)]
        warmup: u32,
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
    /// Run the adapter conformance suite against a configured adapter.
    Conformance {
        /// Adapter name from the runtime config.
        adapter: String,
        /// Tool that echoes its input.
        #[arg(long, default_value = "echo")]
        echo_tool: String,
        /// Tool that always fails (enables the error-propagation check).
        #[arg(long)]
        failing_tool: Option<String>,
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
    /// Mine a transcript of observed tool calls for compiler opportunities.
    Suggest {
        /// JSON array of {"tool", "input"} observed calls ('-' for stdin).
        calls: PathBuf,
        /// Minimum consecutive same-tool calls worth suggesting.
        #[arg(long, default_value_t = DEFAULT_MIN_OCCURRENCES)]
        min_occurrences: usize,
    },
    /// Serve the compiler as an MCP stdio server.
    ServeMcp {
        #[arg(long = "runtime-config", alias = "mcp-config")]
        runtime_config: Option<PathBuf>,
    },
}

/// Parses arguments from the environment and executes the command.
pub async fn run() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

async fn execute(cli: Cli) -> Result<(), CliError> {
    match cli.command {
        Command::Validate { plan } => {
            let plan: Plan = read_json(&plan)?;
            validate(&plan)?;
            println!("valid");
        }
        Command::Layers { plan } => {
            let plan: Plan = read_json(&plan)?;
            let graph = validate(&plan)?;
            print_json(&graph.layers().to_vec())?;
        }
        Command::Optimize { plan } => {
            let plan: Plan = read_json(&plan)?;
            print_json(optimize(plan)?.plan())?;
        }
        Command::Explain { plan } => {
            let plan: Plan = read_json(&plan)?;
            print_json(&explain(plan)?)?;
        }
        Command::CompileIntent { intent } => {
            let intent: IntentPlan = read_json(&intent)?;
            print_json(&compile_intent(intent)?)?;
        }
        Command::CompileRecipe { recipe, params } => {
            let recipe: RecipePlan = read_json(&recipe)?;
            let params = parse_params(&params)?;
            print_json(&compile_recipe_with_params(recipe, params)?)?;
        }
        Command::RunIntent { intent, run } => {
            let intent: IntentPlan = read_json(&intent)?;
            let plan = compile_intent(intent)?;
            let runtime = configured_runtime(run.runtime_config.clone()).await?;
            print_json(&runtime.run_with(plan, run.config()).await?)?;
        }
        Command::RunRecipe {
            recipe,
            params,
            run,
        } => {
            let recipe: RecipePlan = read_json(&recipe)?;
            let params = parse_params(&params)?;
            let plan = compile_recipe_with_params(recipe, params)?;
            let runtime = configured_runtime(run.runtime_config.clone()).await?;
            print_json(&runtime.run_with(plan, run.config()).await?)?;
        }
        Command::Run { plan, run } => {
            let plan: Plan = read_json(&plan)?;
            let runtime = configured_runtime(run.runtime_config.clone()).await?;
            print_json(&runtime.run_with(plan, run.config()).await?)?;
        }
        Command::Bench {
            plan,
            iterations,
            warmup,
            runtime_config,
        } => {
            let plan: Plan = read_json(&plan)?;
            let runtime = configured_runtime(runtime_config).await?;
            print_json(&bench(&runtime, &plan, iterations, warmup).await?)?;
        }
        Command::Conformance {
            adapter,
            echo_tool,
            failing_tool,
            runtime_config,
        } => {
            let runtime = configured_runtime(runtime_config).await?;
            let executor = runtime
                .registry()
                .executor_for(&adapter)
                .ok_or_else(|| CliError::Input(format!("unknown adapter '{adapter}'")))?;
            let mut options = ConformanceOptions::new().with_echo_tool(echo_tool);
            if let Some(failing_tool) = failing_tool {
                options = options.with_failing_tool(failing_tool);
            }
            let report = check_adapter_conformance_with(&adapter, executor, options).await;
            print_json(&report)?;
            if !report.passed {
                return Err(CliError::Input(format!(
                    "adapter '{adapter}' failed conformance"
                )));
            }
        }
        Command::Suggest {
            calls,
            min_occurrences,
        } => {
            let calls: Vec<ObservedCall> = read_json(&calls)?;
            print_json(&suggest_recipes(&calls, min_occurrences))?;
        }
        Command::ServeMcp { runtime_config } => {
            let runtime = configured_runtime(runtime_config).await?;
            serve(runtime).await?;
        }
    }

    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_run_flags() {
        let cli = Cli::parse_from([
            "tool-compiler",
            "run",
            "plan.json",
            "--result-mode",
            "compact",
            "--on-error",
            "continue",
            "--timeout-ms",
            "5000",
        ]);

        let Command::Run { run, .. } = cli.command else {
            panic!("expected run command");
        };
        let config = run.config();
        assert_eq!(
            config.result_mode,
            tool_compiler_runtime::ResultMode::Compact
        );
        assert_eq!(config.on_error, ErrorMode::Continue);
        assert_eq!(config.default_timeout_ms, Some(5000));
    }

    #[test]
    fn read_input_reports_the_missing_path() {
        let error = read_input(Path::new("definitely-missing.json")).unwrap_err();

        assert!(error.to_string().contains("definitely-missing.json"));
    }
}
