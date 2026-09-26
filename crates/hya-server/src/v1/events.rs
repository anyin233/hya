//! `/v1` events domain: replay, session SSE stream, and the global stream.
//!
//! All three surfaces emit the same curated `StreamFrame` protojson; the
//! typed `resync` frame replaces the legacy SSE event-name signal.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use super::Json;
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
    let scope = StreamScope::session(session, request.include_descendants);
    Ok(session_stream(st, scope, request.since_seq, false).into_response())
}

async fn stream_global(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, V1Error> {
    let request: pb::StreamGlobalEventsRequest = super::query_request(&[], &query)?;
    Ok(session_stream(
        st,
        StreamScope::Global,
        request.since_seq,
        request.interactions_only,
    )
    .into_response())
}

/// Which sessions a live stream serves.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamScope {
    /// Every session.
    Global,
    /// One session's frames; with `descendants`, also the live interaction
    /// frames of every session below it in its tree.
    Session {
        /// The streamed session.
        session: SessionId,
        /// Deliver descendants' interaction frames too.
        descendants: bool,
    },
}

impl StreamScope {
    pub(crate) fn session(session: SessionId, descendants: bool) -> Self {
        Self::Session {
            session,
            descendants,
        }
    }

    fn session_id(self) -> Option<SessionId> {
        match self {
            Self::Global => None,
            Self::Session { session, .. } => Some(session),
        }
    }
}

/// Build the SSE stream shared by both scopes.
fn session_stream(
    st: ServerState,
    scope: StreamScope,
    since_seq: u64,
    interactions_only: bool,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events = frame_stream(st, scope, since_seq, interactions_only).map(|frame| {
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
/// Merges four feeds: the engine event bus, the pending permission plane,
/// the pending question plane, and provider-catalog notices (a live-only,
/// process-wide `catalogUpdated` frame on every scope). With
/// `interactions_only` the engine bus (and so its `resync` frames) is left
/// out. Permission and question frames are
/// live-only (`seq == 0`): the pending queues are the authoritative
/// listing, the streams are delivery. A session scope with `descendants`
/// also passes interaction frames whose session lies below the streamed one
/// (walked up the parent chain), each still naming the asking session.
///
/// The bus carries durable envelopes (`seq > 0`, filtered by `since_seq`)
/// and live-only ones (`seq == 0`: assistant text deltas of an in-flight
/// provider round), which always pass: they are never persisted, so no
/// watermark can have covered them. A subscriber that falls more than the
/// bus capacity behind gets one `resync` frame per lag and loses the
/// frames in the gap, live deltas included; the durable log (and so the
/// projection) is unaffected.
///
/// Every stream ends when the server starts shutting down
/// (`StreamShutdown::close`), so connected clients never hold the graceful
/// shutdown open.
pub(crate) fn frame_stream(
    st: ServerState,
    scope: StreamScope,
    since_seq: u64,
    interactions_only: bool,
) -> impl Stream<Item = Result<pb::StreamFrame, tonic::Status>> {
    let closed = st.streams.closed();
    let session = scope.session_id();
    let lineage = st.engine.clone();
    let engine =
        BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| async move {
            match result {
                Ok(envelope) => {
                    if let Some(session) = session
                        && envelope.event.session() != Some(session)
                    {
                        return None;
                    }
                    if !is_live(&envelope) && envelope.seq.0 <= since_seq {
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
    let permission_lineage = lineage.clone();
    #[allow(clippy::result_large_err)]
    let permission =
        BroadcastStream::new(st.permission_requests.subscribe()).filter_map(move |result| {
            let lineage = permission_lineage.clone();
            async move {
                let Ok(value) = result else { return None };
                scoped_interaction_frame(&lineage, &value, scope)
                    .await
                    .map(|frame| Ok(pb::StreamFrame { frame: Some(frame) }))
            }
        });
    #[allow(clippy::result_large_err)]
    let question =
        BroadcastStream::new(st.question_requests.subscribe()).filter_map(move |result| {
            let lineage = lineage.clone();
            async move {
                let Ok(value) = result else { return None };
                scoped_interaction_frame(&lineage, &value, scope)
                    .await
                    .map(|frame| Ok(pb::StreamFrame { frame: Some(frame) }))
            }
        });
    let engine: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(engine);
    let permission: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(permission);
    let question: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(question);
    #[allow(clippy::result_large_err)]
    let catalog = BroadcastStream::new(st.catalog_updates.subscribe())
        .filter_map(|result| async { result.ok().map(|_notice| Ok(catalog_updated_frame())) });
    let catalog: std::pin::Pin<
        Box<dyn Stream<Item = Result<pb::StreamFrame, tonic::Status>> + Send>,
    > = Box::pin(catalog);
    let mut feeds = vec![permission, question, catalog];
    if !interactions_only {
        feeds.push(engine);
    }
    futures::stream::select_all(feeds).take_until(closed)
}

/// The live-only, process-wide `catalogUpdated` frame.
fn catalog_updated_frame() -> pb::StreamFrame {
    pb::StreamFrame {
        frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
            seq: 0,
            session: String::new(),
            time_recorded: super::convert::timestamp(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
                    .unwrap_or_default(),
            ),
            payload: Some(pb::stream_event::Payload::CatalogUpdated(
                pb::CatalogUpdated {},
            )),
        })),
    }
}

/// Live-only envelopes are published with `seq == 0` and never persisted.
fn is_live(envelope: &hya_proto::Envelope) -> bool {
    envelope.seq.0 == 0
}

/// Wire view of one pending permission ask from its `permission.asked`
/// properties (the same view the listing serializes). The payload names the
/// decision (`action`, `resource`, `always`) and, when the ask is correlated
/// with a tool call, the call (`messageId`, `callId`, `tool`, `input`).
pub(crate) fn permission_interaction(properties: &Value) -> pb::Interaction {
    let patterns: Vec<&str> = properties
        .get("patterns")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let action = field_str(properties, "permission");
    let resource = patterns.join(" ");
    let mut payload = serde_json::Map::new();
    payload.insert("action".into(), Value::String(action.clone()));
    payload.insert("resource".into(), Value::String(resource.clone()));
    if let Some(always) = properties.get("always").filter(|always| always.is_array()) {
        payload.insert("always".into(), always.clone());
    }
    if let Some(tool) = properties.get("tool") {
        for (from, to) in [
            ("messageID", "messageId"),
            ("callID", "callId"),
            ("name", "tool"),
        ] {
            if let Some(text) = tool
                .get(from)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                payload.insert(to.into(), Value::String(text.to_owned()));
            }
        }
        if let Some(input) = tool.get("input") {
            payload.insert("input".into(), input.clone());
        }
    }
    let session = field_str(properties, "sessionID");
    pb::Interaction {
        id: field_str(properties, "id"),
        session: session
            .parse::<SessionId>()
            .map(|id| id.to_string())
            .unwrap_or(session),
        r#type: pb::InteractionType::Permission as i32,
        title: format!("{action} {resource}").trim().to_owned(),
        detail: String::new(),
        options: Vec::new(),
        payload: Some(super::convert::to_struct(Value::Object(payload))),
        time_created: None,
    }
}

/// Longest parent chain walked when matching a descendant's frame; subagent
/// trees are far shallower (two layers below the lead).
const MAX_LINEAGE_HOPS: usize = 32;

/// `interaction_frame` under a stream scope: a session scope keeps its own
/// session's frames (and frames with no session), plus — with `descendants`
/// — frames of sessions whose parent chain reaches the streamed session.
async fn scoped_interaction_frame(
    engine: &hya_core::SessionEngine,
    value: &Value,
    scope: StreamScope,
) -> Option<pb::stream_frame::Frame> {
    let frame = interaction_frame(value)?;
    let StreamScope::Session {
        session,
        descendants,
    } = scope
    else {
        return Some(frame);
    };
    let pb::stream_frame::Frame::Event(event) = &frame else {
        return Some(frame);
    };
    if event.session.is_empty() || event.session == session.to_string() {
        return Some(frame);
    }
    if !descendants {
        return None;
    }
    let asking = event.session.parse::<SessionId>().ok()?;
    is_descendant(engine, asking, session)
        .await
        .then_some(frame)
}

/// Whether `ancestor` is on `session`'s parent chain.
async fn is_descendant(
    engine: &hya_core::SessionEngine,
    session: SessionId,
    ancestor: SessionId,
) -> bool {
    let mut current = session;
    for _ in 0..MAX_LINEAGE_HOPS {
        let Ok(projection) = engine.read_projection_shared(current).await else {
            return false;
        };
        match projection.session.parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

/// Wire view of one pending question from its `question.asked` properties
/// (the same view the listing serializes). The first question fills
/// `title`, `detail` (its header), and `options` (its labels); `payload`
/// carries every question as `{questions: [{question, header, options:
/// [{label, description}], multiple?, custom?}]}`.
pub(crate) fn question_interaction(properties: &Value) -> pb::Interaction {
    let first = properties
        .pointer("/questions/0")
        .cloned()
        .unwrap_or(Value::Null);
    let options: Vec<String> = first
        .get("options")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("label").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let questions = properties
        .get("questions")
        .filter(|questions| questions.is_array())
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let mut payload = serde_json::Map::new();
    payload.insert("questions".into(), questions);
    pb::Interaction {
        id: field_str(properties, "id"),
        session: field_str(properties, "sessionID"),
        r#type: pb::InteractionType::Question as i32,
        title: field_str(&first, "question"),
        detail: field_str(&first, "header"),
        options,
        payload: Some(super::convert::to_struct(Value::Object(payload))),
        time_created: None,
    }
}

/// Map one pending-plane broadcast value onto a live frame.
fn interaction_frame(value: &serde_json::Value) -> Option<pb::stream_frame::Frame> {
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let properties = value.get("properties")?;
    let frame_session = field_str(properties, "sessionID");
    let event = match kind {
        "permission.asked" => {
            let interaction = permission_interaction(properties);
            pb::stream_event::Payload::PermissionRequested(pb::PermissionRequested {
                request: interaction.id.clone(),
                interaction: Some(interaction),
            })
        }
        "question.asked" => {
            let interaction = question_interaction(properties);
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
