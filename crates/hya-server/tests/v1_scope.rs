//! `hya serve` has no working directory (ADR-0024): an rpc that needs a
//! directory takes it from the request's `directory` field (a query
//! parameter on GET/DELETE, a body field otherwise) or from the session;
//! without either it fails with `invalid_argument`. Catalog rpcs that only
//! *prefer* a directory answer from global sources. The removed
//! `x-hya-directory` header (and gRPC metadata) is refused on every route.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use tower::ServiceExt;

/// A process agent whose workdir points nowhere: nothing may read it.
async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        perm,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake"),
            system_prompt: "x".to_string(),
            workdir: PathBuf::from("/nonexistent/hya-process-agent-workdir"),
            reasoning: None,
        }),
    )
}

/// Percent-encode a query value.
fn enc(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Send one request naming `directory` in its `directory` field: a query
/// parameter on GET/DELETE, a JSON body field otherwise.
async fn send(
    app: axum::Router,
    method: Method,
    uri: &str,
    directory: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let (uri, body) = match directory {
        None => (uri.to_owned(), body),
        Some(directory) if method == Method::GET || method == Method::DELETE => {
            let separator = if uri.contains('?') { '&' } else { '?' };
            (
                format!("{uri}{separator}directory={}", enc(directory)),
                body,
            )
        }
        Some(directory) => {
            let mut body = if body.is_null() { json!({}) } else { body };
            body["directory"] = json!(directory);
            (uri.to_owned(), body)
        }
    };
    send_raw(app, method, &uri, &[], body).await
}

/// Send one request with extra `headers`.
async fn send_raw(
    app: axum::Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let resp = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, json)
}

/// The refusal of a request that still sends the removed header.
const HEADER_REFUSAL: &str =
    "the x-hya-directory header is no longer supported; set the request's directory field";

fn scratch(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-v1-scope-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// Every rpc that works on a directory, with and without a body.
fn scoped_calls() -> Vec<(Method, &'static str, Value)> {
    vec![
        (Method::GET, "/v1/fs/read?path=notes.txt", Value::Null),
        (Method::GET, "/v1/fs/list", Value::Null),
        (Method::GET, "/v1/fs/find?pattern=*", Value::Null),
        (Method::GET, "/v1/fs/search?query=x", Value::Null),
        (Method::GET, "/v1/fs/symbols?query=x", Value::Null),
        (Method::GET, "/v1/vcs", Value::Null),
        (Method::GET, "/v1/vcs/diff", Value::Null),
        (Method::POST, "/v1/vcs/apply", json!({"patch": ""})),
        (Method::GET, "/v1/worktrees", Value::Null),
        (Method::POST, "/v1/worktrees", json!({})),
        (Method::DELETE, "/v1/worktrees/some-id", Value::Null),
        (Method::POST, "/v1/worktrees/some-id/reset", Value::Null),
        (Method::POST, "/v1/pty", json!({"shell": "/bin/sh"})),
    ]
}

#[tokio::test]
async fn scoped_rpcs_without_a_directory_are_invalid_argument() {
    let app = router(state().await);
    for (method, uri, body) in scoped_calls() {
        let (status, reply) = send(app.clone(), method.clone(), uri, None, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {reply}");
        assert_eq!(
            reply["error"]["code"],
            json!("invalid_argument"),
            "{method} {uri}: {reply}"
        );
        let message = reply["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("directory scope"),
            "{method} {uri}: {message}"
        );
    }
}

#[tokio::test]
async fn a_relative_directory_scope_is_invalid_argument() {
    let app = router(state().await);
    for (method, uri, body) in scoped_calls() {
        let (status, reply) = send(app.clone(), method.clone(), uri, Some("."), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {reply}");
        assert_eq!(reply["error"]["code"], json!("invalid_argument"));
    }
    let (status, reply) = send(
        app,
        Method::GET,
        "/v1/agents",
        Some("relative/dir"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert_eq!(reply["error"]["code"], json!("invalid_argument"));
}

#[tokio::test]
async fn scoped_rpcs_with_a_directory_work_on_it() {
    let app = router(state().await);
    let dir = scratch("with-dir");
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/fs/read?path=notes.txt",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/fs/list",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        reply["entries"].to_string().contains("notes.txt"),
        "{reply}"
    );
    let (status, reply) = send(app, Method::GET, "/v1/vcs", Some(&scope), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn pty_create_takes_its_cwd_from_the_request_or_the_scope() {
    let app = router(state().await);
    let dir = scratch("pty");
    let scope = dir.to_string_lossy().into_owned();
    let (status, reply) = send(
        app.clone(),
        Method::POST,
        "/v1/pty",
        Some(&scope),
        json!({"shell": "/bin/sh"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["cwd"], json!(scope), "{reply}");
    let id = reply["id"].as_str().unwrap().to_owned();
    let _ = send(
        app.clone(),
        Method::DELETE,
        &format!("/v1/pty/{id}"),
        None,
        Value::Null,
    )
    .await;

    let (status, reply) = send(
        app.clone(),
        Method::POST,
        "/v1/pty",
        None,
        json!({"shell": "/bin/sh", "cwd": scope}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let id = reply["id"].as_str().unwrap().to_owned();
    let _ = send(
        app,
        Method::DELETE,
        &format!("/v1/pty/{id}"),
        None,
        Value::Null,
    )
    .await;
}

#[tokio::test]
async fn catalog_rpcs_without_a_directory_answer_from_global_sources() {
    let app = router(state().await);
    for uri in [
        "/v1/agents",
        "/v1/commands",
        "/v1/skills",
        "/v1/bootstrap",
        "/v1/agent-models",
        "/v1/models",
        "/v1/providers",
        "/v1/tools",
        "/v1/permissions/rules",
        "/v1/config",
    ] {
        let (status, reply) = send(app.clone(), Method::GET, uri, None, Value::Null).await;
        assert_ne!(status, StatusCode::BAD_REQUEST, "{uri}: {reply}");
        assert_ne!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}: {reply}");
    }
    let (status, agents) = send(app.clone(), Method::GET, "/v1/agents", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{agents}");
    assert!(agents["agents"].to_string().contains("build"), "{agents}");
    let (status, commands) = send(app, Method::GET, "/v1/commands", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{commands}");
    // Builtin commands are global; `${path}` has no directory to name.
    assert!(
        commands["commands"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()),
        "{commands}"
    );
}

#[tokio::test]
async fn project_commands_come_only_from_the_named_directory() {
    let app = router(state().await);
    let dir = scratch("commands");
    std::fs::create_dir_all(dir.join(".hya/commands")).unwrap();
    std::fs::write(dir.join(".hya/commands/scoped-only.md"), "scoped body").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    let (status, reply) = send(
        app.clone(),
        Method::GET,
        "/v1/commands",
        Some(&scope),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(reply.to_string().contains("scoped-only"), "{reply}");
    let (status, reply) = send(app, Method::GET, "/v1/commands", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(!reply.to_string().contains("scoped-only"), "{reply}");
}

#[tokio::test]
async fn location_reports_the_request_scope_not_a_process_directory() {
    let app = router(state().await);
    let (status, reply) = send(app.clone(), Method::GET, "/v1/location", None, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        reply["directory"].as_str().is_none_or(str::is_empty),
        "{reply}"
    );
    let dir = scratch("location");
    let scope = dir.to_string_lossy().into_owned();
    let (status, reply) = send(app, Method::GET, "/v1/location", Some(&scope), Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(reply["directory"], json!(scope), "{reply}");
}

#[tokio::test]
async fn fork_keeps_the_source_workdir_not_the_process_agent_workdir() {
    let app = router(state().await);
    let dir = scratch("fork");
    let workdir = dir.to_string_lossy().into_owned();
    let (status, created) = send(
        app.clone(),
        Method::POST,
        "/v1/sessions",
        None,
        json!({"agent": "build", "model": "fake", "workdir": workdir}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let source = created["session"]["id"].as_str().unwrap().to_owned();
    let (status, forked) = send(
        app,
        Method::POST,
        &format!("/v1/sessions/{source}/fork"),
        None,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{forked}");
    assert_eq!(forked["session"]["workdir"], json!(workdir), "{forked}");
}

#[tokio::test]
async fn grpc_scoped_rpc_without_a_directory_is_invalid_argument() {
    use hya_api::v1::files_server::Files as _;
    let grpc = V1Grpc::new(state().await);
    let error = grpc
        .read_file(tonic::Request::new(pb::ReadFileRequest {
            path: "notes.txt".to_owned(),
            ..Default::default()
        }))
        .await
        .expect_err("no directory scope");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    assert!(error.message().contains("directory scope"), "{error:?}");
}

/// Every Required-scope rpc takes its scope from the `directory` field: with
/// one it gets past the scope check (a later failure, such as "not a git
/// repository", is fine); without one it asks for a scope. Together with the
/// header refusal below, no scoped handler can read a header.
#[tokio::test]
async fn every_required_rpc_takes_its_scope_from_the_directory_field() {
    let app = router(state().await);
    let dir = scratch("field-table");
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();
    let mut required = scoped_calls();
    required.push((Method::GET, "/v1/projects/current", Value::Null));
    for (method, uri, body) in required {
        let (_, without) = send(app.clone(), method.clone(), uri, None, body.clone()).await;
        assert!(
            without.to_string().contains("needs a directory scope"),
            "{method} {uri} without a scope: {without}"
        );
        let (status, with) = send(app.clone(), method.clone(), uri, Some(&scope), body).await;
        assert!(
            !with.to_string().contains("directory scope"),
            "{method} {uri} with directory={scope}: {status} {with}"
        );
        if uri == "/v1/pty" {
            let id = with["id"].as_str().unwrap().to_owned();
            let _ = send(
                app.clone(),
                Method::DELETE,
                &format!("/v1/pty/{id}"),
                None,
                Value::Null,
            )
            .await;
        }
    }
}

/// A request that still sends `x-hya-directory` is refused on every route —
/// scoped or not, JSON or SSE — before any handler runs, so an old client
/// never silently loses its scope.
#[tokio::test]
async fn a_request_with_the_x_hya_directory_header_is_invalid_argument() {
    let app = router(state().await);
    let dir = scratch("header");
    let scope = dir.to_string_lossy().into_owned();
    let with_field = format!("/v1/fs/list?directory={}", enc(&scope));
    let calls: Vec<(Method, &str, Value)> = vec![
        (Method::GET, "/v1/health", Value::Null),
        (Method::GET, "/v1/location", Value::Null),
        (Method::GET, "/v1/agents", Value::Null),
        (Method::GET, "/v1/fs/list", Value::Null),
        (Method::GET, &with_field, Value::Null),
        (Method::GET, "/v1/sessions", Value::Null),
        (
            Method::POST,
            "/v1/pty",
            json!({"shell": "/bin/sh", "directory": scope}),
        ),
        (Method::GET, "/v1/events/stream", Value::Null),
        (
            Method::GET,
            "/v1/events/stream?interactionsOnly=true",
            Value::Null,
        ),
    ];
    for (method, uri, body) in calls {
        for value in [scope.as_str(), ""] {
            let (status, reply) = send_raw(
                app.clone(),
                method.clone(),
                uri,
                &[("x-hya-directory", value)],
                body.clone(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {uri}: {reply}");
            assert_eq!(reply["error"]["code"], json!("invalid_argument"), "{reply}");
            assert_eq!(reply["error"]["message"], json!(HEADER_REFUSAL), "{reply}");
        }
    }
}

/// The refusal is CORS-readable: a browser client sees the 400 and its
/// message, not an opaque CORS failure.
#[tokio::test]
async fn the_header_refusal_carries_cors_headers() {
    let app = router(state().await);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/agents")
                .header("origin", "http://localhost:3000")
                .header("x-hya-directory", "/anywhere")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response
            .headers()
            .contains_key("access-control-allow-origin"),
        "{:?}",
        response.headers()
    );
}

#[tokio::test]
async fn grpc_scope_comes_from_the_request_field() {
    use hya_api::v1::files_server::Files as _;
    use hya_api::v1::process_server::Process as _;
    use hya_api::v1::worktrees_server::Worktrees as _;
    let grpc = V1Grpc::new(state().await);
    let dir = scratch("grpc-field");
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();
    let read = grpc
        .read_file(tonic::Request::new(pb::ReadFileRequest {
            directory: scope.clone(),
            path: "notes.txt".to_owned(),
            ..Default::default()
        }))
        .await
        .expect("read with a directory field")
        .into_inner();
    assert_eq!(read.content, b"hello".to_vec());
    let location = grpc
        .get_location(tonic::Request::new(pb::GetLocationRequest {
            directory: scope.clone(),
        }))
        .await
        .expect("location")
        .into_inner();
    assert_eq!(location.directory, scope);
    // Body rpcs and the worktree rpcs keyed by path take the field too.
    let deleted = grpc
        .delete_worktree(tonic::Request::new(pb::DeleteWorktreeRequest {
            worktree: "missing".to_owned(),
            directory: scope.clone(),
            ..Default::default()
        }))
        .await;
    assert!(
        deleted
            .as_ref()
            .err()
            .is_none_or(|error| !error.message().contains("directory scope")),
        "{deleted:?}"
    );
    let reset = grpc
        .reset_worktree(tonic::Request::new(pb::ResetWorktreeRequest {
            worktree: "missing".to_owned(),
            directory: scope.clone(),
        }))
        .await;
    assert!(
        reset
            .as_ref()
            .err()
            .is_none_or(|error| !error.message().contains("directory scope")),
        "{reset:?}"
    );
    let created = grpc
        .create_worktree(tonic::Request::new(pb::CreateWorktreeRequest {
            directory: scope.clone(),
            ..Default::default()
        }))
        .await;
    assert!(
        created
            .as_ref()
            .err()
            .is_none_or(|error| !error.message().contains("directory scope")),
        "{created:?}"
    );
}

#[tokio::test]
async fn grpc_pty_create_takes_the_directory_field() {
    use hya_api::v1::pty_server::Pty as _;
    let grpc = V1Grpc::new(state().await);
    let dir = scratch("grpc-pty");
    let scope = dir.to_string_lossy().into_owned();
    let pty = grpc
        .create_pty(tonic::Request::new(pb::CreatePtyRequest {
            directory: scope.clone(),
            shell: "/bin/sh".to_owned(),
            ..Default::default()
        }))
        .await
        .expect("pty with a directory field")
        .into_inner();
    assert_eq!(pty.cwd, scope);
    let _ = grpc
        .delete_pty(tonic::Request::new(pb::DeletePtyRequest { id: pty.id }))
        .await;
}

/// gRPC metadata `x-hya-directory` is refused like the HTTP header, on the
/// in-process binding and on a served listener (unary and streaming).
#[tokio::test]
async fn grpc_x_hya_directory_metadata_is_invalid_argument() {
    use hya_api::v1::files_server::Files as _;
    let dir = scratch("grpc-metadata");
    let scope = dir.to_string_lossy().into_owned();
    fn with_metadata<T>(request: T) -> tonic::Request<T> {
        let mut request = tonic::Request::new(request);
        request
            .metadata_mut()
            .insert("x-hya-directory", "/anywhere".parse().unwrap());
        request
    }

    let grpc = V1Grpc::new(state().await);
    let error = grpc
        .read_file(with_metadata(pb::ReadFileRequest {
            directory: scope.clone(),
            path: "notes.txt".to_owned(),
            ..Default::default()
        }))
        .await
        .expect_err("metadata refused");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    assert_eq!(error.message(), HEADER_REFUSAL);

    let server = hya_server::build(state().await);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = server.clone();
    tokio::spawn(async move { served.serve(listener, std::future::pending()).await });
    let channel = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let error = pb::files_client::FilesClient::new(channel.clone())
        .read_file(with_metadata(pb::ReadFileRequest {
            directory: scope.clone(),
            path: "notes.txt".to_owned(),
            ..Default::default()
        }))
        .await
        .expect_err("metadata refused");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    assert_eq!(error.message(), HEADER_REFUSAL);
    let error = pb::events_client::EventsClient::new(channel.clone())
        .stream_global_events(with_metadata(pb::StreamGlobalEventsRequest::default()))
        .await
        .expect_err("metadata refused on a stream");
    assert_eq!(error.code(), tonic::Code::InvalidArgument, "{error:?}");
    assert_eq!(error.message(), HEADER_REFUSAL);
    // Without the metadata the same call works.
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let read = pb::files_client::FilesClient::new(channel)
        .read_file(pb::ReadFileRequest {
            directory: scope,
            path: "notes.txt".to_owned(),
            ..Default::default()
        })
        .await
        .expect("read")
        .into_inner();
    assert_eq!(read.content, b"hello".to_vec());
}
