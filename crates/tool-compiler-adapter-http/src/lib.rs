use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;
use tool_compiler_ir::{Effects, ToolSpec};
use tool_compiler_runtime::{ToolExecutionError, ToolExecutor};

pub const ADAPTER: &str = "http";

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn request(&self, request: HttpRequest) -> Result<Value, HttpError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub body: Option<String>,
}

#[derive(Clone)]
pub struct HttpExecutor<C> {
    client: C,
}

impl<C> HttpExecutor<C> {
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
            return Err(ToolExecutionError::new(format!(
                "unknown http tool '{tool}'"
            )));
        }

        let request = HttpRequest {
            method: optional_str(&input, "method").unwrap_or("GET").to_owned(),
            url: required_str(&input, "url").map_err(tool_error)?.to_owned(),
            body: optional_str(&input, "body").map(str::to_owned),
        };

        self.client.request(request).await.map_err(tool_error)
    }
}

pub fn read_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::read_only(resources))
}

pub fn pure_endpoint() -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects::pure())
}

pub fn write_endpoint(resources: impl IntoIterator<Item = impl Into<String>>) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(Effects {
        writes: resources.into_iter().map(Into::into).collect(),
        idempotent: false,
        cacheable: false,
        ..Effects::default()
    })
}

pub fn request_tool(effects: Effects) -> ToolSpec {
    ToolSpec::new(ADAPTER).with_effects(effects)
}

fn required_str<'a>(input: &'a Value, key: &str) -> Result<&'a str, HttpError> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| HttpError::MissingField(key.into()))
}

fn optional_str<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

fn tool_error(error: HttpError) -> ToolExecutionError {
    ToolExecutionError::new(error.to_string())
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HttpError {
    #[error("missing string field '{0}'")]
    MissingField(String),
    #[error("http client error: {0}")]
    Client(String),
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    struct EchoHttpClient;

    #[async_trait]
    impl HttpClient for EchoHttpClient {
        async fn request(&self, request: HttpRequest) -> Result<Value, HttpError> {
            Ok(json!({
                "method": request.method,
                "url": request.url,
                "body": request.body
            }))
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
    fn write_endpoint_declares_writes() {
        let spec = write_endpoint(["api:user"]);
        let effects = spec.effects.unwrap();

        assert!(effects.writes.contains("api:user"));
        assert!(!effects.cacheable);
    }

    #[test]
    fn pure_endpoint_declares_pure_effects() {
        let spec = pure_endpoint();
        let effects = spec.effects.unwrap();

        assert!(effects.pure);
        assert!(effects.commutative);
    }

    #[tokio::test]
    async fn executor_builds_http_request() {
        let output = HttpExecutor::new(EchoHttpClient)
            .call(
                "request",
                json!({
                    "method": "POST",
                    "url": "https://example.test",
                    "body": "hello"
                }),
            )
            .await
            .unwrap();

        assert_eq!(output["method"], "POST");
        assert_eq!(output["body"], "hello");
    }

    #[tokio::test]
    async fn executor_requires_url() {
        let error = HttpExecutor::new(EchoHttpClient)
            .call("request", json!({}))
            .await
            .unwrap_err();

        assert!(error.message.contains("url"));
    }
}
