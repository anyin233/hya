//! Small typed `reqwest` client for the native hya-server HTTP API.
//!
//! Create sessions, admit prompts, and fetch persisted event envelopes as JSON.
//! This is **not** the Compat-shaped TUI SDK surface (`hya-sdk`); use this crate
//! for simple native `/sessions/*` integration tests and tooling.

// Fully documented; keep it that way. Removed when the workspace lint
// table is promoted from `warn` to `deny`.
#![deny(missing_docs)]

use hya_proto::api::{CreateSessionRequest, CreateSessionResponse, PromptRequest, PromptResponse};
use hya_proto::{Envelope, SessionId};

/// Failure from a client HTTP call.
#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    /// Transport, status, or JSON decode failure from `reqwest`.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}

/// Thin HTTP client bound to a server base URL (no directory header).
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    /// Build a client targeting `base_url` (e.g. `http://127.0.0.1:8080`).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base: base_url.into(),
            http: reqwest::Client::new(),
        }
    }

    /// `POST /sessions` — create a session and return the server response body.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] on transport failure, non-success status, or JSON decode error.
    pub async fn create_session(
        &self,
        req: &CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ClientError> {
        let resp = self
            .http
            .post(format!("{}/sessions", self.base))
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// `POST /sessions/{id}/prompt` — admit one user prompt into the session.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] on transport failure, non-success status, or JSON decode error.
    pub async fn prompt(
        &self,
        session: SessionId,
        req: &PromptRequest,
    ) -> Result<PromptResponse, ClientError> {
        let url = format!("{}/sessions/{session}/prompt", self.base);
        let resp = self
            .http
            .post(url)
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }

    /// `GET /sessions/{id}/events` — load persisted envelopes as a JSON array.
    ///
    /// This is a **one-shot batch fetch**, not a live SSE stream: the future
    /// completes when the full JSON body is decoded. Pass `since_seq` to request
    /// only events after that sequence (server query `?since_seq=`). On HTTP
    /// error status or decode failure the future returns [`ClientError::Http`];
    /// there is no partial result and no long-lived connection to terminate.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] on transport failure, non-success status, or JSON decode error.
    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<Envelope>, ClientError> {
        let mut url = format!("{}/sessions/{session}/events", self.base);
        if let Some(seq) = since_seq {
            url.push_str(&format!("?since_seq={seq}"));
        }
        let resp = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp)
    }
}
