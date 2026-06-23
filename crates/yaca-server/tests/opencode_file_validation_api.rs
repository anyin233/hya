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
        "yaca-server-file-validation-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
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

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    get_json_with_headers(app, uri, &[]).await
}

async fn get_json_with_headers(
    app: axum::Router,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn opencode_find_file_rejects_invalid_query_params() {
    let app = router(state(tempdir()).await);
    for uri in [
        "/find/file?query=main&limit=0",
        "/find/file?query=main&limit=201",
        "/find/file?query=main&type=symlink",
        "/find/file?query=main&dirs=maybe",
    ] {
        assert_eq!(get_status(app.clone(), uri).await, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn opencode_find_file_matches_fuzzy_file_queries() {
    let workdir = tempdir();
    std::fs::write(workdir.join("src/manifest.rs"), "mod manifest;\n").unwrap();
    let app = router(state(workdir).await);

    let (status, files) = get_json(app, "/find/file?query=mainrs&type=file").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(files[0], "src/main.rs");
}

#[tokio::test]
async fn opencode_file_list_honors_gitignore_negation() {
    let workdir = tempdir();
    std::fs::write(workdir.join(".gitignore"), "*.log\n!important.log\n").unwrap();
    std::fs::write(workdir.join("debug.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("important.log"), "kept\n").unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/file?path=.").await;

    assert_eq!(status, StatusCode::OK);
    let entries = listing.as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "debug.log" && entry["ignored"] == true)
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["path"] == "important.log" && entry["ignored"] == false)
    );
}

#[tokio::test]
async fn opencode_file_list_honors_anchored_gitignore_patterns() {
    let workdir = tempdir();
    std::fs::write(workdir.join(".gitignore"), "/root-only.log\n").unwrap();
    std::fs::write(workdir.join("root-only.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("src/root-only.log"), "kept\n").unwrap();
    let app = router(state(workdir).await);

    let (status, root_listing) = get_json(app.clone(), "/file?path=.").await;
    let (status_src, src_listing) = get_json(app, "/file?path=src").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(status_src, StatusCode::OK);
    assert!(
        root_listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "root-only.log" && entry["ignored"] == true)
    );
    assert!(
        src_listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/root-only.log" && entry["ignored"] == false)
    );
}

#[tokio::test]
async fn opencode_file_list_matches_unanchored_directory_ignores_at_any_depth() {
    let workdir = tempdir();
    std::fs::write(workdir.join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir_all(workdir.join("src/build")).unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/file?path=src").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "src/build" && entry["ignored"] == true)
    );
}

#[tokio::test]
async fn opencode_legacy_file_content_uses_extension_mime_for_binary_files() {
    let workdir = tempdir();
    std::fs::write(workdir.join("clip.avif"), b"avif\0data").unwrap();
    let app = router(state(workdir).await);

    let (status, content) = get_json(app, "/file/content?path=clip.avif").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["type"], "binary");
    assert_eq!(content["mimeType"], "image/avif");
}

#[tokio::test]
async fn opencode_file_list_honors_wildcard_ignore_globs() {
    let workdir = tempdir();
    std::fs::write(workdir.join(".gitignore"), "file?.log\nasset[0-9].log\n").unwrap();
    std::fs::write(workdir.join("file1.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("asset7.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("assetx.log"), "kept\n").unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/file?path=.").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "file1.log" && entry["ignored"] == true)
    );
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "asset7.log" && entry["ignored"] == true)
    );
    assert!(
        listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "assetx.log" && entry["ignored"] == false)
    );
}

#[tokio::test]
async fn opencode_legacy_file_routes_honor_directory_query() {
    let workdir = tempdir();
    let scoped = workdir.join("scoped");
    std::fs::create_dir_all(&scoped).unwrap();
    std::fs::write(scoped.join("target.txt"), "scoped text\n").unwrap();
    let directory = scoped.to_string_lossy();
    let app = router(state(workdir).await);

    let (status, content) = get_json(
        app.clone(),
        &format!("/file/content?path=target.txt&directory={directory}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["content"], "scoped text");

    let (status, content) = get_json_with_headers(
        app.clone(),
        "/file/content?path=target.txt",
        &[("x-opencode-directory", directory.as_ref())],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["content"], "scoped text");

    let (status, matches) = get_json(
        app.clone(),
        &format!("/find?pattern=scoped&directory={directory}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(matches[0]["path"]["text"], "target.txt");

    let (status, files) = get_json(
        app,
        &format!("/find/file?query=target&type=file&directory={directory}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(files, serde_json::json!(["target.txt"]));
}
