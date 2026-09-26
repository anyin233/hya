//! Small typed `reqwest` client for the hya v1 HTTP API.
//!
//! Create sessions, run event-driven turns (admit + wait), replay events
//! (curated wire events and, for tooling, the raw durable envelopes), and
//! answer pending interactions. This is **not** the frontend SDK surface
//! (`hya-sdk`); use this crate for integration tests and tooling.

use hya_api::v1 as pb;
use hya_proto::{Envelope, SessionId};
use serde_json::Value;
use std::time::Duration;

/// Failure from a client HTTP call.
#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    /// Transport, status, or JSON decode failure from `reqwest`.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with the stable v1 error model.
    #[error("api: {code}: {message}")]
    Api {
        /// Stable error code from the v1 table.
        code: String,
        /// Human-readable error message.
        message: String,
    },
    /// Response body failed to decode into the expected v1 type.
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Percent-encode one path segment or query value (RFC 3986 unreserved
/// characters pass through).
fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
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

    async fn call<Req: serde::Serialize, Resp: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<&str>,
        body: Option<&Req>,
    ) -> Result<Resp, ClientError> {
        let mut url = format!("{}{path}", self.base);
        if let Some(query) = query {
            url.push('?');
            url.push_str(query);
        }
        let mut request = self.http.request(method, url);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&bytes)
                && let Some(error) = map.get("error")
                && let (Some(code), Some(message)) = (
                    error.get("code").and_then(Value::as_str),
                    error.get("message").and_then(Value::as_str),
                )
            {
                return Err(ClientError::Api {
                    code: code.to_owned(),
                    message: message.to_owned(),
                });
            }
            return Err(ClientError::Api {
                code: status.as_u16().to_string(),
                message: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `POST /v1/sessions` — create a session.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn create_session(
        &self,
        req: &pb::CreateSessionRequest,
    ) -> Result<pb::CreateSessionResponse, ClientError> {
        self.call(reqwest::Method::POST, "/v1/sessions", None, Some(req))
            .await
    }

    /// `GET /v1/projects` — list the Projects.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_projects(&self) -> Result<pb::ListProjectsResponse, ClientError> {
        self.call(reqwest::Method::GET, "/v1/projects", None, None::<&Value>)
            .await
    }

    /// `GET /v1/projects/{id}` — read one Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn get_project(&self, project: &str) -> Result<pb::ProjectInfo, ClientError> {
        self.call(
            reqwest::Method::GET,
            &format!("/v1/projects/{}", encode_component(project)),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/projects` — create a Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn create_project(
        &self,
        req: &pb::CreateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.call(reqwest::Method::POST, "/v1/projects", None, Some(req))
            .await
    }

    /// `PATCH /v1/projects/{id}` — rename and/or replace the roots.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn update_project(
        &self,
        req: &pb::UpdateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.call(
            reqwest::Method::PATCH,
            &format!("/v1/projects/{}", encode_component(&req.project)),
            None,
            Some(req),
        )
        .await
    }

    /// `DELETE /v1/projects/{id}` — delete a Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn delete_project(
        &self,
        project: &str,
    ) -> Result<pb::DeleteProjectResponse, ClientError> {
        self.call(
            reqwest::Method::DELETE,
            &format!("/v1/projects/{}", encode_component(project)),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/projects/resolve?path=…` — the Project containing `path`.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn resolve_project(
        &self,
        path: &str,
    ) -> Result<pb::ResolveProjectResponse, ClientError> {
        let query = format!("path={}", encode_component(path));
        self.call(
            reqwest::Method::GET,
            "/v1/projects/resolve",
            Some(&query),
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/projects/ensure` — the Project containing `path`, or a new
    /// one rooted at it.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn ensure_project_for_path(
        &self,
        path: &str,
    ) -> Result<pb::EnsureProjectForPathResponse, ClientError> {
        let req = pb::EnsureProjectForPathRequest {
            path: path.to_owned(),
        };
        self.call(
            reqwest::Method::POST,
            "/v1/projects/ensure",
            None,
            Some(&req),
        )
        .await
    }

    /// `GET /v1/sessions?projectId=…` — the sessions of one Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_project_sessions(
        &self,
        project: &str,
    ) -> Result<pb::ListSessionsResponse, ClientError> {
        let query = format!("projectId={}", encode_component(project));
        self.call(
            reqwest::Method::GET,
            "/v1/sessions",
            Some(&query),
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/sessions/{id}/turns` — admit one prompt turn, returning
    /// the running turn handle.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn create_turn(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<pb::CreateTurnResponse, ClientError> {
        let request = pb::CreateTurnRequest {
            session: session.to_string(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: text.into(),
                ..Default::default()
            })),
        };
        self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{session}/turns"),
            None,
            Some(&request),
        )
        .await
    }

    /// `POST /v1/sessions/{id}/turns/{turn}/wait` — block until the turn
    /// reaches a terminal state or the timeout elapses.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn wait_turn(
        &self,
        session: SessionId,
        turn: &str,
        timeout: Duration,
    ) -> Result<pb::TurnInfo, ClientError> {
        let timeout_ms = timeout.as_millis().min(600_000) as u64;
        self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{session}/turns/{turn}/wait"),
            Some(&format!("timeoutMs={timeout_ms}")),
            None::<&Value>,
        )
        .await
    }

    /// Admit a prompt and wait for its terminal state (the synchronous
    /// convenience over the event-driven model).
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn prompt(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, ClientError> {
        let admitted = self.create_turn(session, text).await?;
        let turn = admitted.turn.map(|turn| turn.id).unwrap_or_default();
        self.wait_turn(session, &turn, Duration::from_secs(120))
            .await
    }

    /// `GET /v1/sessions/{id}/events` — replay events after a watermark.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
        include_raw: bool,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        let mut query = String::new();
        if let Some(seq) = since_seq {
            query.push_str(&format!("sinceSeq={seq}"));
        }
        if include_raw {
            if !query.is_empty() {
                query.push('&');
            }
            query.push_str("includeRaw=true");
        }
        let query = if query.is_empty() {
            None
        } else {
            Some(query.as_str())
        };
        self.call(
            reqwest::Method::GET,
            &format!("/v1/sessions/{session}/events"),
            query,
            None::<&Value>,
        )
        .await
    }

    /// Replay the raw durable envelopes for tooling and test harnesses.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<Envelope>, ClientError> {
        let response = self.list_events(session, since_seq, true).await?;
        Ok(response
            .raw_envelopes
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect())
    }

    /// `GET /v1/interactions` — list pending permission/question requests.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_interactions(
        &self,
        kind: Option<pb::InteractionType>,
    ) -> Result<Vec<pb::Interaction>, ClientError> {
        let query = kind.map(|kind| match kind {
            pb::InteractionType::Permission => "type=PERMISSION",
            pb::InteractionType::Question => "type=QUESTION",
            _ => "",
        });
        let response: pb::ListInteractionsResponse = self
            .call(
                reqwest::Method::GET,
                "/v1/interactions",
                query,
                None::<&Value>,
            )
            .await?;
        Ok(response.interactions)
    }

    /// `POST /v1/interactions/{id}/respond` — answer one pending request.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn respond_interaction(
        &self,
        id: &str,
        response: &pb::RespondInteractionRequest,
    ) -> Result<pb::RespondInteractionResponse, ClientError> {
        self.call(
            reqwest::Method::POST,
            &format!("/v1/interactions/{id}/respond"),
            None,
            Some(response),
        )
        .await
    }
}
