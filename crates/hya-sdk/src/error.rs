//! Errors returned by transport, spawn, and decode paths.

use serde_json::Value;
use thiserror::Error;

/// Structured details from a non-success HTTP-style response.
///
/// `body` contains the decoded JSON value when the response is JSON. For a
/// non-JSON body it is a string containing the lossily decoded response text.
/// `raw_body` always preserves the original response bytes so callers can
/// inspect an error body that does not follow the JSON contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdkHttpError {
    /// HTTP status code returned by the transport.
    pub status: u16,
    /// Stable server error code, when the JSON body carries a string `code`.
    pub code: Option<String>,
    /// Human-readable server message, when the JSON body carries a string `message`.
    pub message: Option<String>,
    /// Decoded response body, or a string for non-JSON responses.
    pub body: Value,
    /// Original response bytes, retained even when JSON decoding succeeds.
    pub raw_body: Vec<u8>,
}

impl SdkHttpError {
    /// Return the HTTP status code.
    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    /// Return the stable server error code, when present.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// Return the human-readable server message, when present.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Return the decoded response body.
    #[must_use]
    pub const fn body(&self) -> &Value {
        &self.body
    }

    /// Return the exact response bytes.
    #[must_use]
    pub fn raw_body(&self) -> &[u8] {
        &self.raw_body
    }
}

impl std::fmt::Display for SdkHttpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "status {}", self.status)?;
        if let Some(code) = &self.code {
            write!(formatter, " ({code})")?;
        }
        if let Some(message) = &self.message {
            write!(formatter, ": {message}")?;
        }
        Ok(())
    }
}

/// Decode a non-success response without coupling the caller to HTTP client details.
///
/// JSON objects retain their complete decoded value and expose string `code`
/// and `message` fields when present. The server's `{ "error": { "code",
/// "message" } }` envelope is unwrapped for those convenience fields without
/// changing the preserved `body`. Invalid JSON remains available through both
/// `body` (as text) and `raw_body` (as exact bytes).
#[must_use]
pub fn decode_http_error(status: u16, raw_body: &[u8]) -> SdkError {
    let body = serde_json::from_slice::<Value>(raw_body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(raw_body).into_owned()));
    let fields = body
        .get("error")
        .and_then(Value::as_object)
        .or_else(|| body.as_object());
    let code = fields
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let message = fields
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    SdkError::HttpStatus(Box::new(SdkHttpError {
        status,
        code,
        message,
        body,
        raw_body: raw_body.to_vec(),
    }))
}

/// Errors surfaced by the SDK layer.
///
/// Covers owned-backend spawn readiness, HTTP/SSE transport, the native bridge,
/// and JSON decode. Variants are `#[non_exhaustive]` so new failure modes can
/// land without breaking `match` arms that use a wildcard.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SdkError {
    /// Failed to start the backend subprocess.
    #[error("failed to spawn `compat serve`: {0}")]
    Spawn(String),

    /// Subprocess started but never printed a parseable listen URL in time.
    #[error("server did not become ready within {0:?}")]
    Readiness(std::time::Duration),

    /// Server exited or logged without a `listening on http://…` line.
    #[error("could not parse server listen URL from server output")]
    ListenUrlParse,

    /// Non-success HTTP status or transport failure on a REST call.
    #[error("http error: {0}")]
    Http(String),

    /// Non-success response with preserved status and structured body data.
    #[error("http error: {0}")]
    HttpStatus(Box<SdkHttpError>),

    /// SSE / global event stream failure.
    #[error("event stream error: {0}")]
    EventStream(String),

    /// Native bridge process or IPC failure.
    #[error("native bridge error: {0}")]
    Bridge(String),

    /// Native bridge sent an unexpected protocol message.
    #[error("native bridge protocol error: {0}")]
    Protocol(String),

    /// Call made before the backend connection was established.
    #[error("backend is not connected yet")]
    NotReady,

    /// JSON encode/decode failure.
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Convenience alias used across the SDK surface.
pub type Result<T> = std::result::Result<T, SdkError>;

#[cfg(test)]
mod tests {
    use super::{decode_http_error, SdkError};

    #[test]
    fn decoder_preserves_status_code_message_and_json_body() {
        let raw = br#"{"error":{"code":"WORKFLOW_BUSY","message":"run active"},"run":"r1"}"#;
        let error = decode_http_error(409, raw);
        let SdkError::HttpStatus(error) = error else {
            panic!("expected structured HTTP error");
        };
        assert_eq!(error.status, 409);
        assert_eq!(error.code.as_deref(), Some("WORKFLOW_BUSY"));
        assert_eq!(error.message.as_deref(), Some("run active"));
        assert_eq!(error.body["run"], "r1");
        assert_eq!(error.raw_body.as_slice(), raw);
    }

    #[test]
    fn decoder_keeps_non_json_body_as_text_and_bytes() {
        let error = decode_http_error(403, b"forbidden");
        let SdkError::HttpStatus(error) = error else {
            panic!("expected structured HTTP error");
        };
        assert_eq!(error.status(), 403);
        assert_eq!(error.body().as_str(), Some("forbidden"));
        assert_eq!(error.raw_body(), b"forbidden");
    }

    #[test]
    fn decoder_preserves_structured_workflow_forbidden_body() {
        let raw = br#"{"error":{"code":"WORKFLOW_UNAUTHORIZED","message":"target denied"}}"#;
        let error = decode_http_error(403, raw);
        let SdkError::HttpStatus(error) = error else {
            panic!("expected structured HTTP error");
        };
        assert_eq!(error.status(), 403);
        assert_eq!(error.code(), Some("WORKFLOW_UNAUTHORIZED"));
        assert_eq!(error.message(), Some("target denied"));
        assert_eq!(error.raw_body(), raw);
    }
}
