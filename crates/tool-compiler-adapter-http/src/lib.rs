//! HTTP adapter with an injectable client and an optional reqwest
//! implementation (feature `reqwest`).
//!
//! The `request` tool input is `{ method?, url, headers?, body? }`; `body`
//! may be a string (sent verbatim) or any JSON value (serialized, with
//! `content-type: application/json` defaulted). The response contract is
//! fixed: `{ status, ok, headers, body }`, where `body` is parsed JSON when
//! the response declares a JSON content type and a string otherwise.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;
use tool_compiler_adapter_api::{ToolExecutionError, ToolExecutor};
use tool_compiler_ir::{Effects, ToolSpec};

/// Conventional adapter name for HTTP executors.
pub const ADAPTER: &str = "http";

/// A single HTTP request produced from tool input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method (uppercased).
    pub method: String,
    /// Absolute request URL.
    pub url: String,
    /// Request headers.
    pub headers: BTreeMap<String, String>,
    /// Optional request body, already serialized.
    pub body: Option<String>,
}

/// The response every [`HttpClient`] implementation must produce.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers (lowercased names).
    pub headers: BTreeMap<String, String>,
    /// Parsed JSON body when the content type is JSON, string otherwise.
    pub body: Value,
}

/// Transport implementation executing [`HttpRequest`]s.
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// Executes one request.
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// [`ToolExecutor`] bridging the compiler contract onto an [`HttpClient`].
#[derive(Clone)]
pub struct HttpExecutor<C> {
    client: C,
}

impl<C> HttpExecutor<C> {
    /// Wraps a client.
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C> ToolExecutor for HttpExecutor<C>
where
    C: HttpClient + Send + Sync,
{
    async fn call(&self, tool: &str, input: Value) -> Result<Value, ToolExecutionError> {
        if tool != "request" {
            return Err(
                ToolExecutionError::new(format!("unknown http tool '{tool}'"))
                    .with_code("unknown_tool"),
            );
        }

        let request = build_request(&input).map_err(tool_error)?;
        let response = self.client.request(request).await.map_err(tool_error)?;
        Ok(json!({
            "status": response.status,
            "ok": (200..300).contains(&response.status),
            "headers": response.headers,
            "body": response.body,
        }))
    }
}

fn build_request(input: &Value) -> Result<HttpRequest, HttpError> {
    let url = input
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| HttpError::MissingField("url".into()))?
        .to_owned();
    let method = input
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_uppercase();

    let mut headers = BTreeMap::new();
    if let Some(map) = input.get("headers") {
        let Value::Object(map) = map else {
            return Err(HttpError::InvalidHeaders);
        };
        for (name, value) in map {
            let Value::String(value) = value else {
                return Err(HttpError::InvalidHeaders);
            };
            headers.insert(name.to_lowercase(), value.clone());
        }
    }

    let body = match input.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => {
            headers
                .entry("content-type".into())
                .or_insert_with(|| "application/json".into());
            Some(other.to_string())
        }
    };

    Ok(HttpRequest {
        method,
        url,
        headers,
        body,
    })
}

/// ToolSpec for an idempotent, cacheable read endpoint.
pub fn read_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

/// ToolSpec for a pure endpoint (no side effects at all).
pub fn pure_endpoint() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::pure())
}

/// ToolSpec for a non-idempotent write endpoint (POST-like).
pub fn write_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

/// ToolSpec for an idempotent write endpoint (PUT/DELETE-like): retryable.
pub fn idempotent_write_endpoint(
    resources: impl IntoIterator<Item = impl Into<String>>,
) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: true,
        cacheable: false,
        ..Effects::default()
    })
}

/// ToolSpec wrapping arbitrary effects for the `request` tool.
pub fn request_tool(effects: Effects) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(effects)
}

fn tool_error(error: HttpError) -> ToolExecutionError {
    let (code, retryable) = match &error {
        HttpError::MissingField(_) | HttpError::InvalidHeaders | HttpError::InvalidMethod(_) => {
            ("invalid_input", Some(false))
        }
        HttpError::Timeout => ("timeout", Some(true)),
        HttpError::Client(_) => ("http", None),
    };
    let mut error = ToolExecutionError::new(error.to_string()).with_code(code);
    if let Some(retryable) = retryable {
        error = error.with_retryable(retryable);
    }
    error
}

/// HTTP adapter errors.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HttpError {
    /// A required string field is missing from the input.
    #[error("missing string field '{0}'")]
    MissingField(String),
    /// `headers` must be an object of string values.
    #[error("headers must be an object of string values")]
    InvalidHeaders,
    /// The HTTP method is not valid.
    #[error("invalid http method '{0}'")]
    InvalidMethod(String),
    /// The request timed out.
    #[error("http request timed out")]
    Timeout,
    /// Transport-level failure.
    #[error("http client error: {0}")]
    Client(String),
}

#[cfg(feature = "reqwest")]
mod reqwest_client;
#[cfg(feature = "reqwest")]
pub use reqwest_client::ReqwestHttpClient;

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    struct EchoHttpClient;

    #[async_trait]
    impl HttpClient for EchoHttpClient {
        async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
            Ok(HttpResponse {
                status: 200,
                headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
                body: json!({
                    "method": request.method,
                    "url": request.url,
                    "headers": request.headers,
                    "body": request.body,
                }),
            })
        }
    }

    #[test]
    fn read_endpoint_declares_read_only_effects() {
        let spec = read_endpoint(["api:user"]);
        let effects = spec.effects.unwrap();

        assert!(effects.reads.contains("api:user"));
        assert!(effects.writes.is_empty());
        assert!(effects.cacheable);
    }

    #[test]
    fn write_endpoints_split_idempotency() {
        assert!(!write_endpoint(["api:user"]).effects.unwrap().idempotent);
        assert!(
            idempotent_write_endpoint(["api:user"])
                .effects
                .unwrap()
                .idempotent
        );
    }

    #[tokio::test]
    async fn executor_builds_requests_with_headers_and_json_bodies() {
        let output = HttpExecutor::new(EchoHttpClient)
            .call(
                "request",
                json!({
                    "method": "post",
                    "url": "https://example.test",
                    "headers": { "Authorization": "Bearer x" },
                    "body": { "hello": true }
                }),
            )
            .await
            .unwrap();

        assert_eq!(output["status"], 200);
        assert_eq!(output["ok"], true);
        assert_eq!(output["body"]["method"], "POST");
        assert_eq!(output["body"]["headers"]["authorization"], "Bearer x");
        assert_eq!(
            output["body"]["headers"]["content-type"],
            "application/json"
        );
        assert_eq!(output["body"]["body"], "{\"hello\":true}");
    }

    #[tokio::test]
    async fn executor_requires_url() {
        let error = HttpExecutor::new(EchoHttpClient)
            .call("request", json!({}))
            .await
            .unwrap_err();

        assert_eq!(error.code.as_deref(), Some("invalid_input"));
    }

    #[tokio::test]
    async fn invalid_headers_are_rejected() {
        let error = HttpExecutor::new(EchoHttpClient)
            .call(
                "request",
                json!({ "url": "https://example.test", "headers": { "x": 1 } }),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code.as_deref(), Some("invalid_input"));
    }
}
