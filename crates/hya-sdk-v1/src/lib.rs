//! `hya-sdk-v1` — the typed SDK for the hya v1 API.
//!
//! The successor to `hya-sdk` for frontends built on the consolidated
//! dual-protocol contract: protojson-typed calls over HTTP (`/v1`) plus a
//! live SSE subscription and an in-memory session mirror that folds the
//! curated `StreamFrame`s into a transcript view. gRPC clients can use
//! `hya-api`'s generated clients directly against the same contract.
//!
//! Every request carries the directory scope via the `x-hya-directory`
//! header (D6).

use std::collections::BTreeMap;
use std::time::Duration;

use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use hya_api::v1 as pb;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Directory scope header shared with the HTTP binding.
pub const DIRECTORY_HEADER: &str = "x-hya-directory";

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

/// Typed client over one hya backend's `/v1` surface.
pub struct V1Sdk {
    base: String,
    directory: String,
    http: reqwest::Client,
}

impl V1Sdk {
    /// Build a client for `base_url` scoped to `directory`.
    #[must_use]
    pub fn new(base_url: impl Into<String>, directory: impl Into<String>) -> Self {
        Self {
            base: base_url.into(),
            directory: directory.into(),
            http: reqwest::Client::new(),
        }
    }

    /// The directory scope this client sends on every request.
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
        let mut url = format!("{}{path}", self.base);
        if let Some(query) = query {
            url.push('?');
            url.push_str(&query);
        }
        let mut request = self
            .http
            .request(method, url)
            .header(DIRECTORY_HEADER, &self.directory);
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
                message: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// `GET /v1/bootstrap` — the one-round-trip startup snapshot.
    ///
    /// # Errors
    /// Returns [`SdkError`] on transport or API failure.
    pub async fn bootstrap(&self) -> Result<pb::Bootstrap, SdkError> {
        self.call(reqwest::Method::GET, "/v1/bootstrap", None, None::<&Value>)
            .await
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
            &format!("/v1/sessions/{session}"),
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
        let query =
            page.map(|page| format!("page.cursor={}&page.limit={}", page.cursor, page.limit));
        self.call(reqwest::Method::GET, "/v1/sessions", query, None::<&Value>)
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
                &format!("/v1/sessions/{session}/turns"),
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
                pb::create_turn_request::Kind::Prompt(pb::PromptTurn { text: text.into() }),
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
        self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{session}/turns/{turn}/wait"),
            Some(format!("timeoutMs={timeout_ms}")),
            None::<&Value>,
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
            &format!("/v1/sessions/{session}/messages"),
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
            &format!("/v1/sessions/{session}/todo"),
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
            &format!("/v1/sessions/{session}/events"),
            Some(format!("sinceSeq={since_seq}")),
            None::<&Value>,
        )
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
                &format!("/v1/interactions/{request_id}/respond"),
                None,
                Some(&request),
            )
            .await?;
        Ok(applied.applied)
    }

    /// `GET /v1/sessions/{session}/events/stream` — the live SSE stream.
    ///
    /// The returned stream yields typed frames; a `resync` frame tells the
    /// consumer to re-replay from its `lastSeq`.
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
                self.base
            ))
            .header(DIRECTORY_HEADER, &self.directory)
            .send()
            .await?;
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

/// In-memory transcript mirror folded from curated stream frames.
///
/// Seed with [`V1Sdk::list_messages`], then [`apply`](Self::apply) every
/// live frame; on `resync`, re-seed from the server.
#[derive(Default)]
pub struct V1SessionMirror {
    messages: BTreeMap<String, pb::MessageInfo>,
    /// Highest applied sequence number.
    pub last_seq: u64,
}

impl V1SessionMirror {
    /// Seed the mirror from a projected transcript read.
    #[must_use]
    pub fn from_messages(messages: &[pb::MessageInfo]) -> Self {
        let mut mirror = Self::default();
        for message in messages {
            mirror.messages.insert(message.id.clone(), message.clone());
        }
        mirror
    }

    /// Apply one live frame; returns `true` when a resync is required.
    pub fn apply(&mut self, frame: &pb::StreamFrame) -> bool {
        match &frame.frame {
            Some(pb::stream_frame::Frame::Resync(resync)) => {
                self.last_seq = resync.last_seq;
                true
            }
            Some(pb::stream_frame::Frame::Event(event)) => {
                self.last_seq = self.last_seq.max(event.seq);
                self.apply_event(event);
                false
            }
            None => false,
        }
    }

    fn apply_event(&mut self, event: &pb::StreamEvent) {
        use hya_api::v1::stream_event::Payload as P;
        match &event.payload {
            Some(P::MessageStarted(started)) => {
                self.messages.entry(started.message.clone()).or_default();
            }
            Some(P::MessageFinished(finished)) => {
                if let Some(message) = self.messages.get_mut(&finished.message) {
                    message.finish = finished.finish;
                }
            }
            Some(P::PartStarted(started)) => {
                if let Some(message) = self.messages.get_mut(&started.message) {
                    message.parts.push(pb::PartInfo {
                        id: started.part.clone(),
                        kind: None,
                    });
                }
            }
            Some(P::PartAppended(appended)) => {
                if let Some(message) = self.messages.get_mut(&appended.message)
                    && let Some(part) = message
                        .parts
                        .iter_mut()
                        .find(|part| part.id == appended.part)
                    && let Some(pb::part_info::Kind::Text(text)) = part.kind.as_mut()
                {
                    text.text.push_str(&appended.text_delta);
                }
            }
            _ => {}
        }
    }

    /// The mirrored transcript in append order.
    #[must_use]
    pub fn messages(&self) -> Vec<&pb::MessageInfo> {
        self.messages.values().collect()
    }
}
