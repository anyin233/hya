//! Commit 2 slice: live session create/switch use one bound BundleCatalog.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

use support::{AgentFixture, runtime_with_catalog, tempdir as support_tempdir};

fn tempdir() -> PathBuf {
    support_tempdir("bound-catalog-session")
}

/// Catalog with main `hya-main`/`build` and subagent `research` (role not an auth gate).
fn catalog_runtime() -> Arc<hya_core::RuntimeRegistry> {
    let tools = Arc::new(ToolRegistry::builtins());
    runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").description("Configured default main"),
            AgentFixture::main("build").description("Build main"),
            AgentFixture::subagent("research").description("Known research subagent"),
            AgentFixture::subagent("general").description("General subagent"),
            AgentFixture::subagent("compaction").prompt("compaction system"),
            AgentFixture::subagent("title").prompt("title system"),
            AgentFixture::subagent("summary").prompt("summary system"),
        ],
    )
}

async fn state_with_runtime(
    workdir: PathBuf,
    runtime: Arc<hya_core::RuntimeRegistry>,
) -> (AppState, Arc<SessionEngine>) {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        runtime,
        permission,
        EventBus::default(),
    ));
    let state = AppState::new(
        engine.clone(),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
            reasoning: None,
        }),
    );
    (state, engine)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "raw": text }))
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

async fn post_empty(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
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

fn error_text(body: &Value) -> String {
    if let Some(raw) = body.get("raw").and_then(Value::as_str) {
        return raw.to_string();
    }
    body.to_string()
}

async fn session_count(engine: &SessionEngine) -> usize {
    engine.store().list_sessions().await.unwrap().len()
}

/// Omitted Compat legacy `/session` and v2 `/api/session` honor configured
/// `default_agent: hya-main` from the bound catalog (exact resolve, not general).
#[tokio::test]
async fn omitted_compat_create_honors_configured_default_agent_hya_main() {
    let workdir = tempdir();
    std::fs::write(
        workdir.join("opencode.json"),
        r#"{ "default_agent": "hya-main" }"#,
    )
    .unwrap();
    let workdir_str = workdir.to_string_lossy().into_owned();
    let (state, engine) = state_with_runtime(workdir.clone(), catalog_runtime()).await;
    let app = router(state);

    let (legacy_status, legacy) = post_json(
        app.clone(),
        "/session",
        json!({ "location": { "directory": workdir_str } }),
    )
    .await;
    let (v2_status, v2) = post_json(
        app,
        "/api/session",
        json!({ "location": { "directory": workdir_str } }),
    )
    .await;

    assert_eq!(legacy_status, StatusCode::OK, "legacy body: {legacy}");
    assert_eq!(v2_status, StatusCode::OK, "v2 body: {v2}");
    assert_eq!(
        legacy["agent"], "hya-main",
        "legacy omitted create must use configured default from catalog: {legacy}"
    );
    assert_eq!(
        v2["data"]["agent"], "hya-main",
        "v2 omitted create must use configured default from catalog: {v2}"
    );
    assert_eq!(session_count(&engine).await, 2);
}

/// Explicit unknown agent on native `/sessions` and Compat create surfaces returns
/// typed `UNKNOWN_AGENT_ID` and creates no session/events.
#[tokio::test]
async fn explicit_unknown_create_returns_unknown_agent_id_without_side_effects() {
    let workdir = tempdir();
    let workdir_str = workdir.to_string_lossy().into_owned();
    let (state, engine) = state_with_runtime(workdir.clone(), catalog_runtime()).await;
    let app = router(state);
    assert_eq!(session_count(&engine).await, 0);

    let (native_status, native_body) = post_json(
        app.clone(),
        "/sessions",
        json!({
            "agent": "no-such-agent",
            "model": "fake-model",
            "workdir": workdir_str,
        }),
    )
    .await;
    let (legacy_status, legacy_body) = post_json(
        app.clone(),
        "/session",
        json!({
            "agent": "no-such-agent",
            "location": { "directory": workdir_str },
        }),
    )
    .await;
    let (v2_status, v2_body) = post_json(
        app,
        "/api/session",
        json!({
            "agent": "no-such-agent",
            "location": { "directory": workdir_str },
        }),
    )
    .await;

    for (label, status, body) in [
        ("native", native_status, &native_body),
        ("legacy", legacy_status, &legacy_body),
        ("v2", v2_status, &v2_body),
    ] {
        let text = error_text(body);
        assert!(
            status.is_client_error() || status.is_server_error(),
            "{label} unknown create must fail closed, got {status}: {text}"
        );
        assert!(
            text.contains("UNKNOWN_AGENT_ID"),
            "{label} error must surface UNKNOWN_AGENT_ID, got {status}: {text}"
        );
    }
    assert_eq!(
        session_count(&engine).await,
        0,
        "unknown create must not append any session"
    );
}

/// Explicit unknown `/api/session/:id/agent` switch returns typed UNKNOWN_AGENT_ID
/// and leaves the existing agent/event stream unchanged.
#[tokio::test]
async fn explicit_unknown_agent_switch_returns_unknown_agent_id_without_mutation() {
    let workdir = tempdir();
    let workdir_str = workdir.to_string_lossy().into_owned();
    let (state, engine) = state_with_runtime(workdir.clone(), catalog_runtime()).await;
    let app = router(state);

    let (create_status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "agent": "build",
            "location": { "directory": workdir_str },
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "create body: {created}");
    let session = created["data"]["id"].as_str().unwrap().to_string();
    let session_id: hya_proto::SessionId = session.parse().unwrap();
    let before_events = engine.replay(session_id).await.unwrap().len();

    let (switch_status, switch_body) = post_json(
        app.clone(),
        &format!("/api/session/{session}/agent"),
        json!({ "agent": "no-such-agent" }),
    )
    .await;
    let switch_text = error_text(&switch_body);
    assert!(
        switch_status.is_client_error() || switch_status.is_server_error(),
        "unknown switch must fail closed, got {switch_status}: {switch_text}"
    );
    assert!(
        switch_text.contains("UNKNOWN_AGENT_ID"),
        "unknown switch must surface UNKNOWN_AGENT_ID, got {switch_status}: {switch_text}"
    );

    let (get_status, session_body) = get_json(app.clone(), &format!("/session/{session}")).await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        session_body["agent"], "build",
        "agent must remain unchanged after failed switch: {session_body}"
    );
    let after_events = engine.replay(session_id).await.unwrap().len();
    assert_eq!(
        after_events, before_events,
        "failed switch must not append events"
    );

    let (context_status, context) = get_json(app, &format!("/api/session/{session}/context")).await;
    assert_eq!(context_status, StatusCode::OK);
    let switched = context["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["type"] == "agent-switched");
    assert!(
        !switched,
        "context must not record agent-switched after unknown switch: {context}"
    );
}

/// Explicit known subagent create/switch succeeds — role is not an authorization gate.
#[tokio::test]
async fn explicit_known_subagent_create_and_switch_succeed() {
    let workdir = tempdir();
    let workdir_str = workdir.to_string_lossy().into_owned();
    let (state, _engine) = state_with_runtime(workdir.clone(), catalog_runtime()).await;
    let app = router(state);

    let (create_status, created) = post_json(
        app.clone(),
        "/api/session",
        json!({
            "agent": "research",
            "location": { "directory": workdir_str },
        }),
    )
    .await;
    assert_eq!(create_status, StatusCode::OK, "create body: {created}");
    assert_eq!(
        created["data"]["agent"], "research",
        "known subagent must be a valid root-session identity: {created}"
    );
    let session = created["data"]["id"].as_str().unwrap().to_string();

    let (switch_status, switch_body) = post_json(
        app.clone(),
        &format!("/api/session/{session}/agent"),
        json!({ "agent": "general" }),
    )
    .await;
    assert_eq!(
        switch_status,
        StatusCode::NO_CONTENT,
        "known subagent switch must succeed: {switch_body}"
    );

    let (get_status, session_body) = get_json(app, &format!("/session/{session}")).await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(
        session_body["agent"], "general",
        "session must reflect switched subagent: {session_body}"
    );
}

/// Empty-body omitted create still resolves through product default path against the catalog.
#[tokio::test]
async fn omitted_empty_body_create_uses_agent_spec_name_when_no_config_default() {
    let workdir = tempdir();
    let (state, _engine) = state_with_runtime(workdir, catalog_runtime()).await;
    let app = router(state);

    let (legacy_status, legacy) = post_empty(app.clone(), "/session").await;
    let (v2_status, v2) = post_empty(app, "/api/session").await;

    assert_eq!(legacy_status, StatusCode::OK, "legacy body: {legacy}");
    assert_eq!(v2_status, StatusCode::OK, "v2 body: {v2}");
    // st.agent.name is "build"; must exact-resolve in catalog.
    assert_eq!(legacy["agent"], "build");
    assert_eq!(v2["data"]["agent"], "build");
}
