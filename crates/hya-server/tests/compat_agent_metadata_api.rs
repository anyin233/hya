//! Integration tests for `hya-server`: compat agent metadata api.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{
    AgentSpec, CoreError, EventBus, RuntimeCatalogRefresh, RuntimeRegistry, SessionEngine,
};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

use support::{AgentFixture, runtime_with_catalog, test_runtime};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

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
    state_with_runtime(workdir, test_runtime(Arc::new(ToolRegistry::builtins()))).await
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

fn find_agent<'a>(agents: &'a Value, name: &str) -> &'a Value {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["name"] == name || agent["id"] == name)
        .unwrap_or_else(|| panic!("missing agent {name}: {agents}"))
}

struct CountingRefresh {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl RuntimeCatalogRefresh for CountingRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }
}

#[tokio::test]
async fn agent_catalog_endpoint_refreshes_before_binding() {
    let workdir = tempdir();
    let calls = Arc::new(AtomicUsize::new(0));
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(Arc::new(CountingRefresh {
        calls: Arc::clone(&calls),
    }));
    let app = router(AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        }),
    ));

    let uri = format!("/api/agent?directory={}", workdir.display());
    let (status, body) = get_json(app, &uri).await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body["data"].as_array().unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Configured `default_agent: hya-main` must lead both agent list routes so the
/// TUI primary selector (`current()` = first mode-primary row) defaults to it.
#[tokio::test]
async fn configured_default_agent_hya_main_is_first_among_agent_rows() {
    // Given: explicit catalog with build + hya-main (main) and general (subagent),
    // plus fixed system agents. Workdir opencode.json names hya-main as default.
    let workdir = tempdir();
    std::fs::write(
        workdir.join("opencode.json"),
        r#"{ "default_agent": "hya-main" }"#,
    )
    .unwrap();
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("build").description("Build main"),
            AgentFixture::main("hya-main").description("Configured default main"),
            AgentFixture::subagent("general").description("General subagent"),
            AgentFixture::subagent("compaction").prompt("compaction system"),
            AgentFixture::subagent("title").prompt("title system"),
            AgentFixture::subagent("summary").prompt("summary system"),
        ],
    );
    let app = router(state_with_runtime(workdir.clone(), runtime).await);
    let uri = format!("/agent?directory={}", workdir.display());
    let api_uri = format!("/api/agent?directory={}", workdir.display());

    // When: both legacy and v2 agent routes list bound catalog rows.
    let (status, agents) = get_json(app.clone(), &uri).await;
    let (api_status, api_body) = get_json(app, &api_uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);

    let legacy_names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();
    let api_agents = &api_body["data"];
    let api_ids: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();

    // Then: hya-main is first among returned rows on both routes.
    assert_eq!(
        legacy_names.first().copied(),
        Some("hya-main"),
        "legacy /agent must put configured default first, got {legacy_names:?}"
    );
    assert_eq!(
        api_ids.first().copied(),
        Some("hya-main"),
        "/api/agent must put configured default first, got {api_ids:?}"
    );
    assert_eq!(find_agent(&agents, "hya-main")["mode"], "primary");
    assert_eq!(find_agent(api_agents, "hya-main")["mode"], "primary");
    assert_eq!(find_agent(&agents, "build")["mode"], "primary");
    assert_eq!(find_agent(&agents, "general")["mode"], "subagent");

    // TUI selector = mode primary only; current() takes the first selector row.
    let selector: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["mode"] == "primary")
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        selector.first().copied(),
        Some("hya-main"),
        "TUI primary selector must default to configured hya-main, got {selector:?}"
    );
    assert!(
        !selector.contains(&"general"),
        "subagent general must not enter TUI selector"
    );
}

#[tokio::test]
async fn compat_agent_route_includes_build_from_bound_catalog() {
    // Given: a server whose SessionEngine holds the default test catalog.
    let app = router(state(tempdir()).await);

    // When: the Compat /agent route is listed.
    let (status, agents) = get_json(app, "/agent").await;

    // Then: build is present from the bound catalog (prompt falls back to AgentSpec).
    assert_eq!(status, StatusCode::OK);
    let build = find_agent(&agents, "build");
    assert_eq!(build["mode"], "primary");
    assert_eq!(build["prompt"], "system prompt");
    assert_eq!(build["native"], true);
}

#[tokio::test]
async fn compat_agent_routes_include_bound_catalog_agents() {
    // Given: a server exposing agent metadata over the bound BundleCatalog.
    let app = router(state(tempdir()).await);

    // When: both legacy and v2 agent routes are listed.
    let (status, agents) = get_json(app.clone(), "/agent").await;
    let (api_status, api_agents) = get_json(app, "/api/agent").await;

    // Then: only the bound test catalog is listed; role maps to mode.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);
    assert_eq!(
        agents
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["build", "compaction", "general", "plan", "summary", "title"]
    );

    assert_eq!(find_agent(&agents, "plan")["mode"], "primary");
    assert_eq!(find_agent(&agents, "general")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "compaction")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "compaction")["hidden"], true);
    assert_eq!(find_agent(&agents, "title")["hidden"], true);
    assert_eq!(find_agent(&agents, "summary")["hidden"], true);
    // Legacy /agent omits hidden when false (skip_serializing_if).
    assert!(
        find_agent(&agents, "general")["hidden"].is_null(),
        "reachable subagent must not be wire-hidden on /agent"
    );

    let api_agents = &api_agents["data"];
    assert_eq!(
        api_agents
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["build", "compaction", "general", "plan", "summary", "title"]
    );
    assert_eq!(find_agent(api_agents, "general")["mode"], "subagent");
    assert_eq!(find_agent(api_agents, "general")["hidden"], false);
    assert_eq!(find_agent(api_agents, "title")["system"], "title prompt");
    assert_eq!(
        find_agent(api_agents, "summary")["system"],
        "summary prompt"
    );
    assert_eq!(find_agent(api_agents, "compaction")["hidden"], true);
    // TUI selector = mode primary only (build/plan main; general is subagent).
    let selector: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["mode"] == "primary")
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();
    assert_eq!(selector, vec!["build", "plan"]);
}

#[tokio::test]
async fn compat_agent_routes_ignore_project_legacy_agent_files() {
    // Given: a workspace with Compat agent and mode markdown files.
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

    // Then: cut-over metadata ignores project agent files; only bound catalog remains.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api_status, StatusCode::OK);
    let names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();
    assert!(
        !names.contains(&"reviewer") && !names.contains(&"audit"),
        "legacy project agents must not merge into bound catalog view: {names:?}"
    );
    // plan is not overwritten by .opencode/agents/plan.md.
    let plan = find_agent(&agents, "plan");
    assert_ne!(plan["prompt"], "Plan in project style.");
    assert_eq!(plan["mode"], "primary");
    assert_eq!(find_agent(&agents, "compaction")["hidden"], true);
    assert_eq!(
        find_agent(&agents, "compaction")["prompt"],
        "compaction prompt"
    );

    let api_agents = &api_agents["data"];
    let ids: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"reviewer") && !ids.contains(&"audit"),
        "v2 route must not merge legacy project agents: {ids:?}"
    );
    assert_eq!(find_agent(api_agents, "plan")["mode"], "primary");
}

/// Commit 2 slice: `/api/agent` metadata is the bound BundleCatalog.
///
/// Role maps to selector mode; TUI-selectable set is main/primary only;
/// ordinary can_spawn roster is independent of role; legacy project agent
/// files must not alter this cut-over view.
#[tokio::test]
async fn api_agent_metadata_from_bound_catalog_role_and_can_spawn() {
    // Given: an explicit prepared catalog that is *not* the legacy NATIVE_AGENTS table.
    // - build (main) can_spawn research (subagent)
    // - plan (main) is present but not reachable from build
    // - research (subagent) is reachable
    // - compaction/title/summary (subagent) have no ordinary inbound can_spawn edge
    // - explore is intentionally absent from the catalog
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/agents")).unwrap();
    // Legacy project agent file must not appear in the cut-over metadata view.
    std::fs::write(
        workdir.join(".opencode/agents/reviewer.md"),
        "---\ndescription: Reviews changes\nmode: subagent\n---\nReview carefully.\n",
    )
    .unwrap();
    let tools = Arc::new(ToolRegistry::builtins());
    let runtime = runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("build")
                .description("Default build agent")
                .can_spawn(&["research"]),
            AgentFixture::main("plan").description("Plan agent"),
            AgentFixture::subagent("research")
                .description("Reachable research subagent")
                .prompt("research prompt body"),
            AgentFixture::subagent("compaction").prompt("compaction system"),
            AgentFixture::subagent("title").prompt("title system"),
            AgentFixture::subagent("summary").prompt("summary system"),
        ],
    );
    let app = router(state_with_runtime(workdir.clone(), runtime).await);
    let engine = {
        // Re-bind the same catalog for roster assertions through SessionEngine.
        let providers =
            Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
        let tools = Arc::new(ToolRegistry::builtins());
        let runtime = runtime_with_catalog(
            tools,
            &[
                AgentFixture::main("build")
                    .description("Default build agent")
                    .can_spawn(&["research"]),
                AgentFixture::main("plan").description("Plan agent"),
                AgentFixture::subagent("research")
                    .description("Reachable research subagent")
                    .prompt("research prompt body"),
                AgentFixture::subagent("compaction").prompt("compaction system"),
                AgentFixture::subagent("title").prompt("title system"),
                AgentFixture::subagent("summary").prompt("summary system"),
            ],
        );
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        let store = SessionStore::connect_memory().await.unwrap();
        SessionEngine::new(store, providers, runtime, permission, EventBus::default())
    };

    // When: v2 agent metadata is listed for the workdir that also has legacy agent files.
    let api_uri = format!("/api/agent?directory={}", workdir.display());
    let (api_status, api_body) = get_json(app, &api_uri).await;
    assert_eq!(api_status, StatusCode::OK);
    let api_agents = &api_body["data"];
    let ids: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();

    // Then (a): metadata comes from the explicit PreparedBundle catalog; role → mode.
    assert!(
        ids.contains(&"research"),
        "bound catalog agent `research` must appear, got {ids:?}"
    );
    assert!(
        !ids.contains(&"explore"),
        "absent catalog agent `explore` must not be synthesized from NATIVE_AGENTS, got {ids:?}"
    );
    assert_eq!(find_agent(api_agents, "build")["mode"], "primary");
    assert_eq!(find_agent(api_agents, "plan")["mode"], "primary");
    assert_eq!(find_agent(api_agents, "research")["mode"], "subagent");
    assert_eq!(find_agent(api_agents, "compaction")["mode"], "subagent");
    assert_eq!(
        find_agent(api_agents, "research")["description"],
        "Reachable research subagent"
    );

    // Then (b): TUI selector set is role-main / mode-primary only (research stays out).
    let selector: Vec<&str> = api_agents
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["mode"] == "primary")
        .map(|agent| agent["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        selector,
        vec!["build", "plan"],
        "TUI selector modes must be primary-only, got {selector:?}"
    );
    assert!(
        !selector.contains(&"research"),
        "reachable subagent must not enter the TUI selector"
    );

    // Then (c): caller can_spawn roster includes reachable subagent, excludes unlisted main
    // and fixed system agents (independent of role).
    let binding = engine.bind_runtime(&workdir).unwrap();
    let Ok(roster) = engine.agent_roster_for_binding(&binding, "build") else {
        panic!("caller roster must resolve from bound catalog");
    };
    let roster_names: Vec<&str> = roster.iter().map(|agent| agent.name.as_str()).collect();
    assert_eq!(
        roster_names,
        vec!["research"],
        "roster must be can_spawn-only, got {roster_names:?}"
    );
    assert!(
        !roster_names.contains(&"plan"),
        "unlisted main must not appear"
    );
    for reserved in ["compaction", "title", "summary"] {
        assert!(
            !roster_names.contains(&reserved),
            "{reserved} must not appear in ordinary roster"
        );
    }
    // Wire hidden for autocomplete: fixed system agents are not ordinarily reachable.
    assert_eq!(find_agent(api_agents, "compaction")["hidden"], true);
    assert_eq!(find_agent(api_agents, "title")["hidden"], true);
    assert_eq!(find_agent(api_agents, "summary")["hidden"], true);
    assert_eq!(
        find_agent(api_agents, "research")["hidden"],
        false,
        "reachable subagent must remain autocomplete-visible"
    );

    // Then (d): project legacy agent files do not alter the cut-over metadata view.
    assert!(
        !ids.contains(&"reviewer"),
        "legacy .opencode agent must not merge into bound catalog metadata, got {ids:?}"
    );
}
