#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef, SessionId};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{
    InteractionPlane, PermissionPlane, PermissionRules, QuestionAnswer, QuestionInfo, QuestionKind,
    QuestionOption, QuestionPrompt, ToolRegistry,
};

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-question-batch-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn engine(
    permission: PermissionPlane,
    interaction: InteractionPlane,
) -> (Arc<SessionEngine>, Arc<AgentSpec>) {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(store, router, tools, permission, EventBus::default())
            .with_interaction(interaction),
    );
    let agent = Arc::new(AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: tempdir(),
        reasoning: None,
    });
    (engine, agent)
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn request(app: axum::Router, method: &str, uri: &str, body: Option<Value>) -> Value {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    assert!(resp.status().is_success());
    body_json(resp).await
}

async fn wait_for_data(app: axum::Router, uri: &str) -> Value {
    for _ in 0..100 {
        let body = request(app.clone(), "GET", uri, None).await;
        if body["data"].as_array().is_some_and(|data| !data.is_empty()) {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("pending request did not appear");
}

fn option(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_string(),
        description: description.to_string(),
    }
}

#[tokio::test]
async fn opencode_question_batch_lists_and_replies_atomically() {
    // Given
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (interaction, question_rx) = InteractionPlane::new();
    let scoped_interaction = interaction.clone();
    let (engine, agent) = engine(permission, interaction).await;
    let session = create_session(&engine, &agent).await;
    let app = router(AppState::new(engine, agent).with_question_requests(question_rx));

    let task_session: SessionId = session.parse().unwrap();
    let task = tokio::spawn(async move {
        scoped_interaction
            .for_session(task_session)
            .ask_many(vec![
                QuestionPrompt::new(
                    QuestionInfo {
                        question: "Pick colors".to_string(),
                        header: "Colors".to_string(),
                        options: vec![option("red", "Warm"), option("blue", "Cool")],
                        multiple: true,
                        custom: Some(false),
                    },
                    QuestionKind::Select {
                        options: vec!["red".to_string(), "blue".to_string()],
                        allow_custom: false,
                    },
                ),
                QuestionPrompt::new(
                    QuestionInfo {
                        question: "Branch name?".to_string(),
                        header: "Branch".to_string(),
                        options: Vec::new(),
                        multiple: false,
                        custom: Some(true),
                    },
                    QuestionKind::FreeText {
                        default: Some(String::new()),
                    },
                ),
            ])
            .await
    });

    // When
    let listed = wait_for_data(app.clone(), &format!("/api/session/{session}/question")).await;
    let request_id = listed["data"][0]["id"].as_str().unwrap();

    // Then
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    assert_eq!(listed["data"][0]["questions"].as_array().unwrap().len(), 2);
    assert_eq!(listed["data"][0]["questions"][0]["header"], "Colors");
    assert_eq!(
        listed["data"][0]["questions"][0]["options"][1]["description"],
        "Cool"
    );
    assert_eq!(listed["data"][0]["questions"][0]["multiple"], true);
    assert_eq!(listed["data"][0]["questions"][1]["header"], "Branch");

    let reply = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/session/{session}/question/{request_id}/reply"
        ))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"answers": [["red", "blue"], ["codex/batch"]]}).to_string(),
        ))
        .unwrap();
    let reply = app.oneshot(reply).await.unwrap();
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        task.await.unwrap().unwrap(),
        vec![
            QuestionAnswer::SelectedMany(vec![0, 1]),
            QuestionAnswer::FreeText("codex/batch".to_string())
        ]
    );
}
