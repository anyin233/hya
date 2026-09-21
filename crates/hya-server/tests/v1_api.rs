//! Integration tests for `hya-server`: the `/v1` dual-protocol HTTP binding.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("hello from the agent".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: std::env::temp_dir(),
            reasoning: None,
        }),
    )
}

async fn send(app: axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

async fn create_session(app: &axum::Router) -> String {
    let (status, body) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        json!({
            "agent": "build",
            "model": "fake",
            "workdir": std::env::temp_dir().to_string_lossy(),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn v1_process_config_and_catalog_surfaces_answer() {
    let app = router(state().await);

    let (status, health) = send(app.clone(), Method::GET, "/v1/health", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["ok"], json!(true));

    let (status, _location) = send(app.clone(), Method::GET, "/v1/location", Value::Null).await;
    assert_eq!(status, StatusCode::OK);

    let (status, config) = send(app.clone(), Method::GET, "/v1/config", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(config["values"].is_object());

    let (status, merged) = send(
        app.clone(),
        Method::PATCH,
        "/v1/config",
        json!({"patch": {"v1Probe": {"nested": true}}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{merged}");
    assert_eq!(merged["values"]["v1Probe"]["nested"], json!(true));

    let (status, models) = send(app.clone(), Method::GET, "/v1/models", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        models["models"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let (status, tools) = send(app.clone(), Method::GET, "/v1/tools", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tools["tools"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == "read"))
    );

    let (status, _) = send(
        app.clone(),
        Method::POST,
        "/v1/logs",
        json!({"service": "tui", "level": "LOG_LEVEL_INFO", "message": "probe"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, dispose) = send(app, Method::POST, "/v1/process/dispose", Value::Null).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(dispose["error"]["code"], json!("unavailable"));
}

#[tokio::test]
async fn v1_auth_stores_and_removes_provider_keys() {
    let app = router(state().await);
    let home = std::env::temp_dir().join(format!(
        "hya-v1-auth-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).unwrap();
    // SAFETY: tests within this binary run sequentially for env-dependent
    // assertions and no other test reads XDG_CONFIG_HOME.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", &home) };

    let (status, body) = send(
        app.clone(),
        Method::PUT,
        "/v1/auth/testprovider",
        json!({"apiKey": "secret-key"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], json!("AUTH_STATUS_CREDENTIALED"));

    let stored = home.join("hya/auth/testprovider.yaml");
    let content = std::fs::read_to_string(&stored).unwrap();
    assert!(content.contains("secret-key"));

    let (status, _) = send(
        app.clone(),
        Method::DELETE,
        "/v1/auth/testprovider",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!stored.exists());

    let (status, _body) = send(app, Method::PUT, "/v1/auth/bad..id", json!({"apiKey": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v1_session_lifecycle_create_get_patch_delete() {
    let app = router(state().await);
    let session = create_session(&app).await;

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], json!(session));
    assert_eq!(body["agent"], json!("build"));

    let (status, body) = send(
        app.clone(),
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({"title": "v1 probe"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"], json!("v1 probe"));

    let (status, body) = send(app.clone(), Method::GET, "/v1/sessions", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["sessions"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"] == json!(session)))
    );

    let (status, _) = send(
        app.clone(),
        Method::GET,
        "/v1/sessions/00000000-0000-0000-0000-000000000000",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        app.clone(),
        Method::DELETE,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        app,
        Method::GET,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("session_not_found"));
}

#[tokio::test]
async fn v1_turn_runs_event_driven_and_finishes() {
    let app = router(state().await);
    let session = create_session(&app).await;

    let (status, body) = send(
        app.clone(),
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"prompt": {"text": "say hello"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    assert_eq!(body["turn"]["state"], json!("TURN_STATE_RUNNING"));

    let (status, body) = send(
        app.clone(),
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], json!("TURN_STATE_FINISHED"), "{body}");
    assert_eq!(body["finish"], json!("FINISH_REASON_STOP"));

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}/messages"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let messages = body["messages"].as_array().unwrap();
    assert!(
        messages.iter().any(
            |message| message["parts"].as_array().is_some_and(|parts| parts
                .iter()
                .any(|part| part["text"]["text"] == json!("hello from the agent")))
        )
    );

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/sessions/{session}/events"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
    let next_seq = body["nextSeq"]
        .as_str()
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| body["nextSeq"].as_u64())
        .unwrap_or(0);
    assert!(next_seq > 0);

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/v1/sessions/{session}/events/stream"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(content_type.starts_with("text/event-stream"));
}

#[tokio::test]
async fn v1_invalid_requests_render_the_stable_error_model() {
    let app = router(state().await);

    let (status, body) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions/not-a-session/turns",
        json!({"prompt": {"text": "x"}}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("invalid_argument"));

    let (status, body) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        json!({"agent": "build"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("invalid_argument"));

    let (status, _) = send(app, Method::GET, "/v1/interactions", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn v1_fs_reads_lists_finds_and_searches() {
    let app = router(state().await);
    let root = std::env::temp_dir().join(format!(
        "hya-v1-fs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn alpha() {}\n").unwrap();
    std::fs::write(root.join("notes.txt"), "hello v1 probe\n").unwrap();
    let scope = root.to_string_lossy().into_owned();

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/fs/read?directory={}&path=notes.txt", enc(&scope)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["text"], json!(true));
    assert!(String::from_utf8_lossy(&decode_b64(&body)).contains("hello v1 probe"));

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/fs/list?directory={}", enc(&scope)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["entries"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["name"] == json!("src")))
    );

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!(
            "/v1/fs/find?directory={}&pattern={}",
            enc(&scope),
            enc("**/*.rs")
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["paths"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|p| p.as_str().is_some_and(|p| p.ends_with("a.rs")))
    }));

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!(
            "/v1/fs/search?directory={}&query={}",
            enc(&scope),
            enc("v1 probe")
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["matches"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!(
            "/v1/fs/read?directory={}&path={}",
            enc(&scope),
            enc("../../etc/passwd")
        ),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], json!("invalid_argument"));
}

/// Percent-encode a query component for URI construction in tests.
fn enc(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn decode_b64(body: &Value) -> Vec<u8> {
    use base64::Engine;
    body["content"]
        .as_str()
        .map(|text| {
            base64::engine::general_purpose::STANDARD
                .decode(text)
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn v1_project_vcs_mcp_and_workflow_catalog_answer() {
    let app = router(state().await);
    let root = std::env::temp_dir().join(format!(
        "hya-v1-proj-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let scope = root.to_string_lossy().into_owned();

    let (status, body) = send(app.clone(), Method::GET, "/v1/projects", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["projects"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/vcs?directory={}", enc(&scope)),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["branch"].is_null() || body["branch"] == json!(""));
    assert!(body["files"].as_array().is_none_or(|rows| rows.is_empty()));

    let (status, body) = send(app.clone(), Method::GET, "/v1/mcp", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["servers"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{body}"
    );

    let (status, body) = send(app, Method::GET, "/v1/workflows", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["workflows"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{body}"
    );
}

#[tokio::test]
async fn v1_pty_lifecycle_creates_lists_tokens_and_deletes() {
    let app = router(state().await);

    let (status, body) = send(app.clone(), Method::GET, "/v1/pty/shells", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["shells"]
            .as_array()
            .is_none_or(|rows| rows.iter().all(|row| row.as_str().is_some()))
    );

    let (status, body) = send(
        app.clone(),
        Method::POST,
        "/v1/pty",
        json!({"shell": "/bin/sh", "cwd": std::env::temp_dir().to_string_lossy()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = body["id"].as_str().unwrap().to_owned();

    let (status, body) = send(
        app.clone(),
        Method::GET,
        &format!("/v1/pty/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], json!(id));

    let (status, body) = send(
        app.clone(),
        Method::POST,
        &format!("/v1/pty/{id}/connect-token"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body["token"]
            .as_str()
            .is_some_and(|token| !token.is_empty())
    );
    assert!(
        body["url"]
            .as_str()
            .is_some_and(|url| url.contains("/connect"))
    );

    let (status, _) = send(
        app.clone(),
        Method::DELETE,
        &format!("/v1/pty/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(app, Method::GET, &format!("/v1/pty/{id}"), Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], json!("not_found"));
}

#[tokio::test]
async fn runtime_schemas_lists_the_snapshot_scheme_table() {
    let app = router(state().await);
    let (status, body) = send(app, Method::GET, "/v1/runtime/schemas", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["schemas"],
        json!([]),
        "a runtime without scheme sources publishes an empty table: {body}"
    );
    assert!(
        body["generation"].is_u64(),
        "the response is generation-tagged: {body}"
    );
}
