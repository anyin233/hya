#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const BUILD_AGENT_DESCRIPTION: &str =
    "The default agent. Executes tools based on configured permissions.";

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-agent-metadata-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, permission, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
            reasoning: None,
        }),
    )
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn find_agent<'a>(agents: &'a Value, name: &str) -> &'a Value {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["name"] == name || agent["id"] == name)
        .unwrap_or_else(|| panic!("missing agent {name}: {agents}"))
}

#[tokio::test]
async fn opencode_agent_route_includes_build_description() {
    // Given: a server exposing the OpenCode-compatible instance routes.
    let app = router(state(tempdir()).await);

    // When: the OpenCode /agent route is listed.
    let (status, agents) = get_json(app, "/agent").await;

    // Then: the build agent includes OpenCode's native description field.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents[0]["description"], BUILD_AGENT_DESCRIPTION);
}

#[tokio::test]
async fn opencode_agent_routes_include_native_agent_catalog() {
    // Given: a server exposing the OpenCode-compatible agent metadata routes.
    let app = router(state(tempdir()).await);

    // When: both legacy and v2 agent routes are listed.
    let (status, agents) = get_json(app.clone(), "/agent").await;
    let (api_status, api_agents) = get_json(app, "/api/agent").await;

    // Then: OpenCode's native agents are available with their public metadata.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);
    assert_eq!(
        agents
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "build",
            "plan",
            "general",
            "explore",
            "compaction",
            "title",
            "summary"
        ]
    );

    assert_eq!(find_agent(&agents, "plan")["mode"], "primary");
    assert_eq!(find_agent(&agents, "general")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "explore")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "compaction")["hidden"], true);
    assert_eq!(find_agent(&agents, "title")["hidden"], true);
    assert_eq!(find_agent(&agents, "summary")["hidden"], true);

    let api_agents = &api_agents["data"];
    assert_eq!(
        api_agents
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "build",
            "plan",
            "general",
            "explore",
            "compaction",
            "title",
            "summary"
        ]
    );
    assert_eq!(find_agent(api_agents, "general")["mode"], "subagent");
    assert_eq!(find_agent(api_agents, "compaction")["hidden"], true);
}
