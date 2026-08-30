//! Integration tests for `hya-server`: compat prompt async events api.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use hya_proto::{Envelope, Event, FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_server::router;
use serde_json::json;
use tower::ServiceExt;

mod compat_prompt_async_support;

use compat_prompt_async_support::{
    body_json, create_session, post_prompt_async, shell_state, state, state_with_router,
    wait_until_busy,
};

/// Provider that fails the first root turn and succeeds on the next call.
struct FailOnceProvider {
    inner: FakeProvider,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for FailOnceProvider {
    /// Return the configured test provider id.
    fn id(&self) -> &str {
        self.inner.id()
    }

    /// Claim the same models and capabilities as the deterministic fake provider.
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }

    /// Fail one provider call, then delegate later calls to the fake provider.
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ProviderError::Decode(format!(
                "planned provider failure{}",
                "x".repeat(3_000),
            )));
        }
        self.inner.stream(request, session, message).await
    }
}

/// Submit one Compat V2 background prompt and return its admission status.
async fn post_v2_prompt(app: axum::Router, session: &str, text: &str) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/session/{session}/prompt"))
            .header("content-type", "application/json")
            .body(Body::from(json!({"prompt": {"text": text}}).to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

/// Replay one Session through the native public Events route.
async fn replay_events(app: axum::Router, session: &str) -> Vec<Envelope> {
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/sessions/{session}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_value(body_json(response).await).unwrap()
}

#[tokio::test]
async fn compat_prompt_async_publishes_session_error_event_on_background_failure() {
    let app = router(state_with_router(ProviderRouter::new(), "missing").await);
    let session = create_session(app.clone()).await;

    let event_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(event_resp.status(), StatusCode::OK);
    let mut stream = event_resp.into_body().into_data_stream();
    let connected = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connected event")
        .expect("body chunk")
        .expect("valid chunk");
    assert!(
        String::from_utf8(connected.to_vec())
            .unwrap()
            .contains("server.connected")
    );

    let status = post_prompt_async(app.clone(), &session, "hello async").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let error_frame = tokio::time::timeout(Duration::from_secs(2), async {
        let mut combined = String::new();
        loop {
            let Some(chunk) = stream.next().await else {
                panic!("event stream ended before session.error");
            };
            let bytes = chunk.expect("body chunk");
            combined.push_str(std::str::from_utf8(&bytes).unwrap());
            if combined.contains("\"type\":\"session.error\"") {
                break combined;
            }
        }
    })
    .await
    .expect("session.error event");
    assert!(error_frame.contains(&format!("\"sessionID\":\"{session}\"")));
    assert!(error_frame.contains("\"name\":\"UnknownError\""));
    assert!(error_frame.contains("unknown provider for model: fake"));
}

/// Compat V2 background failures are durable, return idle, and do not poison the Session.
#[tokio::test]
async fn compat_v2_prompt_publishes_failure_returns_idle_and_accepts_later_turn() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FailOnceProvider {
        inner: FakeProvider::scripted(vec![
            FakeStep::Text("recovered answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]),
        calls: Arc::clone(&calls),
    };
    let state = state_with_router(ProviderRouter::new().with(Arc::new(provider)), "fake").await;
    let engine = Arc::clone(&state.engine);
    let app = router(state);
    let session = create_session(app.clone()).await;
    let session_id: SessionId = session.parse().unwrap();
    engine
        .set_title(session_id, "Pinned test title".to_string())
        .await
        .unwrap();

    let first_status = post_v2_prompt(app.clone(), &session, "fail once").await;
    assert_eq!(first_status, StatusCode::OK);

    let failed_events = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = replay_events(app.clone(), &session).await;
            let error_index = events.iter().position(|envelope| {
                matches!(
                    &envelope.event,
                    Event::Error {
                        session: Some(event_session),
                        code,
                        message,
                    } if *event_session == session_id
                        && code == "prompt_async"
                        && message.contains("planned provider failure")
                )
            });
            let idle_index = events.iter().position(|envelope| {
                matches!(
                    &envelope.event,
                    Event::SessionStatus { session: event_session, status }
                        if *event_session == session_id && status["type"] == "idle"
                )
            });
            if matches!((error_index, idle_index), (Some(error), Some(idle)) if idle > error) {
                break events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("durable failure followed by idle status");
    assert!(failed_events.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::Error { message, .. }
                if message.starts_with("decode: planned provider failure")
                    && message.chars().count() == 2_048
        )
    }));

    let second_status = post_v2_prompt(app.clone(), &session, "recover now").await;
    assert_eq!(second_status, StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let events = replay_events(app.clone(), &session).await;
            let recovered = events.iter().any(|envelope| {
                matches!(
                    &envelope.event,
                    Event::TextReplace { text, .. } if text == "recovered answer"
                )
            });
            let finished = events.iter().any(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MessageFinished {
                        finish: FinishReason::Stop,
                        ..
                    }
                )
            });
            if recovered && finished {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("later successful turn in the same Session");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn compat_legacy_prompt_publishes_failure_before_idle() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = FailOnceProvider {
        inner: FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]),
        calls: Arc::clone(&calls),
    };
    let state = state_with_router(ProviderRouter::new().with(Arc::new(provider)), "fake").await;
    let engine = Arc::clone(&state.engine);
    let app = router(state);
    let session = create_session(app.clone()).await;
    let session_id: SessionId = session.parse().unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/message"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "agent": "build",
                        "model": {"providerID": "fake", "modelID": "fake"},
                        "parts": [{"type": "text", "text": "fail once"}]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let events = engine.replay(session_id).await.unwrap();

    let error_index = events
        .iter()
        .position(|envelope| {
            matches!(
                &envelope.event,
                Event::Error { session: Some(event_session), code, message }
                    if *event_session == session_id
                        && code == "prompt"
                        && message.starts_with("decode: planned provider failure")
                        && message.chars().count() == 2_048
            )
        })
        .expect("bounded durable prompt error");
    let idle_index = events
        .iter()
        .rposition(|envelope| {
            matches!(
                &envelope.event,
                Event::SessionStatus { session: event_session, status }
                    if *event_session == session_id && status["type"] == "idle"
            )
        })
        .expect("idle Session status");
    assert!(idle_index > error_index);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn compat_prompt_async_busy_returns_no_content_and_publishes_error() {
    let app = router(shell_state().await);
    let session = create_session(app.clone()).await;

    let event_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(event_resp.status(), StatusCode::OK);
    let mut stream = event_resp.into_body().into_data_stream();
    let connected = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connected event")
        .expect("body chunk")
        .expect("valid chunk");
    assert!(
        String::from_utf8(connected.to_vec())
            .unwrap()
            .contains("server.connected")
    );

    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        shell_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/sessions/{shell_session}/shell"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"command": "sleep 20 && printf should-not-finish"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    });
    wait_until_busy(app.clone(), &session).await;

    let status = post_prompt_async(app.clone(), &session, "blocked").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let error_frame = tokio::time::timeout(Duration::from_secs(2), async {
        let mut combined = String::new();
        loop {
            let Some(chunk) = stream.next().await else {
                panic!("event stream ended before session.error");
            };
            let bytes = chunk.expect("body chunk");
            combined.push_str(std::str::from_utf8(&bytes).unwrap());
            if combined.contains("\"type\":\"session.error\"") {
                break combined;
            }
        }
    })
    .await
    .expect("session.error event");
    assert!(error_frame.contains(&format!("\"sessionID\":\"{session}\"")));
    assert!(error_frame.contains("\"name\":\"UnknownError\""));
    assert!(error_frame.contains("session busy"));

    let abort = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/abort"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(abort.status(), StatusCode::OK);
    tokio::select! {
        result = &mut shell_task => {
            let shell = result.unwrap();
            assert_eq!(shell.status(), StatusCode::OK);
        }
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell did not stop after abort");
        }
    }
}

#[tokio::test]
async fn compat_prompt_async_publishes_session_status_events() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let event_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(event_resp.status(), StatusCode::OK);
    let mut stream = event_resp.into_body().into_data_stream();
    let connected = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("connected event")
        .expect("body chunk")
        .expect("valid chunk");
    assert!(
        String::from_utf8(connected.to_vec())
            .unwrap()
            .contains("server.connected")
    );

    let status = post_prompt_async(app.clone(), &session, "hello async").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let frames = tokio::time::timeout(Duration::from_secs(2), async {
        let mut combined = String::new();
        loop {
            let Some(chunk) = stream.next().await else {
                panic!("event stream ended before status events");
            };
            let bytes = chunk.expect("body chunk");
            combined.push_str(std::str::from_utf8(&bytes).unwrap());
            let has_busy = combined.contains("\"type\":\"session.status\"")
                && combined.contains("\"status\":{\"type\":\"busy\"}");
            let has_idle = combined.contains("\"type\":\"session.status\"")
                && combined.contains("\"status\":{\"type\":\"idle\"}");
            if has_busy && has_idle {
                break combined;
            }
        }
    })
    .await
    .expect("session.status events");
    assert!(frames.contains(&format!("\"sessionID\":\"{session}\"")));
}
