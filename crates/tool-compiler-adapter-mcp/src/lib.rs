//! MCP adapter: persistent stdio sessions (with response demultiplexing and
//! automatic respawn) and an optional HTTP transport (feature `http`).
//!
//! Protocol behaviors worth knowing:
//!
//! - JSON-RPC notifications from the server (no `id`) are skipped, and
//!   server-to-client requests are answered with `method not found` — they
//!   never corrupt an in-flight tool call.
//! - `tools/call` results carrying `isError: true` become
//!   [`ToolExecutionError`]s (code `mcp_tool_error`) instead of successes.
//! - [`hydrate_capabilities`] lists the server's tools and registers
//!   adapter-scoped capabilities derived from MCP tool annotations.
//! - Spawned stdio servers receive `TOOL_COMPILER_DEPTH` = parent depth + 1,
//!   which `serve-mcp` uses to refuse runaway recursive composition.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tool_compiler_adapter_api::{BatchInput, BatchOutput, ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{Effects, ToolSpec};

mod proto;
mod stdio;

pub(crate) use proto::{
    check_protocol_version, initialize_request, initialized_notification, normalize_tool_result,
    parse_tools_list, tool_call_request, tools_list_request,
};
pub use stdio::McpStdioClient;

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::McpHttpClient;

/// Conventional adapter name for MCP executors.
pub const ADAPTER: &str = "mcp";
/// MCP protocol version this adapter speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Environment variable carrying the recursive-composition depth.
pub const DEPTH_ENV_VAR: &str = "TOOL_COMPILER_DEPTH";

/// Configuration of one MCP server connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human label for the server.
    pub name: String,
    /// Transport used to reach it.
    pub transport: McpTransport,
    /// Per-request timeout in milliseconds (default 60s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_ms: Option<u64>,
}

/// Supported MCP transports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransport {
    /// Spawn a local server over stdio.
    Stdio {
        /// Program to spawn.
        command: String,
        /// Program arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Extra environment variables for the child.
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Start the child from an empty environment (plus `env` and the
        /// `inherit` allowlist) instead of inheriting everything.
        #[serde(default)]
        env_clear: bool,
        /// Parent variables allowed through when `env_clear` is set.
        #[serde(default)]
        inherit: Vec<String>,
    },
    /// Reach a remote server over HTTP (requires the `http` feature).
    Http {
        /// Endpoint URL.
        url: String,
        /// Headers sent with every request (auth, etc.).
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// ToolSpec helper wrapping arbitrary effects for an MCP tool.
pub fn mcp_tool(effects: Effects) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(effects)
}

/// ToolSpec helper for a batchable read-only MCP tool.
pub fn mcp_read_tool(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    mcp_tool(Effects {
        batchable: true,
        ..Effects::read_only(resources)
    })
}

/// Client-side MCP session: single calls plus a pipelined batch entry point.
#[async_trait]
pub trait McpClient: Send + Sync {
    /// Calls one tool.
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError>;

    /// Calls several tools, pipelining requests when the transport allows.
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

    /// Lists the server's tools (following pagination).
    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpClientError>;
}

/// One tool call keyed by graph node.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolCall {
    /// Graph node id.
    pub node: String,
    /// MCP tool name.
    pub name: String,
    /// Tool arguments.
    pub arguments: Value,
}

/// One tool result keyed by graph node.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolResult {
    /// Graph node id.
    pub node: String,
    /// Raw `tools/call` result.
    pub result: Value,
}

/// Metadata of one server tool, from `tools/list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// Tool name.
    pub name: String,
    /// Tool description.
    #[serde(default)]
    pub description: Option<String>,
    /// Input JSON Schema.
    #[serde(default)]
    pub input_schema: Option<Value>,
    /// MCP tool annotations (`readOnlyHint`, `idempotentHint`, ...).
    #[serde(default)]
    pub annotations: Value,
}

/// [`ToolExecutor`] bridging the compiler contract onto an [`McpClient`].
#[derive(Clone)]
pub struct McpExecutor<C> {
    client: C,
}

impl<C> McpExecutor<C> {
    /// Wraps a client.
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
        let result = self
            .client
            .call_tool(tool, input)
            .await
            .map_err(tool_error)?;
        normalize_tool_result(result).map_err(tool_error)
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

        let results = self.client.call_tools(calls).await.map_err(tool_error)?;
        results
            .into_iter()
            .map(|result| {
                Ok(BatchOutput {
                    node: result.node,
                    output: normalize_tool_result(result.result).map_err(tool_error)?,
                })
            })
            .collect()
    }
}

/// Derives [`Effects`] from MCP tool annotations, scoped to one server.
///
/// `readOnlyHint: true` becomes a read of `mcp:<server>`; anything else is a
/// conservative write of the same resource. `idempotentHint` maps to
/// `idempotent`. Reads are cacheable only when the annotation also marks the
/// tool idempotent.
pub fn effects_from_annotations(server: &str, annotations: &Value) -> Effects {
    let read_only = annotations
        .get("readOnlyHint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let idempotent = annotations
        .get("idempotentHint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let resource = format!("mcp:{server}");

    if read_only {
        Effects {
            reads: [resource].into_iter().collect(),
            idempotent: true,
            cacheable: idempotent,
            ..Effects::default()
        }
    } else {
        Effects {
            writes: [resource].into_iter().collect(),
            idempotent,
            cacheable: false,
            ..Effects::default()
        }
    }
}

/// One tool's capabilities derived from MCP metadata; the caller registers
/// these on its runtime registry (adapter-scoped).
#[derive(Clone, Debug)]
pub struct DerivedCapability {
    /// Tool name.
    pub tool: String,
    /// Effects derived from the tool's annotations.
    pub effects: Effects,
    /// Tool input schema, when the server declares one.
    pub input_schema: Option<Value>,
    /// Tool description.
    pub description: Option<String>,
}

/// Lists a server's tools and derives per-tool capabilities from their MCP
/// annotations (effects) and input schemas. Hosts register the result on
/// their registry, typically adapter-scoped.
pub async fn derive_capabilities(
    server: &str,
    client: &impl McpClient,
) -> Result<Vec<DerivedCapability>, McpClientError> {
    let tools = client.list_tools().await?;
    Ok(tools
        .into_iter()
        .map(|tool| DerivedCapability {
            effects: effects_from_annotations(server, &tool.annotations),
            input_schema: tool.input_schema.filter(|schema| !schema.is_null()),
            description: tool.description,
            tool: tool.name,
        })
        .collect())
}

/// MCP client errors.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum McpClientError {
    /// Transport-level failure (process, pipe, network).
    #[error("mcp transport error: {0}")]
    Transport(String),
    /// The server violated the protocol.
    #[error("mcp protocol error: {0}")]
    Protocol(String),
    /// The server negotiated an unsupported protocol version.
    #[error("mcp server negotiated unsupported protocol version '{0}'")]
    UnsupportedVersion(String),
    /// JSON-RPC level error from the server.
    #[error("mcp server error {code}: {message}")]
    Server {
        /// JSON-RPC error code.
        code: i64,
        /// Error message.
        message: String,
    },
    /// The request timed out.
    #[error("mcp request timed out after {0}ms")]
    Timeout(u64),
    /// The tool ran and reported failure (`isError: true`).
    #[error("mcp tool failed: {0}")]
    ToolError(String),
}

fn tool_error(error: McpClientError) -> ToolExecutionError {
    let (code, retryable) = match &error {
        McpClientError::Transport(_) => ("mcp_transport", Some(true)),
        McpClientError::Timeout(_) => ("timeout", Some(true)),
        McpClientError::Protocol(_) | McpClientError::UnsupportedVersion(_) => {
            ("mcp_protocol", Some(false))
        }
        McpClientError::Server { .. } => ("mcp_server", None),
        McpClientError::ToolError(_) => ("mcp_tool_error", None),
    };
    let mut mapped = ToolExecutionError::new(error.to_string()).with_code(code);
    if let Some(retryable) = retryable {
        mapped = mapped.with_retryable(retryable);
    }
    mapped
}

#[cfg(test)]
mod tests;
