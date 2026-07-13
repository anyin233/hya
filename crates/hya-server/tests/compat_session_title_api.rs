#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::CreateSessionResponse;
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-compat-session-title-api";

async fn state_with_script(script: Vec<Vec<FakeStep>>) -> AppState {
    let provider = FakeProvider::scripted_turns(script);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        tools,
        perm,
        EventBus::default(),
    ));
    AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn post_status(app: axum::Router, uri: &str, body: Value) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

async fn patch_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn get_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn create_api_session(app: axum::Router, body: Value) -> String {
    let (status, created) = post_json(app, "/api/session", body).await;
    assert_eq!(status, StatusCode::OK);
    created["data"]["id"]
        .as_str()
        .expect("session id")
        .to_string()
}

async fn create_legacy_session(app: axum::Router) -> String {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"agent": "build", "model": "fake", "workdir": WORKDIR}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    created.session.to_string()
}

async fn prompt_without_resume(app: axum::Router, session: &str, text: &str) {
    let (status, _) = post_json(
        app,
        &format!("/api/session/{session}/prompt"),
        json!({"prompt": {"text": text}, "delivery": "queue", "resume": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn compat_legacy_session_create_preserves_requested_title() {
    let app = router(state_with_script(vec![]).await);

    let (status, created) = post_json(app, "/session", json!({"title": "SDK workflow"})).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["title"], "SDK workflow");
}

#[tokio::test]
async fn compat_title_replaces_root_fallback_with_clean_title_output() {
    let app = router(
        state_with_script(vec![vec![
            FakeStep::Text(
                "<think>hidden reasoning</think>\n\nGenerated project title\nIgnored".to_string(),
            ),
            FakeStep::Finish(FinishReason::Stop),
        ]])
        .await,
    );
    let session =
        create_api_session(app.clone(), json!({"location": {"directory": WORKDIR}})).await;

    prompt_without_resume(
        app.clone(),
        &session,
        "please inspect the repository and propose the next migration step",
    )
    .await;

    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (status, body) = get_json(app.clone(), format!("/api/session/{session}")).await;
            assert_eq!(status, StatusCode::OK);
            if body["data"]["title"] == "Generated project title" {
                break body;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("title generated");
    assert_eq!(got["data"]["title"], "Generated project title");
}

#[tokio::test]
async fn compat_title_does_not_replace_child_session_title() {
    let app = router(
        state_with_script(vec![vec![
            FakeStep::Text("Should not be used".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]])
        .await,
    );
    let parent = create_api_session(app.clone(), json!({"location": {"directory": WORKDIR}})).await;
    let child = create_api_session(
        app.clone(),
        json!({"parentID": parent, "location": {"directory": WORKDIR}}),
    )
    .await;

    prompt_without_resume(
        app.clone(),
        &child,
        "child task should stay fallback titled",
    )
    .await;

    let (status, got) = get_json(app, format!("/api/session/{child}")).await;
    assert_eq!(status, StatusCode::OK);
    let title = got["data"]["title"].as_str().expect("title");
    assert!(title.starts_with("Untitled Session_"), "{title}");
}

#[tokio::test]
async fn compat_title_does_not_replace_manual_title() {
    let app = router(
        state_with_script(vec![vec![
            FakeStep::Text("Should not replace manual".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ]])
        .await,
    );
    let session =
        create_api_session(app.clone(), json!({"location": {"directory": WORKDIR}})).await;
    let (status, updated) = patch_json(
        app.clone(),
        format!("/api/session/{session}"),
        json!({"title": "Manual title"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["data"]["title"], "Manual title");

    prompt_without_resume(app.clone(), &session, "later prompt must not rename").await;

    let (status, got) = get_json(app, format!("/api/session/{session}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["data"]["title"], "Manual title");
}

#[tokio::test]
async fn compat_prompt_async_replaces_root_fallback_with_clean_title_output() {
    let app = router(
        state_with_script(vec![
            vec![
                FakeStep::Text(
                    "<think>ignore</think>\n\nAsync generated title\nUnused".to_string(),
                ),
                FakeStep::Finish(FinishReason::Stop),
            ],
            vec![
                FakeStep::Text("async answer".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ])
        .await,
    );
    let session = create_legacy_session(app.clone()).await;

    let status = post_status(
        app.clone(),
        &format!("/session/{session}/prompt_async"),
        json!({"text": "hello async title"}),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let session_body = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let body = get_json(app.clone(), format!("/session/{session}")).await.1;
            if body["title"] == "Async generated title" {
                break body;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("async title generated");
    assert_eq!(session_body["title"], "Async generated title");
}
