#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::{CreateSessionResponse, PromptResponse};
use yaca_proto::{AgentName, FinishReason, MessageId, ModelRef};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-api";

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("assistant answer".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
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

async fn todo_state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "todowrite".to_string(),
                input: json!({
                    "todos": [
                        { "content": "Audit OpenCode todos", "status": "in_progress", "priority": "high" },
                        { "content": "Document remaining gaps", "status": "pending", "priority": "medium" }
                    ]
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("todos updated".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::TodoWrite,
        "*",
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

async fn shell_state() -> AppState {
    std::fs::create_dir_all(WORKDIR).unwrap();
    let provider = FakeProvider::scripted(vec![]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router, parent: Option<&str>) -> String {
    let mut body = json!({"agent": "build", "model": "fake", "workdir": WORKDIR});
    if let Some(parent) = parent {
        body["parent"] = json!(parent.trim_start_matches("ses_"));
    }
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
}

async fn post_prompt(app: axum::Router, session: &str) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/sessions/{session}/prompt"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"text": "hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let prompt: PromptResponse = serde_json::from_value(body_json(resp).await).unwrap();
    assert_eq!(prompt.finish, FinishReason::Stop);
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn post_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn delete_json(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn opencode_session_routes_list_get_and_messages() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let list = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_body = body_json(list).await;
    assert_eq!(list_body[0]["id"], session);
    assert_eq!(list_body[0]["agent"], "build");
    assert_eq!(list_body[0]["model"]["id"], "fake");
    assert_eq!(list_body[0]["model"]["providerID"], "yaca");
    assert_eq!(list_body[0]["directory"], WORKDIR);
    let created = list_body[0]["time"]["created"].as_u64().expect("created");
    let updated = list_body[0]["time"]["updated"].as_u64().expect("updated");
    assert!(updated >= created);

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_body = body_json(get).await;
    assert_eq!(get_body["id"], session);
    assert_eq!(get_body["projectID"], "local");
    assert_eq!(get_body["version"], env!("CARGO_PKG_VERSION"));

    let messages = app
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
    let message_body = body_json(messages).await;
    assert_eq!(message_body[0]["info"]["sessionID"], session);
    assert_eq!(message_body[0]["info"]["role"], "user");
    assert_eq!(message_body[0]["parts"][0]["type"], "text");
    assert_eq!(message_body[0]["parts"][0]["text"], "hello");
    assert_eq!(
        message_body[0]["parts"][0]["messageID"],
        message_body[0]["info"]["id"]
    );
    assert_eq!(message_body[1]["info"]["role"], "assistant");
    assert_eq!(message_body[1]["parts"][0]["text"], "assistant answer");
}

#[tokio::test]
async fn opencode_session_update_sets_title_metadata_permission_and_archive() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, updated) = patch_json(
        app.clone(),
        format!("/session/{session}"),
        json!({"title": "Reviewed parity"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["id"], session);
    assert_eq!(updated["title"], "Reviewed parity");

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(body_json(get).await["title"], "Reviewed parity");

    let (status, metadata_updated) = patch_json(
        app.clone(),
        format!("/session/{session}"),
        json!({
            "metadata": {"owner": "opencode"},
            "permission": [{"permission": "bash", "pattern": "*", "action": "ask"}],
            "time": {"archived": 42}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metadata_updated["metadata"]["owner"], "opencode");
    assert_eq!(metadata_updated["permission"][0]["permission"], "bash");
    assert_eq!(metadata_updated["time"]["archived"], 42);

    let (status, permission_merged) = patch_json(
        app.clone(),
        format!("/session/{session}"),
        json!({"permission": [{"permission": "edit", "pattern": "*.rs", "action": "allow"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        permission_merged["permission"]
            .as_array()
            .expect("permission rules")
            .len(),
        2
    );

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let body = body_json(get).await;
    assert_eq!(body["metadata"]["owner"], "opencode");
    assert_eq!(body["permission"][1]["permission"], "edit");
    assert_eq!(body["time"]["archived"], 42);
}

#[tokio::test]
async fn opencode_session_command_and_shell_routes_return_created_messages() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, command) = post_json(
        app,
        format!("/session/{session}/command"),
        json!({
            "command": "init",
            "arguments": "audit parity"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command["info"]["role"], "user");
    assert_eq!(command["parts"][0]["type"], "text");
    assert_eq!(command["parts"][0]["text"], "/init audit parity");

    let shell_app = router(shell_state().await);
    let shell_session = create_session(shell_app.clone(), None).await;
    let (status, shell) = post_json(
        shell_app,
        format!("/session/{shell_session}/shell"),
        json!({
            "agent": "build",
            "command": "printf opencode-shell-ok"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shell["info"]["role"], "assistant");
    assert_eq!(shell["parts"][0]["type"], "tool");
    assert_eq!(shell["parts"][0]["tool"], "shell");
    assert!(
        shell["parts"][0]["state"]["output"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("opencode-shell-ok"))
    );
}

#[tokio::test]
async fn opencode_session_delete_removes_session() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, deleted) = delete_json(app.clone(), format!("/session/{session}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted, json!(true));

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::NOT_FOUND);

    let list = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    assert!(
        body_json(list)
            .await
            .as_array()
            .expect("sessions")
            .iter()
            .all(|item| item["id"] != session)
    );
}

#[tokio::test]
async fn opencode_session_init_records_requested_command_message() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    let message = MessageId::new().to_string();

    let (status, initialized) = post_json(
        app.clone(),
        format!("/session/{session}/init"),
        json!({
            "messageID": message,
            "providerID": "yaca",
            "modelID": "fake"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initialized, json!(true));

    let one = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message/{message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(one.status(), StatusCode::OK);
    let body = body_json(one).await;
    assert_eq!(body["info"]["id"], message);
    assert_eq!(body["info"]["role"], "user");
    assert_eq!(body["parts"][0]["text"], "/init");
}

#[tokio::test]
async fn opencode_session_routes_page_message_and_children() {
    let app = router(state().await);
    let parent = create_session(app.clone(), None).await;
    let child = create_session(app.clone(), Some(&parent)).await;
    post_prompt(app.clone(), &parent).await;

    let children = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/children"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(children.status(), StatusCode::OK);
    let children_body = body_json(children).await;
    assert_eq!(children_body[0]["id"], child);
    assert_eq!(children_body[0]["parentID"], parent);

    let all = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(all.status(), StatusCode::OK);
    let all_body = body_json(all).await;
    let user_message = all_body[0]["info"]["id"]
        .as_str()
        .expect("message id")
        .to_string();

    let one = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/message/{user_message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(one.status(), StatusCode::OK);
    let one_body = body_json(one).await;
    assert_eq!(one_body["info"]["id"], user_message);
    assert_eq!(one_body["parts"][0]["text"], "hello");

    let first_page = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/message?limit=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_page.status(), StatusCode::OK);
    let cursor = first_page
        .headers()
        .get("x-next-cursor")
        .expect("cursor")
        .to_str()
        .expect("cursor text")
        .to_string();
    let link = first_page
        .headers()
        .get("link")
        .expect("pagination link")
        .to_str()
        .expect("link text")
        .to_string();
    assert!(link.contains(&cursor));
    assert!(link.contains("rel=\"next\""));
    let first_page_body = body_json(first_page).await;
    assert_eq!(first_page_body.as_array().expect("page").len(), 1);

    let second_page = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/message?limit=1&before={cursor}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_page.status(), StatusCode::OK);
    let second_page_body = body_json(second_page).await;
    assert_eq!(second_page_body.as_array().expect("page").len(), 1);

    let bad_before = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{parent}/message?before={cursor}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_before.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opencode_session_deletes_messages_and_parts() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let all = app
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
    assert_eq!(all.status(), StatusCode::OK);
    let all_body = body_json(all).await;
    let user_message = all_body[0]["info"]["id"]
        .as_str()
        .expect("user message id")
        .to_string();
    let assistant_message = all_body[1]["info"]["id"]
        .as_str()
        .expect("assistant message id")
        .to_string();
    let assistant_part = all_body[1]["parts"][0]["id"]
        .as_str()
        .expect("assistant part id")
        .to_string();

    let delete_part = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/session/{session}/message/{assistant_message}/part/{assistant_part}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_part.status(), StatusCode::OK);
    assert_eq!(body_json(delete_part).await, json!(true));

    let assistant = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message/{assistant_message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(assistant.status(), StatusCode::OK);
    assert_eq!(
        body_json(assistant).await["parts"]
            .as_array()
            .expect("parts")
            .len(),
        0
    );

    let delete_message = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/session/{session}/message/{user_message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_message.status(), StatusCode::OK);
    assert_eq!(body_json(delete_message).await, json!(true));

    let deleted = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message/{user_message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NOT_FOUND);

    let remaining = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(remaining.status(), StatusCode::OK);
    let remaining_body = body_json(remaining).await;
    assert_eq!(remaining_body.as_array().expect("messages").len(), 1);
    assert_eq!(remaining_body[0]["info"]["id"], assistant_message);
}

#[tokio::test]
async fn opencode_session_updates_text_parts() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let all = app
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
    assert_eq!(all.status(), StatusCode::OK);
    let all_body = body_json(all).await;
    let message = all_body[0]["info"]["id"]
        .as_str()
        .expect("message id")
        .to_string();
    let part = all_body[0]["parts"][0]["id"]
        .as_str()
        .expect("part id")
        .to_string();

    let updated = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/session/{session}/message/{message}/part/{part}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "id": part,
                        "sessionID": session,
                        "messageID": message,
                        "type": "text",
                        "text": "edited text"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let updated_body = body_json(updated).await;
    assert_eq!(updated_body["id"], part);
    assert_eq!(updated_body["text"], "edited text");

    let message_after = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message/{message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(message_after.status(), StatusCode::OK);
    assert_eq!(
        body_json(message_after).await["parts"][0]["text"],
        "edited text"
    );
}

#[tokio::test]
async fn opencode_session_updates_tool_parts_from_opencode_state() {
    let app = router(shell_state().await);
    let session = create_session(app.clone(), None).await;
    let (status, shell) = post_json(
        app.clone(),
        format!("/session/{session}/shell"),
        json!({
            "agent": "build",
            "command": "printf original"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let message = shell["info"]["id"]
        .as_str()
        .expect("message id")
        .to_string();
    let part = shell["parts"][0]["id"]
        .as_str()
        .expect("part id")
        .to_string();
    let call = shell["parts"][0]["callID"]
        .as_str()
        .expect("call id")
        .to_string();

    let (status, updated) = patch_json(
        app.clone(),
        format!("/session/{session}/message/{message}/part/{part}"),
        json!({
            "id": part,
            "sessionID": session,
            "messageID": message,
            "type": "tool",
            "callID": call,
            "tool": "shell",
            "state": {
                "status": "error",
                "input": {"command": "printf original"},
                "error": "shell failed",
                "time": {"start": 1, "end": 2}
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["state"]["phase"], "error");
    assert_eq!(updated["state"]["message"], "shell failed");

    let message_after = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/message/{message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(message_after.status(), StatusCode::OK);
    let body = body_json(message_after).await;
    assert_eq!(body["parts"][0]["state"]["phase"], "error");
    assert_eq!(body["parts"][0]["state"]["message"], "shell failed");
}

#[tokio::test]
async fn opencode_session_diff_returns_empty_message_summary() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let all = app
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
    assert_eq!(all.status(), StatusCode::OK);
    let all_body = body_json(all).await;
    let message = all_body[0]["info"]["id"].as_str().expect("message id");

    let diff = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/diff?messageID={message}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(diff.status(), StatusCode::OK);
    assert_eq!(body_json(diff).await, json!([]));
}

#[tokio::test]
async fn opencode_session_share_sets_and_clears_share_url() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let share = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/share"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(share.status(), StatusCode::OK);
    let shared = body_json(share).await;
    assert_eq!(shared["share"]["url"], format!("yaca://session/{session}"));

    let get = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    assert_eq!(
        body_json(get).await["share"]["url"],
        format!("yaca://session/{session}")
    );

    let unshare = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/session/{session}/share"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unshare.status(), StatusCode::OK);
    let unshared = body_json(unshare).await;
    assert!(!unshared.as_object().expect("session").contains_key("share"));
}

#[tokio::test]
async fn opencode_session_todo_returns_todowrite_state() {
    let app = router(todo_state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{session}/todo"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await,
        json!([
            { "content": "Audit OpenCode todos", "status": "in_progress", "priority": "high" },
            { "content": "Document remaining gaps", "status": "pending", "priority": "medium" }
        ])
    );
}
