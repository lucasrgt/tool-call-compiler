//! Streamable-HTTP MCP transport (feature `http`).
//!
//! Each JSON-RPC request is POSTed to the endpoint; the server may answer
//! with a plain `application/json` message or a `text/event-stream` whose
//! `data:` events carry JSON-RPC messages (the response is the event whose
//! `id` matches the request). The `Mcp-Session-Id` header returned by
//! `initialize` is echoed on subsequent requests.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{
    McpClient, McpClientError, McpServerConfig, McpToolInfo, McpTransport, check_protocol_version,
    initialize_request, initialized_notification, parse_tools_list, tool_call_request,
    tools_list_request,
};

const SESSION_HEADER: &str = "mcp-session-id";

/// HTTP MCP client.
#[derive(Clone)]
pub struct McpHttpClient {
    config: McpServerConfig,
    client: reqwest::Client,
    next_id: Arc<AtomicU64>,
    session_id: Arc<Mutex<Option<String>>>,
}

impl McpHttpClient {
    /// Creates a client for `config` (must carry an HTTP transport).
    pub fn new(config: McpServerConfig) -> Result<Self, McpClientError> {
        if !matches!(config.transport, McpTransport::Http { .. }) {
            return Err(McpClientError::Transport(
                "McpHttpClient requires an http transport".into(),
            ));
        }
        let timeout = std::time::Duration::from_millis(config.request_timeout_ms.unwrap_or(60_000));
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| McpClientError::Transport(error.to_string()))?;
        Ok(Self {
            config,
            client,
            next_id: Default::default(),
            session_id: Default::default(),
        })
    }

    fn endpoint(&self) -> (&str, &BTreeMap<String, String>) {
        match &self.config.transport {
            McpTransport::Http { url, headers } => (url, headers),
            McpTransport::Stdio { .. } => unreachable!("validated in new()"),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn ensure_session(&self) -> Result<(), McpClientError> {
        let mut session = self.session_id.lock().await;
        if session.is_some() {
            return Ok(());
        }

        let id = self.next_id();
        let (result, new_session) = self.post(initialize_request(id), id, None).await?;
        check_protocol_version(&result)?;
        *session = new_session.or(Some(String::new()));
        let session_header = session.clone().filter(|value| !value.is_empty());
        drop(session);

        // Fire-and-forget per protocol; errors here surface on the next call.
        let _ = self
            .post_notification(initialized_notification(), session_header.as_deref())
            .await;
        Ok(())
    }

    async fn request(&self, id: u64, message: Value) -> Result<Value, McpClientError> {
        self.ensure_session().await?;
        let session = self
            .session_id
            .lock()
            .await
            .clone()
            .filter(|value| !value.is_empty());
        let (result, _) = self.post(message, id, session.as_deref()).await?;
        Ok(result)
    }

    async fn post_notification(
        &self,
        message: Value,
        session: Option<&str>,
    ) -> Result<(), McpClientError> {
        let (url, headers) = self.endpoint();
        let mut builder = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        if let Some(session) = session {
            builder = builder.header(SESSION_HEADER, session);
        }
        builder
            .json(&message)
            .send()
            .await
            .map_err(|error| McpClientError::Transport(error.to_string()))?;
        Ok(())
    }

    /// Posts one request and extracts the matching JSON-RPC response, plus
    /// any session id the server assigned.
    async fn post(
        &self,
        message: Value,
        id: u64,
        session: Option<&str>,
    ) -> Result<(Value, Option<String>), McpClientError> {
        let (url, headers) = self.endpoint();
        let mut builder = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        if let Some(session) = session {
            builder = builder.header(SESSION_HEADER, session);
        }

        let response = builder.json(&message).send().await.map_err(|error| {
            if error.is_timeout() {
                McpClientError::Timeout(self.config.request_timeout_ms.unwrap_or(60_000))
            } else {
                McpClientError::Transport(error.to_string())
            }
        })?;

        if !response.status().is_success() {
            return Err(McpClientError::Transport(format!(
                "http status {}",
                response.status()
            )));
        }
        let new_session = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response
            .text()
            .await
            .map_err(|error| McpClientError::Transport(error.to_string()))?;

        let response_message = if content_type.contains("text/event-stream") {
            sse_response_for(&body, id)?
        } else {
            serde_json::from_str::<Value>(&body)
                .map_err(|error| McpClientError::Protocol(error.to_string()))?
        };

        parse_jsonrpc(response_message).map(|result| (result, new_session))
    }
}

fn sse_response_for(body: &str, id: u64) -> Result<Value, McpClientError> {
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        if message.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(message);
        }
    }
    Err(McpClientError::Protocol(
        "event stream carried no response for the request".into(),
    ))
}

fn parse_jsonrpc(message: Value) -> Result<Value, McpClientError> {
    if let Some(error) = message.get("error") {
        return Err(McpClientError::Server {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(-32000),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown MCP error")
                .to_owned(),
        });
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| McpClientError::Protocol("response missing result".into()))
}

#[async_trait]
impl McpClient for McpHttpClient {
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpClientError> {
        let id = self.next_id();
        self.request(id, tool_call_request(id, name, arguments))
            .await
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>, McpClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let id = self.next_id();
            let result = self
                .request(id, tools_list_request(id, cursor.as_deref()))
                .await?;
            let (mut page, next) = parse_tools_list(&result);
            tools.append(&mut page);
            match next {
                Some(next) => cursor = Some(next),
                None => return Ok(tools),
            }
        }
    }
}
