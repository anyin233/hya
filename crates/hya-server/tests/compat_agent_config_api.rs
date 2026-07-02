#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
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

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-agent-config-test-{nanos}-{}",
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

async fn post_json(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn find_agent<'a>(agents: &'a Value, name: &str) -> &'a Value {
    agent_named(agents, name).unwrap_or_else(|| panic!("missing agent {name}: {agents}"))
}

fn agent_named<'a>(agents: &'a Value, name: &str) -> Option<&'a Value> {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["name"] == name || agent["id"] == name)
}

#[tokio::test]
async fn compat_agent_routes_discover_inline_config_agents() {
    // Given: Compat config files with inline agents and modes.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode")).unwrap();
    std::fs::write(
        workdir.join("opencode.jsonc"),
        r##"{
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
        "reasoning": { "summary": "auto" }
      },
      "customFlag": "from-rest",
      "request": {
        "headers": { "x-agent": "architect" },
        "body": { "reasoning_effort": "high" }
      },
      "permissions": [
        { "action": "read", "resource": "docs/**", "effect": "allow" }
      ],
      "tools": {
        "write": false,
        "webfetch": true
      },
      "permission": {
        "grep": "deny",
        "bash": { "git *": "ask" }
      },
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
  "default_agent": "triage",
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
    let app = router(state(workdir.clone()).await);

    // When: both agent routes are listed for that workspace.
    let uri = format!("/agent?directory={}", workdir.display());
    let api_uri = format!("/api/agent?directory={}", workdir.display());
    let (status, agents) = get_json(app.clone(), &uri).await;
    let (api_status, api_agents) = get_json(app.clone(), &api_uri).await;
    let (create_status, created) = post_json(
        app,
        "/api/session",
        serde_json::json!({ "location": { "directory": workdir.display().to_string() } }),
    )
    .await;

    // Then: inline agents merge with native agents and inline modes become primary agents.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);
    assert_eq!(create_status, StatusCode::OK);
    assert_eq!(agents[0]["name"], "triage");
    let architect = find_agent(&agents, "architect");
    assert_eq!(architect["description"], "Architecture reviewer");
    assert_eq!(architect["mode"], "subagent");
    assert_eq!(architect["hidden"], true);
    assert_eq!(architect["model"]["providerID"], "openai");
    assert_eq!(architect["model"]["modelID"], "gpt-5");
    assert_eq!(architect["temperature"], 0.2);
    assert_eq!(architect["topP"], 0.8);
    assert_eq!(architect["color"], "#A855F7");
    assert_eq!(architect["steps"], 9);
    assert_eq!(architect["options"]["reasoning"]["summary"], "auto");
    assert_eq!(architect["options"]["customFlag"], "from-rest");
    assert_eq!(architect["prompt"], "Think structurally.");
    assert_agent_permissions(&architect["permission"]);

    let plan = find_agent(&agents, "plan");
    assert_eq!(plan["description"], "Inline plan mode");
    assert_eq!(plan["mode"], "primary");
    assert_eq!(plan["native"], true);
    assert_eq!(plan["steps"], 7);
    assert_eq!(plan["prompt"], "Plan inline.");
    assert!(agent_named(&agents, "summary").is_none());

    let triage = find_agent(&agents, "triage");
    assert_eq!(triage["description"], "Triage mode");
    assert_eq!(triage["mode"], "primary");
    assert_eq!(triage["model"]["providerID"], "anthropic");
    assert_eq!(triage["model"]["modelID"], "claude-sonnet");
    assert_global_permissions(&triage["permission"]);
    assert_eq!(created["data"]["agent"], "triage");

    let api_agents = &api_agents["data"];
    assert_eq!(api_agents[0]["id"], "triage");
    let architect = find_agent(api_agents, "architect");
    assert_eq!(architect["description"], "Architecture reviewer");
    assert_eq!(architect["system"], "Think structurally.");
    assert_eq!(architect["model"]["providerID"], "openai");
    assert_eq!(architect["model"]["id"], "gpt-5");
    assert_eq!(architect["model"]["variant"], "high");
    assert_eq!(architect["color"], "#A855F7");
    assert_eq!(architect["steps"], 9);
    assert_eq!(architect["request"]["headers"]["x-agent"], "architect");
    assert_eq!(architect["request"]["body"]["reasoning_effort"], "high");
    assert_agent_permissions(&architect["permissions"]);
    assert_eq!(find_agent(api_agents, "plan")["steps"], 7);
    let triage = find_agent(api_agents, "triage");
    assert_eq!(triage["mode"], "primary");
    assert_global_permissions(&triage["permissions"]);
    assert!(agent_named(api_agents, "summary").is_none());
}

fn assert_global_permissions(permissions: &Value) {
    let permissions = permissions.as_array().unwrap();
    assert!(permissions.contains(
        &serde_json::json!({"permission": "todowrite", "pattern": "*", "action": "deny"})
    ));
}

fn assert_agent_permissions(permissions: &Value) {
    let permissions = permissions.as_array().unwrap();
    assert_global_permissions(&Value::Array(permissions.clone()));
    assert!(permissions.contains(
        &serde_json::json!({"permission": "read", "pattern": "docs/**", "action": "allow"})
    ));
    assert!(
        permissions
            .contains(&serde_json::json!({"permission": "grep", "pattern": "*", "action": "deny"}))
    );
    assert!(
        permissions
            .contains(&serde_json::json!({"permission": "edit", "pattern": "*", "action": "deny"}))
    );
    assert!(permissions.contains(
        &serde_json::json!({"permission": "webfetch", "pattern": "*", "action": "allow"})
    ));
    assert!(
        permissions.contains(
            &serde_json::json!({"permission": "bash", "pattern": "git *", "action": "ask"})
        )
    );
}
