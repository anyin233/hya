//! Typed tooling and process-harness HTTP calls.

use crate::transport::HttpTransport;
use hya_api::v1 as pb;

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
    transport: HttpTransport,
}

impl Client {
    /// Build a client targeting `base_url` (e.g. `http://127.0.0.1:8080`).
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_http_client(base_url, reqwest::Client::new())
    }

    /// Build a client targeting `base_url` that sends every request through
    /// `http` (for example one with default headers, such as the
    /// `x-hya-bridge-token` a `hya bridge` requires).
    #[must_use]
    pub fn with_http_client(base_url: impl Into<String>, http: reqwest::Client) -> Self {
        Self {
            transport: HttpTransport::new(base_url, http, None),
        }
    }

    /// `POST /v1/sessions` — create a session.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn create_session(
        &self,
        req: &pb::CreateSessionRequest,
    ) -> Result<pb::CreateSessionResponse, ClientError> {
        self.transport.create_session(req).await
    }

    /// `GET /v1/projects` — list the Projects.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_projects(&self) -> Result<pb::ListProjectsResponse, ClientError> {
        self.transport.list_projects().await
    }

    /// `GET /v1/projects/{id}` — read one Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn get_project(&self, project: &str) -> Result<pb::ProjectInfo, ClientError> {
        self.transport.get_project(project).await
    }

    /// `POST /v1/projects` — create a Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn create_project(
        &self,
        req: &pb::CreateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.transport.create_project(req).await
    }

    /// `PATCH /v1/projects/{id}` — rename and/or replace the roots.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn update_project(
        &self,
        req: &pb::UpdateProjectRequest,
    ) -> Result<pb::ProjectInfo, ClientError> {
        self.transport.update_project(req).await
    }

    /// `DELETE /v1/projects/{id}` — delete a Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn delete_project(
        &self,
        project: &str,
    ) -> Result<pb::DeleteProjectResponse, ClientError> {
        self.transport.delete_project(project).await
    }

    /// `GET /v1/projects/resolve?path=…` — the Project containing `path`.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn resolve_project(
        &self,
        path: &str,
    ) -> Result<pb::ResolveProjectResponse, ClientError> {
        self.transport.resolve_project(path).await
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
        self.transport.ensure_project_for_path(path).await
    }

    /// List sessions using the server’s default filters and page size.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn list_sessions(&self) -> Result<pb::ListSessionsResponse, ClientError> {
        self.transport.list_sessions(None, None).await
    }

    /// `GET /v1/sessions?projectId=…` — the sessions of one Project.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_project_sessions(
        &self,
        project: &str,
    ) -> Result<pb::ListSessionsResponse, ClientError> {
        self.transport.list_sessions(Some(project), None).await
    }

    /// `POST /v1/sessions/{id}/turns` — admit a typed turn request.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn create_turn_request(
        &self,
        request: &pb::CreateTurnRequest,
    ) -> Result<pb::CreateTurnResponse, ClientError> {
        self.transport.create_turn_request(request).await
    }

    /// `POST /v1/sessions/{id}/turns/{turn}/wait` — wait for the turn.
    ///
    /// # Errors
    /// Returns an error if the transport, server, or response decoding fails.
    pub async fn wait_turn_request(
        &self,
        request: &pb::WaitTurnRequest,
    ) -> Result<pb::TurnInfo, ClientError> {
        self.transport.wait_turn_request(request).await
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
        self.transport.prompt_text(session, text).await
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
        self.transport.list_events_request(request).await
    }

    /// `GET /v1/interactions` — list pending permission/question requests.
    ///
    /// # Errors
    /// Returns [`ClientError::Http`] or [`ClientError::Api`] on failure.
    pub async fn list_interactions(
        &self,
        kind: Option<pb::InteractionType>,
    ) -> Result<Vec<pb::Interaction>, ClientError> {
        self.transport.list_interactions(kind).await
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
        self.transport.respond_interaction(id, response).await
    }
}
