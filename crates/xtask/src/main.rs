//! Repository quality gates: LOC ceiling, formatting, lints, tests,
//! coverage, example/schema validation, and release consistency checks.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod examples;
mod loc;
mod release;

const DEFAULT_MIN_LINE_COVERAGE: u8 = 95;

fn main() -> ExitCode {
    let root = env::current_dir().expect("current directory should be readable");
    let command = env::args().nth(1).unwrap_or_else(|| "check".to_owned());

    let result = match command.as_str() {
        "check" => check(&root),
        "loc" => loc::check_loc(&root),
        "fmt" => fmt(&root),
        "clippy" => clippy(&root),
        "coverage" => coverage(&root),
        "test" => test(&root),
        "examples" => examples::validate_examples(&root),
        "release-check" => release::release_check(&root),
        other => Err(format!("unknown xtask command '{other}'")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::from(1)
        }
    }
}

fn check(root: &Path) -> Result<(), String> {
    loc::check_loc(root)?;
    fmt(root)?;
    clippy(root)?;
    examples::validate_examples(root)?;
    release::release_check(root)?;
    test(root)?;
    coverage(root)
}

fn fmt(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["fmt", "--all", "--", "--check"])
}

fn clippy(root: &Path) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .args([
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", inner_target_dir(root));
    run_command(command, "cargo clippy")
}

fn test(root: &Path) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .args(["test", "--workspace", "--lib", "--tests"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", inner_target_dir(root));
    run_command(command, "cargo test")?;

    let mut doc = Command::new("cargo");
    doc.args(["test", "--workspace", "--doc"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", inner_target_dir(root));
    run_command(doc, "cargo test --doc")
}

fn coverage(root: &Path) -> Result<(), String> {
    let min = min_line_coverage()?;
    let mut command = Command::new("cargo");
    command
        .args([
            "llvm-cov",
            "--workspace",
            "--lib",
            "--tests",
            "--ignore-filename-regex",
            // The binary shim and the xtask itself are process entry points
            // exercised end-to-end, not unit-testable library code.
            r"(crates[\\/]+xtask[\\/]+src[\\/]+.*|crates[\\/]+tool-compiler-cli[\\/]+src[\\/]+main\.rs)",
            "--fail-under-lines",
            &min.to_string(),
        ])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", inner_target_dir(root));
    run_command(command, "cargo llvm-cov")
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<(), String> {
    let mut command = Command::new(program);
    command.args(args).current_dir(root);
    run_command(command, program)
}

pub(crate) fn run_command(mut command: Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("{label} failed to start: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} failed with status {status}"))
    }
}

fn min_line_coverage() -> Result<u8, String> {
    match env::var("TOOL_COMPILER_COVERAGE_MIN") {
        Ok(value) => value
            .parse::<u8>()
            .ok()
            .filter(|coverage| *coverage <= 100)
            .ok_or_else(|| "TOOL_COMPILER_COVERAGE_MIN must be 0..=100".to_owned()),
        Err(_) => Ok(DEFAULT_MIN_LINE_COVERAGE),
    }
}

fn inner_target_dir(root: &Path) -> PathBuf {
    root.join("target").join("xtask-inner")
}

pub(crate) fn read_to_string(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", display(path)))
}

pub(crate) fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
