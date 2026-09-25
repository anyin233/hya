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
///
/// Folding rules (the v1 stream contract):
/// - Parts are keyed by id: a `partStarted` for a known part id (the durable
///   record of a part first seen live) does not add a second part.
/// - Live `partAppended` frames (`seq == 0`) grow a part only until the
///   part is durable (seeded from a read, or a durable `partStarted` /
///   `partReplaced` was applied); after that they are stale and ignored.
/// - `partReplaced` sets the part's whole text, superseding live deltas.
/// - Durable frames at or below [`last_seq`](Self::last_seq) are ignored.
///   A seed does not know its watermark: set `last_seq` to the seq the seed
///   read reflects, or durable deltas it already contains apply again.
#[derive(Default)]
pub struct V1SessionMirror {
    messages: BTreeMap<String, pb::MessageInfo>,
    /// Part ids whose text is durable (seeded or confirmed by the log).
    durable_parts: std::collections::BTreeSet<String>,
    /// Highest applied sequence number.
    pub last_seq: u64,
}

impl V1SessionMirror {
    /// Seed the mirror from a projected transcript read.
    #[must_use]
    pub fn from_messages(messages: &[pb::MessageInfo]) -> Self {
        let mut mirror = Self::default();
        for message in messages {
            for part in &message.parts {
                mirror.durable_parts.insert(part.id.clone());
            }
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
                if event.seq != 0 {
                    if event.seq <= self.last_seq {
                        return false;
                    }
                    self.last_seq = event.seq;
                }
                self.apply_event(event);
                false
            }
            None => false,
        }
    }

    fn apply_event(&mut self, event: &pb::StreamEvent) {
        use hya_api::v1::stream_event::Payload as P;
        let durable = event.seq != 0;
        match &event.payload {
            Some(P::MessageStarted(started)) => {
                let message = self.messages.entry(started.message.clone()).or_default();
                message.id.clone_from(&started.message);
                message.role = started.role;
                if message.session.is_empty() {
                    message.session.clone_from(&event.session);
                }
            }
            Some(P::MessageFinished(finished)) => {
                if let Some(message) = self.messages.get_mut(&finished.message) {
                    message.finish = finished.finish;
                    message.finish_cause = finished.cause;
                }
            }
            Some(P::PartStarted(started)) => {
                if durable {
                    self.durable_parts.insert(started.part.clone());
                }
                if let Some(message) = self.messages.get_mut(&started.message)
                    && !message.parts.iter().any(|part| part.id == started.part)
                {
                    message.parts.push(pb::PartInfo {
                        id: started.part.clone(),
                        kind: empty_part_kind(&started.kind),
                    });
                }
            }
            Some(P::PartAppended(appended)) => {
                if !durable && self.durable_parts.contains(&appended.part) {
                    return;
                }
                if let Some(text) = self.part_text_mut(&appended.message, &appended.part) {
                    text.push_str(&appended.text_delta);
                }
            }
            Some(P::PartReplaced(replaced)) => {
                if durable {
                    self.durable_parts.insert(replaced.part.clone());
                }
                if let Some(text) = self.part_text_mut(&replaced.message, &replaced.part) {
                    text.clone_from(&replaced.text);
                }
            }
            Some(P::ErrorReported(reported)) => {
                if let Some(message) = self.messages.get_mut(&reported.message) {
                    message.error = Some(pb::MessageError {
                        code: reported.code.clone(),
                        message: reported.error_message.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// The text buffer of a text or reasoning part.
    fn part_text_mut(&mut self, message: &str, part: &str) -> Option<&mut String> {
        let part = self
            .messages
            .get_mut(message)?
            .parts
            .iter_mut()
            .find(|candidate| candidate.id == part)?;
        match part.kind.as_mut()? {
            pb::part_info::Kind::Text(text) => Some(&mut text.text),
            pb::part_info::Kind::Reasoning(reasoning) => Some(&mut reasoning.text),
            _ => None,
        }
    }

    /// The mirrored transcript in append order.
    #[must_use]
    pub fn messages(&self) -> Vec<&pb::MessageInfo> {
        self.messages.values().collect()
    }
}

/// An empty part body for a `partStarted.kind` discriminator.
fn empty_part_kind(kind: &str) -> Option<pb::part_info::Kind> {
    match kind {
        "text" => Some(pb::part_info::Kind::Text(pb::TextPart::default())),
        "reasoning" => Some(pb::part_info::Kind::Reasoning(pb::ReasoningPart::default())),
        "tool_call" => Some(pb::part_info::Kind::ToolCall(pb::ToolCallPart::default())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_api::v1::stream_event::Payload as P;

    fn frame(seq: u64, payload: P) -> pb::StreamFrame {
        pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                seq,
                session: "s".into(),
                time_recorded: None,
                payload: Some(payload),
            })),
        }
    }

    fn started(part: &str) -> P {
        P::PartStarted(pb::PartStarted {
            message: "m".into(),
            part: part.into(),
            kind: "text".into(),
        })
    }

    fn appended(part: &str, delta: &str) -> P {
        P::PartAppended(pb::PartAppended {
            message: "m".into(),
            part: part.into(),
            text_delta: delta.into(),
        })
    }

    fn text_of(mirror: &V1SessionMirror) -> Vec<(String, String)> {
        mirror.messages()[0]
            .parts
            .iter()
            .map(|part| {
                let text = match part.kind.as_ref() {
                    Some(pb::part_info::Kind::Text(text)) => text.text.clone(),
                    _ => String::new(),
                };
                (part.id.clone(), text)
            })
            .collect()
    }

    /// Live deltas build the part; the durable start for the same id does
    /// not add a second part.
    #[test]
    fn live_then_durable_part_started_folds_into_one_part() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            3,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(0, started("p")));
        mirror.apply(&frame(0, appended("p", "Hel")));
        mirror.apply(&frame(0, appended("p", "lo")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
        mirror.apply(&frame(5, started("p")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
        let message = mirror.messages()[0];
        assert_eq!(message.id, "m");
        assert_eq!(message.role, pb::Role::Assistant as i32);
        // The durable full text replaces (never appends to) the live text;
        // a late live delta for the now-durable part is stale.
        mirror.apply(&frame(
            6,
            P::PartReplaced(pb::PartReplaced {
                message: "m".into(),
                part: "p".into(),
                text: "Hello, world".into(),
            }),
        ));
        mirror.apply(&frame(0, appended("p", "lo")));
        // An already-applied durable frame is ignored.
        mirror.apply(&frame(6, appended("p", "!!")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello, world".into())]);
        mirror.apply(&frame(
            7,
            P::ErrorReported(pb::ErrorReported {
                message: "m".into(),
                code: "provider_error".into(),
                error_message: "http status 400: nope".into(),
            }),
        ));
        mirror.apply(&frame(
            8,
            P::MessageFinished(pb::MessageFinished {
                message: "m".into(),
                finish: pb::FinishReason::Error as i32,
                usage: None,
                cause: pb::FinishCause::ProviderError as i32,
            }),
        ));
        let message = mirror.messages()[0];
        assert_eq!(message.finish, pb::FinishReason::Error as i32);
        assert_eq!(message.finish_cause, pb::FinishCause::ProviderError as i32);
        assert_eq!(
            message.error.as_ref().map(|error| error.message.as_str()),
            Some("http status 400: nope")
        );
        assert_eq!(mirror.last_seq, 8);
    }

    /// A part seeded from the projection is final: stale live deltas for it
    /// (the subscription buffered them before the read) do not double it.
    #[test]
    fn live_deltas_for_a_seeded_part_are_ignored() {
        let mut mirror = V1SessionMirror::from_messages(&[pb::MessageInfo {
            id: "m".into(),
            role: pb::Role::Assistant as i32,
            parts: vec![pb::PartInfo {
                id: "p".into(),
                kind: Some(pb::part_info::Kind::Text(pb::TextPart {
                    text: "Hello".into(),
                })),
            }],
            ..Default::default()
        }]);
        mirror.apply(&frame(0, started("p")));
        mirror.apply(&frame(0, appended("p", "Hel")));
        mirror.apply(&frame(0, appended("p", "lo")));
        mirror.apply(&frame(9, started("p")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
    }
}
