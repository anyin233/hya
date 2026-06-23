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

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-file-ignore-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, providers, tools, perm, EventBus::default());
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
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
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

fn has_ignored(listing: &Value, path: &str, ignored: bool) -> bool {
    listing
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == path && entry["ignored"] == ignored)
}

#[tokio::test]
async fn opencode_file_list_matches_slash_aware_ignore_globs() {
    let workdir = tempdir();
    std::fs::write(
        workdir.join(".gitignore"),
        "src/*.log\nsrc/**/*.trace\n**/cache/*.tmp\n",
    )
    .unwrap();
    std::fs::create_dir_all(workdir.join("src/deep/cache")).unwrap();
    std::fs::write(workdir.join("src/a.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("src/a.trace"), "ignored\n").unwrap();
    std::fs::write(workdir.join("src/deep/a.log"), "kept\n").unwrap();
    std::fs::write(workdir.join("src/deep/a.trace"), "ignored\n").unwrap();
    std::fs::write(workdir.join("src/deep/cache/a.tmp"), "ignored\n").unwrap();
    let app = router(state(workdir).await);

    let (status, src_listing) = get_json(app.clone(), "/file?path=src").await;
    let (deep_status, deep_listing) = get_json(app.clone(), "/file?path=src/deep").await;
    let (cache_status, cache_listing) = get_json(app, "/file?path=src/deep/cache").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(deep_status, StatusCode::OK);
    assert_eq!(cache_status, StatusCode::OK);
    assert!(has_ignored(&src_listing, "src/a.log", true));
    assert!(has_ignored(&src_listing, "src/a.trace", true));
    assert!(has_ignored(&deep_listing, "src/deep/a.log", false));
    assert!(has_ignored(&deep_listing, "src/deep/a.trace", true));
    assert!(has_ignored(&cache_listing, "src/deep/cache/a.tmp", true));
}
