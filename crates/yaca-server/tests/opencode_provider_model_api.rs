#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-provider-model-api";

fn tempdir() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-metadata-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join(".yaca/skills/demo")).unwrap();
    std::fs::write(
        dir.join(".yaca/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nUse this skill.\n",
    )
    .unwrap();
    dir
}

async fn state(workdir: impl Into<std::path::PathBuf>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("openai/gpt-5"),
            system_prompt: "x".to_string(),
            workdir: workdir.into(),
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

async fn get_status(app: axum::Router, uri: &str) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

#[tokio::test]
async fn opencode_v2_provider_and_model_routes_return_active_catalog() {
    let app = router(state(WORKDIR).await);

    let (status, providers) = get_json(app.clone(), "/api/provider").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["location"]["directory"], WORKDIR);
    assert_eq!(providers["location"]["project"]["directory"], WORKDIR);
    assert_eq!(providers["data"][0]["id"], "openai");
    assert_eq!(providers["data"][0]["name"], "openai");
    assert_eq!(providers["data"][0]["api"]["type"], "native");
    assert_eq!(providers["data"][0]["api"]["settings"], json!({}));
    assert_eq!(providers["data"][0]["request"]["headers"], json!({}));
    assert_eq!(providers["data"][0]["request"]["body"], json!({}));

    let (status, provider) = get_json(app.clone(), "/api/provider/openai").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(provider["data"]["id"], "openai");

    let status = get_status(app.clone(), "/api/provider/anthropic").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, models) = get_json(app, "/api/model").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(models["data"][0]["id"], "gpt-5");
    assert_eq!(models["data"][0]["providerID"], "openai");
    assert_eq!(models["data"][0]["name"], "gpt-5");
    assert_eq!(models["data"][0]["api"]["id"], "gpt-5");
    assert_eq!(models["data"][0]["api"]["type"], "native");
    assert_eq!(models["data"][0]["status"], "active");
    assert_eq!(models["data"][0]["enabled"], true);
    assert_eq!(models["data"][0]["capabilities"]["tools"], false);
    assert_eq!(models["data"][0]["limit"]["context"], 0);
    assert_eq!(models["data"][0]["limit"]["output"], 0);
}

#[tokio::test]
async fn opencode_v2_metadata_routes_return_location_wrapped_data() {
    let workdir = tempdir();
    let expected = std::fs::canonicalize(&workdir).unwrap();
    let expected = expected.to_string_lossy();
    let app = router(state(workdir).await);

    let (status, location) = get_json(app.clone(), "/api/location").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(location["directory"], expected.as_ref());
    assert_eq!(location["project"]["directory"], expected.as_ref());

    let (status, agents) = get_json(app.clone(), "/api/agent").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(agents["location"]["directory"], expected.as_ref());
    assert_eq!(agents["data"][0]["id"], "build");
    assert_eq!(agents["data"][0]["mode"], "primary");
    assert_eq!(agents["data"][0]["model"]["providerID"], "openai");
    assert_eq!(agents["data"][0]["model"]["id"], "gpt-5");
    assert_eq!(agents["data"][0]["request"]["headers"], json!({}));
    assert_eq!(agents["data"][0]["permissions"], json!([]));

    let (status, commands) = get_json(app.clone(), "/api/command").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        commands["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cmd| cmd["name"] == "help" && cmd["template"] == "/help")
    );

    let (status, skills) = get_json(app, "/api/skill").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(skills["data"][0]["name"], "demo");
    assert_eq!(skills["data"][0]["description"], "Demo skill");
    assert_eq!(skills["data"][0]["content"], "Use this skill.\n");
}
