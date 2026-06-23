#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "yaca-server-fs-v2-test-{nanos}-{}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let provider = Arc::new(FakeProvider::scripted(vec![]));
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(store, router, tools, perm, EventBus::default());
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn opencode_v2_fs_list_orders_directories_first_with_trailing_slash() {
    let workdir = tempdir();
    std::fs::write(workdir.join("a_file.txt"), "a\n").unwrap();
    std::fs::create_dir_all(workdir.join("z_dir")).unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/api/fs/list").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listing["data"],
        serde_json::json!([
            {"path": "z_dir/", "type": "directory", "mime": "application/x-directory"},
            {"path": "a_file.txt", "type": "file", "mime": "text/plain"}
        ])
    );
}

#[tokio::test]
async fn opencode_v2_fs_list_uses_extension_mime_types() {
    let workdir = tempdir();
    std::fs::write(workdir.join("icon.svg"), "<svg></svg>\n").unwrap();
    std::fs::write(workdir.join("sound.mp3"), b"ID3").unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/api/fs/list").await;

    assert_eq!(status, StatusCode::OK);
    let files = listing["data"].as_array().unwrap();
    let icon = files
        .iter()
        .find(|item| item["path"] == "icon.svg")
        .unwrap();
    let sound = files
        .iter()
        .find(|item| item["path"] == "sound.mp3")
        .unwrap();
    assert_eq!(icon["mime"], "image/svg+xml");
    assert_eq!(sound["mime"], "audio/mpeg");
}

#[tokio::test]
async fn opencode_v2_fs_find_uses_opencode_default_limit() {
    let workdir = tempdir();
    for index in 0..12 {
        std::fs::write(workdir.join(format!("match-{index:02}.txt")), "match\n").unwrap();
    }
    let app = router(state(workdir).await);

    let (status, found) = get_json(app, "/api/fs/find?query=match&type=file").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["data"].as_array().unwrap().len(), 12);
}

#[tokio::test]
async fn opencode_v2_fs_find_preserves_directory_trailing_slash() {
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join("match-dir")).unwrap();
    let app = router(state(workdir).await);

    let (status, found) = get_json(app, "/api/fs/find?query=match-dir&type=directory").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(found["data"][0]["path"], "match-dir/");
}
