#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

const WORKDIR: &str = "/tmp/hya-opencode-experimental-session-filter-api";
const OTHER_WORKDIR: &str = "/tmp/hya-opencode-experimental-session-filter-api-other";

async fn state() -> AppState {
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
            workdir: WORKDIR.into(),
            reasoning: None,
        }),
    )
}

async fn request_json(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = request(app, method, uri, body).await;
    let status = response.status();
    (status, body_json(response).await)
}

async fn request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn create_session(app: axum::Router) -> String {
    create_session_in(app, WORKDIR).await
}

async fn create_session_in(app: axum::Router, workdir: &str) -> String {
    let (status, body) = request_json(
        app,
        "POST",
        "/sessions",
        Some(json!({"agent": "build", "model": "fake", "workdir": workdir})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body["session"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn experimental_session_list_filters_archived_sessions_by_default() {
    let app = router(state().await);
    let active = create_session(app.clone()).await;
    let archived = create_session(app.clone()).await;
    let active_id = opencode_id(&active);
    let archived_id = opencode_id(&archived);
    let active_time = touch_session(app.clone(), &active, "active", 0).await;
    touch_session(app.clone(), &archived, "archived", active_time).await;

    let (status, _) = request_json(
        app.clone(),
        "PATCH",
        &format!("/session/{archived}"),
        Some(json!({"time": {"archived": 42}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for uri in [
        "/experimental/session",
        "/experimental/session?archived=false",
    ] {
        let (status, sessions) = request_json(app.clone(), "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(session_ids(&sessions).contains(&active_id));
        assert!(!session_ids(&sessions).contains(&archived_id));
    }

    let (status, archived_sessions) =
        request_json(app, "GET", "/experimental/session?archived=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(session_ids(&archived_sessions).contains(&archived_id));
}

#[tokio::test]
async fn experimental_session_list_uses_updated_time_cursor() {
    let app = router(state().await);
    let oldest = create_session(app.clone()).await;
    let middle = create_session(app.clone()).await;
    let newest = create_session(app.clone()).await;
    let oldest_time = touch_session(app.clone(), &oldest, "oldest", 0).await;
    let middle_time = touch_session(app.clone(), &middle, "middle", oldest_time).await;
    touch_session(app.clone(), &newest, "newest", middle_time).await;

    let first = request(app.clone(), "GET", "/experimental/session?limit=2", None).await;
    assert_eq!(first.status(), StatusCode::OK);
    let cursor = first
        .headers()
        .get("x-next-cursor")
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let first_page = body_json(first).await;
    assert_eq!(
        session_ids(&first_page),
        vec![opencode_id(&newest), opencode_id(&middle)]
    );
    assert_eq!(cursor, first_page[1]["time"]["updated"].as_u64().unwrap());

    let second = request(
        app,
        "GET",
        &format!("/experimental/session?limit=2&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    assert!(second.headers().get("x-next-cursor").is_none());
    let second_page = body_json(second).await;
    assert_eq!(session_ids(&second_page), vec![opencode_id(&oldest)]);
}

#[tokio::test]
async fn experimental_session_list_filters_directory() {
    let app = router(state().await);
    let included = create_session_in(app.clone(), WORKDIR).await;
    let excluded = create_session_in(app.clone(), OTHER_WORKDIR).await;
    let included_time = touch_session(app.clone(), &included, "included", 0).await;
    touch_session(app.clone(), &excluded, "excluded", included_time).await;

    let (status, sessions) = request_json(
        app,
        "GET",
        &format!("/experimental/session?directory={WORKDIR}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ids = session_ids(&sessions);
    assert!(ids.contains(&opencode_id(&included)));
    assert!(!ids.contains(&opencode_id(&excluded)));
}

async fn touch_session(app: axum::Router, session: &str, title: &str, after: u64) -> u64 {
    for attempt in 0..100 {
        let (status, body) = request_json(
            app.clone(),
            "PATCH",
            &format!("/session/{session}"),
            Some(json!({"title": format!("{title}-{attempt}")})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let updated = body["time"]["updated"].as_u64().unwrap();
        if updated > after {
            return updated;
        }
    }
    panic!("session timestamp did not advance");
}

fn opencode_id(id: &str) -> String {
    id.to_string()
}

fn session_ids(sessions: &Value) -> Vec<String> {
    sessions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|session| session["id"].as_str().map(ToString::to_string))
        .collect()
}
