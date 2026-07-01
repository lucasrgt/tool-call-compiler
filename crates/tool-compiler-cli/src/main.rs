//! Thin binary entry point; every command lives in the testable library.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tool_compiler_cli::run().await
}
