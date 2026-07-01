//! JSON-RPC/MCP message construction and parsing.

use serde_json::{Value, json};

use crate::{McpClientError, McpToolInfo, PROTOCOL_VERSION};

pub(crate) fn initialize_request(id: u64) -> Value {
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

pub(crate) fn initialized_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
}

pub(crate) fn tool_call_request(id: u64, name: &str, arguments: Value) -> Value {
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

pub(crate) fn tools_list_request(id: u64, cursor: Option<&str>) -> Value {
    let mut params = json!({});
    if let Some(cursor) = cursor {
        params["cursor"] = json!(cursor);
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/list",
        "params": params
    })
}

/// Validates the protocol version a server returned from `initialize`.
///
/// Any dated MCP revision (`YYYY-MM-DD`) is accepted — the tools/call wire
/// shape is stable across revisions — but a value that does not even look
/// like a revision is rejected loudly instead of failing later in obscure
/// ways.
pub(crate) fn check_protocol_version(result: &Value) -> Result<(), McpClientError> {
    let Some(version) = result.get("protocolVersion").and_then(Value::as_str) else {
        return Err(McpClientError::Protocol(
            "initialize result missing protocolVersion".into(),
        ));
    };
    if version.len() == 10 && version.chars().filter(|c| *c == '-').count() == 2 {
        Ok(())
    } else {
        Err(McpClientError::UnsupportedVersion(version.to_owned()))
    }
}

pub(crate) fn parse_tools_list(result: &Value) -> (Vec<McpToolInfo>, Option<String>) {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .map(|tool| McpToolInfo {
                    name: tool
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    input_schema: tool.get("inputSchema").cloned(),
                    annotations: tool.get("annotations").cloned().unwrap_or(Value::Null),
                })
                .collect()
        })
        .unwrap_or_default();
    let cursor = result
        .get("nextCursor")
        .and_then(Value::as_str)
        .map(str::to_owned);
    (tools, cursor)
}

/// Unwraps a `tools/call` result: tool-level errors (`isError: true`) become
/// [`McpClientError::ToolError`]; otherwise `structuredContent` wins over the
/// raw result.
pub(crate) fn normalize_tool_result(result: Value) -> Result<Value, McpClientError> {
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let message = result
            .get("content")
            .and_then(Value::as_array)
            .map(|content| {
                content
                    .iter()
                    .filter_map(|item| item.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "tool reported an error".to_owned());
        return Err(McpClientError::ToolError(message));
    }

    if let Some(value) = result.get("structuredContent") {
        return Ok(value.clone());
    }
    Ok(result)
}
