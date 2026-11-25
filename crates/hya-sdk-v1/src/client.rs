//! Typed tooling and process-harness HTTP calls.

use crate::encode_component;
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

    /// Build a client targeting `base_url` that sends every request through
    /// `http` (for example one with default headers, such as the
    /// `x-hya-bridge-token` a `hya bridge` requires).
    #[must_use]
    pub fn with_http_client(base_url: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            base: base_url.into(),
            http,
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
                message: format!("{status}: {}", String::from_utf8_lossy(&bytes))
                    .trim_end()
                    .to_owned(),
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

    /// `POST /v1/sessions/{id}/turns` — admit a typed turn request.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn create_turn_request(
        &self,
        request: &pb::CreateTurnRequest,
    ) -> Result<pb::CreateTurnResponse, ClientError> {
        self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{}/turns", encode_component(&request.session)),
            None,
            Some(request),
        )
        .await
    }

    /// `POST /v1/sessions/{id}/turns/{turn}/wait` — wait for the turn.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn wait_turn_request(
        &self,
        request: &pb::WaitTurnRequest,
    ) -> Result<pb::TurnInfo, ClientError> {
        let timeout_ms = request.timeout_ms.min(600_000);
        self.call(
            reqwest::Method::POST,
            &format!(
                "/v1/sessions/{}/turns/{}/wait",
                encode_component(&request.session),
                encode_component(&request.turn)
            ),
            Some(&format!("timeoutMs={timeout_ms}")),
            None::<&Value>,
        )
        .await
    }

    /// Admit a text prompt and wait up to 120 seconds for its terminal state.
    ///
    /// # Errors
    /// Returns an error if admission or waiting fails.
    pub async fn prompt_text(
        &self,
        session: &str,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, ClientError> {
        let admitted = self
            .create_turn_request(&pb::CreateTurnRequest {
                session: session.to_owned(),
                kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                    text: text.into(),
                    ..Default::default()
                })),
            })
            .await?;
        self.wait_turn_request(&pb::WaitTurnRequest {
            session: session.to_owned(),
            turn: admitted.turn.map(|turn| turn.id).unwrap_or_default(),
            timeout_ms: 120_000,
        })
        .await
    }

    /// `GET /v1/sessions/{id}/events` — replay curated events, optionally with
    /// opaque raw envelope JSON lines in `raw_envelopes`.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn list_events_request(
        &self,
        request: &pb::ListEventsRequest,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        self.replay_events(
            &request.session,
            (request.since_seq != 0).then_some(request.since_seq),
            request.limit,
            request.include_raw,
        )
        .await
    }

    async fn replay_events(
        &self,
        session: &str,
        since_seq: Option<u64>,
        limit: u32,
        include_raw: bool,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        let mut params = Vec::new();
        if let Some(seq) = since_seq {
            params.push(format!("sinceSeq={seq}"));
        }
        if limit != 0 {
            params.push(format!("limit={limit}"));
        }
        if include_raw {
            params.push("includeRaw=true".into());
        }
        let query = (!params.is_empty()).then(|| params.join("&"));
        self.call(
            reqwest::Method::GET,
            &format!("/v1/sessions/{}/events", encode_component(session)),
            query.as_deref(),
            None::<&Value>,
        )
        .await
    }

    /// Admit a plain prompt using an internal session ID.
    ///
    /// # Errors
    /// Returns an error if admission fails.
    #[deprecated(note = "use Client::create_turn_request with a v1 CreateTurnRequest")]
    pub async fn create_turn(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<pb::CreateTurnResponse, ClientError> {
        self.create_turn_request(&pb::CreateTurnRequest {
            session: session.to_string(),
            kind: Some(pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                text: text.into(),
                ..Default::default()
            })),
        })
        .await
    }

    /// Wait for a turn using an internal session ID.
    ///
    /// # Errors
    /// Returns an error if waiting fails.
    #[deprecated(note = "use Client::wait_turn_request with a v1 WaitTurnRequest")]
    pub async fn wait_turn(
        &self,
        session: SessionId,
        turn: &str,
        timeout: Duration,
    ) -> Result<pb::TurnInfo, ClientError> {
        self.wait_turn_request(&pb::WaitTurnRequest {
            session: session.to_string(),
            turn: turn.to_owned(),
            timeout_ms: timeout.as_millis().min(600_000) as u64,
        })
        .await
    }

    /// Admit a prompt and wait for its terminal state using an internal session ID.
    ///
    /// # Errors
    /// Returns an error if admission or waiting fails.
    #[deprecated(note = "use Client::prompt_text with a string session ID")]
    pub async fn prompt(
        &self,
        session: SessionId,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, ClientError> {
        self.prompt_text(&session.to_string(), text).await
    }

    /// Replay events after a watermark using an internal session ID.
    ///
    /// # Errors
    /// Returns an error if replay fails.
    #[deprecated(note = "use Client::list_events_request with a v1 ListEventsRequest")]
    pub async fn list_events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
        include_raw: bool,
    ) -> Result<pb::ListEventsResponse, ClientError> {
        self.replay_events(&session.to_string(), since_seq, 0, include_raw)
            .await
    }

    /// Replay the raw durable envelopes for tooling and test harnesses.
    ///
    /// # Errors
    /// Returns an error if replay fails.
    #[deprecated(
        note = "use Client::list_events_request with include_raw and consume the opaque JSON lines"
    )]
    pub async fn events(
        &self,
        session: SessionId,
        since_seq: Option<u64>,
    ) -> Result<Vec<Envelope>, ClientError> {
        let response = self
            .replay_events(&session.to_string(), since_seq, 0, true)
            .await?;
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
            &format!("/v1/interactions/{}/respond", encode_component(id)),
            None,
            Some(response),
        )
        .await
    }
}
