//! Batteries-included [`HttpClient`] backed by `reqwest` (feature `reqwest`).

use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use crate::{HttpClient, HttpError, HttpRequest, HttpResponse};

/// Default request timeout applied by [`ReqwestHttpClient::new`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// `reqwest`-based HTTP client with a base URL option, default headers, and
/// a request timeout.
#[derive(Clone, Debug)]
pub struct ReqwestHttpClient {
    client: reqwest::Client,
    base_url: Option<String>,
    default_headers: BTreeMap<String, String>,
}

impl ReqwestHttpClient {
    /// Creates a client with [`DEFAULT_TIMEOUT`].
    pub fn new() -> Result<Self, HttpError> {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Creates a client with an explicit request timeout.
    pub fn with_timeout(timeout: Duration) -> Result<Self, HttpError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| HttpError::Client(error.to_string()))?;
        Ok(Self {
            client,
            base_url: None,
            default_headers: BTreeMap::new(),
        })
    }

    /// Prefixes relative request URLs with `base_url`.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Adds a header sent with every request (per-request headers win).
    pub fn with_default_header(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.default_headers
            .insert(name.into().to_lowercase(), value.into());
        self
    }

    fn full_url(&self, url: &str) -> String {
        match (&self.base_url, url.starts_with("http")) {
            (Some(base), false) => format!("{}/{}", base.trim_end_matches('/'), url.trim_start_matches('/')),
            _ => url.to_owned(),
        }
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = reqwest::Method::from_str(&request.method)
            .map_err(|_| HttpError::InvalidMethod(request.method.clone()))?;
        let mut builder = self.client.request(method, self.full_url(&request.url));

        for (name, value) in &self.default_headers {
            if !request.headers.contains_key(name) {
                builder = builder.header(name, value);
            }
        }
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(|error| {
            if error.is_timeout() {
                HttpError::Timeout
            } else {
                HttpError::Client(error.to_string())
            }
        })?;

        let status = response.status().as_u16();
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            if let Ok(value) = value.to_str() {
                headers.insert(name.as_str().to_lowercase(), value.to_owned());
            }
        }

        let is_json = headers
            .get("content-type")
            .is_some_and(|value| value.contains("json"));
        let text = response
            .text()
            .await
            .map_err(|error| HttpError::Client(error.to_string()))?;
        let body = if is_json {
            serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text))
        } else {
            Value::String(text)
        };

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_joins_relative_paths() {
        let client = ReqwestHttpClient::new()
            .unwrap()
            .with_base_url("https://api.test/");

        assert_eq!(client.full_url("/users"), "https://api.test/users");
        assert_eq!(client.full_url("https://other.test/x"), "https://other.test/x");
    }
}
