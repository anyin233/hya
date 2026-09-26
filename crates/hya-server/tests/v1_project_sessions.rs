//! v1 project-aware session creation (ADR-0024): `CreateSessionRequest`
//! `project_id` / `kind` / optional `workdir` rules, temporary sessions in
//! scratch directories that outlive the session, `SessionInfo.projectId` /
//! `kind`, and the `ListSessions` `projectId` filter.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn app_with_scratch(scratch: PathBuf) -> axum::Router {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
    router(
        AppState::new(
            Arc::new(engine),
            Arc::new(AgentSpec {
                name: AgentName::new("build"),
                model: ModelRef::new("fake"),
                system_prompt: "x".to_string(),
                workdir: std::env::temp_dir(),
                reasoning: None,
            }),
        )
        .with_scratch_root(scratch),
    )
}

async fn app() -> axum::Router {
    app_with_scratch(support::tempdir("session-scratch")).await
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `POST /v1/sessions` with `agent`/`model` filled in.
async fn create(app: &axum::Router, fields: Value) -> (StatusCode, Value) {
    let mut body = json!({ "agent": "build", "model": "fake" });
    for (key, value) in fields.as_object().unwrap() {
        body[key] = value.clone();
    }
    call(app, Method::POST, "/v1/sessions", body).await
}

async fn created(app: &axum::Router, fields: Value) -> Value {
    let (status, body) = create(app, fields).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"].clone()
}

fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    assert_eq!(body["error"]["code"], json!(code), "{body}");
}

async fn project(app: &axum::Router, name: &str, roots: &[&str]) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/projects",
        json!({ "name": name, "roots": roots }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

async fn listed(app: &axum::Router, query: &str) -> Vec<String> {
    let (status, body) = call(
        app,
        Method::GET,
        &format!("/v1/sessions{query}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["sessions"].as_array().map_or_else(Vec::new, |rows| {
        rows.iter()
            .map(|row| row["id"].as_str().unwrap().to_owned())
            .collect()
    })
}

#[tokio::test]
async fn temporary_session_gets_a_private_scratch_dir_that_outlives_it() {
    let scratch = support::tempdir("temp-session-scratch");
    let app = app_with_scratch(scratch.clone()).await;
    let session = created(&app, json!({ "kind": "SESSION_KIND_TEMPORARY" })).await;
    let id = session["id"].as_str().unwrap();
    let workdir = PathBuf::from(session["workdir"].as_str().unwrap());
    assert_eq!(workdir, scratch.join(id), "{session}");
    assert!(workdir.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&workdir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }
    assert_eq!(session["kind"], json!("SESSION_KIND_TEMPORARY"));
    assert!(session.get("projectId").is_none(), "{session}");

    // It joins no Project.
    let (_, projects) = call(&app, Method::GET, "/v1/projects", Value::Null).await;
    assert!(projects.get("projects").is_none(), "{projects}");

    std::fs::write(workdir.join("notes.txt"), "keep me").unwrap();
    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/sessions/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        std::fs::read_to_string(workdir.join("notes.txt")).unwrap(),
        "keep me",
        "the scratch directory is never deleted"
    );
}

#[tokio::test]
async fn temporary_session_refuses_a_project_or_workdir() {
    let app = app().await;
    let id = project(&app, "p", &["/p"]).await;
    let (status, body) = create(
        &app,
        json!({ "kind": "SESSION_KIND_TEMPORARY", "projectId": id }),
    )
    .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    let (status, body) = create(
        &app,
        json!({ "kind": "SESSION_KIND_TEMPORARY", "workdir": "/p" }),
    )
    .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn project_session_defaults_to_the_primary_root() {
    let app = app().await;
    let id = project(&app, "multi", &["/multi/a", "/multi/b"]).await;
    let session = created(
        &app,
        json!({ "projectId": id, "kind": "SESSION_KIND_PROJECT" }),
    )
    .await;
    assert_eq!(session["workdir"], json!("/multi/a"));
    assert_eq!(session["projectId"], json!(id));
    assert_eq!(session["kind"], json!("SESSION_KIND_PROJECT"));

    // Kind unset means a Project session.
    let session = created(&app, json!({ "projectId": id })).await;
    assert_eq!(session["kind"], json!("SESSION_KIND_PROJECT"));

    // A workdir inside any root is kept; outside every root is refused,
    // matching whole path components.
    let session = created(&app, json!({ "projectId": id, "workdir": "/multi/b/src/" })).await;
    assert_eq!(session["workdir"], json!("/multi/b/src"));
    for workdir in [
        "/elsewhere",
        "/multi/bc",
        "/multi",
        "relative",
        "/multi/a/../c",
    ] {
        let (status, body) = create(&app, json!({ "projectId": id, "workdir": workdir })).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    }
}

#[tokio::test]
async fn project_session_needs_an_existing_project() {
    let app = app().await;
    let missing = hya_proto::ProjectId::new().to_string();
    let (status, body) = create(&app, json!({ "projectId": missing })).await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    let (status, body) = create(&app, json!({ "projectId": "bogus" })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    let (status, body) = create(&app, json!({})).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    let (status, body) = create(&app, json!({ "kind": "SESSION_KIND_PROJECT" })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn a_workdir_without_a_project_ensures_one() {
    let app = app().await;
    let first = created(
        &app,
        json!({ "workdir": "/repo", "kind": "SESSION_KIND_PROJECT" }),
    )
    .await;
    let project = first["projectId"].as_str().unwrap().to_owned();
    assert_eq!(first["workdir"], json!("/repo"));
    let (_, info) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{project}"),
        Value::Null,
    )
    .await;
    assert_eq!(info["name"], json!("repo"));
    assert_eq!(info["roots"], json!(["/repo"]));

    // A subdirectory reuses it and keeps the cwd as the workdir.
    let nested = created(&app, json!({ "workdir": "/repo/crates/x" })).await;
    assert_eq!(nested["projectId"], json!(project));
    assert_eq!(nested["workdir"], json!("/repo/crates/x"));
    let (_, projects) = call(&app, Method::GET, "/v1/projects", Value::Null).await;
    assert_eq!(projects["projects"].as_array().unwrap().len(), 1);

    let (status, body) = create(&app, json!({ "workdir": "relative" })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn child_sessions_inherit_and_refuse_project_fields() {
    let app = app().await;
    let id = project(&app, "tree", &["/tree"]).await;
    let root = created(&app, json!({ "projectId": id, "workdir": "/tree/src" })).await;
    let root_id = root["id"].as_str().unwrap();

    let child = created(&app, json!({ "parent": root_id })).await;
    assert_eq!(child["projectId"], json!(id));
    assert_eq!(child["kind"], json!("SESSION_KIND_PROJECT"));
    assert_eq!(child["workdir"], json!("/tree/src"), "parent's workdir");
    let child = created(&app, json!({ "parent": root_id, "workdir": "/tree/other" })).await;
    assert_eq!(child["workdir"], json!("/tree/other"));

    for fields in [
        json!({ "parent": root_id, "projectId": id }),
        json!({ "parent": root_id, "kind": "SESSION_KIND_PROJECT" }),
        json!({ "parent": root_id, "kind": "SESSION_KIND_TEMPORARY" }),
    ] {
        let (status, body) = create(&app, fields).await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    }
    let (status, body) = create(&app, json!({ "parent": "not-a-session" })).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn list_sessions_filters_by_project() {
    let app = app().await;
    let a = project(&app, "a", &["/a"]).await;
    let b = project(&app, "b", &["/b"]).await;
    let in_a = created(&app, json!({ "projectId": a })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child = created(&app, json!({ "parent": in_a })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let in_b = created(&app, json!({ "projectId": b })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let temp = created(&app, json!({ "kind": "SESSION_KIND_TEMPORARY" })).await["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut only_a = listed(&app, &format!("?projectId={a}")).await;
    only_a.sort();
    let mut expected = vec![in_a.clone(), child];
    expected.sort();
    assert_eq!(only_a, expected);
    assert_eq!(
        listed(&app, &format!("?projectId={b}")).await,
        vec![in_b.clone()]
    );
    let all = listed(&app, "").await;
    assert!(all.contains(&in_a) && all.contains(&in_b) && all.contains(&temp));
    let unknown = hya_proto::ProjectId::new();
    assert!(
        listed(&app, &format!("?projectId={unknown}"))
            .await
            .is_empty()
    );
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/sessions?projectId=bogus",
        Value::Null,
    )
    .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");

    // `GetSession` reports the same fields as the listing.
    let (_, info) = call(
        &app,
        Method::GET,
        &format!("/v1/sessions/{in_b}"),
        Value::Null,
    )
    .await;
    assert_eq!(info["projectId"], json!(b));
    assert_eq!(info["kind"], json!("SESSION_KIND_PROJECT"));
}
