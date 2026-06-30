use std::error::Error;
use std::fs;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tool_compiler_graph::validate;
use tool_compiler_ir::Plan;
use tool_compiler_optimizer::optimize;

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Validate { plan: PathBuf },
    Layers { plan: PathBuf },
    Optimize { plan: PathBuf },
}

fn main() -> Result<(), Box<dyn Error>> {
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
    }

    Ok(())
}

fn read_plan(path: PathBuf) -> Result<Plan, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}
