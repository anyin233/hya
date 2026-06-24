#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::{CreateSessionResponse, PromptResponse};
use yaca_proto::{AgentName, FinishReason, ModelRef, PartId, SessionId};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-revert-api";

async fn state(target_file: &str) -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    std::fs::write(format!("{WORKDIR}/{target_file}"), "old\n").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": target_file,
                    "oldString": "old\n",
                    "newString": "new\n",
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("assistant answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Bash, "**", Mode::Allow),
        Rule::new(Action::Edit, "**", Mode::Allow),
    ]));
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn two_edit_state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    std::fs::write(format!("{WORKDIR}/first-revert-target.txt"), "first old\n").unwrap();
    std::fs::write(
        format!("{WORKDIR}/second-revert-target.txt"),
        "second old\n",
    )
    .unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": "first-revert-target.txt",
                    "oldString": "first old\n",
                    "newString": "first new\n",
                }),
            },
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": "second-revert-target.txt",
                    "oldString": "second old\n",
                    "newString": "second new\n",
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("assistant answer".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Edit,
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
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
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
}

async fn create_session(app: axum::Router) -> String {
    let response = request(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": WORKDIR})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(response).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
}

async fn wait_until_busy(app: axum::Router, session: &str) {
    for _ in 0..100 {
        let response = request(app.clone(), "GET", "/session/status", None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        if body[session]["type"] == "busy" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session did not become busy");
}

async fn prompt_message(app: axum::Router, session: &str) -> String {
    let response = request(
        app,
        "POST",
        &format!("/sessions/{session}/prompt"),
        Some(json!({"text": "revert me"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let prompt: PromptResponse = serde_json::from_value(body_json(response).await).unwrap();
    assert_eq!(prompt.finish, FinishReason::Stop);
    prompt.message.to_string()
}

#[tokio::test]
async fn opencode_session_revert_records_and_clears_reverted_message() {
    let target_file = "records-revert-target.txt";
    let app = router(state(target_file).await);
    let session = create_session(app.clone()).await;
    let message = prompt_message(app.clone(), &session).await;

    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": message})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::OK);
    let reverted = body_json(reverted).await;
    assert_eq!(reverted["id"], session);
    assert_eq!(reverted["revert"]["messageID"], message);
    assert_eq!(
        reverted["summary"],
        json!({"additions": 1, "deletions": 1, "files": 1})
    );
    let revert_diff = reverted["revert"]["diff"].as_str().expect("revert diff");
    assert!(revert_diff.contains("-old"));
    assert!(revert_diff.contains("+new"));
    assert!(reverted.get("metadata").is_none());

    let diff = request(
        app.clone(),
        "GET",
        &format!("/session/{session}/diff"),
        None,
    )
    .await;
    assert_eq!(diff.status(), StatusCode::OK);
    let diff = body_json(diff).await;
    assert_eq!(diff.as_array().expect("diff items").len(), 1);
    assert_eq!(diff[0]["file"], target_file);
    assert_eq!(diff[0]["status"], "modified");

    let unreverted = request(app, "POST", &format!("/session/{session}/unrevert"), None).await;
    assert_eq!(unreverted.status(), StatusCode::OK);
    let unreverted = body_json(unreverted).await;
    assert_eq!(unreverted["id"], session);
    assert!(unreverted.get("revert").is_none());
}

#[tokio::test]
async fn opencode_session_revert_missing_message_id_is_noop() {
    let app = router(state("missing-message-revert-target.txt").await);
    let session = create_session(app.clone()).await;

    let reverted = request(
        app,
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::OK);
    let reverted = body_json(reverted).await;
    assert_eq!(reverted["id"], session);
    assert!(reverted.get("revert").is_none());
    assert!(reverted.get("summary").is_none());
}

#[tokio::test]
async fn opencode_session_revert_rejects_foreign_part_id_as_noop() {
    let app = router(state("foreign-part-revert-target.txt").await);
    let session = create_session(app.clone()).await;
    let message = prompt_message(app.clone(), &session).await;
    let foreign_part = PartId::new().to_string();

    let reverted = request(
        app,
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": message, "partID": foreign_part})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::OK);
    let reverted = body_json(reverted).await;
    assert_eq!(reverted["id"], session);
    assert!(reverted.get("revert").is_none());
    assert!(reverted.get("summary").is_none());
}

#[tokio::test]
async fn opencode_session_revert_scopes_diff_to_matching_part_id() {
    let app = router(two_edit_state().await);
    let session = create_session(app.clone()).await;
    prompt_message(app.clone(), &session).await;

    let messages = request(
        app.clone(),
        "GET",
        &format!("/session/{session}/message"),
        None,
    )
    .await;
    assert_eq!(messages.status(), StatusCode::OK);
    let messages = body_json(messages).await;
    let tool_message = messages
        .as_array()
        .expect("messages")
        .iter()
        .find(|message| {
            message["parts"].as_array().is_some_and(|parts| {
                parts.iter().filter(|part| part["type"] == "tool").count() == 2
            })
        })
        .expect("tool message");
    let message = tool_message["info"]["id"].as_str().expect("message id");
    let part = tool_message["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["tool"] == "edit")
        .and_then(|part| part["id"].as_str())
        .expect("tool part");

    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": message, "partID": part})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::OK);
    let reverted = body_json(reverted).await;
    assert_eq!(reverted["revert"]["messageID"], message);
    assert_eq!(reverted["revert"]["partID"], part);
    assert_eq!(
        reverted["summary"],
        json!({"additions": 1, "deletions": 1, "files": 1})
    );
    let revert_diff = reverted["revert"]["diff"].as_str().expect("revert diff");
    assert!(revert_diff.contains("first new"));
    assert!(!revert_diff.contains("second new"));

    let diff = request(app, "GET", &format!("/session/{session}/diff"), None).await;
    assert_eq!(diff.status(), StatusCode::OK);
    let diff = body_json(diff).await;
    assert_eq!(diff.as_array().expect("diff items").len(), 1);
    assert_eq!(diff[0]["file"], "first-revert-target.txt");
}

#[tokio::test]
async fn opencode_session_revert_missing_session_returns_not_found() {
    let app = router(state("missing-session-revert-target.txt").await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{missing}/revert"),
        Some(json!({"messageID": "msg_missing"})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(reverted).await, expected);

    let unreverted = request(app, "POST", &format!("/session/{missing}/unrevert"), None).await;
    assert_eq!(unreverted.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unreverted).await, expected);
}

#[tokio::test]
async fn opencode_session_revert_rejects_busy_sessions() {
    let app = router(state("busy-revert-target.txt").await);
    let session = create_session(app.clone()).await;
    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        request(
            shell_app,
            "POST",
            &format!("/sessions/{shell_session}/shell"),
            Some(json!({"command": "sleep 20 && printf should-not-finish"})),
        )
        .await
    });
    wait_until_busy(app.clone(), &session).await;

    let expected = json!({
        "name": "SessionBusyError",
        "data": {
            "sessionID": session,
            "message": format!("Session is busy: {session}"),
        },
    });
    let reverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/revert"),
        Some(json!({"messageID": "msg_missing"})),
    )
    .await;
    assert_eq!(reverted.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(reverted).await, expected);

    let unreverted = request(
        app.clone(),
        "POST",
        &format!("/session/{session}/unrevert"),
        None,
    )
    .await;
    assert_eq!(unreverted.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(unreverted).await, expected);

    let aborted = request(app, "POST", &format!("/session/{session}/abort"), None).await;
    assert_eq!(aborted.status(), StatusCode::OK);
    assert_eq!(body_json(aborted).await, json!(true));
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
}
