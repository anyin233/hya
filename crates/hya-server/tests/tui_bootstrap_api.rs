//! `/tui/bootstrap` single-RTT startup payload.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt as _;

use support::{AgentFixture, runtime_with_catalog, tempdir as support_tempdir, test_runtime};

fn tempdir() -> PathBuf {
    support_tempdir("tui-bootstrap")
}

async fn app_with_runtime(
    workdir: PathBuf,
    runtime: Arc<hya_core::RuntimeRegistry>,
) -> axum::Router {
    let store = SessionStore::connect_memory().await.unwrap();
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
    let engine = Arc::new(SessionEngine::new(
        store,
        providers,
        runtime,
        permission,
        EventBus::default(),
    ));
    let agent = Arc::new(AgentSpec {
        name: hya_proto::AgentName::new("build"),
        model: hya_proto::ModelRef::new("dev/fake"),
        system_prompt: "test".into(),
        workdir,
        reasoning: None,
    });
    router(AppState::new(engine, agent))
}

async fn app() -> axum::Router {
    app_with_runtime(
        std::env::temp_dir(),
        test_runtime(Arc::new(ToolRegistry::builtins())),
    )
    .await
}

async fn get_json(app: axum::Router, path: &str, workdir: &std::path::Path) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("x-opencode-directory", workdir.to_string_lossy().as_ref())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn find_agent<'a>(agents: &'a Value, name: &str) -> &'a Value {
    agents
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["name"] == name)
        .unwrap_or_else(|| panic!("missing agent {name}: {agents}"))
}

#[tokio::test]
async fn tui_bootstrap_returns_required_startup_fields() {
    let workdir = std::env::temp_dir();
    let (status, body) = get_json(app().await, "/tui/bootstrap", &workdir).await;
    assert_eq!(status, StatusCode::OK);
    for key in [
        "config",
        "providers",
        "provider_list",
        "capabilities",
        "agents",
        "sessions",
        "commands",
        "lsp",
        "mcp",
        "formatter",
        "session_status",
        "vcs",
        "path",
        "project",
    ] {
        assert!(body.get(key).is_some(), "missing {key}");
    }
    // Slim command entries must not ship full skill templates.
    if let Some(commands) = body.get("commands").and_then(Value::as_array) {
        for command in commands {
            assert!(
                command.get("template").is_none(),
                "bootstrap command entries must omit template bodies"
            );
            assert!(command.get("name").is_some());
        }
    }
}

/// `/tui/bootstrap` agents come from the bound BundleCatalog; legacy project
/// agent files are ignored. Role-main rows are primary; subagents stay present
/// with mode subagent.
#[tokio::test]
async fn tui_bootstrap_returns_bound_catalog_rows_and_ignores_legacy_agent_files() {
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/agents")).unwrap();
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
            AgentFixture::subagent("research").description("Reachable research subagent"),
            AgentFixture::subagent("compaction").prompt("compaction system"),
            AgentFixture::subagent("title").prompt("title system"),
            AgentFixture::subagent("summary").prompt("summary system"),
        ],
    );
    let app = app_with_runtime(workdir.clone(), runtime).await;

    let (status, body) = get_json(app, "/tui/bootstrap", &workdir).await;
    assert_eq!(status, StatusCode::OK, "bootstrap body: {body}");
    let agents = body
        .get("agents")
        .expect("bootstrap must include agents")
        .clone();
    let names: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();

    assert!(
        names.contains(&"build") && names.contains(&"plan") && names.contains(&"research"),
        "bound catalog rows must appear: {names:?}"
    );
    assert!(
        !names.contains(&"reviewer"),
        "legacy project agent file must not merge into bootstrap agents: {names:?}"
    );
    assert_eq!(find_agent(&agents, "build")["mode"], "primary");
    assert_eq!(find_agent(&agents, "plan")["mode"], "primary");
    assert_eq!(find_agent(&agents, "research")["mode"], "subagent");
    assert_eq!(find_agent(&agents, "compaction")["mode"], "subagent");

    let selector: Vec<&str> = agents
        .as_array()
        .unwrap()
        .iter()
        .filter(|agent| agent["mode"] == "primary")
        .map(|agent| agent["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        selector,
        vec!["build", "hya-main", "plan"],
        "TUI selector set must be role-main / primary only: {selector:?}"
    );
    assert!(
        !selector.contains(&"research"),
        "subagent must remain present but outside primary selector"
    );
}
