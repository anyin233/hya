#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-file-ignore-{nanos}-{}-{id}",
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

#[tokio::test]
async fn opencode_file_list_honors_escaped_ignore_literals() {
    let workdir = tempdir();
    std::fs::write(
        workdir.join(".gitignore"),
        "literal\\*.log\nliteral\\[ab].log\n\\#hash.log\n\\!important.log\nspace\\ name.log\n",
    )
    .unwrap();
    std::fs::write(workdir.join("literal*.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("literalx.log"), "kept\n").unwrap();
    std::fs::write(workdir.join("literal[ab].log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("literala.log"), "kept\n").unwrap();
    std::fs::write(workdir.join("#hash.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("!important.log"), "ignored\n").unwrap();
    std::fs::write(workdir.join("space name.log"), "ignored\n").unwrap();
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/file?path=.").await;

    assert_eq!(status, StatusCode::OK);
    assert!(has_ignored(&listing, "literal*.log", true));
    assert!(has_ignored(&listing, "literalx.log", false));
    assert!(has_ignored(&listing, "literal[ab].log", true));
    assert!(has_ignored(&listing, "literala.log", false));
    assert!(has_ignored(&listing, "#hash.log", true));
    assert!(has_ignored(&listing, "!important.log", true));
    assert!(has_ignored(&listing, "space name.log", true));
}

#[tokio::test]
async fn opencode_file_list_preserves_significant_ignore_rule_spaces() {
    let workdir = tempdir();
    std::fs::write(
        workdir.join(".gitignore"),
        " leading.log\nescaped-trailing\\ \n*.keep\n! keep.keep\nescaped-keep*\n!escaped-keep\\ \n",
    )
    .unwrap();
    for path in [
        " leading.log",
        "leading.log",
        "escaped-trailing ",
        "escaped-trailing",
        "keep.keep",
        " keep.keep",
        "escaped-keep ",
        "escaped-keep",
    ] {
        std::fs::write(workdir.join(path), "x\n").unwrap();
    }
    let app = router(state(workdir).await);

    let (status, listing) = get_json(app, "/file?path=.").await;

    assert_eq!(status, StatusCode::OK);
    assert!(has_ignored(&listing, " leading.log", true));
    assert!(has_ignored(&listing, "leading.log", false));
    assert!(has_ignored(&listing, "escaped-trailing ", true));
    assert!(has_ignored(&listing, "escaped-trailing", false));
    assert!(has_ignored(&listing, "keep.keep", true));
    assert!(has_ignored(&listing, " keep.keep", false));
    assert!(has_ignored(&listing, "escaped-keep ", false));
    assert!(has_ignored(&listing, "escaped-keep", true));
}
