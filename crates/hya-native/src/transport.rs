//! In-process native transport: drives the in-process hya `axum::Router` via
//! `tower::ServiceExt::oneshot` — no TCP, no reqwest. This is the Rust analogue of compat's
//! in-process `app.fetch`. Status/empty-body/JSON handling mirrors `hya_sdk::HttpTransport`
//! (`crates/hya-sdk/src/client.rs`) so the shared `Client` logic behaves identically.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use hya_sdk::error::{decode_http_error, Result, SdkError};
use hya_sdk::{ApiClient, Transport, DIRECTORY_HEADER};

/// A [`Transport`] that routes every request through the in-process hya `Router` instead of HTTP.
///
/// Cloning an `axum::Router` is shallow (it wraps the routing table in `Arc`s), so cloning it
/// per request — as `oneshot` requires — is cheap.
pub struct HyaNativeTransport {
    router: axum::Router,
    directory: String,
    base_url: String,
}

impl HyaNativeTransport {
    /// Build a transport scoped to `directory` (sent as the directory header on every request).
    #[must_use]
    pub fn new(router: axum::Router, directory: impl Into<String>) -> Self {
        Self {
            router,
            directory: directory.into(),
            // Synthetic, never dialled — present only to satisfy `Client::base_url`.
            base_url: "hya-native://in-process".to_owned(),
        }
    }
}

#[async_trait]
impl Transport for HyaNativeTransport {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn directory(&self) -> &str {
        &self.directory
    }

    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let body_bytes = match body {
            Some(value) => Body::from(serde_json::to_vec(value)?),
            None => Body::empty(),
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(DIRECTORY_HEADER, &self.directory);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(body_bytes)
            .map_err(|e| SdkError::Http(e.to_string()))?;

        // `Router` (after `with_state`) is a `tower::Service<Request<Body>>` with `Error = Infallible`,
        // so this is a direct in-process function call — no socket is ever opened.
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;

        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?
            .to_bytes();

        if !status.is_success() {
            return Err(decode_http_error(status.as_u16(), &bytes));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| SdkError::Http(e.to_string()))
    }
}

/// Native-transport [`hya_sdk::Client`]: the same surface as `HttpClient`, backed by the in-process router.
pub type HyaNativeClient = ApiClient<HyaNativeTransport>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use hya_app::{HyaRuntime, RuntimeOptions};
    async fn offline_runtime() -> HyaRuntime {
        HyaRuntime::start(RuntimeOptions {
            model: None,
            db: String::new(),
            yolo: true,
            default_agent: None,
            force_offline: true,
        })
        .await
        .expect("offline runtime should start")
    }

    #[tokio::test]
    async fn get_health_returns_object_over_the_v1_router() {
        let rt = offline_runtime().await;
        let transport = HyaNativeTransport::new(rt.router().clone(), "/tmp");
        let value = transport
            .request("GET", "/v1/health", None)
            .await
            .expect("GET /v1/health should succeed");
        assert!(
            value.is_object(),
            "expected a /v1/health object, got {value}"
        );
    }

    #[tokio::test]
    async fn directory_is_carried_and_request_succeeds() {
        let rt = offline_runtime().await;
        let transport = HyaNativeTransport::new(rt.router().clone(), "/tmp/hya-test-dir");
        assert_eq!(transport.directory(), "/tmp/hya-test-dir");
        // The directory header is injected on every request; a request carrying it must still succeed.
        transport
            .request("GET", "/v1/health", None)
            .await
            .expect("GET /v1/health with a directory header should succeed");
    }
}
