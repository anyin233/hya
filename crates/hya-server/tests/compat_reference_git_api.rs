#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

fn workdir() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-compat-reference-git-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn expected_repo_path(segments: &[&str]) -> String {
    let mut path = std::env::var_os("XDG_DATA_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share/compat/repos")
        },
        |data| PathBuf::from(data).join("compat").join("repos"),
    );
    for segment in segments {
        path.push(segment);
    }
    path.to_string_lossy().into_owned()
}

async fn state(workdir: &str) -> AppState {
    std::fs::create_dir_all(format!("{workdir}/docs")).unwrap();
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
            workdir: workdir.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = if body.is_null() {
        Body::empty()
    } else {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_string())
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn compat_reference_api_lists_git_sources() {
    let workdir = workdir();
    let app = router(state(&workdir).await);

    let (status, _config) = request_json(
        app.clone(),
        Method::PATCH,
        "/global/config",
        json!({
            "references": {
                "docs": "./docs",
                "effect": {
                    "repository": "Effect-TS/effect",
                    "branch": "main",
                    "description": "Effect reference"
                },
                "bad": "not-a-repo",
                "badbranch": {
                    "repository": "owner/repo",
                    "branch": "../main"
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, references) =
        request_json(app.clone(), Method::GET, "/api/reference", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(references["location"]["directory"], workdir);
    let data = references["data"].as_array().unwrap();
    let docs = data.iter().find(|item| item["name"] == "docs").unwrap();
    assert_eq!(docs["description"], Value::Null);
    assert_eq!(docs["hidden"], Value::Null);
    assert_eq!(docs["source"]["type"], "local");
    let effect = data.iter().find(|item| item["name"] == "effect").unwrap();
    assert_eq!(
        effect["path"],
        expected_repo_path(&["github.com", "Effect-TS", "effect"])
    );
    assert_eq!(effect["description"], "Effect reference");
    assert_eq!(effect["hidden"], Value::Null);
    assert_eq!(effect["source"]["type"], "git");
    assert_eq!(effect["source"]["repository"], "Effect-TS/effect");
    assert_eq!(effect["source"]["branch"], "main");
    assert!(data.iter().all(|item| item["name"] != "bad"));
    assert!(data.iter().all(|item| item["name"] != "badbranch"));
}
