#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use yaca_core::{AgentSpec, EventBus, SessionEngine};
use yaca_proto::api::CreateSessionResponse;
use yaca_proto::{AgentName, ModelRef};
use yaca_provider::{FakeProvider, ProviderRouter};
use yaca_server::{AppState, router};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

const WORKDIR: &str = "/tmp/yaca-opencode-session-v2-list-api";
const OTHER_WORKDIR: &str = "/tmp/yaca-opencode-session-v2-list-api-other";

async fn state() -> AppState {
    let router =
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![]))));
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn create_session(app: axum::Router, parent: Option<&str>) -> String {
    create_session_in(app, parent, WORKDIR).await
}

async fn create_session_in(app: axum::Router, parent: Option<&str>, workdir: &str) -> String {
    let mut body = json!({"agent": "build", "model": "fake", "workdir": workdir});
    if let Some(parent) = parent {
        body["parent"] = json!(parent.trim_start_matches("ses_"));
    }
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let created: CreateSessionResponse = serde_json::from_value(body_json(resp).await).unwrap();
    format!("ses_{}", created.session.as_uuid().simple())
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
    (status, body_json(resp).await)
}

async fn patch_json(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    (status, body_json(resp).await)
}

#[tokio::test]
async fn opencode_v2_session_list_filters_directory() {
    let app = router(state().await);
    let included = create_session_in(app.clone(), None, WORKDIR).await;
    let excluded = create_session_in(app.clone(), None, OTHER_WORKDIR).await;

    let (status, body) = get_json(app, &format!("/api/session?directory={WORKDIR}")).await;
    assert_eq!(status, StatusCode::OK);
    let ids = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&included.as_str()));
    assert!(!ids.contains(&excluded.as_str()));
}

#[tokio::test]
async fn opencode_v2_session_list_filters_archived_by_default() {
    let app = router(state().await);
    let active = create_session(app.clone(), None).await;
    let archived = create_session(app.clone(), None).await;

    let (status, _) = patch_json(
        app.clone(),
        format!("/api/session/{archived}"),
        json!({"time": {"archived": 99}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = get_json(app, "/api/session?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    let ids = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ids.contains(&active.as_str()));
    assert!(!ids.contains(&archived.as_str()));
}

#[tokio::test]
async fn opencode_v2_session_list_filters_roots_start_and_limit() {
    let app = router(state().await);
    let parent = create_session(app.clone(), None).await;
    let child = create_session(app.clone(), Some(&parent)).await;

    let (status, empty) = get_json(app.clone(), "/api/session?limit=0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        empty["data"]
            .as_array()
            .expect("zero-limit list")
            .is_empty()
    );

    let (status, roots) = get_json(app.clone(), "/api/session?roots=true&limit=10").await;
    assert_eq!(status, StatusCode::OK);
    let root_ids = roots["data"].as_array().expect("root list");
    assert!(root_ids.iter().any(|item| item["id"] == parent));
    assert!(root_ids.iter().all(|item| item["id"] != child));

    let (status, future) = get_json(app, "/api/session?start=9223372036854775807").await;
    assert_eq!(status, StatusCode::OK);
    assert!(future["data"].as_array().expect("future list").is_empty());
}

#[tokio::test]
async fn opencode_v2_session_list_invalid_cursor_returns_typed_error() {
    let app = router(state().await);

    let (status, body) = get_json(app, "/api/session?cursor=invalid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({
            "_tag": "InvalidCursorError",
            "message": "Invalid cursor",
        })
    );
}

#[tokio::test]
async fn opencode_v2_session_list_invalid_workspace_returns_typed_error() {
    let app = router(state().await);

    let (status, body) = get_json(app, "/api/session?workspace=bad").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({
            "_tag": "InvalidRequestError",
            "message": "Invalid workspace query parameter",
            "kind": "Query",
            "field": "workspace",
        })
    );
}

#[tokio::test]
async fn opencode_v2_session_list_cursor_preserves_query_shape() {
    let app = router(state().await);
    create_session(app.clone(), None).await;
    create_session(app.clone(), None).await;

    let (status, first) = get_json(
        app.clone(),
        &format!("/api/session?limit=1&order=asc&search=Untitled&directory={WORKDIR}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = first["data"][0]["id"].as_str().expect("session id");
    let cursor = first["cursor"]["next"].as_str().expect("next cursor");
    let decoded: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).expect("cursor b64")).unwrap();
    assert_eq!(decoded["order"], "asc");
    assert_eq!(decoded["search"], "Untitled");
    assert_eq!(decoded["directory"], WORKDIR);
    assert_eq!(decoded["anchor"]["id"], session);
    assert_eq!(decoded["anchor"]["direction"], "next");
    assert!(decoded["anchor"]["time"].as_u64().is_some());

    let (status, second) = get_json(app, &format!("/api/session?cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(second["data"][0]["id"], session);
}
