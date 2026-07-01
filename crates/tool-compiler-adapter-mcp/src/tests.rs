use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{Value, json};
use tool_compiler_adapter_api::BatchInput;

use super::*;

fn stdio_config(script: &str) -> McpServerConfig {
    McpServerConfig {
        name: "fake".into(),
        transport: McpTransport::Stdio {
            command: "node".into(),
            args: vec!["-e".into(), script.into()],
            env: BTreeMap::new(),
            env_clear: false,
            inherit: Vec::new(),
        },
        request_timeout_ms: Some(10_000),
    }
}

struct EchoClient;

#[async_trait]
impl McpClient for EchoClient {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        Ok(json!({
            "structuredContent": { "name": name, "arguments": arguments },
            "content": [{ "type": "text", "text": "ok" }],
            "isError": false
        }))
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpClientError> {
        Ok(vec![McpToolInfo {
            name: "read_file".into(),
            description: Some("reads".into()),
            input_schema: Some(json!({ "type": "object" })),
            annotations: json!({ "readOnlyHint": true, "idempotentHint": true }),
        }])
    }
}

struct ToolErrorClient;

#[async_trait]
impl McpClient for ToolErrorClient {
    async fn call_tool(&self, _name: &str, _arguments: Value) -> Result<Value, McpClientError> {
        Ok(json!({
            "content": [{ "type": "text", "text": "file not found" }],
            "isError": true
        }))
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpClientError> {
        Ok(Vec::new())
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
        json!({ "name": "read_file", "arguments": { "path": "README.md" } })
    );
}

#[tokio::test]
async fn tool_level_errors_become_execution_errors() {
    let error = McpExecutor::new(ToolErrorClient)
        .call("read_file", json!({}))
        .await
        .unwrap_err();

    assert_eq!(error.code.as_deref(), Some("mcp_tool_error"));
    assert!(error.message.contains("file not found"));
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

#[test]
fn annotations_map_to_effects() {
    let read = effects_from_annotations(
        "files",
        &json!({ "readOnlyHint": true, "idempotentHint": true }),
    );
    assert!(read.reads.contains("mcp:files"));
    assert!(read.cacheable);
    assert!(read.idempotent);

    let read_volatile = effects_from_annotations("files", &json!({ "readOnlyHint": true }));
    assert!(!read_volatile.cacheable);

    let write = effects_from_annotations("files", &json!({}));
    assert!(write.writes.contains("mcp:files"));
    assert!(!write.idempotent);
}

#[tokio::test]
async fn derive_capabilities_carries_schemas_and_effects() {
    let capabilities = derive_capabilities("files", &EchoClient).await.unwrap();

    assert_eq!(capabilities.len(), 1);
    assert_eq!(capabilities[0].tool, "read_file");
    assert!(capabilities[0].effects.reads.contains("mcp:files"));
    assert!(capabilities[0].input_schema.is_some());
}

#[test]
fn protocol_versions_are_validated() {
    assert!(check_protocol_version(&json!({ "protocolVersion": "2025-06-18" })).is_ok());
    assert!(check_protocol_version(&json!({ "protocolVersion": "1.0" })).is_err());
    assert!(check_protocol_version(&json!({})).is_err());
}

#[test]
fn read_tool_declares_batchable_read_effects() {
    let spec = mcp_read_tool(["file:README.md"]);
    let effects = spec.effects.unwrap();

    assert!(effects.batchable);
    assert!(effects.cacheable);
    assert!(effects.reads.contains("file:README.md"));
}

#[tokio::test]
async fn stdio_client_pipelines_calls_and_skips_notifications() {
    let client = McpStdioClient::new(stdio_config(FAKE_SERVER));

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
    assert_eq!(results[0].result["structuredContent"]["arguments"]["id"], "a");
    assert_eq!(results[1].result["structuredContent"]["arguments"]["id"], "b");
}

#[tokio::test]
async fn stdio_client_reuses_sessions_and_respawns_dead_ones() {
    let client = McpStdioClient::new(stdio_config(FAKE_SERVER));

    let first = client.call_tool("echo", json!({ "n": 1 })).await.unwrap();
    let second = client.call_tool("echo", json!({ "n": 2 })).await.unwrap();
    assert_eq!(
        first["structuredContent"]["pid"],
        second["structuredContent"]["pid"]
    );

    // Ask the fake server to die; the next call must respawn a new process.
    let _ = client.call_tool("die", json!({})).await;
    let third = client.call_tool("echo", json!({ "n": 3 })).await.unwrap();
    assert_ne!(
        first["structuredContent"]["pid"],
        third["structuredContent"]["pid"]
    );
}

#[tokio::test]
async fn stdio_client_reports_stderr_on_transport_failure() {
    let script = r#"
console.error('booting broken server');
console.error('fatal: cannot start');
process.exit(1);
"#;
    let client = McpStdioClient::new(stdio_config(script));

    let error = client.call_tool("echo", json!({})).await.unwrap_err();

    let text = error.to_string();
    assert!(matches!(
        error,
        McpClientError::Transport(_) | McpClientError::Protocol(_)
    ));
    assert!(text.contains("cannot start"), "missing stderr tail: {text}");
}

#[tokio::test]
async fn stdio_client_maps_server_errors() {
    let client = McpStdioClient::new(stdio_config(ERROR_SERVER));

    let error = client.call_tool("echo", json!({})).await.unwrap_err();

    assert!(matches!(error, McpClientError::Server { .. }));
}

#[tokio::test]
async fn stdio_client_lists_tools_with_pagination() {
    let client = McpStdioClient::new(stdio_config(LISTING_SERVER));

    let tools = client.list_tools().await.unwrap();

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0].name, "first");
    assert_eq!(tools[1].name, "second");
    assert_eq!(tools[1].annotations["readOnlyHint"], true);
}

#[tokio::test]
async fn rejects_unsupported_protocol_versions() {
    let script = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: req.id, result: { protocolVersion: 'bogus' } }));
  }
});
"#;
    let client = McpStdioClient::new(stdio_config(script));

    let error = client.call_tool("echo", json!({})).await.unwrap_err();

    assert!(matches!(error, McpClientError::UnsupportedVersion(_)));
}

/// Echo server that also emits notifications (which clients must skip) and
/// supports a `die` tool that kills the process.
const FAKE_SERVER: &str = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0.1.0' } }
    }));
  } else if (req.method === 'tools/call') {
    console.log(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/message', params: { level: 'info', data: 'noise' } }));
    if (req.params.name === 'die') { process.exit(0); }
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: {
        structuredContent: { name: req.params.name, arguments: req.params.arguments, pid: process.pid },
        content: [{ type: 'text', text: 'ok' }],
        isError: false
      }
    }));
  }
});
"#;

const ERROR_SERVER: &str = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0.1.0' } }
    }));
  } else if (req.method === 'tools/call') {
    console.log(JSON.stringify({ jsonrpc: '2.0', id: req.id, error: { code: -32000, message: 'tool exploded' } }));
  }
});
"#;

const LISTING_SERVER: &str = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
rl.on('line', line => {
  const req = JSON.parse(line);
  if (req.method === 'initialize') {
    console.log(JSON.stringify({
      jsonrpc: '2.0', id: req.id,
      result: { protocolVersion: '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'fake', version: '0.1.0' } }
    }));
  } else if (req.method === 'tools/list') {
    if (req.params && req.params.cursor === 'page2') {
      console.log(JSON.stringify({
        jsonrpc: '2.0', id: req.id,
        result: { tools: [{ name: 'second', annotations: { readOnlyHint: true } }] }
      }));
    } else {
      console.log(JSON.stringify({
        jsonrpc: '2.0', id: req.id,
        result: { tools: [{ name: 'first', inputSchema: { type: 'object' } }], nextCursor: 'page2' }
      }));
    }
  }
});
"#;
