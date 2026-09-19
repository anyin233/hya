//! `/v1` events domain: replay, session SSE stream, and the global stream.
//!
//! All three surfaces emit the same curated `StreamFrame` protojson; the
//! typed `resync` frame replaces the legacy SSE event-name signal.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::extract::{Path as AxumPath, Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::Value;
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;

use crate::ServerState;
use hya_api::v1 as pb;
use hya_proto::SessionId;

use super::V1Error;
use super::convert::stream_event;
use super::session::parse_session;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions/:id/events", get(list_events))
        .route("/v1/sessions/:id/events/stream", get(stream_session))
        .route("/v1/events/stream", get(stream_global))
}

async fn list_events(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListEventsResponse>, V1Error> {
    let request: pb::ListEventsRequest = super::query_request(&[("session", id.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    let envelopes = st.engine.replay(session).await?;
    if envelopes.is_empty() {
        return Err(V1Error::session_not_found(&request.session));
    }
    let limit = if request.limit == 0 {
        usize::MAX
    } else {
        request.limit as usize
    };
    let mut events = Vec::new();
    let mut raw_envelopes = Vec::new();
    let mut next_seq = request.since_seq;
    for envelope in envelopes
        .into_iter()
        .filter(|envelope| envelope.seq.0 > request.since_seq)
        .take(limit)
    {
        next_seq = next_seq.max(envelope.seq.0);
        if let Some(event) = stream_event(&envelope) {
            events.push(event);
        }
        if request.include_raw
            && let Ok(line) = serde_json::to_string(&envelope)
        {
            raw_envelopes.push(line);
        }
    }
    Ok(Json(pb::ListEventsResponse {
        session: session.to_string(),
        events,
        next_seq,
        raw_envelopes,
    }))
}

async fn stream_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, V1Error> {
    let request: pb::StreamSessionEventsRequest =
        super::query_request(&[("session", id.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    if !st.engine.session_exists(session).await? {
        return Err(V1Error::session_not_found(&request.session));
    }
    Ok(session_stream(st, Some(session), request.since_seq).into_response())
}

async fn stream_global(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, V1Error> {
    let request: pb::StreamGlobalEventsRequest = super::query_request(&[], &query)?;
    Ok(session_stream(st, None, request.since_seq).into_response())
}

/// Build the SSE stream shared by both scopes.
fn session_stream(
    st: ServerState,
    session: Option<SessionId>,
    since_seq: u64,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events = frame_stream(st, session, since_seq).map(|frame| {
        let event = match frame {
            Ok(frame) => SseEvent::default().json_data(frame).unwrap_or_default(),
            Err(status) => SseEvent::default().event("error").data(status.message()),
        };
        Ok(event)
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// The shared live frame producer backing both SSE and gRPC streams.
///
/// Merges three feeds: the engine event bus (durable events), the pending
/// permission plane, and the pending question plane. Permission and
/// question frames are live-only (`seq == 0`): the pending queues are the
/// authoritative listing, the streams are delivery.
pub(crate) fn frame_stream(
    st: ServerState,
    session: Option<SessionId>,
    since_seq: u64,
) -> impl Stream<Item = Result<pb::StreamFrame, tonic::Status>> {
    let engine =
        BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| async move {
            match result {
                Ok(envelope) => {
                    if let Some(session) = session
                        && envelope.event.session() != Some(session)
                    {
                        return None;
                    }
                    if envelope.seq.0 <= since_seq {
                        return None;
                    }
                    let frame = match stream_event(&envelope) {
                        Some(event) => pb::stream_frame::Frame::Event(event),
                        None => return None,
                    };
                    Some(Ok(pb::StreamFrame { frame: Some(frame) }))
                }
                Err(_lagged) => Some(Ok(pb::StreamFrame {
                    frame: Some(pb::stream_frame::Frame::Resync(pb::ResyncFrame {
                        last_seq: since_seq,
                    })),
                })),
            }
        });
    // Pending planes never error; the Result wrapper matches the merged
    // engine-stream item type (tonic::Status is large but never constructed
    // on these branches).
    #[allow(clippy::result_large_err)]
    let permission = BroadcastStream::new(st.permission_requests.subscribe()).filter_map(
        move |result| async move {
            let Ok(value) = result else { return None };
            interaction_frame(&value, session)
                .map(|frame| Ok(pb::StreamFrame { frame: Some(frame) }))
        },
    );
    #[allow(clippy::result_large_err)]
    let question = BroadcastStream::new(st.question_requests.subscribe()).filter_map(
        move |result| async move {
            let Ok(value) = result else { return None };
            interaction_frame(&value, session)
                .map(|frame| Ok(pb::StreamFrame { frame: Some(frame) }))
        },
    );
    let engine: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(engine);
    let permission: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(permission);
    let question: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(question);
    futures::stream::select_all([engine, permission, question])
}

/// Map one pending-plane broadcast value onto a live frame, honoring the
/// session filter for session-scoped streams.
fn interaction_frame(
    value: &serde_json::Value,
    session: Option<SessionId>,
) -> Option<pb::stream_frame::Frame> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let properties = value.get("properties")?;
    let frame_session = field_str(properties, "sessionID");
    if let Some(session) = session
        && !frame_session.is_empty()
        && frame_session != session.to_string()
    {
        return None;
    }
    let event = match kind {
        "permission.asked" => {
            let interaction = pb::Interaction {
                id: field_str(properties, "id"),
                session: frame_session.clone(),
                r#type: pb::InteractionType::Permission as i32,
                title: format!(
                    "{} {}",
                    field_str(properties, "permission"),
                    properties
                        .get("patterns")
                        .and_then(Value::as_array)
                        .map(|rows| {
                            rows.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_default()
                )
                .trim()
                .to_owned(),
                detail: String::new(),
                options: Vec::new(),
                payload: None,
                time_created: None,
            };
            pb::stream_event::Payload::PermissionRequested(pb::PermissionRequested {
                request: interaction.id.clone(),
                interaction: Some(interaction),
            })
        }
        "question.asked" => {
            let first = properties
                .pointer("/questions/0")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let options: Vec<String> = first
                .get("options")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            row.get("label").and_then(Value::as_str).map(str::to_owned)
                        })
                        .collect()
                })
                .unwrap_or_default();
            let interaction = pb::Interaction {
                id: field_str(properties, "id"),
                session: frame_session.clone(),
                r#type: pb::InteractionType::Question as i32,
                title: field_str(&first, "question"),
                detail: field_str(&first, "header"),
                options,
                payload: None,
                time_created: None,
            };
            pb::stream_event::Payload::QuestionRequested(pb::QuestionRequested {
                request: interaction.id.clone(),
                interaction: Some(interaction),
            })
        }
        "permission.replied" | "question.replied" | "question.rejected" => {
            pb::stream_event::Payload::InteractionResolved(pb::InteractionResolved {
                request: field_str(properties, "requestID"),
            })
        }
        _ => return None,
    };
    Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
        seq: 0,
        session: frame_session.clone(),
        time_recorded: super::convert::timestamp(now_millis()),
        payload: Some(event),
    }))
}

fn field_str(value: &serde_json::Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
