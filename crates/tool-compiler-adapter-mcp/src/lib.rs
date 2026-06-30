use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tool_compiler_ir::{Effects, ToolSpec};
use tool_compiler_runtime::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};

pub const ADAPTER: &str = "mcp";
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
}

pub fn mcp_tool(effects: Effects) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(effects)
}

pub fn mcp_read_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    mcp_tool(Effects {
        batchable: true,
        ..Effects::read_only(resources)
    })
}

#[async_trait]
pub trait McpClient: Send + Sync {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError>;

    async fn call_tools(
        &self,
        calls: Vec<McpToolCall>,
    ) -> Result<Vec<McpToolResult>, McpClientError> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(McpToolResult {
                node: call.node,
                result: self.call_tool(&call.name, call.arguments).await?,
            });
        }
        Ok(results)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpToolCall {
    pub node: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpToolResult {
    pub node: String,
    pub result: Value,
}

#[derive(Clone)]
pub struct McpExecutor<C> {
    client: C,
}

impl<C> McpExecutor<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C> ToolExecutor for McpExecutor<C>
where
    C: McpClient + Send + Sync,
{
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        self.client
            .call_tool(tool, input)
            .await
            .map(normalize_tool_result)
            .map_err(tool_error)
    }

    async fn call_batch(
        &self,
        tool: &str,
        inputs: Vec<BatchInput>,
    ) -> Result<Vec<BatchOutput>, ToolExecutionError> {
        let calls = inputs
            .into_iter()
            .map(|input| McpToolCall {
                node: input.node,
                name: tool.to_owned(),
                arguments: input.input,
            })
            .collect();

        self.client
            .call_tools(calls)
            .await
            .map(|results| {
                results
                    .into_iter()
                    .map(|result| BatchOutput {
                        node: result.node,
                        output: normalize_tool_result(result.result),
                    })
                    .collect()
            })
            .map_err(tool_error)
    }
}

#[derive(Clone, Debug)]
pub struct McpStdioClient {
    config: McpServerConfig,
    next_id: std::sync::Arc<AtomicU64>,
    session: std::sync::Arc<Mutex<Option<McpStdioSession>>>,
}

#[derive(Debug)]
struct McpStdioSession {
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
}

impl McpStdioClient {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            next_id: Default::default(),
            session: Default::default(),
        }
    }
}

#[async_trait]
impl McpClient for McpStdioClient {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        let calls = vec![McpToolCall {
            node: "single".into(),
            name: name.to_owned(),
            arguments,
        }];
        let mut results = self.call_tools(calls).await?;
        results
            .pop()
            .map(|result| result.result)
            .ok_or_else(|| McpClientError::Protocol("server returned no tool result".into()))
    }

    async fn call_tools(
        &self,
        calls: Vec<McpToolCall>,
    ) -> Result<Vec<McpToolResult>, McpClientError> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            *guard = Some(start_session(&self.config, self.next_id()).await?);
        }
        let session = guard.as_mut().expect("session initialized above");

        let mut pending = BTreeMap::new();
        for call in &calls {
            let id = self.next_id();
            pending.insert(id, call.node.clone());
            write_json(
                &mut session.stdin,
                tool_call_request(id, &call.name, call.arguments.clone()),
            )
            .await?;
        }

        let mut results = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let (id, result) = read_response(&mut session.lines).await?;
            if let Some(node) = pending.remove(&id) {
                results.push(McpToolResult { node, result });
            }
        }

        Ok(results)
    }
}

async fn start_session(
    config: &McpServerConfig,
    init_id: u64,
) -> Result<McpStdioSession, McpClientError> {
    let McpTransport::Stdio { command, args, env } = &config.transport;
    let mut child = spawn_stdio(command, args, env)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| McpClientError::Transport("missing child stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| McpClientError::Transport("missing child stdout".into()))?;
    let mut lines = BufReader::new(stdout).lines();

    write_json(&mut stdin, initialize_request(init_id)).await?;
    expect_response(&mut lines, init_id).await?;
    write_json(&mut stdin, initialized_notification()).await?;

    Ok(McpStdioSession {
        _child: child,
        stdin,
        lines,
    })
}

fn spawn_stdio(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<tokio::process::Child, McpClientError> {
    let mut last_error = None;

    for candidate in command_candidates(command) {
        match Command::new(&candidate)
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == ErrorKind::NotFound => last_error = Some(error),
            Err(error) => return Err(McpClientError::Transport(error.to_string())),
        }
    }

    Err(McpClientError::Transport(
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| format!("program not found: {command}")),
    ))
}

fn command_candidates(command: &str) -> Vec<String> {
    let mut candidates = vec![command.to_owned()];
    #[cfg(windows)]
    {
        if !command.ends_with(".cmd") && !command.ends_with(".exe") {
            candidates.push(format!("{command}.cmd"));
            candidates.push(format!("{command}.exe"));
        }
    }
    candidates
}

impl McpStdioClient {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum McpClientError {
    #[error("mcp transport error: {0}")]
    Transport(String),
    #[error("mcp protocol error: {0}")]
    Protocol(String),
    #[error("mcp server error {code}: {message}")]
    Server { code: i64, message: String },
}

fn normalize_tool_result(result: Value) -> Value {
    if let Some(value) = result.get("structuredContent") {
        return value.clone();
    }
    result
}

fn tool_error(error: McpClientError) -> ToolExecutionError {
    ToolExecutionError::new(error.to_string())
}

fn initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "tool-call-compiler",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    })
}

fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
}

fn tool_call_request(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments
        }
    })
}

async fn write_json(
    stdin: &mut tokio::process::ChildStdin,
    message: Value,
) -> Result<(), McpClientError> {
    let line = serde_json::to_vec(&message)
        .map_err(|error| McpClientError::Protocol(error.to_string()))?;
    stdin
        .write_all(&line)
        .await
        .map_err(|error| McpClientError::Transport(error.to_string()))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| McpClientError::Transport(error.to_string()))
}

async fn expect_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    expected: u64,
) -> Result<Value, McpClientError> {
    loop {
        let (id, result) = read_response(lines).await?;
        if id == expected {
            return Ok(result);
        }
    }
}

async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<(u64, Value), McpClientError> {
    let line = lines
        .next_line()
        .await
        .map_err(|error| McpClientError::Transport(error.to_string()))?
        .ok_or_else(|| McpClientError::Transport("server closed stdout".into()))?;
    let response: Value =
        serde_json::from_str(&line).map_err(|error| McpClientError::Protocol(error.to_string()))?;
    let id = response
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| McpClientError::Protocol("response missing id".into()))?;

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32000);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown MCP error")
            .to_owned();
        return Err(McpClientError::Server { code, message });
    }

    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| McpClientError::Protocol("response missing result".into()))?;
    Ok((id, result))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    struct EchoClient;

    #[async_trait]
    impl McpClient for EchoClient {
        async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
            Ok(json!({
                "structuredContent": {
                    "name": name,
                    "arguments": arguments
                },
                "content": [{ "type": "text", "text": "ok" }],
                "isError": false
            }))
        }
    }

    struct PlainClient;

    #[async_trait]
    impl McpClient for PlainClient {
        async fn call_tool(&self, _name: &str, arguments: Value) -> Result<Value, McpClientError> {
            Ok(json!({ "content": [{ "type": "text", "text": arguments }] }))
        }
    }

    struct ErrorClient;

    #[async_trait]
    impl McpClient for ErrorClient {
        async fn call_tool(&self, _name: &str, _arguments: Value) -> Result<Value, McpClientError> {
            Err(McpClientError::Server {
                code: -32000,
                message: "server said no".into(),
            })
        }
    }

    #[tokio::test]
    async fn executor_normalizes_structured_content() {
        let output = McpExecutor::new(EchoClient)
            .call("read_file", json!({ "path": "README.md" }))
            .await
            .unwrap();

        assert_eq!(
            output,
            json!({
                "name": "read_file",
                "arguments": { "path": "README.md" }
            })
        );
    }

    #[tokio::test]
    async fn executor_maps_batch_outputs_to_node_ids() {
        let outputs = McpExecutor::new(EchoClient)
            .call_batch(
                "read_file",
                vec![
                    BatchInput {
                        node: "a".into(),
                        input: json!({ "path": "a.md" }),
                    },
                    BatchInput {
                        node: "b".into(),
                        input: json!({ "path": "b.md" }),
                    },
                ],
            )
            .await
            .unwrap();

        assert_eq!(outputs[0].node, "a");
        assert_eq!(outputs[1].output["arguments"], json!({ "path": "b.md" }));
    }

    #[tokio::test]
    async fn executor_keeps_unstructured_tool_result() {
        let output = McpExecutor::new(PlainClient)
            .call("plain", json!("ok"))
            .await
            .unwrap();

        assert_eq!(output["content"][0]["text"], json!("ok"));
    }

    #[tokio::test]
    async fn executor_maps_client_errors() {
        let error = McpExecutor::new(ErrorClient)
            .call("bad", json!({}))
            .await
            .unwrap_err();

        assert!(error.message.contains("server said no"));
    }

    #[test]
    fn read_tool_declares_batchable_read_effects() {
        let spec = mcp_read_tool(["file:README.md"]);
        let effects = spec.effects.unwrap();

        assert!(effects.batchable);
        assert!(effects.cacheable);
        assert!(effects.reads.contains("file:README.md"));
    }

    #[test]
    fn command_candidates_include_windows_cmd_shim() {
        let candidates = command_candidates("npx");

        assert!(candidates.contains(&"npx".to_owned()));
        #[cfg(windows)]
        assert!(candidates.contains(&"npx.cmd".to_owned()));
    }

    #[test]
    fn spawn_stdio_reports_missing_program() {
        let error = spawn_stdio("__tool_compiler_missing_command__", &[], &BTreeMap::new())
            .expect_err("missing command should fail");

        assert!(matches!(error, McpClientError::Transport(_)));
    }

    #[tokio::test]
    async fn stdio_client_speaks_json_rpc_lines() {
        let client = McpStdioClient::new(McpServerConfig {
            name: "fake".into(),
            transport: McpTransport::Stdio {
                command: "node".into(),
                args: vec!["-e".into(), fake_mcp_server_script().into()],
                env: BTreeMap::new(),
            },
        });

        let results = client
            .call_tools(vec![
                McpToolCall {
                    node: "a".into(),
                    name: "echo".into(),
                    arguments: json!({ "id": "a" }),
                },
                McpToolCall {
                    node: "b".into(),
                    name: "echo".into(),
                    arguments: json!({ "id": "b" }),
                },
            ])
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].result["structuredContent"]["arguments"]["id"],
            "a"
        );
        assert_eq!(
            results[1].result["structuredContent"]["arguments"]["id"],
            "b"
        );
    }

    #[tokio::test]
    async fn stdio_client_call_tool_uses_single_result() {
        let client = McpStdioClient::new(McpServerConfig {
            name: "fake".into(),
            transport: McpTransport::Stdio {
                command: "node".into(),
                args: vec!["-e".into(), fake_mcp_server_script().into()],
                env: BTreeMap::new(),
            },
        });

        let result = client
            .call_tool("echo", json!({ "id": "single" }))
            .await
            .unwrap();

        assert_eq!(result["structuredContent"]["arguments"]["id"], "single");
    }

    #[tokio::test]
    async fn stdio_client_reuses_session_between_calls() {
        let client = McpStdioClient::new(McpServerConfig {
            name: "fake".into(),
            transport: McpTransport::Stdio {
                command: "node".into(),
                args: vec!["-e".into(), fake_mcp_server_script().into()],
                env: BTreeMap::new(),
            },
        });

        let first = client
            .call_tool("echo", json!({ "id": "first" }))
            .await
            .unwrap();
        let second = client
            .call_tool("echo", json!({ "id": "second" }))
            .await
            .unwrap();

        assert_eq!(
            first["structuredContent"]["pid"],
            second["structuredContent"]["pid"]
        );
    }

    #[tokio::test]
    async fn stdio_client_maps_server_errors() {
        let client = McpStdioClient::new(McpServerConfig {
            name: "fake".into(),
            transport: McpTransport::Stdio {
                command: "node".into(),
                args: vec!["-e".into(), fake_mcp_error_server_script().into()],
                env: BTreeMap::new(),
            },
        });

        let error = client.call_tool("echo", json!({})).await.unwrap_err();

        assert!(matches!(error, McpClientError::Server { .. }));
    }

    fn fake_mcp_server_script() -> &'static str {
        r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0',
      id: req.id,
      result: {
        protocolVersion: '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'fake', version: '0.1.0' }
      }
    }));
  } else if (req.method === 'tools/call') {
    console.log(JSON.stringify({
      jsonrpc: '2.0',
      id: req.id,
      result: {
        structuredContent: { name: req.params.name, arguments: req.params.arguments, pid: process.pid },
        content: [{ type: 'text', text: 'ok' }],
        isError: false
      }
    }));
  }
});
"#
    }

    fn fake_mcp_error_server_script() -> &'static str {
        r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0',
      id: req.id,
      result: {
        protocolVersion: '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'fake', version: '0.1.0' }
      }
    }));
  } else if (req.method === 'tools/call') {
    console.log(JSON.stringify({
      jsonrpc: '2.0',
      id: req.id,
      error: { code: -32000, message: 'tool exploded' }
    }));
  }
});
"#
    }
}
