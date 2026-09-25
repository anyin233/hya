//! v1 session permission modes: `UpdateSession.permission_mode`,
//! `SessionInfo.permission_mode`, the `SessionUpdated` stream mapping,
//! `ListPermissionModes`, and switching to `yolo` allowing the tree's
//! pending permission asks.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef, PermissionRequestId, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::permission::{Action, AskRequest, Decision, RememberScope, Resource};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tower::ServiceExt;

async fn base_state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
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

async fn mode_of(app: &axum::Router, session: &str) -> Value {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["permissionMode"].clone()
}

async fn set_mode(app: &axum::Router, session: &str, mode: &str) -> (StatusCode, Value) {
    call(
        app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({ "permissionMode": mode }),
    )
    .await
}

#[tokio::test]
async fn update_session_sets_and_reports_the_tree_mode() {
    let app = router(base_state().await);
    let root = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    assert_eq!(mode_of(&app, &root).await, json!("manual"));

    let (status, body) = set_mode(&app, &child, "yolo").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["permissionMode"], json!("yolo"));
    assert_eq!(mode_of(&app, &root).await, json!("yolo"));
    assert_eq!(mode_of(&app, &child).await, json!("yolo"));

    let (status, list) = call(&app, Method::GET, "/v1/sessions", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["permissionMode"] == json!("yolo")),
        "{list}"
    );

    for invalid in ["danger", "acme/approver/careful", "careful"] {
        let (status, body) = set_mode(&app, &root, invalid).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}: {body}");
        assert_eq!(body["error"]["code"], json!("invalid_argument"));
    }
    assert_eq!(mode_of(&app, &root).await, json!("yolo"));
}

#[tokio::test]
async fn mode_changes_replay_as_session_updated_on_the_root() {
    let app = router(base_state().await);
    let root = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    assert_eq!(set_mode(&app, &child, "yolo").await.0, StatusCode::OK);
    let (status, events) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{root}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{events}");
    let updated = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|event| event.get("sessionUpdated"))
        .expect("a sessionUpdated event on the root");
    assert_eq!(updated["permissionMode"], json!("yolo"));
    assert!(updated.get("title").is_none(), "{updated}");
}

#[tokio::test]
async fn list_permission_modes_starts_with_the_builtins() {
    let app = router(base_state().await);
    let (status, body) = call(&app, Method::GET, "/v1/permission-modes", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let modes = body["modes"].as_array().unwrap();
    assert_eq!(modes.len(), 2, "{body}");
    assert_eq!(modes[0]["id"], json!("manual"));
    assert_eq!(modes[0]["source"], json!("builtin"));
    assert_eq!(modes[1]["id"], json!("yolo"));
    assert_eq!(modes[1]["source"], json!("builtin"));
    assert!(
        modes[1]["title"]
            .as_str()
            .is_some_and(|title| !title.is_empty())
    );
}

fn ask(
    asks: &mpsc::UnboundedSender<AskRequest>,
    session: SessionId,
    command: &str,
) -> (String, oneshot::Receiver<Decision>) {
    let (reply, rx) = oneshot::channel();
    let id = PermissionRequestId::new();
    asks.send(AskRequest {
        id,
        session: Some(session),
        message_id: None,
        call_id: None,
        action: Action::Bash,
        resource: Resource::Command(command.to_string()),
        remember: RememberScope::LegacyAction,
        reply,
    })
    .unwrap();
    (id.to_string(), rx)
}

async fn pending_ids(app: &axum::Router) -> Vec<String> {
    let (status, body) = call(
        app,
        Method::GET,
        "/v1/interactions?type=INTERACTION_TYPE_PERMISSION",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // protojson omits an empty repeated field.
    body["interactions"]
        .as_array()
        .map_or_else(Vec::new, |rows| {
            rows.iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect()
        })
}

async fn wait_pending(app: &axum::Router, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while pending_ids(app).await.len() < count {
        assert!(tokio::time::Instant::now() < deadline, "asks never arrived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Collect SSE frames from `uri` until one satisfies `predicate`.
async fn frame_matching(
    app: axum::Router,
    uri: String,
    predicate: impl Fn(&Value) -> bool,
) -> bool {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let mut stream = resp.into_body().into_data_stream();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(bytes))) =
            tokio::time::timeout(Duration::from_millis(500), stream.next()).await
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(data) = line.strip_prefix("data:")
                && let Ok(frame) = serde_json::from_str::<Value>(data.trim())
                && predicate(&frame)
            {
                return true;
            }
        }
    }
    false
}

#[tokio::test]
async fn switching_to_yolo_allows_the_trees_pending_asks_once() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let app = router(base_state().await.with_permission_requests(ask_rx));
    let root = create(&app, None).await;
    let child = create(&app, Some(&root)).await;
    let other = create(&app, None).await;

    let (child_id, child_reply) = ask(&ask_tx, child.parse().unwrap(), "printf child");
    let (root_id, root_reply) = ask(&ask_tx, root.parse().unwrap(), "printf root");
    let (other_id, mut other_reply) = ask(&ask_tx, other.parse().unwrap(), "printf other");
    wait_pending(&app, 3).await;

    let resolved = tokio::spawn(frame_matching(
        app.clone(),
        "/v1/events/stream".to_string(),
        move |frame| frame["event"]["interactionResolved"]["request"] == json!(child_id),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = set_mode(&app, &root, "yolo").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(child_reply.await.unwrap(), Decision::AllowOnce);
    assert_eq!(root_reply.await.unwrap(), Decision::AllowOnce);
    assert!(
        resolved.await.unwrap(),
        "an interactionResolved frame must announce the auto-approval"
    );
    assert!(other_reply.try_recv().is_err(), "other trees keep waiting");
    assert_eq!(pending_ids(&app).await, std::slice::from_ref(&other_id));
    assert!(!pending_ids(&app).await.contains(&root_id));

    // An ask that reaches the pending plane after the switch (a call that
    // read the mode just before it changed) is allowed on arrival.
    let (_late_id, late_reply) = ask(&ask_tx, child.parse().unwrap(), "printf late");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), late_reply)
            .await
            .expect("late ask answered")
            .unwrap(),
        Decision::AllowOnce
    );
    assert_eq!(pending_ids(&app).await, [other_id]);

    // Switching back to manual leaves new asks pending.
    assert_eq!(set_mode(&app, &root, "manual").await.0, StatusCode::OK);
    let (_manual_id, mut manual_reply) = ask(&ask_tx, child.parse().unwrap(), "printf manual");
    wait_pending(&app, 2).await;
    assert!(manual_reply.try_recv().is_err());
}
