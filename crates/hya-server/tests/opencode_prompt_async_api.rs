#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_proto::SessionId;
use hya_server::router;
use serde_json::{Value, json};
use tower::ServiceExt;

mod opencode_prompt_async_support;

use opencode_prompt_async_support::{create_session, get_messages, post_prompt_async, state};

#[tokio::test]
async fn opencode_prompt_async_returns_no_content_and_records_messages() {
    let app = router(state().await);
    let session = create_session(app.clone()).await;

    let status = post_prompt_async(app.clone(), &session, "hello async").await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let messages = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = get_messages(app.clone(), &session).await;
            if body[1]["parts"][1]["text"] == "async answer" {
                break body;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async prompt completed");
    assert_eq!(messages[0]["parts"][0]["text"], "hello async");
    assert_eq!(messages[1]["parts"][1]["text"], "async answer");
}

#[tokio::test]
async fn opencode_prompt_async_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{missing}/prompt_async"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "never"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
    );
}
