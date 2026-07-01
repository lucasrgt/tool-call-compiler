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
            (Some(base), false) => format!(
                "{}/{}",
                base.trim_end_matches('/'),
                url.trim_start_matches('/')
            ),
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
        assert_eq!(
            client.full_url("https://other.test/x"),
            "https://other.test/x"
        );
    }

    /// Minimal fixed-response HTTP server on a random local port.
    fn spawn_fixed_server(response: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let mut buffer = [0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut buffer);
                let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn requests_parse_json_bodies_and_headers() {
        let url = spawn_fixed_server(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 13\r\n\r\n{\"ok\": true }",
        );
        let client = ReqwestHttpClient::new()
            .unwrap()
            .with_default_header("x-default", "yes");

        let response = client
            .request(HttpRequest {
                method: "GET".into(),
                url,
                headers: [("x-per-request".to_owned(), "1".to_owned())].into(),
                body: None,
            })
            .await
            .unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body["ok"], serde_json::json!(true));
        assert_eq!(
            response.headers.get("content-type").map(String::as_str),
            Some("application/json")
        );
    }

    #[tokio::test]
    async fn non_json_bodies_stay_strings_and_posts_send_bodies() {
        let url = spawn_fixed_server(
            "HTTP/1.1 201 Created\r\ncontent-type: text/plain\r\ncontent-length: 7\r\n\r\ncreated",
        );
        let client = ReqwestHttpClient::new().unwrap();

        let response = client
            .request(HttpRequest {
                method: "POST".into(),
                url,
                headers: Default::default(),
                body: Some("payload".into()),
            })
            .await
            .unwrap();

        assert_eq!(response.status, 201);
        assert_eq!(response.body, Value::String("created".into()));
    }

    #[tokio::test]
    async fn transport_failures_and_bad_methods_are_reported() {
        let client = ReqwestHttpClient::new().unwrap();

        let refused = client
            .request(HttpRequest {
                method: "GET".into(),
                url: "http://127.0.0.1:1".into(),
                headers: Default::default(),
                body: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(refused, HttpError::Client(_)));

        let bad_method = client
            .request(HttpRequest {
                method: "NOT A METHOD".into(),
                url: "http://127.0.0.1:1".into(),
                headers: Default::default(),
                body: None,
            })
            .await
            .unwrap_err();
        assert!(matches!(bad_method, HttpError::InvalidMethod(_)));
    }
}
