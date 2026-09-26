//! v1 session-list push: a global-stream subscriber (also with
//! `interactionsOnly`) sees root sessions being created, renamed, archived,
//! unarchived, deleted, and going busy and idle — and none of the child
//! (subagent) sessions' list changes — over SSE and gRPC.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// Answers every round with one text part, but only after the test adds a
/// permit to `gate`, so a turn stays running (busy) until released.
struct GatedProvider {
    gate: Arc<Semaphore>,
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let gate = Arc::clone(&self.gate);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let stream = futures::stream::once(async move {
            gate.acquire().await.unwrap().forget();
        })
        .flat_map(move |()| futures::stream::iter(events.clone().into_iter().map(Ok)));
        Ok(Box::pin(stream))
    }
}

async fn state(gate: Arc<Semaphore>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(GatedProvider { gate })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn create(app: &axum::Router, parent: Option<&str>) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": std::env::temp_dir().to_string_lossy(),
            "parent": parent.unwrap_or_default(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

async fn patch(app: &axum::Router, session: &str, body: Value) {
    let (status, body) = call(app, Method::PATCH, &format!("/v1/sessions/{session}"), body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn delete(app: &axum::Router, session: &str) {
    let (status, body) = call(
        app,
        Method::DELETE,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Prompt `session` and wait for the turn; `gate` gets the permit that lets
/// the provider answer once the turn is running.
async fn run_turn(app: &axum::Router, session: &str, gate: &Semaphore) {
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "prompt": { "text": "hi" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(100)).await;
    gate.add_permits(1);
    let (status, body) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Collect SSE frames from `uri` until `stop` matches one (included) or
/// the deadline passes. The stream is subscribed before this returns.
async fn collect_sse(
    app: &axum::Router,
    uri: &str,
    stop: impl Fn(&Value) -> bool + Send + 'static,
) -> tokio::task::JoinHandle<Vec<Value>> {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut buffer = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
            let Ok(Some(Ok(bytes))) = chunk else {
                continue;
            };
            buffer.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(end) = buffer.find("\n\n") {
                let block: String = buffer.drain(..end + 2).collect();
                for line in block.lines() {
                    let Some(data) = line.strip_prefix("data:") else {
                        continue;
                    };
                    if let Ok(frame) = serde_json::from_str::<Value>(data.trim()) {
                        let done = stop(&frame);
                        seen.push(frame);
                        if done {
                            return seen;
                        }
                    }
                }
            }
        }
        seen
    })
}

/// `(session, kind)` of one event frame, where `kind` names the payload and,
/// for `sessionUpdated`, its set fields (`sessionUpdated.title`, …, with
/// `busy`/`archived` values). `resync` frames map to `("", "resync")`.
fn summary(frame: &Value) -> (String, String) {
    if frame.get("resync").is_some() {
        return (String::new(), "resync".to_owned());
    }
    let event = &frame["event"];
    let session = event["session"].as_str().unwrap_or_default().to_owned();
    let object = event.as_object().unwrap();
    let key = object
        .keys()
        .find(|key| !matches!(key.as_str(), "seq" | "session" | "timeRecorded"))
        .cloned()
        .unwrap_or_default();
    let kind = if key == "sessionUpdated" {
        let fields = event["sessionUpdated"].as_object().unwrap();
        let mut parts: Vec<String> = fields
            .iter()
            .map(|(name, value)| match value {
                Value::Bool(flag) => format!("{name}={flag}"),
                _ => name.clone(),
            })
            .collect();
        parts.sort();
        format!("sessionUpdated.{}", parts.join(","))
    } else {
        key
    };
    (session, kind)
}

fn is_deleted(frame: &Value, session: &str) -> bool {
    frame["event"]["sessionDeleted"].is_object() && frame["event"]["session"] == session
}

/// Whether `wanted` occurs in order (not necessarily adjacent) in `kinds`.
fn in_order(kinds: &[String], wanted: &[&str]) -> bool {
    let mut next = wanted.iter().peekable();
    for kind in kinds {
        if next.peek().is_some_and(|want| *want == kind) {
            next.next();
        }
    }
    next.peek().is_none()
}

/// Drive the whole list lifecycle of one root session plus a child of it.
/// Returns `(root, child)`.
async fn lifecycle(app: &axum::Router, gate: &Semaphore) -> (String, String) {
    let root = create(app, None).await;
    let child = create(app, Some(&root)).await;
    patch(app, &root, json!({ "title": "Renamed" })).await;
    patch(app, &child, json!({ "title": "Child renamed" })).await;
    patch(app, &root, json!({ "archived": true })).await;
    patch(app, &root, json!({ "archived": false })).await;
    run_turn(app, &child, gate).await;
    run_turn(app, &root, gate).await;
    // The idle transition is pushed after the turn's run ends; give it a beat.
    tokio::time::sleep(Duration::from_millis(200)).await;
    delete(app, &child).await;
    delete(app, &root).await;
    (root, child)
}

const ROOT_LIST: &[&str] = &[
    "sessionStarted",
    "sessionUpdated.title",
    "sessionUpdated.archived=true",
    "sessionUpdated.archived=false",
    "sessionUpdated.busy=true",
    "sessionUpdated.busy=false",
    "sessionDeleted",
];

#[tokio::test]
async fn interactions_only_stream_carries_root_session_list_frames() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    // Stops at the first deletion frame: the child's (a leak) or the root's.
    let frames = collect_sse(&app, "/v1/events/stream?interactionsOnly=true", |frame| {
        frame["event"]["sessionDeleted"].is_object()
    })
    .await;
    let (root, child) = lifecycle(&app, &gate).await;
    let frames = frames.await.unwrap();
    let rows: Vec<(String, String)> = frames.iter().map(summary).collect();

    // Nothing of the child: not its creation, rename, busy, or deletion.
    assert!(
        rows.iter().all(|(session, _)| *session != child),
        "child-session noise leaked: {rows:#?}"
    );
    // No engine traffic: text, messages, tool parts.
    assert!(
        rows.iter().all(|(_, kind)| {
            kind.starts_with("sessionStarted")
                || kind.starts_with("sessionUpdated")
                || kind == "sessionDeleted"
        }),
        "{rows:#?}"
    );
    let root_kinds: Vec<String> = rows
        .iter()
        .filter(|(session, _)| *session == root)
        .map(|(_, kind)| kind.clone())
        .collect();
    assert!(in_order(&root_kinds, ROOT_LIST), "{root_kinds:#?}");
    assert_eq!(
        root_kinds.last().map(String::as_str),
        Some("sessionDeleted")
    );
    // Busy and deleted frames are live-only (no seq); the rest are durable.
    for frame in &frames {
        let event = &frame["event"];
        let live =
            event["sessionDeleted"].is_object() || event["sessionUpdated"].get("busy").is_some();
        assert_eq!(event.get("seq").is_none(), live, "{frame}");
    }
    assert!(
        frames.iter().any(|frame| is_deleted(frame, &root)),
        "{rows:#?}"
    );
}

#[tokio::test]
async fn unfiltered_global_stream_carries_busy_and_deleted_frames() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    let frames = collect_sse(&app, "/v1/events/stream", |frame| {
        frame["event"]["sessionDeleted"].is_object()
    })
    .await;
    let root = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    run_turn(&app, &child, &gate).await;
    run_turn(&app, &root, &gate).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    delete(&app, &child).await;
    delete(&app, &root).await;
    let frames = frames.await.unwrap();
    let rows: Vec<(String, String)> = frames.iter().map(summary).collect();
    let list_kinds = |session: &str| -> Vec<String> {
        rows.iter()
            .filter(|(owner, kind)| {
                owner == session
                    && (kind.starts_with("sessionUpdated.busy") || kind == "sessionDeleted")
            })
            .map(|(_, kind)| kind.clone())
            .collect()
    };
    assert_eq!(
        list_kinds(&root),
        vec![
            "sessionUpdated.busy=true",
            "sessionUpdated.busy=false",
            "sessionDeleted"
        ],
        "{rows:#?}"
    );
    assert!(list_kinds(&child).is_empty(), "{rows:#?}");
    // The unfiltered stream still carries the engine traffic.
    assert!(
        rows.iter()
            .any(|(session, kind)| *session == root && kind == "messageStarted"),
        "{rows:#?}"
    );
}

/// The same list frames over gRPC `StreamGlobalEvents{interactionsOnly}`,
/// with the deletion itself made over gRPC `DeleteSession`.
#[tokio::test]
async fn session_list_frames_match_over_grpc() {
    use tonic::transport::Server;
    let gate = Arc::new(Semaphore::new(0));
    let app_state = state(Arc::clone(&gate)).await;
    let app = router(app_state.clone());
    let grpc = V1Grpc::new(app_state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serve = Server::builder()
        .add_service(pb::session_server::SessionServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    let channel = tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut events = pb::events_client::EventsClient::new(channel.clone())
        .stream_global_events(pb::StreamGlobalEventsRequest {
            interactions_only: true,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut sessions = pb::session_client::SessionClient::new(channel);
    let sse = collect_sse(&app, "/v1/events/stream?interactionsOnly=true", |frame| {
        frame["event"]["sessionDeleted"].is_object()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;

    let root = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    patch(&app, &root, json!({ "title": "Renamed" })).await;
    run_turn(&app, &root, &gate).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    sessions
        .delete_session(tonic::Request::new(pb::DeleteSessionRequest {
            session: child.clone(),
        }))
        .await
        .unwrap();
    sessions
        .delete_session(tonic::Request::new(pb::DeleteSessionRequest {
            session: root.clone(),
        }))
        .await
        .unwrap();

    let mut grpc_rows = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = events.next().await {
            let Some(pb::stream_frame::Frame::Event(event)) = frame.unwrap().frame else {
                grpc_rows.push((String::new(), "resync".to_owned()));
                continue;
            };
            let kind = match &event.payload {
                Some(pb::stream_event::Payload::SessionStarted(_)) => "sessionStarted".to_owned(),
                Some(pb::stream_event::Payload::SessionUpdated(update)) => {
                    if let Some(busy) = update.busy {
                        assert_eq!(event.seq, 0, "busy is live-only");
                        format!("sessionUpdated.busy={busy}")
                    } else if update.title.is_some() {
                        "sessionUpdated.title".to_owned()
                    } else {
                        "sessionUpdated.other".to_owned()
                    }
                }
                Some(pb::stream_event::Payload::SessionDeleted(_)) => {
                    assert_eq!(event.seq, 0, "deleted is live-only");
                    "sessionDeleted".to_owned()
                }
                _ => "other".to_owned(),
            };
            let done = kind == "sessionDeleted";
            grpc_rows.push((event.session.clone(), kind));
            if done {
                break;
            }
        }
    })
    .await
    .expect("gRPC session-list frames");
    let expected = vec![
        (root.clone(), "sessionStarted".to_owned()),
        (root.clone(), "sessionUpdated.title".to_owned()),
        (root.clone(), "sessionUpdated.busy=true".to_owned()),
        (root.clone(), "sessionUpdated.busy=false".to_owned()),
        (root.clone(), "sessionDeleted".to_owned()),
    ];
    assert_eq!(grpc_rows, expected);
    let sse_rows: Vec<(String, String)> = sse.await.unwrap().iter().map(summary).collect();
    let sse_rows: Vec<(String, String)> = sse_rows
        .into_iter()
        .map(|(session, kind)| {
            if kind.starts_with("sessionUpdated.") && kind.contains("title") {
                (session, "sessionUpdated.title".to_owned())
            } else {
                (session, kind)
            }
        })
        .collect();
    assert_eq!(sse_rows, expected);
}
