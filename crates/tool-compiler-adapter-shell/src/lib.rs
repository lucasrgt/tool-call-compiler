//! Process adapter: direct program execution, no shell interpolation.
//!
//! The `run` tool spawns `program` with `args`, optional `stdin`, an
//! optional per-call `cwd` and `timeout_ms`, and returns status, stdout, and
//! stderr (both capped). On Windows, bare program names are retried with
//! launcher-shim extensions (`.cmd`, `.exe`, `.bat`), so `npm`-style shims
//! work.
//!
//! Environment hygiene: by default the child inherits the parent
//! environment. Use [`ShellExecutor::with_env_clear`] to start from an empty
//! environment and [`ShellExecutor::with_inherited_var`] to allowlist
//! specific parent variables (`PATH` is almost always needed) — that is the
//! configuration that keeps parent secrets away from arbitrary commands.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tool_compiler_adapter_api::process::command_candidates;
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{Effects, ToolSpec};

/// Conventional adapter name for shell executors.
pub const ADAPTER: &str = "shell";

/// Default cap applied to captured stdout and stderr, each.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Shell executor configuration.
#[derive(Clone, Debug, Default)]
pub struct ShellExecutor {
    cwd: Option<PathBuf>,
    env: BTreeMap<String, String>,
    env_clear: bool,
    inherit: Vec<String>,
    default_timeout: Option<Duration>,
    max_output_bytes: usize,
}

impl ShellExecutor {
    /// Creates an executor with inherited environment and no timeout.
    pub fn new() -> Self {
        Self {
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            ..Self::default()
        }
    }

    /// Sets the default working directory for commands.
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds one environment variable visible to every command.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Starts children from an empty environment instead of inheriting the
    /// parent's (explicit `with_env` values and the inherit allowlist still
    /// apply).
    pub fn with_env_clear(mut self, clear: bool) -> Self {
        self.env_clear = clear;
        self
    }

    /// Allowlists a parent environment variable to survive `env_clear`.
    pub fn with_inherited_var(mut self, key: impl Into<String>) -> Self {
        self.inherit.push(key.into());
        self
    }

    /// Sets a default timeout for commands that pass no `timeout_ms`.
    pub fn with_default_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.default_timeout = Some(Duration::from_millis(timeout_ms));
        self
    }

    /// Caps captured stdout/stderr (each) at `max_bytes`.
    pub fn with_max_output_bytes(mut self, max_bytes: usize) -> Self {
        self.max_output_bytes = max_bytes.max(1);
        self
    }
}

#[async_trait]
impl ToolExecutor for ShellExecutor {
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        match tool {
            "run" => run_command(self, input).await.map_err(tool_error),
            other => Err(
                ToolExecutionError::new(format!("unknown shell tool '{other}'"))
                    .with_code("unknown_tool"),
            ),
        }
    }
}

/// ToolSpec for `run`.
///
/// A shell command is arbitrary side effects, so the default declaration is
/// deliberately conservative: it writes the shared `shell:world` resource,
/// which serializes shell commands with each other and with anything else
/// declaring that resource. Callers that know a command is safe should
/// declare tighter effects themselves instead of weakening this default.
pub fn run_tool() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: ["shell:world"].into_iter().map(String::from).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

async fn run_command(executor: &ShellExecutor, input: Value) -> Result<Value, ShellError> {
    let program = required_str(&input, "program")?;
    let args = string_array(&input, "args")?;
    let stdin_payload = input
        .get("stdin")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let timeout = input
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .or(executor.default_timeout);
    let cwd = input
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|per_call| match (&executor.cwd, per_call.is_absolute()) {
            (Some(base), false) => base.join(per_call),
            _ => per_call,
        })
        .or_else(|| executor.cwd.clone());

    let mut child = spawn(
        executor,
        program,
        &args,
        cwd.as_deref(),
        stdin_payload.is_some(),
    )?;

    if let Some(payload) = stdin_payload
        && let Some(mut stdin) = child.stdin.take()
    {
        stdin.write_all(payload.as_bytes()).await?;
        drop(stdin);
    }

    let waited = match timeout {
        Some(limit) => match tokio::time::timeout(limit, child.wait_with_output()).await {
            Ok(output) => output?,
            Err(_) => {
                // wait_with_output consumed the child; the kill_on_drop flag
                // set at spawn time reaps it. Report the timeout.
                return Err(ShellError::TimedOut(limit.as_millis() as u64));
            }
        },
        None => child.wait_with_output().await?,
    };

    let (stdout, stdout_truncated) = cap_output(&waited.stdout, executor.max_output_bytes);
    let (stderr, stderr_truncated) = cap_output(&waited.stderr, executor.max_output_bytes);
    let mut output = json!({
        "status": waited.status.code(),
        "success": waited.status.success(),
        "stdout": stdout,
        "stderr": stderr,
    });
    if stdout_truncated {
        output["stdout_truncated"] = json!(true);
    }
    if stderr_truncated {
        output["stderr_truncated"] = json!(true);
    }
    Ok(output)
}

fn spawn(
    executor: &ShellExecutor,
    program: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
    piped_stdin: bool,
) -> Result<tokio::process::Child, ShellError> {
    let mut last_error: Option<std::io::Error> = None;

    for candidate in command_candidates(program) {
        let mut command = Command::new(&candidate);
        command.args(args);
        if executor.env_clear {
            command.env_clear();
            for key in &executor.inherit {
                if let Ok(value) = std::env::var(key) {
                    command.env(key, value);
                }
            }
        }
        command.envs(&executor.env);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                last_error = Some(error);
            }
            Err(error) => return Err(error.into()),
        }
    }

    Err(last_error.map(ShellError::Io).unwrap_or_else(|| {
        ShellError::Io(std::io::Error::other(format!(
            "program not found: {program}"
        )))
    }))
}

fn cap_output(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    if bytes.len() <= max_bytes {
        (String::from_utf8_lossy(bytes).into_owned(), false)
    } else {
        (
            String::from_utf8_lossy(&bytes[..max_bytes]).into_owned(),
            true,
        )
    }
}

fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, ShellError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ShellError::MissingField(key.into()))
}

fn string_array(input: &Value, key: &str) -> Result<Vec<String>, ShellError> {
    input
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_owned)
                        .ok_or(ShellError::InvalidArgs)
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn tool_error(error: ShellError) -> ToolExecutionError {
    let code = match &error {
        ShellError::MissingField(_) | ShellError::InvalidArgs => "invalid_input",
        ShellError::TimedOut(_) => "timeout",
        ShellError::Io(_) => "io",
    };
    ToolExecutionError::new(error.to_string()).with_code(code)
}

/// Shell adapter errors.
#[derive(Debug, Error)]
pub enum ShellError {
    /// A required string field is missing from the input.
    #[error("missing string field '{0}'")]
    MissingField(String),
    /// `args` must be an array of strings.
    #[error("args must be an array of strings")]
    InvalidArgs,
    /// The command exceeded its timeout and was killed.
    #[error("command timed out after {0}ms")]
    TimedOut(u64),
    /// Underlying I/O error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn runs_program_without_shell_string() {
        let output = ShellExecutor::new()
            .call(
                "run",
                json!({
                    "program": "rustc",
                    "args": ["--version"]
                }),
            )
            .await
            .unwrap();

        assert_eq!(output["success"], true);
        assert!(output["stdout"].as_str().unwrap().contains("rustc"));
    }

    #[tokio::test]
    async fn rejects_invalid_args() {
        let error = ShellExecutor::new()
            .call("run", json!({ "program": "rustc", "args": [1] }))
            .await
            .unwrap_err();

        assert_eq!(error.code.as_deref(), Some("invalid_input"));
    }

    #[tokio::test]
    async fn pipes_stdin_to_the_child() {
        let echo_stdin = if cfg!(windows) {
            json!({ "program": "cmd", "args": ["/C", "more"], "stdin": "hello stdin\n" })
        } else {
            json!({ "program": "cat", "stdin": "hello stdin\n" })
        };

        let output = ShellExecutor::new().call("run", echo_stdin).await.unwrap();

        assert_eq!(output["success"], true);
        assert!(output["stdout"].as_str().unwrap().contains("hello stdin"));
    }

    #[tokio::test]
    async fn kills_commands_past_their_timeout() {
        let slow = if cfg!(windows) {
            json!({ "program": "ping", "args": ["-n", "10", "127.0.0.1"], "timeout_ms": 100 })
        } else {
            json!({ "program": "sleep", "args": ["5"], "timeout_ms": 100 })
        };

        let error = ShellExecutor::new().call("run", slow).await.unwrap_err();

        assert_eq!(error.code.as_deref(), Some("timeout"));
    }

    #[tokio::test]
    async fn caps_captured_output() {
        let executor = ShellExecutor::new().with_max_output_bytes(8);
        let output = executor
            .call("run", json!({ "program": "rustc", "args": ["--version"] }))
            .await
            .unwrap();

        assert_eq!(output["stdout_truncated"], true);
        assert!(output["stdout"].as_str().unwrap().len() <= 8);
    }

    #[tokio::test]
    async fn env_clear_hides_parent_variables() {
        // SAFETY: test-local variable name; no other test reads it.
        unsafe { std::env::set_var("TOOL_COMPILER_SECRET", "leak me") };
        let program = if cfg!(windows) { "cmd" } else { "sh" };
        let args = if cfg!(windows) {
            vec!["/C".to_owned(), "set".to_owned()]
        } else {
            vec!["-c".to_owned(), "env".to_owned()]
        };

        let executor = ShellExecutor::new()
            .with_env_clear(true)
            .with_inherited_var("PATH")
            .with_inherited_var("SystemRoot");
        let output = executor
            .call("run", json!({ "program": program, "args": args }))
            .await
            .unwrap();

        let stdout = output["stdout"].as_str().unwrap();
        assert!(!stdout.contains("TOOL_COMPILER_SECRET"));
    }

    #[test]
    fn run_tool_serializes_by_default() {
        let effects = run_tool().effects.unwrap();

        assert!(effects.writes.contains("shell:world"));
        assert!(!effects.cacheable);
        assert!(effects.declares_side_effects());
    }
}
