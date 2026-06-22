#![allow(clippy::unwrap_used, clippy::expect_used)]

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
        "yaca-server-file-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "fn main() {\n    println!(\"hello yaca\");\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("README.md"), "# yaca\n\nhello docs\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored.log\nbuild/\n").unwrap();
    std::fs::write(dir.join("ignored.log"), "skip\n").unwrap();
    std::fs::create_dir_all(dir.join("build")).unwrap();
    std::fs::write(dir.join("build/cache.txt"), "skip\n").unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
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
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn opencode_file_routes_return_legacy_shapes() {
    let app = router(state(tempdir()).await);

    let (status, content) = get_json(app.clone(), "/file/content?path=src/main.rs").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content["type"], "text");
    assert_eq!(
        content["content"],
        "fn main() {\n    println!(\"hello yaca\");\n}"
    );

    let (status, listing) = get_json(app.clone(), "/file?path=src").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing[0]["name"], "main.rs");
    assert_eq!(listing[0]["path"], "src/main.rs");
    assert_eq!(listing[0]["type"], "file");
    assert_eq!(listing[0]["ignored"], false);

    let (status, root_listing) = get_json(app.clone(), "/file?path=.").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        root_listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "ignored.log" && entry["ignored"] == true)
    );
    assert!(
        root_listing
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "build" && entry["ignored"] == true)
    );

    let (status, matches) = get_json(app.clone(), "/find?pattern=hello").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(matches[0]["path"]["text"], "README.md");
    assert_eq!(matches[0]["line_number"], 3);
    assert_eq!(matches[0]["submatches"][0]["match"]["text"], "hello");

    let (status, files) = get_json(app.clone(), "/find/file?query=main&type=file").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(files, serde_json::json!(["src/main.rs"]));

    let (status, symbols) = get_json(app.clone(), "/find/symbol?query=main").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(symbols, serde_json::json!([]));

    let (status, file_status) = get_json(app, "/file/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(file_status, serde_json::json!([]));
}
