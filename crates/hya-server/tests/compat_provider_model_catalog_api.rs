//! Integration tests for `hya-server`: compat provider model catalog api.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{
    HttpProvider, ProviderAuthState, ProviderCatalogResult, ProviderCatalogSnapshot,
    ProviderCatalogSource, ProviderCatalogState, ProviderKind, ProviderRouter,
};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

async fn state() -> AppState {
    let openai = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        "https://api.openai.test/v1",
        Some("sk-test".to_string()),
        ["gpt-5".to_string(), "gpt-4.1".to_string()],
    )
    .unwrap();
    let anthropic = HttpProvider::new(
        "anthropic",
        ProviderKind::Anthropic,
        "https://api.anthropic.test/v1",
        Some("sk-test".to_string()),
        ["claude-sonnet-4-6".to_string()],
    )
    .unwrap();
    let router = ProviderRouter::new()
        .with(Arc::new(openai))
        .with(Arc::new(anthropic));
    let states = [
        ProviderCatalogState {
            provider_id: "openai".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            source: ProviderCatalogSource::Configured,
            auth: ProviderAuthState::Credentialed,
            result: ProviderCatalogResult::Models,
        },
        ProviderCatalogState {
            provider_id: "anthropic".to_string(),
            kind: ProviderKind::Anthropic,
            source: ProviderCatalogSource::Configured,
            auth: ProviderAuthState::Credentialed,
            result: ProviderCatalogResult::Models,
        },
        ProviderCatalogState {
            provider_id: "needs-auth".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            source: ProviderCatalogSource::None,
            auth: ProviderAuthState::AuthRequired,
            result: ProviderCatalogResult::Unavailable,
        },
    ];
    let snapshot = Arc::new(ProviderCatalogSnapshot::build(
        router.catalog(),
        states,
        Some(ModelRef::new("openai/gpt-5")),
    ));
    let providers = Arc::new(router.with_catalog_snapshot(snapshot));
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
            model: ModelRef::new("openai/gpt-5"),
            system_prompt: "x".to_string(),
            workdir: "/tmp/hya-compat-provider-model-catalog".into(),
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn compat_v2_catalog_routes_return_configured_provider_models() {
    let app = router(state().await);

    let (status, providers) = get_json(app.clone(), "/api/provider").await;
    assert_eq!(status, StatusCode::OK);
    let provider_ids: Vec<_> = providers["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|provider| provider["id"].as_str().unwrap())
        .collect();
    assert_eq!(provider_ids, ["anthropic", "needs-auth", "openai"]);
    let needs_auth = providers["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["id"] == "needs-auth")
        .unwrap();
    assert_eq!(needs_auth["models"], serde_json::json!({}));
    assert_eq!(needs_auth["source"], "none");
    assert_eq!(needs_auth["auth"], "auth_required");
    assert_eq!(needs_auth["result"], "unavailable");

    let (status, provider) = get_json(app.clone(), "/api/provider/anthropic").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(provider["data"]["id"], "anthropic");

    let (status, missing) = get_json(app.clone(), "/api/provider/missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["_tag"], "ProviderNotFoundError");

    assert_eq!(
        provider["data"]["models"]["claude-sonnet-4-6"]["status"],
        "configured"
    );
    assert_eq!(
        provider["data"]["models"]["claude-sonnet-4-6"]["enabled"],
        true
    );
    let (status, models) = get_json(app, "/api/model").await;
    assert_eq!(status, StatusCode::OK);
    let model_ids: Vec<_> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| {
            (
                model["providerID"].as_str().unwrap(),
                model["id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        model_ids,
        [
            ("anthropic", "claude-sonnet-4-6"),
            ("openai", "gpt-4.1"),
            ("openai", "gpt-5"),
        ]
    );
    let gpt5 = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["providerID"] == "openai" && model["id"] == "gpt-5")
        .unwrap();
    assert_eq!(gpt5["capabilities"]["tools"], true);
    assert_eq!(gpt5["limit"]["context"], 200_000);
    assert_eq!(gpt5["status"], "configured");
    assert_eq!(gpt5["source"], "configured");

    let (status, legacy) = get_json(router(state().await), "/provider").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy["connected"], serde_json::json!([]));
    assert_eq!(legacy["defaultModel"]["providerID"], "openai");
    assert_eq!(legacy["defaultModel"]["modelID"], "gpt-5");
}
