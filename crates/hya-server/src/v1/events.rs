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
    if !st.engine.session_exists(session).await? {
        return Err(V1Error::session_not_found(&request.session));
    }
    let envelopes = st
        .engine
        .store()
        .replay_page(session, request.since_seq, request.limit)
        .await?;
    let mut events = Vec::new();
    let mut raw_envelopes = Vec::new();
    let mut next_seq = request.since_seq;
    for envelope in envelopes {
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

/// A converted frame and its SSE encoding, shared by every subscriber.
pub(crate) struct SharedFrame {
    pub(crate) frame: pb::StreamFrame,
    sse: SseEvent,
}

impl SharedFrame {
    fn new(frame: pb::StreamFrame) -> std::sync::Arc<Self> {
        let sse = SseEvent::default()
            .json_data(&frame)
            .unwrap_or_else(|error| SseEvent::default().event("error").data(error.to_string()));
        std::sync::Arc::new(Self { frame, sse })
    }
}

#[derive(Clone)]
pub(crate) struct FrameHub {
    frames: tokio::sync::broadcast::Sender<std::sync::Arc<PublishedFrame>>,
}

impl FrameHub {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            frames: tokio::sync::broadcast::channel(capacity).0,
        }
    }
}

struct PublishedFrame {
    audience: Audience,
    encoded: std::sync::Arc<SharedFrame>,
}

enum Audience {
    Engine {
        session: Option<SessionId>,
        seq: u64,
        list: bool,
    },
    Interaction,
    All,
    Global,
    Resync {
        global_only: bool,
    },
}

/// Subscribe to the source channels synchronously, before returning the router.
/// Conversion and JSON encoding happen here, before the shared broadcast.
pub(crate) fn spawn_frame_hub(st: &ServerState) {
    use tokio::sync::broadcast::error::RecvError;
    let mut bus = st.engine.bus().subscribe();
    let mut permissions = st.permission_requests.subscribe();
    let mut questions = st.question_requests.subscribe();
    let mut catalogs = st.catalog_updates.subscribe();
    let mut projects = st.projects_updates.subscribe();
    let mut sessions = st.session_list.subscribe();
    let st = st.clone();
    let spawning = st.clone();
    spawning.spawn_background(async move {
        loop {
            let (audience, frame) = tokio::select! {
                _ = st.streams.closed() => break,
                received = bus.recv() => match received {
                    Ok(envelope) => {
                        if st.frames.frames.receiver_count() == 0 { continue; }
                        let Some(event) = counted_stream_event(&st, &envelope) else { continue };
                        let list = is_session_list_event(&st, &envelope.event).await;
                        (Audience::Engine { session: envelope.event.session(), seq: envelope.seq.0, list },
                            pb::StreamFrame { frame: Some(pb::stream_frame::Frame::Event(event)) })
                    }
                    Err(RecvError::Lagged(_)) => (Audience::Resync { global_only: false }, resync_frame(0)),
                    Err(RecvError::Closed) => break,
                },
                received = permissions.recv() => {
                    let Ok(value) = received else { continue };
                    let Some(frame) = interaction_frame(&value) else { continue };
                    (Audience::Interaction, pb::StreamFrame { frame: Some(frame) })
                },
                received = questions.recv() => {
                    let Ok(value) = received else { continue };
                    let Some(frame) = interaction_frame(&value) else { continue };
                    (Audience::Interaction, pb::StreamFrame { frame: Some(frame) })
                },
                received = catalogs.recv() => {
                    let Ok(notice) = received else { continue };
                    (Audience::All, catalog_updated_frame(&notice))
                },
                received = projects.recv() => {
                    let Ok(()) = received else { continue };
                    (Audience::Global, projects_updated_frame())
                },
                received = sessions.recv() => match received {
                    Ok(notice) => (Audience::Global, session_list_frame(notice)),
                    Err(RecvError::Lagged(_)) => (Audience::Resync { global_only: true }, resync_frame(0)),
                    Err(RecvError::Closed) => break,
                },
            };
            let _ = st.frames.frames.send(std::sync::Arc::new(PublishedFrame {
                audience,
                encoded: SharedFrame::new(frame),
            }));
        }
    });
}

async fn is_session_list_event(st: &ServerState, event: &hya_proto::Event) -> bool {
    match event {
        hya_proto::Event::SessionCreated { parent, .. } => parent.is_none(),
        hya_proto::Event::AgentArchived { .. } | hya_proto::Event::AgentRestarted { .. } => true,
        hya_proto::Event::SessionTitled { session, .. }
        | hya_proto::Event::AgentSwitched { session, .. }
        | hya_proto::Event::ModelSwitched { session, .. }
        | hya_proto::Event::SessionPermissionModeSet { session, .. }
        | hya_proto::Event::SessionArchived { session, .. }
        | hya_proto::Event::SessionUnarchived { session, .. } => {
            crate::session_list::is_root(st, *session).await
        }
        _ => false,
    }
}

fn session_stream(
    st: ServerState,
    scope: StreamScope,
    since_seq: u64,
    interactions_only: bool,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events =
        frame_stream(st, scope, since_seq, interactions_only).map(|frame| Ok(frame.sse.clone()));
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// SSE and gRPC share conversion, scope filtering, lag and shutdown handling.
/// Durable frames respect the watermark; live frames (`seq == 0`) always pass.
/// Descendant scopes include only descendants' interactions. Global list-only
/// streams include durable root-session list events and all live notices.
pub(crate) fn frame_stream(
    st: ServerState,
    scope: StreamScope,
    since_seq: u64,
    interactions_only: bool,
) -> impl Stream<Item = std::sync::Arc<SharedFrame>> {
    let closed = st.streams.closed();
    let watching = scope.session_id().map(|session| st.watchers.watch(session));
    let shutdown = st.streams.clone();
    let frames = BroadcastStream::new(st.frames.frames.subscribe()).filter_map(move |result| {
        let st = st.clone();
        async move {
            let published = match result {
                Ok(frame) => frame,
                Err(_) => return Some(SharedFrame::new(resync_frame(since_seq))),
            };
            let visible = match published.audience {
                Audience::Engine { session, seq, list } => {
                    (seq == 0 || seq > since_seq)
                        && scope
                            .session_id()
                            .is_none_or(|wanted| Some(wanted) == session)
                        && (!interactions_only || (seq > 0 && list))
                }
                Audience::Interaction => {
                    interaction_in_scope(&st.engine, &published.encoded.frame, scope).await
                }
                Audience::All => true,
                Audience::Global => matches!(scope, StreamScope::Global),
                Audience::Resync { global_only } => {
                    return (!global_only || matches!(scope, StreamScope::Global))
                        .then(|| SharedFrame::new(resync_frame(since_seq)));
                }
            };
            visible.then(|| published.encoded.clone())
        }
    });
    let stopping = futures::stream::once(async move {
        shutdown
            .reason()
            .map(|reason| SharedFrame::new(server_stopping_frame(reason)))
    })
    .filter_map(futures::future::ready);
    frames.take_until(closed).chain(stopping).map(move |frame| {
        let _watching = &watching;
        frame
    })
}

fn resync_frame(last_seq: u64) -> pb::StreamFrame {
    pb::StreamFrame {
        frame: Some(pb::stream_frame::Frame::Resync(pb::ResyncFrame {
            last_seq,
        })),
    }
}

/// One live session-list notice as its frame (`seq` 0).
fn session_list_frame(notice: crate::session_list::SessionListNotice) -> pb::StreamFrame {
    use crate::session_list::SessionListNotice as N;
    let (session, payload) = match notice {
        N::Busy { session, busy } => (
            session,
            pb::stream_event::Payload::SessionUpdated(pb::SessionUpdated {
                busy: Some(busy),
                ..Default::default()
            }),
        ),
        N::Deleted { session } => (
            session,
            pb::stream_event::Payload::SessionDeleted(pb::SessionDeleted {}),
        ),
    };
    pb::StreamFrame {
        frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
            seq: 0,
            session: session.to_string(),
            time_recorded: now_timestamp(),
            payload: Some(payload),
        })),
    }
}

/// Milliseconds since the Unix epoch, as the frames' `timeRecorded`.
fn now_timestamp() -> Option<pbjson_types::Timestamp> {
    super::convert::timestamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
            .unwrap_or_default(),
    )
}

/// The live-only, process-wide `serverStopping` frame: the last frame of
/// every stream when the server shuts down.
fn server_stopping_frame(reason: crate::streams::ShutdownReason) -> pb::StreamFrame {
    pb::StreamFrame {
        frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
            seq: 0,
            session: String::new(),
            time_recorded: now_timestamp(),
            payload: Some(pb::stream_event::Payload::ServerStopping(
                pb::ServerStopping {
                    reason: reason.as_str().to_owned(),
                },
            )),
        })),
    }
}

/// The live-only, process-wide `catalogUpdated` frame of one
/// `catalog.updated` notice (`crate::state::catalog_notice`): `projectId`
/// names the Project whose catalog changed, empty for a global change.
fn catalog_updated_frame(notice: &serde_json::Value) -> pb::StreamFrame {
    let project_id = notice["properties"]["projectId"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    process_notice_frame(pb::stream_event::Payload::CatalogUpdated(
        pb::CatalogUpdated { project_id },
    ))
}

/// The live-only, process-wide `projectsUpdated` frame (global stream only).
fn projects_updated_frame() -> pb::StreamFrame {
    process_notice_frame(pb::stream_event::Payload::ProjectsUpdated(
        pb::ProjectsUpdated {},
    ))
}

/// A live-only (`seq` 0), session-less notice frame stamped now.
fn process_notice_frame(payload: pb::stream_event::Payload) -> pb::StreamFrame {
    pb::StreamFrame {
        frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
            seq: 0,
            session: String::new(),
            time_recorded: now_timestamp(),
            payload: Some(payload),
        })),
    }
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

/// Match interaction sessions after conversion, without rebuilding the frame.
async fn interaction_in_scope(
    engine: &hya_core::SessionEngine,
    frame: &pb::StreamFrame,
    scope: StreamScope,
) -> bool {
    let StreamScope::Session {
        session,
        descendants,
    } = scope
    else {
        return true;
    };
    let Some(pb::stream_frame::Frame::Event(event)) = &frame.frame else {
        return true;
    };
    if event.session.is_empty() || event.session == session.to_string() {
        return true;
    }
    if !descendants {
        return false;
    }
    let Ok(asking) = event.session.parse::<SessionId>() else {
        return false;
    };
    is_descendant(engine, asking, session).await
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

fn counted_stream_event(
    st: &ServerState,
    envelope: &hya_proto::Envelope,
) -> Option<pb::StreamEvent> {
    #[cfg(test)]
    st.stream_conversions
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = st;
    stream_event(envelope)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod fanout_tests {
    use super::*;
    use hya_core::{AgentCatalog, AgentSpec, EventBus, RuntimeRegistry, SessionEngine};
    use hya_proto::{AgentName, Event, ModelRef};
    use hya_provider::ProviderRouter;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
    use std::sync::{Arc, atomic::Ordering};

    async fn state() -> crate::AppState {
        let catalog = hya_bundle::BundleCatalog::from_prepared(&[]).unwrap();
        let runtime = RuntimeRegistry::from_snapshot(
            ToolRegistry::from_tool_families(&[]).snapshot(),
            Arc::new(AgentCatalog::new(Arc::new(catalog)).unwrap()),
        );
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let engine = SessionEngine::new(
            hya_store::SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new()),
            Arc::new(runtime),
            permission,
            EventBus::default(),
        );
        crate::AppState::new(
            Arc::new(engine),
            Arc::new(AgentSpec {
                name: AgentName::new("test"),
                model: ModelRef::new("fake"),
                system_prompt: String::new(),
                workdir: std::env::temp_dir(),
                reasoning: None,
            }),
        )
    }

    #[tokio::test]
    async fn one_conversion_per_event_independent_of_subscriber_count() {
        for subscribers in [1, 8] {
            let app = state().await;
            let st = app.server_state();
            let mut streams = (0..subscribers)
                .map(|_| Box::pin(frame_stream(st.clone(), StreamScope::Global, 0, false)))
                .collect::<Vec<_>>();
            st.engine.bus().publish(hya_proto::Envelope {
                seq: hya_proto::EventSeq(0),
                ts_millis: 0,
                event: Event::SessionTitled {
                    session: SessionId::new(),
                    title: "shared".into(),
                },
            });
            for stream in &mut streams {
                tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
                    .await
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(
                st.stream_conversions.load(Ordering::Relaxed),
                1,
                "conversion count with {subscribers} subscribers"
            );
            app.shutdown().await;
        }
    }
    #[tokio::test]
    async fn shutdown_joins_server_services_and_reaper_children() {
        let app = state().await;
        let engine = std::sync::Arc::downgrade(&app.engine);
        let (permission, asks) = PermissionPlane::new(PermissionRules::default());
        let (questions, receiver) = hya_tool::InteractionPlane::new();
        let app = app
            .with_permission_requests(asks)
            .with_question_requests(receiver);
        let router = crate::router(app.clone());
        let st = app.server_state();
        // Seven long-lived services plus at most one startup sweep. Reaching
        // nine tracked tasks proves at least one delayed reaper child exists.
        for _ in 0..3 {
            st.watchers
                .schedule(SessionId::new(), crate::ephemeral::Wait::Startup);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while st.tasks.len() < 9 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        app.shutdown().await;
        assert!(st.stop.is_cancelled());
        assert!(st.tasks.is_empty(), "server tasks survived shutdown");
        // Keep the request senders and router alive through the join: channel
        // closure must not be what stopped either interaction bridge.
        drop((permission, questions, router, st, app));
        assert!(
            engine.upgrade().is_none(),
            "a detached server task retained the engine"
        );
    }

    #[tokio::test]
    async fn router_retains_owner_and_last_drop_cancels_services() {
        let app = state().await;
        let router = crate::router(app.clone());
        let st = app.server_state();
        drop(app);
        assert!(!st.stop.is_cancelled(), "live router lost its task owner");
        drop(router);
        assert!(st.stop.is_cancelled());
        st.tasks.close();
        tokio::time::timeout(std::time::Duration::from_secs(5), st.tasks.wait())
            .await
            .unwrap();
        assert!(st.tasks.is_empty());
    }
}
