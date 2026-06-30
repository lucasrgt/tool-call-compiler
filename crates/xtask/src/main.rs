use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const MAX_RUST_FILE_LINES: usize = 500;
const DEFAULT_MIN_LINE_COVERAGE: u8 = 95;

fn main() -> ExitCode {
    let root = env::current_dir().expect("current directory should be readable");
    let command = env::args().nth(1).unwrap_or_else(|| "check".to_owned());

    let result = match command.as_str() {
        "check" => check(&root),
        "loc" => check_loc(&root),
        "coverage" => coverage(&root),
        "test" => test(&root),
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
    check_loc(root)?;
    fmt(root)?;
    test(root)?;
    coverage(root)
}

fn check_loc(root: &Path) -> Result<(), String> {
    let rust_files = collect_rust_files(root);
    let mut errors = Vec::new();

    for file in rust_files {
        let content = read_to_string(&file)?;
        let lines = production_line_count(&content);
        if lines > MAX_RUST_FILE_LINES {
            errors.push(format!(
                "{} has {lines} production lines; max is {MAX_RUST_FILE_LINES}",
                display(&file)
            ));
        }
    }

    if errors.is_empty() {
        println!("xtask loc: passed");
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn fmt(root: &Path) -> Result<(), String> {
    run(root, "cargo", &["fmt", "--all", "--", "--check"])
}

fn test(root: &Path) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command
        .args(["test", "--workspace", "--lib", "--tests"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", inner_target_dir(root));
    run_command(command, "cargo test")
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
            r"(crates[\\/]+xtask[\\/]+src[\\/]+main\.rs|crates[\\/]+tool-compiler-cli[\\/]+src[\\/]+main\.rs)",
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

fn run_command(mut command: Command, label: &str) -> Result<(), String> {
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

fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_rust_files_inner(root, &mut files);
    files
}

fn collect_rust_files_inner(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");

        if path.is_dir() {
            if matches!(
                name,
                ".git" | ".codegraph" | "target" | "node_modules" | "dist"
            ) {
                continue;
            }
            collect_rust_files_inner(&path, files);
        } else if path.extension().and_then(OsStr::to_str) == Some("rs") {
            files.push(path);
        }
    }
}

fn production_line_count(content: &str) -> usize {
    let lines: Vec<&str> = content.lines().collect();
    let mut count = 0;
    let mut index = 0;

    while index < lines.len() {
        if starts_cfg_test_module(&lines, index) {
            index = skip_braced_item(&lines, index);
        } else {
            count += 1;
            index += 1;
        }
    }

    count
}

fn starts_cfg_test_module(lines: &[&str], index: usize) -> bool {
    lines[index].trim() == "#[cfg(test)]"
        && lines
            .get(index + 1)
            .is_some_and(|line| line.trim_start().starts_with("mod tests"))
}

fn skip_braced_item(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut saw_open = false;

    for (offset, line) in lines[start..].iter().enumerate() {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' if saw_open => depth -= 1,
                _ => {}
            }
        }

        if saw_open && depth <= 0 {
            return start + offset + 1;
        }
    }

    lines.len()
}

fn inner_target_dir(root: &Path) -> PathBuf {
    root.join("target").join("xtask-inner")
}

fn read_to_string(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("failed to read {}: {error}", display(path)))
}

fn display(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
