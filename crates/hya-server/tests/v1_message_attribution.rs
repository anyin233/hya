//! v1 per-message attribution: every assistant message reports the agent and
//! model its own turn ran with (not the session's current binding), plus
//! creation / last-update times, on both the transcript read and the live
//! `messageStarted` frame.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, TokenUsage,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// Claims every model and answers with one short text part plus usage, so
/// each round records a `UsageRecorded` for the model the engine sent.
struct EchoProvider;

#[async_trait]
impl Provider for EchoProvider {
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
        req: CompletionRequest,
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
                delta: format!("answered by {}", req.model),
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
                tokens: Some(TokenUsage {
                    input: 3,
                    output: 2,
                    ..TokenUsage::default()
                }),
                cause: None,
            },
        ];
        let stream = futures::stream::iter(events).then(|event| async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(event)
        });
        Ok(Box::pin(stream))
    }
}

async fn state() -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(EchoProvider)));
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
            model: ModelRef::new("fake/alpha"),
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

/// Collect SSE `StreamFrame`s until `done` matches one (inclusive) or 10 s pass.
async fn sse_frames_until(
    app: axum::Router,
    uri: String,
    done: impl Fn(&Value) -> bool + Send + 'static,
) -> Vec<Value> {
    let resp = app
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
    let mut frames = Vec::new();
    let mut buffer = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let chunk = tokio::time::timeout(Duration::from_millis(500), stream.next()).await;
        let Ok(Some(Ok(bytes))) = chunk else {
            continue;
        };
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = buffer.find("\n\n") {
            let record: String = buffer.drain(..end + 2).collect();
            for line in record.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(frame) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                let finished = done(&frame);
                frames.push(frame);
                if finished {
                    return frames;
                }
            }
        }
    }
    frames
}

/// Run one prompt to completion.
async fn run_turn(app: &axum::Router, session: &str, text: &str) {
    let (status, created) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": text}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let turn = created["turn"]["id"].as_str().unwrap();
    let (status, waited) = call(
        app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["state"], json!("TURN_STATE_FINISHED"), "{waited}");
}

async fn assistant_messages(app: &axum::Router, session: &str) -> Vec<Value> {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == json!("ROLE_ASSISTANT"))
        .cloned()
        .collect()
}

fn millis(timestamp: &Value) -> i64 {
    let parsed: pbjson_types::Timestamp = serde_json::from_value(timestamp.clone())
        .unwrap_or_else(|error| panic!("RFC 3339 timestamp expected, got {timestamp}: {error}"));
    parsed.seconds * 1000 + i64::from(parsed.nanos) / 1_000_000
}

#[tokio::test]
async fn each_assistant_message_reports_its_own_agent_model_and_times() {
    let app = router(state().await);
    let (status, body) = call(
        &app,
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake/alpha",
            "workdir": std::env::temp_dir().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let session = body["session"]["id"].as_str().unwrap().to_owned();

    run_turn(&app, &session, "first").await;

    // Switch both bindings between turns; the second turn's live
    // `messageStarted` must already carry the new attribution.
    let (status, updated) = call(
        &app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({"model": "fake/beta", "agent": "plan"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    let collector = tokio::spawn(sse_frames_until(
        app.clone(),
        format!("/v1/sessions/{session}/events/stream"),
        |frame| frame["event"]["messageStarted"]["role"] == json!("ROLE_ASSISTANT"),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    run_turn(&app, &session, "second").await;

    let assistants = assistant_messages(&app, &session).await;
    assert_eq!(assistants.len(), 2, "{assistants:#?}");
    let attribution: Vec<(&Value, &Value)> = assistants
        .iter()
        .map(|message| (&message["agent"], &message["model"]))
        .collect();
    assert_eq!(
        attribution,
        vec![
            (&json!("build"), &json!("fake/alpha")),
            (&json!("plan"), &json!("fake/beta")),
        ],
        "{assistants:#?}"
    );
    for message in &assistants {
        let created = millis(&message["timeCreated"]);
        let updated = millis(&message["timeUpdated"]);
        assert!(created > 0 && created <= updated, "{message:#}");
    }
    assert!(
        millis(&assistants[0]["timeUpdated"]) <= millis(&assistants[1]["timeCreated"]),
        "{assistants:#?}"
    );

    let frames = collector.await.unwrap();
    let started = frames
        .iter()
        .find_map(|frame| {
            let started = &frame["event"]["messageStarted"];
            (started["role"] == json!("ROLE_ASSISTANT")).then_some(started)
        })
        .unwrap_or_else(|| panic!("assistant messageStarted missing: {frames:#?}"));
    assert_eq!(started["message"], assistants[1]["id"]);
    assert_eq!(started["agent"], json!("plan"));
    assert_eq!(started["model"], json!("fake/beta"));
}
