//! v1 interaction stream frames: pending permission/question requests and
//! their resolutions arrive on the live event streams, and an "always"
//! permission reply persists a saved rule.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::PermissionRequestId;
use hya_proto::{AgentName, ModelRef, QuestionRequestId, SessionId};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::interaction::{QuestionKind, QuestionRequest};
use hya_tool::permission::{Action, AskRequest, RememberScope, Resource};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

async fn base_state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
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

async fn respond(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
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

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
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
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Collect SSE frames until `predicate` matches or the deadline passes.
async fn frames_until(
    app: &axum::Router,
    uri: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Vec<Value> {
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
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Ok(Some(Ok(bytes))) = chunk else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            if let Ok(frame) = serde_json::from_str::<Value>(data.trim()) {
                if predicate(&frame) {
                    return {
                        seen.push(frame);
                        seen
                    };
                }
                seen.push(frame);
            }
        }
    }
    seen
}

#[tokio::test]
async fn permission_asks_and_resolutions_stream_as_interaction_frames() {
    let (ask_tx, ask_rx) = mpsc::unbounded_channel::<AskRequest>();
    let state = base_state().await.with_permission_requests(ask_rx);
    let app = router(state);

    // Open the global stream first, then push an ask through the bridge.
    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(&stream_app, "/v1/events/stream", |frame| {
            frame.get("event").is_some_and(|event| {
                event
                    .get("permissionRequested")
                    .is_some_and(|request| !request["request"].as_str().unwrap_or("").is_empty())
            })
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let request_id = PermissionRequestId::new();
    let id = request_id.to_string();
    ask_tx
        .send(AskRequest {
            id: request_id,
            session: Some(SessionId::new()),
            message_id: None,
            call_id: None,
            action: Action::Bash,
            resource: Resource::Command("printf hi > file".to_string()),
            remember: RememberScope::LegacyAction,
            reply: reply_tx,
        })
        .expect("ask should flow through the bridge");

    let frames = collector.await.expect("collector task");
    let asked = frames
        .iter()
        .find_map(|frame| frame["event"]["permissionRequested"].as_object().cloned())
        .expect("a permissionRequested frame must arrive");
    assert_eq!(asked["request"].as_str().unwrap(), id);
    let interaction = &asked["interaction"];
    assert_eq!(interaction["type"], json!("INTERACTION_TYPE_PERMISSION"));
    assert!(
        interaction["title"]
            .as_str()
            .is_some_and(|title| title.contains("bash")),
        "title should carry the action: {interaction}"
    );

    // Subscribe for the resolution before replying (broadcast is
    // subscriber-live only), then answer "always".
    let resolved_app = app.clone();
    let resolved_id = id.clone();
    let resolved = tokio::spawn(async move {
        frames_until(&resolved_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("interactionResolved")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = respond(
        &app,
        &format!("/v1/interactions/{id}/respond"),
        json!({"permission": {"allowed": true, "persist": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["applied"], json!(true));
    let _ = reply_rx.await;
    let resolved = resolved.await.expect("resolved collector");
    assert!(
        resolved
            .iter()
            .any(|frame| frame["event"]["interactionResolved"]["request"] == json!(resolved_id)),
        "an interactionResolved frame must fire for {resolved_id}: {resolved:?}"
    );

    let (status, rules) = get_json(&app, "/v1/permissions/rules").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        rules["rules"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"].as_str().is_some())),
        "the always reply must persist a saved rule: {rules}"
    );
}

#[tokio::test]
async fn question_asks_stream_and_reject_resolves() {
    let (question_tx, question_rx) = mpsc::unbounded_channel::<QuestionRequest>();
    let state = base_state().await.with_question_requests(question_rx);
    let app = router(state);

    let stream_app = app.clone();
    let collector = tokio::spawn(async move {
        frames_until(&stream_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("questionRequested")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let id = QuestionRequestId::new();
    let id_str = id.to_string();
    question_tx
        .send(QuestionRequest {
            id,
            session: Some(SessionId::new()),
            prompt: "Continue?".to_string(),
            info: hya_tool::interaction::QuestionInfo {
                question: "Continue?".to_string(),
                header: "Confirm".to_string(),
                options: vec![],
                multiple: false,
                custom: None,
            },
            kind: QuestionKind::FreeText { default: None },
            questions: vec![hya_tool::interaction::QuestionPrompt::new(
                hya_tool::interaction::QuestionInfo {
                    question: "Continue?".to_string(),
                    header: "Confirm".to_string(),
                    options: vec![],
                    multiple: false,
                    custom: None,
                },
                QuestionKind::FreeText { default: None },
            )],
            reply: hya_tool::QuestionReply::Many(reply_tx),
        })
        .expect("question should flow through the bridge");

    let frames = collector.await.expect("collector task");
    let asked = frames
        .iter()
        .find_map(|frame| frame["event"]["questionRequested"].as_object().cloned())
        .expect("a questionRequested frame must arrive");
    assert_eq!(asked["request"].as_str().unwrap(), id_str);
    assert_eq!(
        asked["interaction"]["type"],
        json!("INTERACTION_TYPE_QUESTION")
    );
    assert_eq!(asked["interaction"]["title"], json!("Continue?"));

    let resolved_app = app.clone();
    let resolved_id = id_str.clone();
    let resolved = tokio::spawn(async move {
        frames_until(&resolved_app, "/v1/events/stream", |frame| {
            frame["event"]
                .get("interactionResolved")
                .is_some_and(Value::is_object)
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, body) = respond(
        &app,
        &format!("/v1/interactions/{id_str}/respond"),
        json!({"question": {"rejected": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let resolved = resolved.await.expect("resolved collector");
    assert!(
        resolved
            .iter()
            .any(|frame| frame["event"]["interactionResolved"]["request"] == json!(resolved_id)),
        "rejection must resolve: {resolved:?}"
    );
}
