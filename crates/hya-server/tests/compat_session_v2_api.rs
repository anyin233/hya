#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;

fn workdir() -> &'static str {
    static WORKDIR: OnceLock<String> = OnceLock::new();
    WORKDIR.get_or_init(|| {
        let dir = std::env::temp_dir().join("hya-compat-session-v2-api");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(&dir)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    })
}

static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn isolated_workdir(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-{prefix}-{nanos}-{serial}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn write_skill(workdir: &str, name: &str, body: &str) {
    let dir = std::path::Path::new(workdir).join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Run {name}\n---\n{body}"),
    )
    .unwrap();
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

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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

async fn post_empty(app: axum::Router, uri: String) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

async fn post_prompt(app: axum::Router, session: &str) -> StatusCode {
    let (status, _) = post_json(
        app,
        &format!("/sessions/{session}/prompt"),
        json!({"text": "hello"}),
    )
    .await;
    status
}

#[tokio::test]
async fn compat_v2_session_routes_create_get_and_list_wrapped_data() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();

    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "id": requested,
            "agent": "plan",
            "model": {"providerID": "anthropic", "id": "claude-sonnet"},
            "location": {"directory": workdir()}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["data"]["id"], requested);
    assert_eq!(created["data"]["agent"], "plan");
    assert_eq!(created["data"]["model"]["providerID"], "anthropic");
    assert_eq!(created["data"]["model"]["id"], "claude-sonnet");
    assert_eq!(created["data"]["directory"], workdir());
    assert_eq!(created["data"]["path"], "");
    assert_eq!(created["data"]["cost"], 0);
    assert_eq!(
        created["data"]["tokens"],
        json!({"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}})
    );

    let (status, existing) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "id": requested,
            "agent": "build",
            "model": {"providerID": "openai", "id": "gpt-5"},
            "location": {"directory": "/tmp/ignored"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(existing["data"]["id"], requested);
    assert_eq!(existing["data"]["agent"], "plan");
    assert_eq!(existing["data"]["model"]["providerID"], "anthropic");

    let (status, got) = get_json(app.clone(), format!("/api/session/{requested}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["data"]["id"], requested);

    let (status, updated) = patch_json(
        app.clone(),
        format!("/api/session/{requested}"),
        json!({"title": "Visible session"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["data"]["title"], "Visible session");

    let (status, _) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, listed) = get_json(app, "/api/session?limit=1".to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["data"].as_array().expect("data").len(), 1);
    assert!(listed["cursor"]["next"].as_str().is_some());
    assert!(listed["cursor"]["previous"].as_str().is_some());
}

#[tokio::test]
async fn compat_v2_session_update_sets_title_metadata_permission_archive_and_searches_it() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();
    let (status, _) = post_json(
        app.clone(),
        "/api/session",
        json!({"id": requested, "location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, updated) = patch_json(
        app.clone(),
        format!("/api/session/{requested}"),
        json!({"title": "Compat parity"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["data"]["id"], requested);
    assert_eq!(updated["data"]["title"], "Compat parity");

    let (status, metadata_updated) = patch_json(
        app.clone(),
        format!("/api/session/{requested}"),
        json!({
            "metadata": {"lane": "v2"},
            "permission": [{"permission": "bash", "pattern": "*", "action": "ask"}],
            "time": {"archived": 99}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metadata_updated["data"]["metadata"]["lane"], "v2");
    assert_eq!(
        metadata_updated["data"]["permission"][0]["permission"],
        "bash"
    );
    assert_eq!(metadata_updated["data"]["time"]["archived"], 99);

    let (status, permission_merged) = patch_json(
        app.clone(),
        format!("/api/session/{requested}"),
        json!({"permission": [{"permission": "edit", "pattern": "*.rs", "action": "allow"}]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        permission_merged["data"]["permission"]
            .as_array()
            .expect("permission rules")
            .len(),
        2
    );

    let (status, listed) = get_json(app, "/api/session?search=Compat&limit=10".to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["data"], json!([]));
}

#[tokio::test]
async fn compat_v2_session_command_and_shell_routes_return_wrapped_messages() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, command) = post_json(
        app,
        &format!("/api/session/{session}/command"),
        json!({
            "command": "init",
            "arguments": "audit parity"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command["data"]["info"]["role"], "user");
    assert_eq!(command["data"]["parts"][0]["text"], "/init audit parity");

    let shell_app = router(shell_state().await);
    let (status, created) = post_json(
        shell_app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let shell_session = created["data"]["id"].as_str().expect("session id");
    let (status, shell) = post_json(
        shell_app,
        &format!("/api/session/{shell_session}/shell"),
        json!({
            "agent": "build",
            "command": "printf compat-v2-shell-ok"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shell["data"]["info"]["role"], "assistant");
    assert_eq!(shell["data"]["parts"][0]["type"], "tool");
    assert_eq!(shell["data"]["parts"][0]["tool"], "shell");
    assert_eq!(shell["data"]["parts"][0]["state"]["status"], "completed");
    assert!(
        shell["data"]["parts"][0]["state"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("compat-v2-shell-ok"))
    );
}

#[tokio::test]
async fn compat_v2_session_command_without_text_uses_skill_template_body() {
    let workdir = isolated_workdir("compat-v2-session-skill-command");
    write_skill(
        &workdir,
        "deploy",
        "Deploy $1 into $2.\nFull prompt: $ARGUMENTS\n",
    );
    let app = router(state().await);

    let (status, commands) =
        get_json(app.clone(), format!("/api/command?directory={workdir}")).await;
    assert_eq!(status, StatusCode::OK);
    let deploy = commands["data"]
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

    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, command) = post_json(
        app.clone(),
        &format!("/api/session/{session}/command"),
        json!({
            "command": "deploy",
            "arguments": "web production"
        }),
    )
    .await;
    let expected = "Deploy web into production.\nFull prompt: web production\n";
    assert_eq!(status, StatusCode::OK);
    assert_eq!(command["data"]["info"]["role"], "user");
    assert_eq!(command["data"]["parts"][0]["type"], "text");
    assert_eq!(command["data"]["parts"][0]["text"], expected);
    assert_ne!(
        command["data"]["parts"][0]["text"],
        "/deploy web production"
    );

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        context["data"]
            .as_array()
            .expect("context")
            .iter()
            .any(|item| item["type"] == "user" && item["text"] == expected)
    );
}

#[tokio::test]
async fn compat_v2_session_delete_removes_session() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();
    let (status, _) = post_json(
        app.clone(),
        "/api/session",
        json!({"id": requested, "location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, deleted) = delete_json(app.clone(), format!("/api/session/{requested}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["data"], true);

    let (status, _) = get_json(app.clone(), format!("/api/session/{requested}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, listed) = get_json(app, "/api/session?limit=10".to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed["data"]
            .as_array()
            .expect("sessions")
            .iter()
            .all(|item| item["id"] != requested)
    );
}

#[tokio::test]
async fn compat_v2_session_init_records_requested_command_message() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");
    let message = MessageId::new().to_string();

    let (status, initialized) = post_json(
        app.clone(),
        &format!("/api/session/{session}/init"),
        json!({
            "messageID": message,
            "providerID": "hya",
            "modelID": "fake"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initialized["data"], true);

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        context["data"]
            .as_array()
            .expect("context")
            .iter()
            .any(|item| item["id"] == message && item["text"] == "/init")
    );
}

#[tokio::test]
async fn compat_v2_session_compact_reports_unavailable_and_wait_returns_when_idle() {
    let app = router(state().await);
    let requested = SessionId::new().to_string();
    let (status, _) = post_json(
        app.clone(),
        "/api/session",
        json!({"id": requested, "location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, compact) =
        post_empty(app.clone(), format!("/api/session/{requested}/compact")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(compact["_tag"], "ServiceUnavailableError");
    assert_eq!(compact["message"], "Session compact is not available yet");
    assert_eq!(compact["service"], "session.compact");

    let (status, wait) = post_empty(app.clone(), format!("/api/session/{requested}/wait")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(wait, Value::Null);

    let missing = SessionId::new().to_string();
    let (status, _) = post_empty(app.clone(), format!("/api/session/{missing}/compact")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post_empty(app, format!("/api/session/{missing}/wait")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn compat_v2_session_context_returns_v2_messages() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");
    assert_eq!(post_prompt(app.clone(), session).await, StatusCode::OK);

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["data"][0]["type"], "user");
    assert_eq!(context["data"][0]["text"], "hello");
    assert!(context["data"][0]["time"]["created"].as_u64().is_some());
    assert_eq!(context["data"][1]["type"], "assistant");
    assert_eq!(context["data"][1]["agent"], "build");
    assert_eq!(context["data"][1]["model"]["id"], "fake");
    assert_eq!(context["data"][1]["content"][1]["type"], "text");
    assert_eq!(context["data"][1]["content"][1]["text"], "assistant answer");
    assert_eq!(context["data"][1]["finish"], "stop");
}

#[tokio::test]
async fn compat_v2_session_prompt_admits_without_resume() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");

    let (status, admitted) = post_json(
        app.clone(),
        &format!("/api/session/{session}/prompt"),
        json!({"prompt": {"text": "queued"}, "delivery": "queue", "resume": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admitted["data"]["sessionID"], session);
    assert_eq!(admitted["data"]["prompt"]["text"], "queued");
    assert_eq!(admitted["data"]["delivery"], "queue");
    assert!(admitted["data"]["admittedSeq"].as_u64().is_some());
    assert!(admitted["data"]["timeCreated"].as_u64().is_some());
    assert!(admitted["data"]["promotedSeq"].is_null());

    let (status, context) = get_json(app, format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["data"].as_array().expect("messages").len(), 1);
    assert_eq!(context["data"][0]["text"], "queued");
}

#[tokio::test]
async fn compat_v2_session_prompt_preserves_files_and_agents_in_context() {
    let app = router(state().await);
    let (status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({"location": {"directory": workdir()}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = created["data"]["id"].as_str().expect("session id");
    let files = json!([
        {
            "uri": "file:///tmp/hya-compat-session-v2-api/notes.txt",
            "mime": "text/plain",
            "name": "notes.txt",
            "description": "session notes",
            "source": {"text": "@notes.txt", "start": 0, "end": 10}
        }
    ]);
    let agents = json!([
        {
            "name": "build",
            "source": {"text": "@build", "start": 11, "end": 17}
        }
    ]);

    let (status, admitted) = post_json(
        app.clone(),
        &format!("/api/session/{session}/prompt"),
        json!({
            "prompt": {
                "text": "queued with context",
                "files": files,
                "agents": agents
            },
            "delivery": "queue",
            "resume": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(admitted["data"]["prompt"]["files"], files);
    assert_eq!(admitted["data"]["prompt"]["agents"], agents);

    let (status, context) = get_json(app.clone(), format!("/api/session/{session}/context")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["data"][0]["type"], "user");
    assert_eq!(context["data"][0]["text"], "queued with context");
    assert_eq!(context["data"][0]["files"], files);
    assert_eq!(context["data"][0]["agents"], agents);

    let (status, messages) =
        get_json(app, format!("/api/session/{session}/message?order=asc")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(messages["data"][0]["files"], files);
    assert_eq!(messages["data"][0]["agents"], agents);
}
