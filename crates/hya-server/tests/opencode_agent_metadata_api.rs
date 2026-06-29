#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

const BUILD_AGENT_DESCRIPTION: &str =
    "The default agent. Executes tools based on configured permissions.";

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-agent-metadata-test-{nanos}-{serial}-{}",
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
            "compaction",
            "explore",
            "general",
            "plan",
            "summary",
            "title"
        ]
    );

    assert_eq!(find_agent(&agents, "plan")["mode"], "primary");
    assert_eq!(find_agent(&agents, "general")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "explore")["mode"], "subagent");
    assert!(
        find_agent(&agents, "explore")["prompt"]
            .as_str()
            .unwrap()
            .contains("file search specialist")
    );
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
            "compaction",
            "explore",
            "general",
            "plan",
            "summary",
            "title"
        ]
    );
    assert_eq!(find_agent(api_agents, "general")["mode"], "subagent");
    assert!(
        find_agent(api_agents, "explore")["system"]
            .as_str()
            .unwrap()
            .contains("file search specialist")
    );
    assert!(
        find_agent(api_agents, "title")["system"]
            .as_str()
            .unwrap()
            .contains("title generator")
    );
    assert!(
        find_agent(api_agents, "summary")["system"]
            .as_str()
            .unwrap()
            .contains("pull request description")
    );
    assert_eq!(find_agent(api_agents, "compaction")["hidden"], true);
}

#[tokio::test]
async fn opencode_agent_routes_discover_project_agent_files() {
    // Given: a workspace with OpenCode agent and mode markdown files.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/agents")).unwrap();
    std::fs::create_dir_all(workdir.join(".opencode/modes")).unwrap();
    std::fs::write(
        workdir.join(".opencode/agents/reviewer.md"),
        "---\ndescription: Reviews changes\nmode: subagent\nhidden: true\nmodel: anthropic/claude\n---\nReview carefully.\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join(".opencode/modes/audit.md"),
        "---\ndescription: Audit mode\n---\nAudit thoroughly.\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join(".opencode/agents/plan.md"),
        "---\ndescription: Custom plan mode\n---\nPlan in project style.\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join(".opencode/agents/compaction.md"),
        "---\ndescription: Custom compaction\n---\nCompact in project style.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: both legacy and v2 agent routes are listed for that workspace.
    let uri = format!("/agent?directory={}", workdir.display());
    let api_uri = format!("/api/agent?directory={}", workdir.display());
    let (status, agents) = get_json(app.clone(), &uri).await;
    let (api_status, api_agents) = get_json(app, &api_uri).await;

    // Then: project agents are merged with native agents and preserve metadata.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);
    let reviewer = find_agent(&agents, "reviewer");
    assert_eq!(reviewer["description"], "Reviews changes");
    assert_eq!(reviewer["mode"], "subagent");
    assert_eq!(reviewer["hidden"], true);
    assert_eq!(reviewer["model"]["providerID"], "anthropic");
    assert_eq!(reviewer["model"]["modelID"], "claude");
    assert_eq!(reviewer["prompt"], "Review carefully.");

    let audit = find_agent(&agents, "audit");
    assert_eq!(audit["description"], "Audit mode");
    assert_eq!(audit["mode"], "primary");
    assert_eq!(audit["prompt"], "Audit thoroughly.");

    let plan = find_agent(&agents, "plan");
    assert_eq!(plan["description"], "Custom plan mode");
    assert_eq!(plan["mode"], "primary");
    assert_eq!(plan["native"], true);
    assert_eq!(plan["prompt"], "Plan in project style.");
    assert_eq!(find_agent(&agents, "compaction")["hidden"], true);

    let api_agents = &api_agents["data"];
    let reviewer = find_agent(api_agents, "reviewer");
    assert_eq!(reviewer["description"], "Reviews changes");
    assert_eq!(reviewer["model"]["providerID"], "anthropic");
    assert_eq!(reviewer["model"]["id"], "claude");
    assert_eq!(reviewer["system"], "Review carefully.");
    assert_eq!(find_agent(api_agents, "audit")["mode"], "primary");
}
