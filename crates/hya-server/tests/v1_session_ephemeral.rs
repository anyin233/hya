//! Ephemeral sessions (docs/protocol/README.md "Ephemeral sessions";
//! ADR-0023 amendment "The daemon drops unused sessions"): a session created
//! with `ephemeral: true` is deleted by the server once it is still unused
//! and no `StreamSessionEvents` stream watches it (after a grace), with a
//! live `sessionDeleted` frame on the global stream. A first message, a
//! title, an archive, or a fork taken from it keep it for good. A session
//! nobody ever watched is checked after the creation grace, and leftovers
//! are swept when a server starts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, EphemeralGrace, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Answers every round with one short text part.
struct TextProvider;

#[async_trait]
impl Provider for TextProvider {
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
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

async fn engine() -> Arc<SessionEngine> {
    engine_on(SessionStore::connect_memory().await.unwrap())
}

fn engine_on(store: SessionStore) -> Arc<SessionEngine> {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(TextProvider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    Arc::new(SessionEngine::new(
        store,
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    ))
}

fn state(engine: Arc<SessionEngine>, grace: EphemeralGrace) -> AppState {
    AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
    .with_ephemeral_grace(grace)
}

/// Short grace after the last watcher leaves; long ones otherwise, so only
/// the check under test can fire.
fn unwatched(after: Duration) -> EphemeralGrace {
    EphemeralGrace {
        unwatched: after,
        unclaimed: Duration::from_secs(60),
        startup: Duration::from_secs(60),
    }
}

async fn app_with(grace: EphemeralGrace) -> axum::Router {
    router(state(engine().await, grace))
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

async fn create(app: &axum::Router, extra: Value) -> Value {
    let mut body = json!({
        "agent": "build",
        "model": "fake",
        "workdir": std::env::temp_dir().to_string_lossy(),
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    let (status, body) = call(app, Method::POST, "/v1/sessions", body).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"].clone()
}

async fn create_ephemeral(app: &axum::Router) -> String {
    let session = create(app, json!({ "ephemeral": true })).await;
    assert_eq!(session["ephemeral"], true, "{session}");
    session["id"].as_str().unwrap().to_owned()
}

async fn get(app: &axum::Router, session: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await
}

async fn exists(app: &axum::Router, session: &str) -> bool {
    get(app, session).await.0 == StatusCode::OK
}

/// Open `session`'s event stream and keep it open until the returned body is
/// dropped (a watching client).
async fn watch(app: &axum::Router, session: &str) -> Body {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/sessions/{session}/events/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.into_body()
}

/// Wait up to `within` for `session` to be gone.
async fn gone_within(app: &axum::Router, session: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if !exists(app, session).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Collect the global stream's frames until `stop` matches one (included) or
/// 15 s pass. Subscribed before this returns.
async fn collect_global(
    app: &axum::Router,
    stop: impl Fn(&Value) -> bool + Send + 'static,
) -> tokio::task::JoinHandle<Vec<Value>> {
    let mut stream = watch_uri(app, "/v1/events/stream").await.into_data_stream();
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

async fn watch_uri(app: &axum::Router, uri: &str) -> Body {
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
    resp.into_body()
}

fn is_deleted(frame: &Value, session: &str) -> bool {
    frame["event"]["sessionDeleted"].is_object() && frame["event"]["session"] == session
}

#[tokio::test]
async fn a_watched_ephemeral_session_is_kept_and_dropped_after_the_last_watcher_leaves() {
    let app = app_with(unwatched(Duration::from_millis(300))).await;
    let session = create_ephemeral(&app).await;
    let target = session.clone();
    let global = collect_global(&app, move |frame| is_deleted(frame, &target)).await;

    let first = watch(&app, &session).await;
    let second = watch(&app, &session).await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(exists(&app, &session).await, "watched: kept");

    // One of two viewers leaves: still watched, still kept past the grace.
    drop(first);
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(exists(&app, &session).await, "still watched: kept");
    let (_, info) = get(&app, &session).await;
    assert_eq!(info["ephemeral"], true, "{info}");

    // The last one leaves: dropped after the grace, and every list learns it.
    drop(second);
    assert!(gone_within(&app, &session, Duration::from_secs(5)).await);
    let frames = global.await.unwrap();
    assert!(
        frames.iter().any(|frame| is_deleted(frame, &session)),
        "{frames:?}"
    );
}

#[tokio::test]
async fn a_watcher_back_within_the_grace_keeps_the_session() {
    let app = app_with(unwatched(Duration::from_millis(600))).await;
    let session = create_ephemeral(&app).await;
    drop(watch(&app, &session).await);
    // A reconnecting client resubscribes before the grace ends.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let back = watch(&app, &session).await;
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(exists(&app, &session).await);
    drop(back);
    assert!(gone_within(&app, &session, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_first_prompt_a_title_an_archive_or_a_fork_keeps_the_session() {
    let app = app_with(unwatched(Duration::from_millis(200))).await;
    let prompted = create_ephemeral(&app).await;
    let titled = create_ephemeral(&app).await;
    let archived = create_ephemeral(&app).await;
    let forked = create_ephemeral(&app).await;
    let watchers = [
        watch(&app, &prompted).await,
        watch(&app, &titled).await,
        watch(&app, &archived).await,
        watch(&app, &forked).await,
    ];

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{prompted}/turns"),
        json!({ "prompt": { "text": "hi" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{prompted}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for (session, patch) in [
        (&titled, json!({ "title": "Named" })),
        (&archived, json!({ "archived": true })),
    ] {
        let (status, body) = call(
            &app,
            Method::PATCH,
            &format!("/v1/sessions/{session}"),
            patch,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{forked}/fork"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(
        body["session"]["ephemeral"], true,
        "a fork is not ephemeral"
    );

    for session in [&prompted, &titled, &archived, &forked] {
        let (_, info) = get(&app, session).await;
        assert_ne!(info["ephemeral"], true, "{session}: {info}");
    }
    drop(watchers);
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    for session in [&prompted, &titled, &archived, &forked] {
        assert!(exists(&app, session).await, "{session} was used: kept");
    }
}

#[tokio::test]
async fn sessions_created_without_the_flag_or_as_children_are_never_dropped() {
    let app = app_with(EphemeralGrace {
        unwatched: Duration::from_millis(100),
        unclaimed: Duration::from_millis(100),
        startup: Duration::from_millis(100),
    })
    .await;
    let plain = create(&app, json!({})).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let parent = create(&app, json!({ "title": "Parent" })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child = create(&app, json!({ "parent": parent, "ephemeral": true })).await;
    assert_ne!(child["ephemeral"], true, "{child}");
    let child = child["id"].as_str().unwrap().to_owned();
    let titled = create(&app, json!({ "title": "Named", "ephemeral": true })).await;
    assert_ne!(titled["ephemeral"], true, "{titled}");
    let titled = titled["id"].as_str().unwrap().to_owned();
    for session in [&plain, &child, &titled] {
        drop(watch(&app, session).await);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    for session in [&plain, &parent, &child, &titled] {
        assert!(exists(&app, session).await, "{session} kept");
    }
}

#[tokio::test]
async fn a_session_nobody_ever_watched_is_dropped_after_the_creation_grace() {
    let app = app_with(EphemeralGrace {
        unwatched: Duration::from_secs(60),
        unclaimed: Duration::from_millis(300),
        startup: Duration::from_secs(60),
    })
    .await;
    let session = create_ephemeral(&app).await;
    assert!(gone_within(&app, &session, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_server_start_sweeps_unused_ephemeral_sessions_left_over() {
    let dir = support::tempdir("ephemeral-sweep");
    let db = dir.join("sessions.db").to_string_lossy().into_owned();
    // A previous server (killed before it could drop them) left these behind.
    let (left_over, used, plain) = {
        let engine = engine_on(SessionStore::connect(&db).await.unwrap());
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                engine
                    .create(CreateSession {
                        parent: None,
                        agent: AgentName::new("build"),
                        model: ModelRef::new("fake"),
                        workdir: std::env::temp_dir().to_string_lossy().into_owned(),
                        project: None,
                        kind: hya_proto::SessionKind::Project,
                    })
                    .await
                    .unwrap(),
            );
        }
        assert!(engine.set_session_ephemeral(ids[0], true).await.unwrap());
        assert!(engine.set_session_ephemeral(ids[1], true).await.unwrap());
        engine.set_title(ids[1], "Used".to_owned()).await.unwrap();
        (ids[0], ids[1], ids[2])
    };
    let app = router(state(
        engine_on(SessionStore::connect(&db).await.unwrap()),
        EphemeralGrace {
            unwatched: Duration::from_secs(60),
            unclaimed: Duration::from_secs(60),
            startup: Duration::from_millis(300),
        },
    ));
    assert!(gone_within(&app, &left_over.to_string(), Duration::from_secs(5)).await);
    assert!(exists(&app, &used.to_string()).await);
    assert!(exists(&app, &plain.to_string()).await);
}

#[tokio::test]
async fn the_startup_sweep_spares_a_session_a_client_watches_again() {
    let dir = support::tempdir("ephemeral-sweep");
    let db = dir.join("sessions.db").to_string_lossy().into_owned();
    let session = {
        let engine = engine_on(SessionStore::connect(&db).await.unwrap());
        let session = engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: std::env::temp_dir().to_string_lossy().into_owned(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            })
            .await
            .unwrap();
        engine.set_session_ephemeral(session, true).await.unwrap();
        session.to_string()
    };
    let app = router(state(
        engine_on(SessionStore::connect(&db).await.unwrap()),
        EphemeralGrace {
            unwatched: Duration::from_secs(60),
            unclaimed: Duration::from_secs(60),
            startup: Duration::from_millis(400),
        },
    ));
    // The client that showed it reconnects after the restart.
    let viewer = watch(&app, &session).await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    assert!(exists(&app, &session).await);
    drop(viewer);
}

/// Dropping an unused Project session changes the Project's session count:
/// every client's Project list learns it through `projectsUpdated`.
#[tokio::test]
async fn dropping_a_project_session_publishes_projects_updated() {
    let app = app_with(EphemeralGrace {
        unwatched: Duration::from_secs(60),
        unclaimed: Duration::from_millis(300),
        startup: Duration::from_secs(60),
    })
    .await;
    let session = create_ephemeral(&app).await;
    let (_, info) = get(&app, &session).await;
    assert!(
        info["projectId"].as_str().is_some_and(|id| !id.is_empty()),
        "{info}"
    );
    let target = session.clone();
    let seen_delete = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen_projects = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (deleted, projects) = (seen_delete.clone(), seen_projects.clone());
    // Subscribed after the creation's own `projectsUpdated` went out.
    let global = collect_global(&app, move |frame| {
        if is_deleted(frame, &target) {
            deleted.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if frame["event"]["projectsUpdated"].is_object() {
            projects.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        deleted.load(std::sync::atomic::Ordering::SeqCst)
            && projects.load(std::sync::atomic::Ordering::SeqCst)
    })
    .await;
    assert!(gone_within(&app, &session, Duration::from_secs(5)).await);
    let frames = global.await.unwrap();
    assert!(
        seen_delete.load(std::sync::atomic::Ordering::SeqCst)
            && seen_projects.load(std::sync::atomic::Ordering::SeqCst),
        "{frames:?}"
    );
}

/// A temporary session dropped while unused leaves its scratch directory on
/// disk: hya never deletes scratch directories (ADR-0024).
#[tokio::test]
async fn dropping_a_temporary_session_keeps_its_scratch_directory() {
    let scratch = support::tempdir("ephemeral-scratch");
    let app = router(
        state(
            engine().await,
            EphemeralGrace {
                unwatched: Duration::from_secs(60),
                unclaimed: Duration::from_millis(300),
                startup: Duration::from_secs(60),
            },
        )
        .with_scratch_root(&scratch),
    );
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "kind": "SESSION_KIND_TEMPORARY",
            "ephemeral": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["session"]["ephemeral"], true, "{body}");
    let session = body["session"]["id"].as_str().unwrap().to_owned();
    let workdir = std::path::PathBuf::from(body["session"]["workdir"].as_str().unwrap());
    assert!(workdir.starts_with(&scratch), "{body}");
    std::fs::write(workdir.join("notes.txt"), "kept").unwrap();

    assert!(gone_within(&app, &session, Duration::from_secs(5)).await);
    assert_eq!(
        std::fs::read_to_string(workdir.join("notes.txt")).unwrap(),
        "kept"
    );
}
