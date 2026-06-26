#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
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

fn skill_named<'a>(skills: &'a Value, name: &str) -> &'a Value {
    skills
        .as_array()
        .and_then(|items| items.iter().find(|skill| skill["name"] == name))
        .unwrap_or_else(|| panic!("missing skill {name}: {skills}"))
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

async fn request_status(app: axum::Router, method: Method, uri: &str, body: Value) -> StatusCode {
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
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

    let (status, missing) = get_json(app.clone(), "/api/provider/anthropic").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["_tag"], "ProviderNotFoundError");
    assert_eq!(missing["providerID"], "anthropic");

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
    let demo_skill = skill_named(&skills["data"], "demo");
    assert_eq!(demo_skill["name"], "demo");
    assert_eq!(demo_skill["description"], "Demo skill");
    assert_eq!(demo_skill["content"], "Use this skill.\n");
}

#[tokio::test]
async fn opencode_legacy_provider_routes_return_active_catalog_and_reject_bad_oauth() {
    let app = router(state(WORKDIR).await);

    let (status, providers) = get_json(app.clone(), "/provider").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["all"][0]["id"], "openai");
    assert_eq!(providers["default"]["openai"], "gpt-5");
    assert_eq!(providers["connected"], json!(["openai"]));

    let (status, auth) = get_json(app.clone(), "/provider/auth").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(auth.as_object().unwrap().len(), 1);
    assert_eq!(auth["openai"][0]["type"], "api");
    assert_eq!(auth["openai"][0]["label"], "API key");

    let status = request_status(
        app.clone(),
        Method::POST,
        "/provider/openai/oauth/authorize",
        json!({"method": "bad"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let status = request_status(
        app,
        Method::POST,
        "/provider/openai/oauth/callback",
        json!({"method": "bad"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn opencode_legacy_config_routes_return_active_provider_data() {
    let app = router(state(WORKDIR).await);

    let (status, config) = get_json(app.clone(), "/config").await;
    assert_eq!(status, StatusCode::OK);
    assert!(config.is_object());

    let (status, updated) = request_json(
        app.clone(),
        Method::PATCH,
        "/config",
        json!({"username": "httpapi-local"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["username"], "httpapi-local");

    let (status, config) = get_json(app.clone(), "/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(config["username"], "httpapi-local");

    let (status, global_config) = get_json(app.clone(), "/global/config").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(global_config["username"], "httpapi-local");

    let status = request_status(
        app.clone(),
        Method::PATCH,
        "/config",
        json!({"username": 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, providers) = get_json(app, "/config/providers").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(providers["providers"][0]["id"], "openai");
    assert_eq!(providers["default"]["openai"], "gpt-5");
}
