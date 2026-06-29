#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::{CreateSessionResponse, PromptResponse};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-abort-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Bash,
        "**",
        Mode::Allow,
    )]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get_status(app: axum::Router) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri("/session/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn wait_until_busy(app: axum::Router, session: &str) {
    for _ in 0..100 {
        let status = get_status(app.clone()).await;
        assert_eq!(status.status(), StatusCode::OK);
        let body = body_json(status).await;
        if body[session]["type"] == "busy" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session did not become busy");
}

#[tokio::test]
async fn opencode_abort_cancels_running_shell() {
    let dir = tempdir();
    let app = router(state(dir.clone()).await);

    let empty_status = get_status(app.clone()).await;
    assert_eq!(empty_status.status(), StatusCode::OK);
    assert_eq!(body_json(empty_status).await, json!({}));

    let create = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "agent": "build",
                        "model": "fake",
                        "workdir": dir.to_string_lossy(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let created: CreateSessionResponse = serde_json::from_value(body_json(create).await).unwrap();
    let session = created.session.to_string();

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

    let abort = app
        .clone()
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
    assert_eq!(body_json(abort).await, json!(true));

    let shell = tokio::select! {
        joined = &mut shell_task => joined.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell request did not finish after abort");
        }
    };
    assert_eq!(shell.status(), StatusCode::OK);
    let response: PromptResponse = serde_json::from_value(body_json(shell).await).unwrap();
    assert_eq!(response.finish, FinishReason::Cancelled);

    let final_status = get_status(app).await;
    assert_eq!(final_status.status(), StatusCode::OK);
    assert_eq!(body_json(final_status).await, json!({}));
}
