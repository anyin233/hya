//! Characterization after legacy agent-definition authority deletion.
//!
//! Inline JSON/JSONC agent definitions and per-agent permission/options/reasoning
//! overlays must not reappear through `/agent` or `/api/agent`. The approved
//! `default_agent` key remains readable for selection/sort only.

#![allow(clippy::unwrap_used)]

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
    support_tempdir("agent-config")
}

/// Catalog with mains `hya-main`/`build` for default_agent selection checks.
fn catalog_runtime() -> Arc<hya_core::RuntimeRegistry> {
    let tools = Arc::new(ToolRegistry::builtins());
    runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("hya-main").description("Configured default main"),
            AgentFixture::main("build").description("Build main"),
            AgentFixture::main("plan").description("Plan main"),
            AgentFixture::subagent("general").description("General subagent"),
            AgentFixture::subagent("compaction").prompt("compaction system"),
            AgentFixture::subagent("title").prompt("title system"),
            AgentFixture::subagent("summary").prompt("summary system"),
        ],
    )
}

async fn state_with_runtime(workdir: PathBuf, runtime: Arc<hya_core::RuntimeRegistry>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, runtime, permission, EventBus::default());
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

fn agent_names(agents: &Value) -> Vec<&str> {
    agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| {
            agent["name"]
                .as_str()
                .or_else(|| agent["id"].as_str())
                .unwrap()
        })
        .collect()
}

fn find_agent<'a>(agents: &'a Value, name: &str) -> &'a Value {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["name"] == name || agent["id"] == name)
        .unwrap_or_else(|| panic!("missing agent {name}: {agents}"))
}

/// Inline agent definitions in JSON/JSONC must not create reusable catalog rows,
/// and legacy per-agent permissions/options/reasoning must not project as overlays.
#[tokio::test]
async fn inline_config_agents_and_overlays_do_not_appear_in_agent_routes() {
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode")).unwrap();
    std::fs::write(
        workdir.join("opencode.jsonc"),
        r##"{
  // Inline definitions and overlays must be ignored by metadata projection.
  "permissions": [
    { "action": "todowrite", "resource": "*", "effect": "deny" }
  ],
  "agent": {
    "architect": {
      "description": "Architecture reviewer",
      "mode": "subagent",
      "hidden": true,
      "model": "openai/gpt-5",
      "variant": "high",
      "temperature": 0.2,
      "top_p": 0.8,
      "color": "#A855F7",
      "steps": 9,
      "options": {
        "reasoning": { "summary": "auto" },
        "reasoningEffort": "high"
      },
      "customFlag": "from-rest",
      "permissions": [
        { "action": "read", "resource": "docs/**", "effect": "allow" }
      ],
      "prompt": "Think structurally."
    },
    "plan": {
      "description": "Inline plan mode",
      "maxSteps": 7,
      "prompt": "Plan inline."
    },
    "summary": {
      "disable": true
    }
  }
}
"##,
    )
    .unwrap();
    std::fs::write(
        workdir.join(".opencode/opencode.json"),
        r#"{
  "default_agent": "hya-main",
  "mode": {
    "triage": {
      "description": "Triage mode",
      "model": "anthropic/claude-sonnet",
      "prompt": "Triage issues."
    }
  }
}
"#,
    )
    .unwrap();

    let app = router(state_with_runtime(workdir.clone(), catalog_runtime()).await);
    let uri = format!("/agent?directory={}", workdir.display());
    let api_uri = format!("/api/agent?directory={}", workdir.display());
    let (status, agents) = get_json(app.clone(), &uri).await;
    let (api_status, api_body) = get_json(app, &api_uri).await;

    assert_eq!(status, StatusCode::OK, "legacy body: {agents}");
    assert_eq!(api_status, StatusCode::OK, "api body: {api_body}");

    let legacy_names = agent_names(&agents);
    let api_agents = &api_body["data"];
    let api_ids = agent_names(api_agents);

    // Inline agent / mode names never become reusable definitions.
    for forbidden in ["architect", "triage"] {
        assert!(
            !legacy_names.contains(&forbidden),
            "legacy /agent must not list inline config agent {forbidden}: {legacy_names:?}"
        );
        assert!(
            !api_ids.contains(&forbidden),
            "/api/agent must not list inline config agent {forbidden}: {api_ids:?}"
        );
    }

    // summary remains from the bound catalog (disable:true no longer removes it).
    assert!(
        legacy_names.contains(&"summary"),
        "bound catalog summary must remain listed: {legacy_names:?}"
    );
    assert!(
        api_ids.contains(&"summary"),
        "bound catalog summary must remain listed on /api/agent: {api_ids:?}"
    );

    // plan is catalog main, not the inline overlay (description/prompt/steps).
    let plan = find_agent(&agents, "plan");
    assert_ne!(
        plan.get("description").and_then(Value::as_str),
        Some("Inline plan mode"),
        "inline plan description must not overlay catalog plan: {plan}"
    );
    assert_ne!(
        plan.get("prompt").and_then(Value::as_str),
        Some("Plan inline."),
        "inline plan prompt must not overlay catalog plan: {plan}"
    );
    assert!(
        plan.get("steps").is_none() || plan["steps"].is_null(),
        "inline maxSteps must not project onto catalog plan: {plan}"
    );

    // Config-level permissions and agent options/reasoning are not per-agent overlays.
    let build = find_agent(&agents, "build");
    let build_permission = &build["permission"];
    assert!(
        build_permission
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
            || !build_permission
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| rule["permission"] == "todowrite"),
        "config permissions must not project as agent overlays: {build_permission}"
    );
    assert_eq!(
        build.get("options").cloned().unwrap_or(json!({})),
        json!({}),
        "legacy agent options/reasoning must not project: {build}"
    );

    let api_build = find_agent(api_agents, "build");
    let api_permissions = &api_build["permissions"];
    assert!(
        api_permissions
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true)
            || !api_permissions
                .as_array()
                .unwrap()
                .iter()
                .any(|rule| rule["permission"] == "todowrite"),
        "config permissions must not project on /api/agent: {api_permissions}"
    );

    // Valid default_agent still sorts the native catalog definition first.
    // Stronger session-create coverage lives in bound_catalog_session_api.
    assert_eq!(
        legacy_names.first().copied(),
        Some("hya-main"),
        "valid default_agent must promote hya-main on /agent: {legacy_names:?}"
    );
    assert_eq!(
        api_ids.first().copied(),
        Some("hya-main"),
        "valid default_agent must promote hya-main on /api/agent: {api_ids:?}"
    );
    assert_eq!(find_agent(&agents, "hya-main")["mode"], "primary");
    assert_eq!(find_agent(api_agents, "hya-main")["mode"], "primary");
}
