//! `hya-sdk-v1` — the typed SDK for the hya v1 API.
//!
//! The successor to `hya-sdk` for frontends built on the consolidated
//! dual-protocol contract: protojson-typed calls over HTTP (`/v1`) plus a
//! live SSE subscription and an in-memory session mirror that folds the
//! curated `StreamFrame`s into a transcript view. gRPC clients can use
//! `hya-api`'s generated clients directly against the same contract.
//!
//! Streams are deliberately single-attempt: `stream_session` returns one SSE
//! connection and does not retry or select a successor server. Consumers MUST
//! reconnect after transport closure or `serverStopping`; after reconnect (or
//! `resync`) they MUST re-read the projection or replay durable events from
//! their last applied sequence. Live `seq == 0` deltas can be lost during a
//! process restart and are not replayed; durable round parts are the authority.
//!
//! Scoped calls name the client's directory in the request's `directory`
//! field (a query parameter on GET/DELETE, a body field otherwise); the
//! server refuses the removed `x-hya-directory` header.

mod client;
mod mirror;

pub use client::{Client, ClientError};
pub use mirror::V1SessionMirror;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use hya_api::v1 as pb;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// One failed SDK call.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Transport or decode failure.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    /// The stable v1 error model.
    #[error("api {code}: {message}")]
    Api {
        /// Stable code from the v1 table.
        code: String,
        /// Human-readable message.
        message: String,
    },
    /// SSE stream produced a frame that failed to decode.
    #[error("stream decode: {0}")]
    StreamDecode(#[from] serde_json::Error),
    /// The SSE transport itself failed.
    #[error("stream: {0}")]
    Stream(String),
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

/// Typed client over one hya backend's `/v1` surface.
pub struct V1Sdk {
    base: String,
    directory: String,
    http: reqwest::Client,
    request_timeout: Option<Duration>,
}

impl V1Sdk {
    /// Build a client for `base_url` scoped to `directory`.
    #[must_use]
    pub fn new(base_url: impl Into<String>, directory: impl Into<String>) -> Self {
        Self {
            base: base_url.into(),
            directory: directory.into(),
            http: reqwest::Client::new(),
            request_timeout: None,
        }
    }

    /// Build the CLI's loopback daemon client: no environment proxy, a
    /// 5-second connect timeout, and a 30-second timeout for ordinary calls.
    /// Turn waits and SSE streams have no request timeout.
    ///
    /// # Errors
    /// Returns a transport error if the HTTP client cannot be built.
    pub fn for_local_daemon(
        base_url: impl Into<String>,
        directory: impl Into<String>,
    ) -> Result<Self, SdkError> {
        Ok(Self {
            base: base_url.into(),
            directory: directory.into(),
            http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .build()?,
            request_timeout: Some(Duration::from_secs(30)),
        })
    }

    /// The directory scope this client names in scoped calls' `directory`
    /// field (empty: no scope, the global view).
    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }

    async fn call<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<String>,
        body: Option<&Req>,
    ) -> Result<Resp, SdkError> {
        self.call_with_timeout(method, path, query, body, self.request_timeout)
            .await
    }

    async fn call_with_timeout<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: Option<String>,
        body: Option<&Req>,
        timeout: Option<Duration>,
    ) -> Result<Resp, SdkError> {
        let mut url = format!("{}{path}", self.base);
        if let Some(query) = query {
            url.push('?');
            url.push_str(&query);
        }
        let mut request = self.http.request(method, url);
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
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
                return Err(SdkError::Api {
                    code: code.to_owned(),
                    message: message.to_owned(),
                });
            }
            return Err(SdkError::Api {
                code: status.as_u16().to_string(),
                message: format!("{status}: {}", String::from_utf8_lossy(&bytes))
                    .trim_end()
                    .to_owned(),
            });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `GET /v1/bootstrap` — the one-round-trip startup snapshot of the
    /// client's directory scope (the global view without one).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn bootstrap(&self) -> Result<pb::Bootstrap, SdkError> {
        self.call(
            reqwest::Method::GET,
            "/v1/bootstrap",
            self.directory_query(),
            None::<&Value>,
        )
        .await
    }

    /// `directory=<scope>` for a scoped GET, `None` without a scope.
    fn directory_query(&self) -> Option<String> {
        (!self.directory.is_empty())
            .then(|| format!("directory={}", encode_component(&self.directory)))
    }

    /// `POST /v1/sessions` — create a session.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn create_session(
        &self,
        request: pb::CreateSessionRequest,
    ) -> Result<pb::SessionInfo, SdkError> {
        let response: pb::CreateSessionResponse = self
            .call(reqwest::Method::POST, "/v1/sessions", None, Some(&request))
            .await?;
        Ok(response.session.unwrap_or_default())
    }

    /// `GET /v1/sessions/{id}` — read one session.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn get_session(&self, session: &str) -> Result<pb::SessionInfo, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}",
                session = encode_component(session)
            ),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/sessions` — list sessions.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_sessions(
        &self,
        page: Option<pb::PageRequest>,
    ) -> Result<pb::ListSessionsResponse, SdkError> {
        let query = page.map(|page| {
            format!(
                "page.cursor={}&page.limit={}",
                encode_component(&page.cursor),
                page.limit
            )
        });
        self.call(reqwest::Method::GET, "/v1/sessions", query, None::<&Value>)
            .await
    }

    /// `GET /v1/sessions?projectId=…` — list the sessions (root and
    /// subagent) of one Project.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_project_sessions(
        &self,
        project: &str,
        page: Option<pb::PageRequest>,
    ) -> Result<pb::ListSessionsResponse, SdkError> {
        let mut query = format!("projectId={}", encode_component(project));
        if let Some(page) = page {
            query.push_str(&format!(
                "&page.cursor={}&page.limit={}",
                encode_component(&page.cursor),
                page.limit
            ));
        }
        self.call(
            reqwest::Method::GET,
            "/v1/sessions",
            Some(query),
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/projects` — the Projects, most recently updated first.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_projects(&self) -> Result<Vec<pb::ProjectInfo>, SdkError> {
        let response: pb::ListProjectsResponse = self
            .call(reqwest::Method::GET, "/v1/projects", None, None::<&Value>)
            .await?;
        Ok(response.projects)
    }

    /// `GET /v1/projects/{id}` — read one Project.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure (`not_found`).
    pub async fn get_project(&self, project: &str) -> Result<pb::ProjectInfo, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!("/v1/projects/{}", encode_component(project)),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/projects` — create a Project with a name and roots.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure (`invalid_argument`).
    pub async fn create_project(
        &self,
        name: &str,
        roots: Vec<String>,
    ) -> Result<pb::ProjectInfo, SdkError> {
        let request = pb::CreateProjectRequest {
            name: name.to_owned(),
            roots,
        };
        self.call(reqwest::Method::POST, "/v1/projects", None, Some(&request))
            .await
    }

    /// `PATCH /v1/projects/{id}` — rename and/or replace the roots (empty
    /// `roots` keeps them) in one step.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn update_project(
        &self,
        request: pb::UpdateProjectRequest,
    ) -> Result<pb::ProjectInfo, SdkError> {
        self.call(
            reqwest::Method::PATCH,
            &format!("/v1/projects/{}", encode_component(&request.project)),
            None,
            Some(&request),
        )
        .await
    }

    /// `DELETE /v1/projects/{id}` — delete a Project (`failed_precondition`
    /// while a non-archived root session uses it).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn delete_project(&self, project: &str) -> Result<(), SdkError> {
        let _: pb::DeleteProjectResponse = self
            .call(
                reqwest::Method::DELETE,
                &format!("/v1/projects/{}", encode_component(project)),
                None,
                None::<&Value>,
            )
            .await?;
        Ok(())
    }

    /// `GET /v1/projects/resolve?path=…` — the Project containing `path`, if
    /// any (never creates one).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn resolve_project(&self, path: &str) -> Result<Option<pb::ProjectInfo>, SdkError> {
        let response: pb::ResolveProjectResponse = self
            .call(
                reqwest::Method::GET,
                "/v1/projects/resolve",
                Some(format!("path={}", encode_component(path))),
                None::<&Value>,
            )
            .await?;
        Ok(response.project)
    }

    /// `POST /v1/projects/ensure` — the Project containing `path`, or a new
    /// one rooted at it.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn ensure_project_for_path(
        &self,
        path: &str,
    ) -> Result<pb::EnsureProjectForPathResponse, SdkError> {
        let request = pb::EnsureProjectForPathRequest {
            path: path.to_owned(),
        };
        self.call(
            reqwest::Method::POST,
            "/v1/projects/ensure",
            None,
            Some(&request),
        )
        .await
    }

    /// `POST /v1/sessions/{id}/turns` — admit a prompt turn.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn create_turn(
        &self,
        session: &str,
        kind: pb::create_turn_request::Kind,
    ) -> Result<pb::TurnInfo, SdkError> {
        let request = pb::CreateTurnRequest {
            session: session.to_owned(),
            kind: Some(kind),
        };
        let response: pb::CreateTurnResponse = self
            .call(
                reqwest::Method::POST,
                &format!(
                    "/v1/sessions/{session}/turns",
                    session = encode_component(session)
                ),
                None,
                Some(&request),
            )
            .await?;
        Ok(response.turn.unwrap_or_default())
    }

    /// Admit a prompt and wait for its terminal state.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn prompt(
        &self,
        session: &str,
        text: impl Into<String>,
    ) -> Result<pb::TurnInfo, SdkError> {
        let admitted = self
            .create_turn(
                session,
                pb::create_turn_request::Kind::Prompt(pb::PromptTurn {
                    text: text.into(),
                    ..Default::default()
                }),
            )
            .await?;
        self.wait_turn(session, &admitted.id, Duration::from_secs(120))
            .await
    }

    /// `POST /v1/sessions/{id}/turns/{turn}/wait`.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn wait_turn(
        &self,
        session: &str,
        turn: &str,
        timeout: Duration,
    ) -> Result<pb::TurnInfo, SdkError> {
        let timeout_ms = timeout.as_millis().min(600_000) as u64;
        self.call_with_timeout(
            reqwest::Method::POST,
            &format!(
                "/v1/sessions/{session}/turns/{turn}/wait",
                session = encode_component(session),
                turn = encode_component(turn)
            ),
            Some(format!("timeoutMs={timeout_ms}")),
            None::<&Value>,
            None,
        )
        .await
    }

    /// `GET /v1/sessions/{id}/messages` — the projected transcript.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_messages(&self, session: &str) -> Result<pb::ListMessagesResponse, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}/messages",
                session = encode_component(session)
            ),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/sessions/{id}/todo`.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn todo(&self, session: &str) -> Result<pb::TodoList, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}/todo",
                session = encode_component(session)
            ),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/sessions/{id}/events` — replay curated events.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_events(
        &self,
        session: &str,
        since_seq: u64,
    ) -> Result<pb::ListEventsResponse, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}/events",
                session = encode_component(session)
            ),
            Some(format!("sinceSeq={since_seq}")),
            None::<&Value>,
        )
        .await
    }

    /// `GET /v1/sessions/{id}/events?includeRaw=true` — replay events after
    /// `since_seq` together with the canonical durable envelope JSON lines
    /// (`raw_envelopes`; an opaque internal shape for tooling).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_raw_events(
        &self,
        session: &str,
        since_seq: u64,
    ) -> Result<pb::ListEventsResponse, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}/events",
                session = encode_component(session)
            ),
            Some(format!("sinceSeq={since_seq}&includeRaw=true")),
            None::<&Value>,
        )
        .await
    }

    /// `PATCH /v1/sessions/{id}` — update title, model, agent, background,
    /// permission mode, or archived state (only the fields that are set).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn update_session(
        &self,
        request: pb::UpdateSessionRequest,
    ) -> Result<pb::SessionInfo, SdkError> {
        let path = format!("/v1/sessions/{}", encode_component(&request.session));
        self.call(reqwest::Method::PATCH, &path, None, Some(&request))
            .await
    }

    /// `POST /v1/sessions/{id}/turns/{turn}/cancel` — cancel a running turn.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn cancel_turn(&self, session: &str, turn: &str) -> Result<pb::TurnInfo, SdkError> {
        self.call(
            reqwest::Method::POST,
            &format!(
                "/v1/sessions/{session}/turns/{turn}/cancel",
                session = encode_component(session),
                turn = encode_component(turn)
            ),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/sessions/{id}/archive` — archive a subagent session, as its
    /// team lead's `archive` tool does.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn archive_subagent(
        &self,
        session: &str,
    ) -> Result<pb::ArchiveSubagentResponse, SdkError> {
        let request = pb::ArchiveSubagentRequest {
            session: session.to_string(),
        };
        self.call(
            reqwest::Method::POST,
            &format!(
                "/v1/sessions/{session}/archive",
                session = encode_component(session)
            ),
            None,
            Some(&request),
        )
        .await
    }

    /// `POST /v1/sessions/{id}/steer` — send the user's message to a subagent
    /// session on its parent's DM.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn steer_subagent(
        &self,
        session: &str,
        text: &str,
    ) -> Result<pb::SteerSubagentResponse, SdkError> {
        let request = pb::SteerSubagentRequest {
            session: session.to_string(),
            text: text.to_string(),
        };
        self.call(
            reqwest::Method::POST,
            &format!(
                "/v1/sessions/{session}/steer",
                session = encode_component(session)
            ),
            None,
            Some(&request),
        )
        .await
    }

    /// `GET /v1/sessions/{id}/workflow` — the session's workflow state.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn workflow_state(&self, session: &str) -> Result<pb::WorkflowState, SdkError> {
        self.call(
            reqwest::Method::GET,
            &format!(
                "/v1/sessions/{session}/workflow",
                session = encode_component(session)
            ),
            None,
            None::<&Value>,
        )
        .await
    }

    /// `POST /v1/sessions/{id}/workflow` — submit one workflow command. A
    /// run returns once it is admitted; follow it with
    /// [`workflow_state`](Self::workflow_state).
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn submit_workflow_command(
        &self,
        request: pb::SubmitWorkflowCommandRequest,
    ) -> Result<pb::SubmitWorkflowCommandResponse, SdkError> {
        let path = format!(
            "/v1/sessions/{}/workflow",
            encode_component(&request.session)
        );
        self.call(reqwest::Method::POST, &path, None, Some(&request))
            .await
    }

    /// `GET /v1/interactions` — pending permission/question requests.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn list_interactions(&self) -> Result<Vec<pb::Interaction>, SdkError> {
        let response: pb::ListInteractionsResponse = self
            .call(
                reqwest::Method::GET,
                "/v1/interactions",
                None,
                None::<&Value>,
            )
            .await?;
        Ok(response.interactions)
    }

    /// `POST /v1/interactions/{id}/respond` — answer one pending request.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn respond_interaction(
        &self,
        request_id: &str,
        response: pb::respond_interaction_request::Response,
    ) -> Result<bool, SdkError> {
        let request = pb::RespondInteractionRequest {
            request: request_id.to_owned(),
            response: Some(response),
        };
        let applied: pb::RespondInteractionResponse = self
            .call(
                reqwest::Method::POST,
                &format!(
                    "/v1/interactions/{request_id}/respond",
                    request_id = encode_component(request_id)
                ),
                None,
                Some(&request),
            )
            .await?;
        Ok(applied.applied)
    }

    /// `GET /v1/sessions/{session}/events/stream` — one live SSE attempt.
    ///
    /// The returned stream yields typed frames; a `resync` frame tells the
    /// consumer to replay from its last durable sequence. This method does not
    /// retry, reconnect, or bootstrap a successor process: callers own that
    /// policy. On `serverStopping` or an unexpected close, reconnect and
    /// re-read/replay durable state. In-flight `seq == 0` provider deltas may
    /// be absent after a process restart; durable parts/transcript reads win.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport failure.
    pub async fn stream_session(
        &self,
        session: &str,
        since_seq: u64,
    ) -> Result<
        std::pin::Pin<Box<dyn Stream<Item = Result<pb::StreamFrame, SdkError>> + Send>>,
        SdkError,
    > {
        let response = self
            .http
            .get(format!(
                "{}/v1/sessions/{session}/events/stream?sinceSeq={since_seq}",
                self.base,
                session = encode_component(session),
            ))
            .send()
            .await?
            .error_for_status()?;
        let stream = response
            .bytes_stream()
            .eventsource()
            .filter_map(|event| async move {
                match event {
                    Ok(event) => {
                        if event.event == "error" || event.data.is_empty() {
                            return None;
                        }
                        match serde_json::from_str::<pb::StreamFrame>(&event.data) {
                            Ok(frame) => Some(Ok(frame)),
                            Err(error) => Some(Err(SdkError::StreamDecode(error))),
                        }
                    }
                    Err(error) => Some(Err(SdkError::Stream(error.to_string()))),
                }
            });
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[tokio::test]
    async fn daemon_timeouts_leave_turn_waits_and_streams_unbounded() {
        let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
        let app = axum::Router::new().fallback(move || {
            let (release, blocked) = tokio::sync::oneshot::channel();
            requests.send(release).unwrap();
            async move {
                let _ = blocked.await;
                axum::Json(serde_json::json!({}))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let sdk = std::sync::Arc::new(V1Sdk::for_local_daemon(base, "").unwrap());

        let ordinary = {
            let sdk = sdk.clone();
            tokio::spawn(async move { sdk.get_session("test").await })
        };
        let release = received.recv().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(
            matches!(ordinary.await.unwrap(), Err(SdkError::Http(error)) if error.is_timeout())
        );
        tokio::time::resume();
        let _ = release.send(());

        let wait = {
            let sdk = sdk.clone();
            tokio::spawn(async move {
                sdk.wait_turn("test", "turn", Duration::from_secs(120))
                    .await
            })
        };
        let release = received.recv().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(
            !wait.is_finished(),
            "turn waits must outlive the ordinary deadline"
        );
        tokio::time::resume();
        release.send(()).unwrap();
        wait.await.unwrap().unwrap();

        let stream = tokio::spawn(async move { sdk.stream_session("test", 0).await });
        let release = received.recv().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(
            !stream.is_finished(),
            "SSE must outlive the ordinary deadline"
        );
        tokio::time::resume();
        release.send(()).unwrap();
        let _ = stream.await.unwrap().unwrap();
        server.abort();
    }
}
