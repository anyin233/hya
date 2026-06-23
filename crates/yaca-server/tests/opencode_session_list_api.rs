#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
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

const WORKDIR: &str = "/tmp/yaca-opencode-session-list-api";

async fn state() -> AppState {
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(app: axum::Router, parent: Option<&str>) -> String {
    let mut body = json!({"agent": "build", "model": "fake", "workdir": WORKDIR});
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
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn opencode_session_list_filters_roots_start_search_and_limit() {
    let app = router(state().await);
    let parent = create_session(app.clone(), None).await;
    let child = create_session(app.clone(), Some(&parent)).await;

    let (status, parent_body) = patch_json(
        app.clone(),
        format!("/session/{parent}"),
        json!({"title": "alpha root"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _child) = patch_json(
        app.clone(),
        format!("/session/{child}"),
        json!({"title": "beta child"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let created = parent_body["time"]["created"].as_u64().expect("created");
    let mut updated = created;
    for _ in 0..64 {
        let (status, parent_body) = patch_json(
            app.clone(),
            format!("/session/{parent}"),
            json!({"title": "alpha root"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        updated = parent_body["time"]["updated"].as_u64().expect("updated");
        if updated > created {
            break;
        }
    }
    assert!(updated > created);

    let roots = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session?roots=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(roots.status(), StatusCode::OK);
    let roots_body = body_json(roots).await;
    assert!(
        roots_body
            .as_array()
            .expect("roots")
            .iter()
            .all(|item| item["id"] != child)
    );

    let search = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session?search=alpha")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(search.status(), StatusCode::OK);
    let search_body = body_json(search).await;
    assert_eq!(search_body.as_array().expect("search").len(), 1);
    assert_eq!(search_body[0]["id"], parent);

    let limit = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/session?limit=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(limit.status(), StatusCode::OK);
    assert_eq!(body_json(limit).await.as_array().expect("limit").len(), 1);

    let start = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/session?start={updated}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start.status(), StatusCode::OK);
    let start_body = body_json(start).await;
    assert!(
        start_body
            .as_array()
            .expect("start")
            .iter()
            .any(|item| item["id"] == parent)
    );
}
