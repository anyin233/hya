//! v1 Project service (ADR-0024): CRUD, `ResolveProject`,
//! `EnsureProjectForPath`, `GetCurrentProject`, the directory listing, store
//! error mapping over HTTP and gRPC, `ProjectInfo.session_count` / `busy`,
//! and the live `projectsUpdated` frame on the global event stream.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

/// Answers every round with one text part once the test adds a permit to
/// `gate`, so a turn stays running until released.
struct GatedProvider {
    gate: Arc<Semaphore>,
}

#[async_trait]
impl Provider for GatedProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let gate = Arc::clone(&self.gate);
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: "done".to_string(),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let stream = futures::stream::once(async move {
            gate.acquire().await.unwrap().forget();
        })
        .flat_map(move |()| futures::stream::iter(events.clone().into_iter().map(Ok)));
        Ok(Box::pin(stream))
    }
}

async fn state(gate: Arc<Semaphore>) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(GatedProvider { gate })));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    );
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
    .with_scratch_root(support::tempdir("project-scratch"))
}

async fn app() -> axum::Router {
    router(state(Arc::new(Semaphore::new(0))).await)
}

async fn call_with(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Value,
    directory: Option<&str>,
) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(directory) = directory {
        request = request.header("x-hya-directory", directory);
    }
    let resp = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn call(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    call_with(app, method, uri, body, None).await
}

async fn create_project(app: &axum::Router, name: &str, roots: &[&str]) -> Value {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/projects",
        json!({ "name": name, "roots": roots }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn assert_error(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "{body}");
    assert_eq!(body["error"]["code"], json!(code), "{body}");
}

async fn create_session(app: &axum::Router, project: &str) -> String {
    let (status, body) = call(
        app,
        Method::POST,
        "/v1/sessions",
        json!({ "agent": "build", "model": "fake", "projectId": project }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["session"]["id"].as_str().unwrap().to_owned()
}

async fn list_projects(app: &axum::Router) -> Vec<Value> {
    let (status, body) = call(app, Method::GET, "/v1/projects", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["projects"].as_array().cloned().unwrap_or_default()
}

async fn project_row(app: &axum::Router, id: &str) -> Value {
    list_projects(app)
        .await
        .into_iter()
        .find(|row| row["id"] == json!(id))
        .expect("listed project")
}

#[tokio::test]
async fn project_crud_round_trip() {
    let app = app().await;
    let created = create_project(&app, " app ", &["/work/app", "/work/app-docs/"]).await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("prj_"), "{created}");
    assert_eq!(created["name"], json!("app"));
    assert_eq!(created["roots"], json!(["/work/app", "/work/app-docs"]));
    assert!(created["createdAt"].is_string(), "{created}");
    assert!(created["updatedAt"].is_string(), "{created}");
    assert!(created.get("sessionCount").is_none(), "{created}");
    assert!(created.get("busy").is_none(), "{created}");
    assert!(created.get("directory").is_none(), "{created}");

    let (status, got) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, created);

    let listed = list_projects(&app).await;
    assert_eq!(listed, vec![created.clone()]);

    let (status, dirs) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{id}/directories"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dirs}");
    assert_eq!(dirs["directories"], json!(["/work/app", "/work/app-docs"]));

    // Name only, roots only, then both in one call.
    let (status, renamed) = call(
        &app,
        Method::PATCH,
        &format!("/v1/projects/{id}"),
        json!({ "name": "renamed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["name"], json!("renamed"));
    assert_eq!(renamed["roots"], created["roots"]);
    let (status, rerooted) = call(
        &app,
        Method::PATCH,
        &format!("/v1/projects/{id}"),
        json!({ "roots": ["/elsewhere"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rerooted}");
    assert_eq!(rerooted["name"], json!("renamed"));
    assert_eq!(rerooted["roots"], json!(["/elsewhere"]));
    let (status, both) = call(
        &app,
        Method::PATCH,
        &format!("/v1/projects/{id}"),
        json!({ "name": "both", "roots": ["/x", "/y"] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{both}");
    assert_eq!(both["name"], json!("both"));
    assert_eq!(both["roots"], json!(["/x", "/y"]));
    // An invalid half applies nothing.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/projects/{id}"),
        json!({ "name": "lost", "roots": ["relative"] }),
    )
    .await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    let (_, after) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(after["name"], json!("both"));

    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    assert!(list_projects(&app).await.is_empty());
}

#[tokio::test]
async fn project_errors_map_to_stable_codes() {
    let app = app().await;
    for body in [
        json!({ "name": "", "roots": ["/a"] }),
        json!({ "name": "  ", "roots": ["/a"] }),
        json!({ "name": "x", "roots": [] }),
        json!({ "name": "x" }),
        json!({ "name": "x", "roots": ["relative/dir"] }),
        json!({ "name": "x", "roots": ["/a/../b"] }),
    ] {
        let (status, reply) = call(&app, Method::POST, "/v1/projects", body.clone()).await;
        assert_error(status, &reply, StatusCode::BAD_REQUEST, "invalid_argument");
    }
    let missing = hya_proto::ProjectId::new().to_string();
    for (method, uri, body) in [
        (Method::GET, format!("/v1/projects/{missing}"), Value::Null),
        (
            Method::PATCH,
            format!("/v1/projects/{missing}"),
            json!({ "name": "x" }),
        ),
        (
            Method::DELETE,
            format!("/v1/projects/{missing}"),
            Value::Null,
        ),
        (
            Method::GET,
            format!("/v1/projects/{missing}/directories"),
            Value::Null,
        ),
        (
            Method::POST,
            format!("/v1/projects/{missing}/init-git"),
            json!({}),
        ),
    ] {
        let (status, reply) = call(&app, method.clone(), &uri, body).await;
        assert_error(status, &reply, StatusCode::NOT_FOUND, "not_found");
    }
    let (status, reply) = call(&app, Method::GET, "/v1/projects/not-an-id", Value::Null).await;
    assert_error(status, &reply, StatusCode::BAD_REQUEST, "invalid_argument");

    // Delete is refused while a live root session uses the Project.
    let project = create_project(&app, "busy", &[&std::env::temp_dir().to_string_lossy()]).await;
    let id = project["id"].as_str().unwrap().to_owned();
    let session = create_session(&app, &id).await;
    let (status, reply) = call(
        &app,
        Method::DELETE,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_error(status, &reply, StatusCode::CONFLICT, "failed_precondition");
    // Archiving the session lifts the refusal.
    let (status, reply) = call(
        &app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({ "archived": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = call(
        &app,
        Method::DELETE,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn project_errors_over_grpc_use_canonical_codes() {
    let state = state(Arc::new(Semaphore::new(0))).await;
    let app = router(state.clone());
    let channel = grpc_channel(state).await;
    let mut projects = pb::project_client::ProjectClient::new(channel);

    let error = projects
        .create_project(pb::CreateProjectRequest {
            name: "x".into(),
            roots: vec!["relative".into()],
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    let error = projects
        .get_project(pb::GetProjectRequest {
            project: hya_proto::ProjectId::new().to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound, "{error:?}");

    let created = projects
        .create_project(pb::CreateProjectRequest {
            name: "grpc".into(),
            roots: vec![std::env::temp_dir().to_string_lossy().into_owned()],
        })
        .await
        .unwrap()
        .into_inner();
    create_session(&app, &created.id).await;
    let error = projects
        .delete_project(pb::DeleteProjectRequest {
            project: created.id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{error:?}");

    let updated = projects
        .update_project(pb::UpdateProjectRequest {
            project: created.id.clone(),
            name: Some("renamed".into()),
            roots: vec!["/r1".into(), "/r2".into()],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.roots, vec!["/r1".to_owned(), "/r2".to_owned()]);
    assert_eq!(updated.session_count, 1);

    let resolved = projects
        .resolve_project(pb::ResolveProjectRequest {
            path: "/r2/sub".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resolved.project.map(|project| project.id), Some(created.id));
    let listed = projects
        .list_projects(pb::ListProjectsRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.projects.len(), 1);
}

async fn grpc_channel(state: AppState) -> tonic::transport::Channel {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let grpc = V1Grpc::new(state);
    let serve = tonic::transport::Server::builder()
        .add_service(pb::project_server::ProjectServer::new(grpc.clone()))
        .add_service(pb::events_server::EventsServer::new(grpc))
        .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener));
    tokio::spawn(async move {
        let _ = serve.await;
    });
    tonic::transport::Channel::from_shared(format!("http://{address}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

#[tokio::test]
async fn resolve_matches_roots_without_creating() {
    let app = app().await;
    let outer = create_project(&app, "outer", &["/w"]).await;
    let inner = create_project(&app, "inner", &["/w/inner", "/other"]).await;

    let resolve = |path: &str| {
        let app = app.clone();
        let uri = format!("/v1/projects/resolve?path={}", path.replace('/', "%2F"));
        async move { call(&app, Method::GET, &uri, Value::Null).await }
    };
    let (status, body) = resolve("/w/inner/src").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["project"]["id"], inner["id"], "longest root wins");
    let (_, body) = resolve("/w/innerx").await;
    assert_eq!(body["project"]["id"], outer["id"], "component-wise match");
    let (_, body) = resolve("/other").await;
    assert_eq!(body["project"]["id"], inner["id"]);
    let (status, body) = resolve("/nowhere").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.get("project").is_none(), "{body}");
    assert_eq!(list_projects(&app).await.len(), 2, "resolve never creates");

    let (status, body) = resolve("relative").await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    let (status, body) = call(&app, Method::GET, "/v1/projects/resolve", Value::Null).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn ensure_reuses_a_containing_project_or_creates_one() {
    let app = app().await;
    let (status, first) = call(
        &app,
        Method::POST,
        "/v1/projects/ensure",
        json!({ "path": "/work/repo/" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["created"], json!(true));
    assert_eq!(first["project"]["name"], json!("repo"));
    assert_eq!(first["project"]["roots"], json!(["/work/repo"]));

    let (status, again) = call(
        &app,
        Method::POST,
        "/v1/projects/ensure",
        json!({ "path": "/work/repo/crates/a" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert!(again.get("created").is_none(), "{again}");
    assert_eq!(again["project"]["id"], first["project"]["id"]);

    let (status, root) = call(
        &app,
        Method::POST,
        "/v1/projects/ensure",
        json!({ "path": "/" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{root}");
    assert_eq!(root["created"], json!(true));
    assert_eq!(root["project"]["name"], json!("/"));

    for path in ["", "relative", "/a/../b"] {
        let (status, body) = call(
            &app,
            Method::POST,
            "/v1/projects/ensure",
            json!({ "path": path }),
        )
        .await;
        assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
    }
}

#[tokio::test]
async fn current_project_resolves_the_directory_scope() {
    let app = app().await;
    let project = create_project(&app, "scoped", &["/scoped"]).await;
    let (status, body) = call_with(
        &app,
        Method::GET,
        "/v1/projects/current",
        Value::Null,
        Some("/scoped/sub"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], project["id"]);
    let (status, body) = call(
        &app,
        Method::GET,
        "/v1/projects/current?directory=%2Fscoped",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"], project["id"]);
    let (status, body) = call_with(
        &app,
        Method::GET,
        "/v1/projects/current",
        Value::Null,
        Some("/unscoped"),
    )
    .await;
    assert_error(status, &body, StatusCode::NOT_FOUND, "not_found");
    let (status, body) = call(&app, Method::GET, "/v1/projects/current", Value::Null).await;
    assert_error(status, &body, StatusCode::BAD_REQUEST, "invalid_argument");
}

#[tokio::test]
async fn init_git_runs_in_the_primary_root() {
    if !support::git_available() {
        return;
    }
    let app = app().await;
    let dir = support::tempdir("project-init-git");
    let project = create_project(&app, "git", &[&dir.to_string_lossy()]).await;
    let id = project["id"].as_str().unwrap();
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/projects/{id}/init-git"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["initialized"], json!(true));
    assert!(dir.join(".git").exists());
}

#[tokio::test]
async fn session_count_and_busy_follow_the_projects_sessions() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    let project = create_project(&app, "live", &[&std::env::temp_dir().to_string_lossy()]).await;
    let id = project["id"].as_str().unwrap().to_owned();
    let other = create_project(&app, "idle", &["/idle"]).await;

    let session = create_session(&app, &id).await;
    let row = project_row(&app, &id).await;
    assert_eq!(row["sessionCount"], json!(1), "{row}");
    assert!(row.get("busy").is_none(), "{row}");

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "prompt": { "text": "work" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    assert_eq!(project_row(&app, &id).await["busy"], json!(true));
    let (_, got) = call(
        &app,
        Method::GET,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(got["busy"], json!(true), "{got}");
    let other_id = other["id"].as_str().unwrap();
    assert!(project_row(&app, other_id).await.get("busy").is_none());

    // An archived session does not make its Project busy.
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({ "archived": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(project_row(&app, &id).await.get("busy").is_none());
    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/sessions/{session}"),
        json!({ "archived": false }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(project_row(&app, &id).await["busy"], json!(true));

    gate.add_permits(1);
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    tokio::time::timeout(Duration::from_secs(5), async {
        while project_row(&app, &id).await.get("busy").is_some() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("busy clears after the turn");

    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(project_row(&app, &id).await.get("sessionCount").is_none());
}

/// Read `data:` frames from an open SSE response until `predicate` holds or
/// the deadline passes; returns every frame seen.
async fn frames_until(
    stream: &mut (impl futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    predicate: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        let chunk = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        let Ok(Some(Ok(bytes))) = chunk else {
            continue;
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            if let Ok(frame) = serde_json::from_str::<Value>(data.trim()) {
                let done = predicate(&frame);
                seen.push(frame);
                if done {
                    return seen;
                }
            }
        }
    }
    seen
}

async fn open_stream(
    app: &axum::Router,
    uri: &str,
) -> impl futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin + use<> {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let stream = resp.into_body().into_data_stream();
    // Let the stream subscribe before the test triggers anything.
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream
}

fn is_projects_updated(frame: &Value) -> bool {
    frame["event"]["projectsUpdated"].is_object()
}

async fn expect_projects_updated(
    stream: &mut (impl futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
    what: &str,
) {
    let frames = frames_until(stream, is_projects_updated).await;
    let last = frames.last().unwrap_or(&Value::Null);
    assert!(is_projects_updated(last), "{what}: {frames:#?}");
    assert!(last["event"].get("seq").is_none(), "live-only: {last}");
    assert!(
        last["event"].get("session").is_none(),
        "process-wide: {last}"
    );
}

#[tokio::test]
async fn projects_updated_reaches_the_global_stream() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(state(Arc::clone(&gate)).await);
    let mut global = open_stream(&app, "/v1/events/stream").await;
    let mut interactions = open_stream(&app, "/v1/events/stream?interactionsOnly=true").await;

    let project = create_project(&app, "live", &[&std::env::temp_dir().to_string_lossy()]).await;
    let id = project["id"].as_str().unwrap().to_owned();
    expect_projects_updated(&mut global, "create project").await;
    expect_projects_updated(&mut interactions, "create project (interactions only)").await;

    let (status, body) = call(
        &app,
        Method::PATCH,
        &format!("/v1/projects/{id}"),
        json!({ "name": "renamed" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expect_projects_updated(&mut interactions, "update project").await;

    let session = create_session(&app, &id).await;
    expect_projects_updated(&mut interactions, "session created").await;

    // A session stream does not carry the process-wide Project notice.
    let mut session_stream =
        open_stream(&app, &format!("/v1/sessions/{session}/events/stream")).await;

    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({ "prompt": { "text": "work" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let turn = body["turn"]["id"].as_str().unwrap().to_owned();
    expect_projects_updated(&mut interactions, "turn started (busy)").await;
    gate.add_permits(1);
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expect_projects_updated(&mut interactions, "turn finished (idle)").await;
    assert!(project_row(&app, &id).await.get("busy").is_none());

    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/sessions/{session}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expect_projects_updated(&mut interactions, "session deleted").await;

    let (status, body) = call(
        &app,
        Method::DELETE,
        &format!("/v1/projects/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expect_projects_updated(&mut interactions, "project deleted").await;

    let session_frames = frames_until(&mut session_stream, is_projects_updated).await;
    assert!(
        !session_frames.iter().any(is_projects_updated),
        "{session_frames:#?}"
    );
}
