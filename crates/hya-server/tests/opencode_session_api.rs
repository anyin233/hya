#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::api::{CreateSessionResponse, PromptResponse};
use hya_proto::{AgentName, FinishReason, MessageId, ModelRef, PartId, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

fn workdir() -> &'static str {
    static WORKDIR: OnceLock<String> = OnceLock::new();
    WORKDIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("hya-opencode-session-api");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    })
}
static NEXT_TEMP_WORKDIR_ID: AtomicU64 = AtomicU64::new(0);

fn temp_workdir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_WORKDIR_ID.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("{prefix}-{nanos}-{serial}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

async fn state() -> AppState {
    state_with_workdir(PathBuf::from(workdir())).await
}

async fn state_with_workdir(workdir: PathBuf) -> AppState {
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
            workdir,
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
            workdir: workdir().into(),
            reasoning: None,
        }),
    )
}

async fn shell_state() -> AppState {
    std::fs::create_dir_all(workdir()).unwrap();
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
            workdir: workdir().into(),
            reasoning: None,
        }),
    )
}

async fn edit_state() -> AppState {
    std::fs::create_dir_all(workdir()).unwrap();
    std::fs::write(format!("{}/diff-target.txt", workdir()), "old\n").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": "diff-target.txt",
                    "oldString": "old\n",
                    "newString": "new\n",
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("edit complete".to_string()),
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
            workdir: workdir().into(),
            reasoning: None,
        }),
    )
}

async fn two_edit_state() -> AppState {
    std::fs::create_dir_all(workdir()).unwrap();
    std::fs::write(format!("{}/first-target.txt", workdir()), "first old\n").unwrap();
    std::fs::write(format!("{}/second-target.txt", workdir()), "second old\n").unwrap();
    let provider = FakeProvider::scripted_turns(vec![
        vec![
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": "first-target.txt",
                    "oldString": "first old\n",
                    "newString": "first new\n",
                }),
            },
            FakeStep::ToolCall {
                name: "edit".to_string(),
                input: json!({
                    "filePath": "second-target.txt",
                    "oldString": "second old\n",
                    "newString": "second new\n",
                }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("edits complete".to_string()),
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
            workdir: workdir().into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router, parent: Option<&str>) -> String {
    create_session_in(app, parent, Path::new(workdir())).await
}

async fn create_session_in(app: axum::Router, parent: Option<&str>, workdir: &Path) -> String {
    let mut body = json!({
        "agent": "build",
        "model": "fake",
        "workdir": workdir.display().to_string()
    });
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
    created.session.to_string()
}

async fn wait_until_busy(app: axum::Router, session: &str) {
    for _ in 0..100 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/session/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        if body_json(resp).await[session]["type"] == "busy" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("session did not become busy");
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

async fn get_text(app: axum::Router, uri: String) -> (StatusCode, String) {
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn opencode_session_read_delete_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let message = MessageId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    for uri in [
        format!("/session/{missing}"),
        format!("/session/{missing}/children"),
        format!("/session/{missing}/todo"),
        format!("/session/{missing}/message"),
        format!("/session/{missing}/message/{message}"),
    ] {
        let (status, body) = get_json(app.clone(), uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body, expected);
    }

    let (status, body) = delete_json(app, format!("/session/{missing}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);
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
    assert_eq!(list_body[0]["model"]["providerID"], "hya");
    assert_eq!(list_body[0]["directory"], workdir());
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
    assert_eq!(message_body[1]["parts"][0]["type"], "step-start");
    assert_eq!(message_body[1]["parts"][1]["text"], "assistant answer");
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
async fn opencode_session_update_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let (status, body) = patch_json(
        app,
        format!("/session/{missing}"),
        json!({"title": "never"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
    );
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
    assert_eq!(shell["parts"][0]["state"]["status"], "completed");
    assert!(
        shell["parts"][0]["state"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("opencode-shell-ok"))
    );
}

#[tokio::test]
async fn opencode_session_command_without_text_uses_skill_template_body() {
    let workdir = temp_workdir("hya-opencode-session-skill-command");
    std::fs::create_dir_all(workdir.join(".opencode/skills/deploy")).unwrap();
    std::fs::write(
        workdir.join(".opencode/skills/deploy/SKILL.md"),
        "---\nname: deploy\ndescription: Deploy the current project\n---\nDeploy $1 into $2.\nFull prompt: $ARGUMENTS\n",
    )
    .unwrap();
    let app = router(state_with_workdir(workdir.clone()).await);

    let (status, commands) = get_json(
        app.clone(),
        format!("/command?directory={}", workdir.display()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deploy = commands
        .as_array()
        .expect("commands")
        .iter()
        .find(|command| command["name"] == "deploy")
        .expect("deploy command");
    assert_eq!(deploy["source"], "skill");
    assert_eq!(
        deploy["template"],
        "Deploy $1 into $2.\nFull prompt: $ARGUMENTS\n"
    );

    let session = create_session_in(app.clone(), None, &workdir).await;
    let (status, command) = post_json(
        app.clone(),
        format!("/session/{session}/command"),
        json!({
            "command": "deploy",
            "arguments": "web production"
        }),
    )
    .await;
    let expected = "Deploy web into production.\nFull prompt: web production\n";
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command["info"]["role"], "user");
    assert_eq!(command["parts"][0]["type"], "text");
    assert_eq!(command["parts"][0]["text"], expected);
    assert_ne!(command["parts"][0]["text"], "/deploy web production");

    let (status, messages) = get_json(app, format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        messages.as_array().expect("messages")[0]["parts"][0]["text"],
        expected
    );
}

#[tokio::test]
async fn opencode_session_command_and_init_missing_session_return_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let (status, body) = post_json(
        app.clone(),
        format!("/session/{missing}/command"),
        json!({"command": "init", "arguments": "never"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    let (status, body) = post_json(
        app,
        format!("/session/{missing}/init"),
        json!({
            "messageID": MessageId::new().to_string(),
            "providerID": "fake",
            "modelID": "fake",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);
}

#[tokio::test]
async fn opencode_session_shell_busy_returns_legacy_error() {
    let app = router(shell_state().await);
    let session = create_session(app.clone(), None).await;
    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        post_json(
            shell_app,
            format!("/sessions/{shell_session}/shell"),
            json!({"command": "sleep 20 && printf should-not-finish"}),
        )
        .await
    });
    wait_until_busy(app.clone(), &session).await;

    let (status, body) = post_json(
        app.clone(),
        format!("/session/{session}/shell"),
        json!({"command": "printf blocked"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({
            "name": "SessionBusyError",
            "data": {
                "sessionID": session,
                "message": format!("Session is busy: {session}"),
            },
        })
    );

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
    assert_eq!(body_json(abort).await, json!(true));
    let (shell_status, shell_body) = tokio::select! {
        joined = &mut shell_task => joined.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell request did not finish after abort");
        }
    };
    assert_eq!(shell_status, StatusCode::OK);
    let response: PromptResponse = serde_json::from_value(shell_body).unwrap();
    assert_eq!(response.finish, FinishReason::Cancelled);
}

#[tokio::test]
async fn opencode_session_command_and_init_busy_return_bad_request() {
    let app = router(shell_state().await);
    let session = create_session(app.clone(), None).await;
    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        post_json(
            shell_app,
            format!("/sessions/{shell_session}/shell"),
            json!({"command": "sleep 20 && printf should-not-finish"}),
        )
        .await
    });
    wait_until_busy(app.clone(), &session).await;

    for (uri, body) in [
        (
            format!("/session/{session}/command"),
            json!({"command": "init", "arguments": "blocked"}),
        ),
        (
            format!("/session/{session}/init"),
            json!({
                "messageID": MessageId::new().to_string(),
                "providerID": "fake",
                "modelID": "fake",
            }),
        ),
    ] {
        let (status, body) = post_json(app.clone(), uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["name"], "BadRequest");
        assert!(
            body["data"]["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty())
        );
    }

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
    let (shell_status, _shell_body) = tokio::select! {
        joined = &mut shell_task => joined.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell request did not finish after abort");
        }
    };
    assert_eq!(shell_status, StatusCode::OK);
}

#[tokio::test]
async fn opencode_session_shell_missing_session_returns_not_found() {
    let app = router(shell_state().await);
    let missing = SessionId::new().to_string();
    let (status, body) = post_json(
        app,
        format!("/session/{missing}/shell"),
        json!({"command": "printf never"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
    );
}

#[tokio::test]
async fn opencode_session_delete_missing_session_returns_not_found() {
    let app = router(shell_state().await);
    let missing = SessionId::new().to_string();
    let message = MessageId::new().to_string();
    let part = PartId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let (status, body) =
        delete_json(app.clone(), format!("/session/{missing}/message/{message}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    let (status, body) = delete_json(
        app,
        format!("/session/{missing}/message/{message}/part/{part}"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);
}

#[tokio::test]
async fn opencode_session_delete_message_busy_returns_legacy_error() {
    let app = router(shell_state().await);
    let session = create_session(app.clone(), None).await;
    let shell_app = app.clone();
    let shell_session = session.clone();
    let mut shell_task = tokio::spawn(async move {
        post_json(
            shell_app,
            format!("/sessions/{shell_session}/shell"),
            json!({"command": "sleep 20 && printf should-not-finish"}),
        )
        .await
    });
    wait_until_busy(app.clone(), &session).await;

    let message = MessageId::new();
    let (status, body) =
        delete_json(app.clone(), format!("/session/{session}/message/{message}")).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({
            "name": "SessionBusyError",
            "data": {
                "sessionID": session,
                "message": format!("Session is busy: {session}"),
            },
        })
    );

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
    let (shell_status, _shell_body) = tokio::select! {
        joined = &mut shell_task => joined.unwrap(),
        () = tokio::time::sleep(Duration::from_secs(3)) => {
            shell_task.abort();
            panic!("shell request did not finish after abort");
        }
    };
    assert_eq!(shell_status, StatusCode::OK);
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
            "providerID": "hya",
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
async fn opencode_session_message_invalid_cursor_returns_bad_request() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, body) = get_text(
        app,
        format!("/session/{session}/message?limit=1&before=not-base64"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!body.trim().is_empty());
    assert!(body.contains("invalid cursor") || body.contains("Invalid cursor"));
}

#[tokio::test]
async fn opencode_session_message_missing_message_returns_not_found() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, body) = get_text(
        app,
        format!("/session/{session}/message/{}", MessageId::new()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("message not found"));
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
        2
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
async fn opencode_session_legacy_deletes_missing_message_and_part_as_noops() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let missing_message = MessageId::new();
    let (status, body) = delete_json(
        app.clone(),
        format!("/session/{session}/message/{missing_message}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!(true));

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
    let missing_part = PartId::new();

    let (status, body) = delete_json(
        app.clone(),
        format!("/session/{session}/message/{message}/part/{missing_part}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!(true));
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
async fn opencode_session_update_part_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let message = MessageId::new().to_string();
    let part = PartId::new().to_string();

    let (status, body) = patch_json(
        app,
        format!("/session/{missing}/message/{message}/part/{part}"),
        json!({
            "id": part,
            "sessionID": missing,
            "messageID": message,
            "type": "text",
            "text": "never"
        }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
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
    assert_eq!(updated["state"]["status"], "error");
    assert_eq!(updated["state"]["error"], "shell failed");

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
    assert_eq!(body["parts"][0]["state"]["status"], "error");
    assert_eq!(body["parts"][0]["state"]["error"], "shell failed");
}

#[tokio::test]
async fn opencode_session_diff_returns_recorded_edit_file_diff() {
    let app = router(edit_state().await);
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
    let body = body_json(diff).await;
    assert_eq!(body.as_array().expect("diff items").len(), 1);
    assert_eq!(body[0]["file"], "diff-target.txt");
    assert_eq!(body[0]["additions"], 1);
    assert_eq!(body[0]["deletions"], 1);
    assert_eq!(body[0]["status"], "modified");
    let patch = body[0]["patch"].as_str().expect("patch");
    assert!(patch.contains("-old"));
    assert!(patch.contains("+new"));
}

#[tokio::test]
async fn opencode_session_diff_filters_recorded_edit_by_part_id() {
    let app = router(two_edit_state().await);
    let session = create_session(app.clone(), None).await;
    post_prompt(app.clone(), &session).await;

    let (status, messages) = get_json(app.clone(), format!("/session/{session}/message")).await;
    assert_eq!(status, StatusCode::OK);
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

    let (status, body) = get_json(
        app,
        format!("/session/{session}/diff?messageID={message}&partID={part}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().expect("diff items").len(), 1);
    assert_eq!(body[0]["file"], "first-target.txt");
    let patch = body[0]["patch"].as_str().expect("patch");
    assert!(patch.contains("first new"));
    assert!(!patch.contains("second new"));
}

#[tokio::test]
async fn opencode_session_diff_invalid_message_id_returns_bad_request() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, body) = get_json(app, format!("/session/{session}/diff?messageID=bad")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"name": "BadRequest", "data": {"message": "invalid message id"}})
    );
}

#[tokio::test]
async fn opencode_session_diff_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();

    let (status, body) = get_json(app, format!("/session/{missing}/diff")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({
            "name": "NotFoundError",
            "data": { "message": format!("Session not found: {missing}") },
        })
    );
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
    assert_eq!(shared["share"]["url"], format!("hya://session/{session}"));

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
        format!("hya://session/{session}")
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
async fn opencode_session_share_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let (status, body) =
        post_json(app.clone(), format!("/session/{missing}/share"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);

    let (status, body) = delete_json(app, format!("/session/{missing}/share")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);
}

#[tokio::test]
async fn opencode_session_fork_copies_metadata_and_messages() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;
    let (status, _updated) = patch_json(
        app.clone(),
        format!("/session/{session}"),
        json!({
            "title": "Root session",
            "metadata": {"source": "fork-test"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    post_prompt(app.clone(), &session).await;

    let fork = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/fork"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fork.status(), StatusCode::OK);
    let fork_body = body_json(fork).await;
    let fork_id = fork_body["id"].as_str().expect("fork id").to_string();
    assert_ne!(fork_id, session);
    assert_eq!(fork_body["title"], "Root session (fork #1)");
    assert_eq!(fork_body["metadata"]["source"], "fork-test");
    assert!(
        !fork_body
            .as_object()
            .expect("session")
            .contains_key("parentID")
    );

    let messages = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session/{fork_id}/message"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(messages.status(), StatusCode::OK);
    let body = body_json(messages).await;
    assert_eq!(body.as_array().expect("messages").len(), 2);
    assert_eq!(body[0]["parts"][0]["text"], "hello");
    assert_eq!(body[1]["parts"][0]["text"], "assistant answer");
}

#[tokio::test]
async fn opencode_session_fork_missing_session_returns_not_found() {
    let app = router(state().await);
    let missing = SessionId::new().to_string();
    let expected = json!({
        "name": "NotFoundError",
        "data": { "message": format!("Session not found: {missing}") },
    });

    let (status, body) = post_json(app, format!("/session/{missing}/fork"), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, expected);
}

#[tokio::test]
async fn opencode_session_fork_rejects_invalid_json_body() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let fork = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/session/{session}/fork"))
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fork.status(), StatusCode::BAD_REQUEST);
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

#[tokio::test]
async fn opencode_session_todo_returns_empty_array_for_fresh_session() {
    let app = router(state().await);
    let session = create_session(app.clone(), None).await;

    let (status, body) = get_json(app, format!("/session/{session}/todo")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
}
