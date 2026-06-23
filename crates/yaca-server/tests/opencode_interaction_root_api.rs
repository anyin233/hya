#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{
    Action, InteractionPlane, PermissionPlane, PermissionRules, QuestionAnswer, QuestionKind,
    Resource, ToolRegistry,
};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-interaction-root-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn engine(
    permission: PermissionPlane,
    interaction: Option<InteractionPlane>,
) -> (Arc<SessionEngine>, Arc<AgentSpec>) {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let providers = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let store = SessionStore::connect_memory().await.unwrap();
    let mut engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    if let Some(interaction) = interaction {
        engine = engine.with_interaction(interaction);
    }
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: tempdir(),
        reasoning: None,
    });
    (Arc::new(engine), agent)
}

async fn create_session(engine: &SessionEngine, agent: &AgentSpec) -> String {
    engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
        })
        .await
        .unwrap()
        .to_string()
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

async fn wait_for_array(app: axum::Router, uri: &str) -> Value {
    for _ in 0..100 {
        let response = request(app.clone(), "GET", uri, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body.as_array().is_some_and(|items| !items.is_empty()) {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("pending request did not appear");
}

#[tokio::test]
async fn opencode_permission_root_routes_list_reply_and_type_missing_errors() {
    let (permission, permission_rx) = PermissionPlane::new(PermissionRules::default());
    let scoped_permission = permission.clone();
    let (engine, agent) = engine(permission, None).await;
    let session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_permission_requests(permission_rx));

    let task_session = session.parse().unwrap();
    let task = tokio::spawn(async move {
        scoped_permission
            .for_session(task_session)
            .assert(Action::Bash, Resource::Command("pwd".to_string()))
            .await
    });

    let listed = wait_for_array(app.clone(), "/permission").await;
    let request_id = listed[0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed[0]["sessionID"], session);

    let invalid = request(
        app.clone(),
        "POST",
        "/permission/invalid-permission-id/reply",
        Some(json!({"reply": "once"})),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let missing = request(
        app.clone(),
        "POST",
        "/permission/per_missing/reply",
        Some(json!({"reply": "once"})),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(missing).await,
        json!({
            "_tag": "PermissionNotFoundError",
            "requestID": "per_missing",
            "message": "Permission request not found: per_missing"
        })
    );

    let reply = request(
        app,
        "POST",
        &format!("/permission/{request_id}/reply"),
        Some(json!({"reply": "once"})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(body_json(reply).await, json!(true));
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn opencode_question_root_routes_list_reply_reject_and_type_missing_errors() {
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, question_rx) = InteractionPlane::new();
    let scoped_interaction = interaction.clone();
    let (engine, agent) = engine(permission, Some(interaction)).await;
    let session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_question_requests(question_rx));

    let task_session = session.parse().unwrap();
    let task = tokio::spawn(async move {
        scoped_interaction
            .for_session(task_session)
            .ask(
                "Continue?".to_string(),
                QuestionKind::Select {
                    options: vec!["yes".to_string(), "no".to_string()],
                    allow_custom: false,
                },
            )
            .await
    });

    let listed = wait_for_array(app.clone(), "/question").await;
    let request_id = listed[0]["id"].as_str().unwrap().to_string();
    assert_eq!(listed[0]["sessionID"], session);

    let invalid = request(
        app.clone(),
        "POST",
        "/question/invalid-question-id/reply",
        Some(json!({"answers": [["yes"]]})),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let missing = request(app.clone(), "POST", "/question/que_missing/reject", None).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(missing).await,
        json!({
            "_tag": "QuestionNotFoundError",
            "requestID": "que_missing",
            "message": "Question request not found: que_missing"
        })
    );

    let reply = request(
        app,
        "POST",
        &format!("/question/{request_id}/reply"),
        Some(json!({"answers": [["yes"]]})),
    )
    .await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(body_json(reply).await, json!(true));
    assert!(matches!(
        task.await.unwrap(),
        Ok(QuestionAnswer::Selected(index)) if index == 0
    ));
}
