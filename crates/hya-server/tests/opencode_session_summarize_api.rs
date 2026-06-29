#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{
    AgentSpec, CompactionConfig, CreateSession, EventBus, ModelSummarizer, SessionEngine,
};
use hya_proto::{AgentName, FinishReason, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-session-summarize-api";

async fn state_with_session() -> (AppState, String) {
    let provider = FakeProvider::scripted(vec![
        FakeStep::Text("CONDENSED summary".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let summarizer = Arc::new(ModelSummarizer::new(router.clone(), ModelRef::new("fake")));
    let engine = Arc::new(
        SessionEngine::new(store, router, tools, perm, EventBus::default()).with_compaction(
            summarizer,
            CompactionConfig {
                token_threshold: 1,
                keep_recent: 1,
            },
        ),
    );
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: WORKDIR.to_string(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "first detail".to_string())
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "second detail".to_string())
        .await
        .unwrap();
    let app_state = AppState::new(
        engine,
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    );
    (app_state, session.to_string())
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn opencode_session_summarize_persists_summary_message_and_compaction_metadata() {
    let (state, session) = state_with_session().await;
    let app = router(state);

    let update = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/session/{session}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"metadata": {"owner": "opencode"}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/summarize"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"providerID": "hya", "modelID": "fake", "auto": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!(true));

    let messages = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(messages.status(), StatusCode::OK);
    let body = body_json(messages).await;
    let summary = body
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| message["info"]["role"] == "system")
        .expect("summary message");
    assert_eq!(summary["parts"][0]["type"], "text");
    let summary_text = summary["parts"][0]["text"].as_str().expect("summary text");
    assert!(summary_text.contains("CONDENSED summary"), "{summary_text}");

    let session_info = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(session_info.status(), StatusCode::OK);
    let body = body_json(session_info).await;
    assert_eq!(body["metadata"]["owner"], "opencode");
    assert_eq!(
        body["metadata"]["_hyaOpenCodeCompaction"]["type"],
        "compaction"
    );
    assert_eq!(body["metadata"]["_hyaOpenCodeCompaction"]["auto"], true);
    assert_eq!(
        body["metadata"]["_hyaOpenCodeCompaction"]["providerID"],
        "hya"
    );
    assert_eq!(
        body["metadata"]["_hyaOpenCodeCompaction"]["modelID"],
        "fake"
    );
    assert_eq!(
        body["metadata"]["_hyaOpenCodeCompaction"]["messageID"],
        summary["info"]["id"]
    );
    assert_eq!(
        body["metadata"]["_hyaOpenCodeCompaction"]["partID"],
        summary["parts"][0]["id"]
    );
}

#[tokio::test]
async fn opencode_session_summarize_missing_session_returns_not_found() {
    let (state, _) = state_with_session().await;
    let app = router(state);
    let missing = SessionId::new().to_string();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{missing}/summarize"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"providerID": "hya", "modelID": "fake", "auto": false}).to_string(),
                ))
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
