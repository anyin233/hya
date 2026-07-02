#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use hya_provider::ProviderRouter;
use hya_server::router;
use serde_json::json;
use tower::ServiceExt;

mod compat_prompt_async_support;

use compat_prompt_async_support::{
    create_session, post_prompt_async, shell_state, state, state_with_router, wait_until_busy,
};

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
