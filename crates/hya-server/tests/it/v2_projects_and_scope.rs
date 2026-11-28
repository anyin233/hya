//! Projects, project-aware session placement, explicit `directory` scope
//! enforcement, removed `x-hya-directory` header guard, non-blocking VCS, and
//! per-Project catalog overlays & notices (ADR-0024, ADR-0027).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures::StreamExt;
use hya_api::v2 as pb;
use hya_engine::{
    ApiMethod, ApiPathTemplate, ApiScope, BundleApiProvider, BundleApiReply, BundleApiRequest,
    CatalogScope, CoreError, EventBus, RuntimeCatalogRefresh, RuntimePermissionMode,
    RuntimeRegistry, RuntimeSource, RuntimeSourceId, ScopeOverlay, SessionEngine, SourceApi,
};
use hya_proto::FinishReason;
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V2Grpc, build, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;

use crate::harness::{
    GatedProvider, assert_err, assert_error, create_session_id, create_session_with, del, enc,
    engine_on, get_json, grpc_channel, grpc_code, grpc_json, id_str, ok_del, ok_get, ok_patch,
    ok_post, patch, post, send, send_raw, serve_server, state_of, state_with_provider,
};
use crate::support::{self, AgentFixture};

const SCOPED_BUNDLE: &str = "hya/server-tests-scoped";
const HEADER_REFUSAL: &str =
    "the x-hya-directory header is no longer supported; set the request's directory field";

async fn project_state(gate: Arc<Semaphore>) -> AppState {
    state_with_provider(GatedProvider::new(gate))
        .await
        .with_scratch_root(support::tempdir("project-scratch"))
}

async fn create_project(app: &axum::Router, name: &str, roots: &[&str]) -> Value {
    ok_post(app, "/v2/projects", json!({ "name": name, "roots": roots })).await
}

async fn list_projects(app: &axum::Router) -> Vec<Value> {
    ok_get(app, "/v2/projects").await["projects"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn project_row(app: &axum::Router, id: &str) -> Value {
    list_projects(app)
        .await
        .into_iter()
        .find(|r| r["id"] == json!(id))
        .expect("listed project")
}

async fn listed_session_ids(app: &axum::Router, query: &str) -> Vec<String> {
    ok_get(app, &format!("/v2/sessions{query}")).await["sessions"]
        .as_array()
        .map_or_else(Vec::new, |rows| {
            rows.iter().map(|r| id_str(&r["id"])).collect()
        })
}

#[tokio::test]
async fn project_crud_and_error_codes_over_http_and_grpc() {
    let state = project_state(Arc::new(Semaphore::new(0))).await;
    let app = router(state.clone());

    let created = create_project(&app, " app ", &["/work/app", "/work/app-docs/"]).await;
    let id = id_str(&created["id"]);
    assert!(id.starts_with("prj_"));
    assert_eq!(
        (&created["name"], &created["roots"]),
        (&json!("app"), &json!(["/work/app", "/work/app-docs"]))
    );
    assert!(created["createdAt"].is_string() && created["updatedAt"].is_string());
    assert!(created.get("sessionCount").is_none() && created.get("busy").is_none());

    assert_eq!(ok_get(&app, &format!("/v2/projects/{id}")).await, created);
    assert_eq!(list_projects(&app).await, vec![created.clone()]);
    assert_eq!(
        ok_get(&app, &format!("/v2/projects/{id}/directories")).await["directories"],
        json!(["/work/app", "/work/app-docs"])
    );

    let renamed = ok_patch(
        &app,
        &format!("/v2/projects/{id}"),
        json!({"name": "renamed"}),
    )
    .await;
    assert_eq!(
        (&renamed["name"], &renamed["roots"]),
        (&json!("renamed"), &created["roots"])
    );
    let both = ok_patch(
        &app,
        &format!("/v2/projects/{id}"),
        json!({"name": "both", "roots": ["/x", "/y"]}),
    )
    .await;
    assert_eq!(
        (&both["name"], &both["roots"]),
        (&json!("both"), &json!(["/x", "/y"]))
    );

    // An invalid half applies nothing; delete yields 404.
    assert_err(
        patch(
            &app,
            &format!("/v2/projects/{id}"),
            json!({"name": "lost", "roots": ["relative"]}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    assert_eq!(
        ok_get(&app, &format!("/v2/projects/{id}")).await["name"],
        json!("both")
    );
    let _ = ok_del(&app, &format!("/v2/projects/{id}")).await;
    assert_err(
        get_json(&app, &format!("/v2/projects/{id}")).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    // Validation & not-found error tables.
    for body in [
        json!({"name": "", "roots": ["/a"]}),
        json!({"name": "  ", "roots": ["/a"]}),
        json!({"name": "x", "roots": []}),
        json!({"name": "x"}),
        json!({"name": "x", "roots": ["relative/dir"]}),
        json!({"name": "x", "roots": ["/a/../b"]}),
    ] {
        assert_err(
            post(&app, "/v2/projects", body).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }
    let missing = hya_proto::ProjectId::new().to_string();
    for (method, uri, body) in [
        (Method::GET, format!("/v2/projects/{missing}"), Value::Null),
        (
            Method::PATCH,
            format!("/v2/projects/{missing}"),
            json!({"name": "x"}),
        ),
        (
            Method::DELETE,
            format!("/v2/projects/{missing}"),
            Value::Null,
        ),
        (
            Method::GET,
            format!("/v2/projects/{missing}/directories"),
            Value::Null,
        ),
        (
            Method::POST,
            format!("/v2/projects/{missing}/init-git"),
            json!({}),
        ),
    ] {
        assert_err(
            send(&app, method, &uri, body).await,
            StatusCode::NOT_FOUND,
            "not_found",
        );
    }
    assert_err(
        get_json(&app, "/v2/projects/not-an-id").await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );

    // Delete refused while a live root session uses the Project; archiving lifts refusal.
    let busy_id = id_str(
        &create_project(&app, "busy", &[&std::env::temp_dir().to_string_lossy()]).await["id"],
    );
    let sess = create_session_id(&app, json!({"projectId": busy_id})).await;
    assert_err(
        del(&app, &format!("/v2/projects/{busy_id}")).await,
        StatusCode::CONFLICT,
        "failed_precondition",
    );
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{sess}"),
        json!({"archived": true}),
    )
    .await;
    let _ = ok_del(&app, &format!("/v2/projects/{busy_id}")).await;

    // gRPC canonical codes & CRUD parity.
    let mut projects = pb::project_client::ProjectClient::new(grpc_channel(state).await);
    assert_eq!(
        grpc_code(
            projects
                .create_project(pb::CreateProjectRequest {
                    name: "x".into(),
                    roots: vec!["relative".into()]
                })
                .await
        ),
        tonic::Code::InvalidArgument
    );
    assert_eq!(
        grpc_code(
            projects
                .get_project(pb::GetProjectRequest {
                    project: hya_proto::ProjectId::new().to_string()
                })
                .await
        ),
        tonic::Code::NotFound
    );
    let grpc_proj = projects
        .create_project(pb::CreateProjectRequest {
            name: "grpc".into(),
            roots: vec![std::env::temp_dir().to_string_lossy().into_owned()],
        })
        .await
        .unwrap()
        .into_inner();
    let _ = create_session_id(&app, json!({"projectId": grpc_proj.id})).await;
    assert_eq!(
        grpc_code(
            projects
                .delete_project(pb::DeleteProjectRequest {
                    project: grpc_proj.id.clone()
                })
                .await
        ),
        tonic::Code::FailedPrecondition
    );
    let updated = projects
        .update_project(pb::UpdateProjectRequest {
            project: grpc_proj.id.clone(),
            name: Some("renamed".into()),
            roots: vec!["/r1".into(), "/r2".into()],
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.session_count, 1);
    let resolved = projects
        .resolve_project(pb::ResolveProjectRequest {
            path: "/r2/sub".into(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resolved.project.map(|p| p.id), Some(grpc_proj.id));
}

#[tokio::test]
async fn project_resolve_ensure_current_and_init_git() {
    let app = router(project_state(Arc::new(Semaphore::new(0))).await);
    let outer = create_project(&app, "outer", &["/w"]).await;
    let inner = create_project(&app, "inner", &["/w/inner", "/other"]).await;

    assert_eq!(
        ok_get(
            &app,
            &format!("/v2/projects/resolve?path={}", enc("/w/inner/src"))
        )
        .await["project"]["id"],
        inner["id"]
    );
    assert_eq!(
        ok_get(
            &app,
            &format!("/v2/projects/resolve?path={}", enc("/w/innerx"))
        )
        .await["project"]["id"],
        outer["id"]
    );
    assert_eq!(
        ok_get(
            &app,
            &format!("/v2/projects/resolve?path={}", enc("/other"))
        )
        .await["project"]["id"],
        inner["id"]
    );
    assert!(
        ok_get(
            &app,
            &format!("/v2/projects/resolve?path={}", enc("/nowhere"))
        )
        .await
        .get("project")
        .is_none()
    );
    assert_eq!(list_projects(&app).await.len(), 2, "resolve never creates");
    assert_err(
        get_json(&app, "/v2/projects/resolve?path=relative").await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    assert_err(
        get_json(&app, "/v2/projects/resolve").await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );

    // Ensure creates or reuses containing project, handles `/`, rejects bad paths.
    let first = ok_post(&app, "/v2/projects/ensure", json!({"path": "/work/repo/"})).await;
    assert_eq!(
        (
            &first["created"],
            &first["project"]["name"],
            &first["project"]["roots"]
        ),
        (&json!(true), &json!("repo"), &json!(["/work/repo"]))
    );
    let again = ok_post(
        &app,
        "/v2/projects/ensure",
        json!({"path": "/work/repo/crates/a"}),
    )
    .await;
    assert!(again.get("created").is_none() && again["project"]["id"] == first["project"]["id"]);
    let root = ok_post(&app, "/v2/projects/ensure", json!({"path": "/"})).await;
    assert_eq!(root["project"]["name"], json!("/"));
    let _ = ok_del(
        &app,
        &format!("/v2/projects/{}", root["project"]["id"].as_str().unwrap()),
    )
    .await;
    for path in ["", "relative", "/a/../b"] {
        assert_err(
            post(&app, "/v2/projects/ensure", json!({"path": path})).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }

    // Current project resolves directory scope; init-git runs in primary root.
    let scoped = create_project(&app, "scoped", &["/scoped"]).await;
    assert_eq!(
        ok_get(&app, "/v2/projects/current?directory=%2Fscoped%2Fsub").await["id"],
        scoped["id"]
    );
    assert_err(
        get_json(&app, "/v2/projects/current?directory=%2Funscoped").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_err(
        get_json(&app, "/v2/projects/current").await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );

    if support::git_available() {
        let dir = support::tempdir("project-init-git");
        let id = id_str(&create_project(&app, "git", &[&dir.to_string_lossy()]).await["id"]);
        assert_eq!(
            ok_post(&app, &format!("/v2/projects/{id}/init-git"), json!({})).await["initialized"],
            json!(true)
        );
        assert!(dir.join(".git").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[tokio::test]
async fn session_count_and_busy_follow_the_projects_sessions() {
    let gate = Arc::new(Semaphore::new(0));
    let app = router(project_state(Arc::clone(&gate)).await);
    let id = id_str(
        &create_project(&app, "live", &[&std::env::temp_dir().to_string_lossy()]).await["id"],
    );
    let other_id = id_str(&create_project(&app, "idle", &["/idle"]).await["id"]);

    let session = create_session_id(&app, json!({"projectId": id})).await;
    let row = project_row(&app, &id).await;
    assert_eq!(row["sessionCount"], json!(1));
    assert!(row.get("busy").is_none());

    let turn = id_str(
        &ok_post(
            &app,
            &format!("/v2/sessions/{session}/turns"),
            json!({"prompt": {"text": "work"}}),
        )
        .await["turn"]["id"],
    );
    assert_eq!(project_row(&app, &id).await["busy"], json!(true));
    assert_eq!(
        ok_get(&app, &format!("/v2/projects/{id}")).await["busy"],
        json!(true)
    );
    assert!(project_row(&app, &other_id).await.get("busy").is_none());

    // Archived session does not mark its Project busy.
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{session}"),
        json!({"archived": true}),
    )
    .await;
    assert!(project_row(&app, &id).await.get("busy").is_none());
    let _ = ok_patch(
        &app,
        &format!("/v2/sessions/{session}"),
        json!({"archived": false}),
    )
    .await;
    assert_eq!(project_row(&app, &id).await["busy"], json!(true));

    gate.add_permits(1);
    let _ = ok_post(
        &app,
        &format!("/v2/sessions/{session}/turns/{turn}/wait?timeoutMs=10000"),
        Value::Null,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while project_row(&app, &id).await.get("busy").is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("busy clears");
    let _ = ok_del(&app, &format!("/v2/sessions/{session}")).await;
    assert!(project_row(&app, &id).await.get("sessionCount").is_none());
}

#[tokio::test]
async fn temporary_session_scratch_dir_and_validation() {
    let scratch = support::tempdir("temp-session-scratch");
    let app = router(
        state_with_provider(FakeProvider::scripted(vec![]))
            .await
            .with_scratch_root(scratch.clone()),
    );
    let session = create_session_with(&app, json!({"kind": "SESSION_KIND_TEMPORARY"}))
        .await
        .1["session"]
        .clone();
    let id = session["id"].as_str().unwrap();
    let workdir = PathBuf::from(session["workdir"].as_str().unwrap());
    assert_eq!(workdir, scratch.join(id));
    assert!(workdir.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&workdir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(session["kind"], json!("SESSION_KIND_TEMPORARY"));
    assert!(session.get("projectId").is_none() && list_projects(&app).await.is_empty());

    std::fs::write(workdir.join("notes.txt"), "keep me").unwrap();
    let _ = ok_del(&app, &format!("/v2/sessions/{id}")).await;
    assert_eq!(
        std::fs::read_to_string(workdir.join("notes.txt")).unwrap(),
        "keep me"
    );

    let pid = id_str(&create_project(&app, "p", &["/p"]).await["id"]);
    for bad in [
        json!({"kind": "SESSION_KIND_TEMPORARY", "projectId": pid}),
        json!({"kind": "SESSION_KIND_TEMPORARY", "workdir": "/p"}),
    ] {
        assert_err(
            create_session_with(&app, bad).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

#[tokio::test]
async fn project_session_placement_inheritance_filtering_and_validation() {
    let app = router(project_state(Arc::new(Semaphore::new(0))).await);
    let id = id_str(&create_project(&app, "multi", &["/multi/a", "/multi/b"]).await["id"]);

    let s1 = create_session_with(
        &app,
        json!({"projectId": id, "kind": "SESSION_KIND_PROJECT"}),
    )
    .await
    .1["session"]
        .clone();
    assert_eq!(
        (&s1["workdir"], &s1["projectId"], &s1["kind"]),
        (
            &json!("/multi/a"),
            &json!(id),
            &json!("SESSION_KIND_PROJECT")
        )
    );
    let s2 = create_session_with(&app, json!({"projectId": id, "workdir": "/multi/b/src/"}))
        .await
        .1["session"]
        .clone();
    assert_eq!(s2["workdir"], json!("/multi/b/src"));
    for bad in [
        "/elsewhere",
        "/multi/bc",
        "/multi",
        "relative",
        "/multi/a/../c",
    ] {
        assert_err(
            create_session_with(&app, json!({"projectId": id, "workdir": bad})).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }

    assert_err(
        create_session_with(
            &app,
            json!({"projectId": hya_proto::ProjectId::new().to_string()}),
        )
        .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    for bad in [
        json!({"projectId": "bogus"}),
        json!({}),
        json!({"kind": "SESSION_KIND_PROJECT"}),
        json!({"workdir": "relative"}),
    ] {
        assert_err(
            create_session_with(&app, bad).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }

    // Workdir without project ensures one and reuses it for subdirs.
    let repo_proj = id_str(
        &create_session_with(&app, json!({"workdir": "/repo"}))
            .await
            .1["session"]["projectId"],
    );
    let nested_s = create_session_with(&app, json!({"workdir": "/repo/crates/x"}))
        .await
        .1["session"]
        .clone();
    assert_eq!(
        (&nested_s["projectId"], &nested_s["workdir"]),
        (&json!(repo_proj), &json!("/repo/crates/x"))
    );

    // Child sessions inherit project/kind/workdir and refuse explicit projectId/kind.
    let root_id = s2["id"].as_str().unwrap();
    let child = create_session_with(&app, json!({"parent": root_id}))
        .await
        .1["session"]
        .clone();
    assert_eq!(
        (&child["projectId"], &child["kind"], &child["workdir"]),
        (
            &json!(id),
            &json!("SESSION_KIND_PROJECT"),
            &json!("/multi/b/src")
        )
    );
    for bad in [
        json!({"parent": root_id, "projectId": id}),
        json!({"parent": root_id, "kind": "SESSION_KIND_PROJECT"}),
        json!({"parent": root_id, "kind": "SESSION_KIND_TEMPORARY"}),
        json!({"parent": "not-a-session"}),
    ] {
        assert_err(
            create_session_with(&app, bad).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
    }

    // ListSessions projectId filter + validation.
    let temp = create_session_id(&app, json!({"kind": "SESSION_KIND_TEMPORARY"})).await;
    assert_eq!(
        listed_session_ids(&app, &format!("?projectId={id}"))
            .await
            .len(),
        3
    );
    assert_eq!(
        listed_session_ids(&app, &format!("?projectId={repo_proj}"))
            .await
            .len(),
        2
    );
    assert!(listed_session_ids(&app, "").await.contains(&temp));
    assert!(
        listed_session_ids(&app, &format!("?projectId={}", hya_proto::ProjectId::new()))
            .await
            .is_empty()
    );
    assert_err(
        get_json(&app, "/v2/sessions?projectId=bogus").await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    assert_eq!(
        ok_get(
            &app,
            &format!("/v2/sessions/{}", nested_s["id"].as_str().unwrap())
        )
        .await["projectId"],
        json!(repo_proj)
    );
}

async fn nonexistent_cwd_state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (engine, _) = engine_on(
        SessionStore::connect_memory().await.unwrap(),
        provider,
        PermissionRules::default(),
    );
    state_of(engine, "/nonexistent/hya-process-agent-workdir")
}

async fn send_scoped(
    app: &axum::Router,
    method: Method,
    uri: &str,
    directory: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let (uri, body) = match directory {
        None => (uri.to_owned(), body),
        Some(dir) if method == Method::GET || method == Method::DELETE => {
            let sep = if uri.contains('?') { '&' } else { '?' };
            (format!("{uri}{sep}directory={}", enc(dir)), body)
        }
        Some(dir) => {
            let mut body = if body.is_null() { json!({}) } else { body };
            body["directory"] = json!(dir);
            (uri.to_owned(), body)
        }
    };
    send(app, method, &uri, body).await
}

#[tokio::test]
async fn scoped_rpcs_require_absolute_directory_field_and_honor_it() {
    let app = router(nonexistent_cwd_state().await);
    let dir = std::fs::canonicalize(support::tempdir("scope-table")).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    let calls = [
        (Method::GET, "/v2/fs/read?path=notes.txt", Value::Null),
        (Method::GET, "/v2/fs/list", Value::Null),
        (Method::GET, "/v2/fs/find?pattern=*", Value::Null),
        (Method::GET, "/v2/fs/search?query=x", Value::Null),
        (Method::GET, "/v2/fs/symbols?query=x", Value::Null),
        (Method::GET, "/v2/vcs", Value::Null),
        (Method::GET, "/v2/vcs/diff", Value::Null),
        (Method::POST, "/v2/vcs/apply", json!({"patch": ""})),
        (Method::GET, "/v2/worktrees", Value::Null),
        (Method::POST, "/v2/worktrees", json!({})),
        (Method::DELETE, "/v2/worktrees/some-id", Value::Null),
        (Method::POST, "/v2/worktrees/some-id/reset", Value::Null),
        (Method::POST, "/v2/pty", json!({"shell": "/bin/sh"})),
        (Method::GET, "/v2/projects/current", Value::Null),
    ];
    for (method, uri, body) in calls {
        let (s, reply) = send_scoped(&app, method.clone(), uri, None, body.clone()).await;
        assert_error(s, &reply, StatusCode::BAD_REQUEST, "invalid_argument");
        assert!(
            reply.to_string().contains("directory scope"),
            "{method} {uri}: {reply}"
        );
        assert_err(
            send_scoped(&app, method.clone(), uri, Some("."), body.clone()).await,
            StatusCode::BAD_REQUEST,
            "invalid_argument",
        );
        let (_, with) = send_scoped(&app, method.clone(), uri, Some(&scope), body).await;
        assert!(
            !with.to_string().contains("directory scope"),
            "{method} {uri}: {with}"
        );
        if uri == "/v2/pty" {
            assert_eq!(with["cwd"], json!(scope));
            let _ = del(&app, &format!("/v2/pty/{}", with["id"].as_str().unwrap())).await;
        }
    }

    assert_err(
        send_scoped(
            &app,
            Method::GET,
            "/v2/agents",
            Some("relative/dir"),
            Value::Null,
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_argument",
    );
    let pty = ok_post(&app, "/v2/pty", json!({"shell": "/bin/sh", "cwd": scope})).await;
    let _ = del(&app, &format!("/v2/pty/{}", pty["id"].as_str().unwrap())).await;
    assert!(
        ok_get(&app, "/v2/location").await["directory"]
            .as_str()
            .is_none_or(str::is_empty)
    );
    assert_eq!(
        send_scoped(&app, Method::GET, "/v2/location", Some(&scope), Value::Null)
            .await
            .1["directory"],
        json!(scope)
    );

    let source = create_session_id(&app, json!({"workdir": scope})).await;
    assert_eq!(
        ok_post(&app, &format!("/v2/sessions/{source}/fork"), json!({})).await["session"]["workdir"],
        json!(scope)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn grpc_scope_field_and_removed_x_hya_directory_header_guard() {
    use hya_api::v2::{
        files_server::Files as _, process_server::Process as _, pty_server::Pty as _,
        worktrees_server::Worktrees as _,
    };

    let state = nonexistent_cwd_state().await;
    let app = router(state.clone());
    let dir = std::fs::canonicalize(support::tempdir("scope-grpc")).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello").unwrap();
    let scope = dir.to_string_lossy().into_owned();

    // HTTP rejects x-hya-directory on every route (with CORS headers readable).
    for (method, uri, body) in [
        (Method::GET, "/v2/health".into(), Value::Null),
        (Method::GET, "/v2/agents".into(), Value::Null),
        (
            Method::GET,
            format!("/v2/fs/list?directory={}", enc(&scope)),
            Value::Null,
        ),
        (
            Method::POST,
            "/v2/pty".into(),
            json!({"shell": "/bin/sh", "directory": scope}),
        ),
        (Method::GET, "/v2/events/stream".into(), Value::Null),
    ] {
        let (status, reply) =
            send_raw(&app, method, &uri, &[("x-hya-directory", &scope)], body).await;
        assert_error(status, &reply, StatusCode::BAD_REQUEST, "invalid_argument");
        assert_eq!(reply["error"]["message"], json!(HEADER_REFUSAL));
    }
    let cors = app
        .clone()
        .oneshot(
            Request::get("/v2/agents")
                .header("origin", "http://localhost:3000")
                .header("x-hya-directory", "/anywhere")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cors.status(), StatusCode::BAD_REQUEST);
    assert!(cors.headers().contains_key("access-control-allow-origin"));

    // gRPC requires directory field and honors it across files/location/worktrees/pty.
    let grpc = V2Grpc::new(state.clone());
    let err = grpc
        .read_file(tonic::Request::new(pb::ReadFileRequest {
            path: "notes.txt".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert!(
        err.code() == tonic::Code::InvalidArgument && err.message().contains("directory scope")
    );
    let read_req = || pb::ReadFileRequest {
        directory: scope.clone(),
        path: "notes.txt".into(),
        ..Default::default()
    };
    assert_eq!(
        grpc.read_file(tonic::Request::new(read_req()))
            .await
            .unwrap()
            .into_inner()
            .content,
        b"hello".to_vec()
    );
    assert_eq!(
        grpc.get_location(tonic::Request::new(pb::GetLocationRequest {
            directory: scope.clone()
        }))
        .await
        .unwrap()
        .into_inner()
        .directory,
        scope
    );
    assert!(
        !grpc
            .delete_worktree(tonic::Request::new(pb::DeleteWorktreeRequest {
                worktree: "missing".into(),
                directory: scope.clone(),
                ..Default::default()
            }))
            .await
            .unwrap_err()
            .message()
            .contains("directory scope")
    );
    let pty = grpc
        .create_pty(tonic::Request::new(pb::CreatePtyRequest {
            directory: scope.clone(),
            shell: "/bin/sh".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(pty.cwd, scope);
    let _ = grpc
        .delete_pty(tonic::Request::new(pb::DeletePtyRequest { id: pty.id }))
        .await;

    // gRPC metadata x-hya-directory refused in-process and on served listener (unary + stream).
    fn with_meta<T>(req: T) -> tonic::Request<T> {
        let mut r = tonic::Request::new(req);
        r.metadata_mut()
            .insert("x-hya-directory", "/anywhere".parse().unwrap());
        r
    }
    let check_refused = |err: tonic::Status| {
        assert_eq!(
            (err.code(), err.message()),
            (tonic::Code::InvalidArgument, HEADER_REFUSAL)
        )
    };
    check_refused(grpc.read_file(with_meta(read_req())).await.unwrap_err());
    let (addr, _task) = serve_server(&build(state), std::future::pending()).await;
    let ch = tonic::transport::Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    check_refused(
        pb::files_client::FilesClient::new(ch.clone())
            .read_file(with_meta(read_req()))
            .await
            .unwrap_err(),
    );
    check_refused(
        pb::events_client::EventsClient::new(ch)
            .stream_global_events(with_meta(pb::StreamGlobalEventsRequest::default()))
            .await
            .unwrap_err(),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn slow_vcs_snapshot_does_not_block_health() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    if !support::git_available() {
        return;
    }
    let app = router(nonexistent_cwd_state().await);
    let repo = support::init_git_repo("vcs-nonblocking");
    let (marker, release, hook) = (
        repo.join(".git/monitor-entered"),
        repo.join(".git/monitor-release"),
        repo.join(".git/slow-monitor"),
    );
    std::fs::write(&hook, "#!/bin/sh\n: > .git/monitor-entered\nwhile [ ! -f .git/monitor-release ]; do sleep 0.01; done\nprintf 'token\\0/\\0'\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    support::git(&repo, &["config", "core.fsmonitor", hook.to_str().unwrap()]);
    std::fs::write(repo.join("README.md"), "changed\n").unwrap();

    let expired = Arc::new(AtomicBool::new(false));
    let (wd_exp, wd_rel) = (expired.clone(), release.clone());
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !wd_rel.exists() {
            if Instant::now() >= deadline {
                wd_exp.store(true, Ordering::SeqCst);
                let _ = std::fs::write(&wd_rel, "r");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let vcs_app = app.clone();
    let scope = repo.to_string_lossy().into_owned();
    let vcs = tokio::spawn(async move {
        send_scoped(&vcs_app, Method::GET, "/v2/vcs", Some(&scope), Value::Null).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let (status, _) = get_json(&app, "/v2/health").await;
    std::fs::write(&release, "r").unwrap();
    watchdog.join().unwrap();
    assert_eq!(status, StatusCode::OK);
    assert!(!expired.load(Ordering::SeqCst));
    let (s, reply) = vcs.await.unwrap();
    assert_eq!(
        (s, &reply["dirty"], &reply["files"][0]["path"]),
        (StatusCode::OK, &json!(1), &json!("README.md"))
    );
    let _ = std::fs::remove_dir_all(&repo);
}

// ---- Per-Project Catalog Overlays (ADR-0027) ----

struct Answer;
#[async_trait]
impl BundleApiProvider for Answer {
    async fn request(&self, _: BundleApiRequest) -> Result<BundleApiReply, String> {
        Ok(BundleApiReply {
            status: 200,
            body: json!({}),
        })
    }
}

struct ProjectBundleRefresh;
#[async_trait]
impl RuntimeCatalogRefresh for ProjectBundleRefresh {
    async fn refresh_if_changed(&self, _: &RuntimeRegistry) -> Result<bool, CoreError> {
        Ok(false)
    }
    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        if !matches!(scope, CatalogScope::Project { .. })
            || runtime.scope_overlay(&scope.key()).is_some()
        {
            return Ok(false);
        }
        let mut agents = support::test_agents();
        agents.push(AgentFixture::main("scoped"));
        let mut overlay = ScopeOverlay::new(support::agent_catalog_with_tui(&agents));
        overlay.bundle_sources = vec![
            RuntimeSource::new(
                RuntimeSourceId::bundle(SCOPED_BUNDLE),
                [7; 32],
                Arc::new(()),
                Vec::new(),
            )
            .with_apis(
                vec![SourceApi {
                    id: "usage".into(),
                    method: ApiMethod::Get,
                    scope: ApiScope::Session,
                    path: ApiPathTemplate::parse("/usage").unwrap(),
                    description: String::new(),
                    request_schema: None,
                    response_schema: None,
                }],
                Arc::new(Answer),
            )
            .with_permission_modes(vec![RuntimePermissionMode {
                id: "careful".into(),
                title: "Careful".into(),
                description: String::new(),
            }]),
        ];
        runtime
            .publish_scope(scope.key(), overlay)
            .map_err(CoreError::runtime_refresh)?;
        Ok(true)
    }
}

async fn catalog_state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".into()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(provider))),
            support::test_runtime(Arc::new(hya_tool_loader::builtins())),
            perm,
            EventBus::default(),
        )
        .with_catalog_refresh(Arc::new(ProjectBundleRefresh)),
    );
    state_of(engine, "/nonexistent/hya-process-agent-workdir")
}

struct CatalogFixture {
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
    c: PathBuf,
}
impl Drop for CatalogFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_skill(root: &Path, name: &str, desc: &str) {
    let dir = root.join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n{name} body\n"),
    )
    .unwrap();
}

fn write_command(root: &Path, name: &str, body: &str) {
    let dir = root.join(".hya/commands");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
}

fn catalog_fixture(label: &str) -> CatalogFixture {
    let root = std::fs::canonicalize(support::tempdir(&format!("cat-{label}"))).unwrap();
    let (a, b, c) = (root.join("a"), root.join("b"), root.join("c"));
    for d in [&a, &b, &c] {
        std::fs::create_dir_all(d).unwrap();
    }
    for (d, n, desc) in [
        (&a, "alpha", "alpha skill"),
        (&a, "shared", "from a"),
        (&b, "beta", "beta skill"),
        (&b, "shared", "from b"),
        (&c, "gamma", "gamma skill"),
    ] {
        write_skill(d, n, desc);
    }
    for (d, n, body) in [
        (&a, "from-a", "a command"),
        (&a, "dup", "dup from a"),
        (&b, "from-b", "b command"),
        (&b, "dup", "dup from b"),
        (&c, "from-c", "c command"),
    ] {
        write_command(d, n, body);
    }
    CatalogFixture { root, a, b, c }
}

fn field_of(reply: &Value, key: &str, name: &str, field: &str) -> Option<String> {
    reply[key]
        .as_array()?
        .iter()
        .find(|r| r["name"] == name)?
        .get(field)?
        .as_str()
        .map(str::to_owned)
}

#[tokio::test]
async fn catalog_listings_and_command_turns_follow_project_directory_and_global_scopes() {
    let app = router(catalog_state().await);
    let f = catalog_fixture("tiers");
    create_project(
        &app,
        "demo",
        &[&f.a.to_string_lossy(), &f.b.to_string_lossy()],
    )
    .await;
    let inside = f.a.join("nested");
    std::fs::create_dir_all(&inside).unwrap();
    let (in_q, b_q, c_q) = (
        enc(&inside.to_string_lossy()),
        enc(&f.b.to_string_lossy()),
        enc(&f.c.to_string_lossy()),
    );

    // Two-root Project scope: both roots listed, requested dir wins collisions, bundle overlay active.
    let skills = ok_get(&app, &format!("/v2/skills?directory={in_q}")).await;
    assert_eq!(
        field_of(&skills, "skills", "shared", "description").as_deref(),
        Some("from a")
    );
    assert!(
        field_of(&skills, "skills", "beta", "name").is_some()
            && field_of(&skills, "skills", "gamma", "name").is_none()
    );
    assert_eq!(
        field_of(
            &ok_get(&app, &format!("/v2/skills?directory={b_q}")).await,
            "skills",
            "shared",
            "description"
        )
        .as_deref(),
        Some("from b")
    );

    let cmds = ok_get(&app, &format!("/v2/commands?directory={in_q}")).await;
    assert_eq!(
        (
            field_of(&cmds, "commands", "dup", "template").as_deref(),
            field_of(&cmds, "commands", "from-b", "template").as_deref()
        ),
        (Some("dup from a"), Some("b command"))
    );
    let boot = ok_get(&app, &format!("/v2/bootstrap?directory={in_q}")).await;
    assert!(
        field_of(&boot, "agents", "scoped", "name").is_some()
            && field_of(&boot, "skills", "beta", "name").is_some()
            && field_of(&boot, "commands", "from-b", "name").is_some()
    );

    // Unresolved directory: own inert tiers only, no Project bundle.
    let c_skills = ok_get(&app, &format!("/v2/skills?directory={c_q}")).await;
    assert!(
        field_of(&c_skills, "skills", "gamma", "name").is_some()
            && field_of(&c_skills, "skills", "alpha", "name").is_none()
    );
    assert!(
        field_of(
            &ok_get(&app, &format!("/v2/agents?directory={c_q}")).await,
            "agents",
            "scoped",
            "name"
        )
        .is_none()
    );

    // Global view answers across all optional-scope catalog endpoints without project items.
    for uri in [
        "/v2/agents",
        "/v2/commands",
        "/v2/skills",
        "/v2/bootstrap",
        "/v2/models",
        "/v2/providers",
        "/v2/tools",
        "/v2/permissions/rules",
        "/v2/config",
    ] {
        let _ = ok_get(&app, uri).await;
    }
    assert!(
        field_of(
            &ok_get(&app, "/v2/agents").await,
            "agents",
            "scoped",
            "name"
        )
        .is_none()
    );
    assert!(field_of(&ok_get(&app, "/v2/skills").await, "skills", "alpha", "name").is_none());
    assert!(
        field_of(
            &ok_get(&app, "/v2/commands").await,
            "commands",
            "from-a",
            "name"
        )
        .is_none()
    );

    // Session command turn expands across every root of the session's Project.
    write_command(&inside, "dup", "nested command");
    let session = create_session_id(&app, json!({"workdir": inside.to_string_lossy()})).await;
    let s_cmds = ok_get(&app, &format!("/v2/commands?session={session}")).await;
    assert_eq!(
        (
            field_of(&s_cmds, "commands", "dup", "template").as_deref(),
            field_of(&s_cmds, "commands", "from-b", "template").as_deref()
        ),
        (Some("nested command"), Some("b command"))
    );
    let _ = ok_post(
        &app,
        &format!("/v2/sessions/{session}/turns"),
        json!({"command": {"command": "from-b", "arguments": ""}}),
    )
    .await;
    let mut text = String::new();
    for _ in 0..200 {
        text = ok_get(&app, &format!("/v2/sessions/{session}/messages"))
            .await
            .to_string();
        if text.contains("b command") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(text.contains("b command"), "{text}");
}

#[tokio::test]
async fn project_mutations_and_provider_updates_emit_catalog_updated() {
    use pb::events_server::Events as _;

    let state = catalog_state().await;
    let app = router(state.clone());
    let f = catalog_fixture("notices");
    let id = id_str(&create_project(&app, "demo", &[&f.a.to_string_lossy()]).await["id"]);
    let uri = format!("/v2/skills?directory={}", enc(&f.a.to_string_lossy()));
    assert!(field_of(&ok_get(&app, &uri).await, "skills", "gamma", "name").is_none());

    let mut stream = V2Grpc::new(state.clone())
        .stream_global_events(tonic::Request::new(pb::StreamGlobalEventsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    async fn next_notice(
        stream: &mut <V2Grpc as pb::events_server::Events>::StreamGlobalEventsStream,
    ) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(frame) = stream.next().await {
                if let Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                    payload: Some(pb::stream_event::Payload::CatalogUpdated(n)),
                    ..
                })) = frame.unwrap().frame
                {
                    return n.project_id;
                }
            }
            panic!("stream ended")
        })
        .await
        .unwrap()
    }

    let _ = ok_patch(
        &app,
        &format!("/v2/projects/{id}"),
        json!({"roots": [f.a.to_string_lossy(), f.c.to_string_lossy()]}),
    )
    .await;
    assert_eq!(next_notice(&mut stream).await, id);
    assert!(field_of(&ok_get(&app, &uri).await, "skills", "gamma", "name").is_some());

    // Name-only / identical-roots patch emits nothing; sentinel global notice arrives next.
    let _ = ok_patch(
        &app,
        &format!("/v2/projects/{id}"),
        json!({"name": "renamed", "roots": [f.a.to_string_lossy(), f.c.to_string_lossy()]}),
    )
    .await;
    state.notify_catalog_updated();
    assert_eq!(next_notice(&mut stream).await, "");

    // Deleting project emits `catalogUpdated { projectId }` and drops project overlay.
    let _ = ok_del(&app, &format!("/v2/projects/{id}")).await;
    assert_eq!(next_notice(&mut stream).await, id);
    assert!(
        field_of(
            &ok_get(
                &app,
                &format!("/v2/agents?directory={}", enc(&f.a.to_string_lossy()))
            )
            .await,
            "agents",
            "scoped",
            "name"
        )
        .is_none()
    );
    assert_eq!(
        serde_json::to_value(pb::CatalogUpdated {
            project_id: "p".into()
        })
        .unwrap(),
        json!({"projectId": "p"})
    );
}

#[tokio::test]
async fn scoped_tui_extensions_permission_modes_and_bundle_apis() {
    use pb::{bundle_api_server::BundleApi as _, catalog_server::Catalog as _};

    let state = catalog_state().await;
    let app = router(state.clone());
    let grpc = V2Grpc::new(state);
    let f = catalog_fixture("scoped-exts");
    create_project(
        &app,
        "demo",
        &[&f.a.to_string_lossy(), &f.b.to_string_lossy()],
    )
    .await;
    let project_session = create_session_id(&app, json!({"workdir": f.a.to_string_lossy()})).await;
    let plain_session = create_session_id(&app, json!({"kind": "SESSION_KIND_TEMPORARY"})).await;
    let (a_q, b_q, c_q) = (
        enc(&f.a.to_string_lossy()),
        enc(&f.b.to_string_lossy()),
        enc(&f.c.to_string_lossy()),
    );

    // TUI extensions: text files included, binary excluded, `known` digest cache hit/miss/validation.
    let find_scoped_ext = |resp: &Value| {
        resp["extensions"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["bundleId"] == SCOPED_BUNDLE))
            .cloned()
    };
    assert!(
        find_scoped_ext(&ok_get(&app, &format!("/v2/tui-extensions?directory={c_q}")).await)
            .is_none()
    );
    let ext = find_scoped_ext(&ok_get(&app, &format!("/v2/tui-extensions?directory={a_q}")).await)
        .expect("scoped tui extension present in project scope");
    assert_eq!(ext["entry"], "extensions/main.ts");
    assert_eq!(
        ext["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["extensions/main.ts", "extensions/module.ts"]
    );
    let digest = ext["preparedDigest"].as_str().unwrap();
    let cached = ok_get(
        &app,
        &format!("/v2/tui-extensions?directory={a_q}&known={digest}"),
    )
    .await;
    let cached_ext = find_scoped_ext(&cached).expect("cached scoped tui extension");
    assert_eq!(cached_ext["cached"], true);
    assert!(cached_ext["files"].as_array().is_none_or(Vec::is_empty));
    assert_eq!(
        get_json(
            &app,
            &format!("/v2/tui-extensions?directory={a_q}&known=not-a-digest")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Permission modes & bundle APIs follow session > directory > global, matching over HTTP & gRPC.
    let careful = format!("{SCOPED_BUNDLE}/careful");
    let has_careful = |v: &Value| {
        v["modes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == careful)
    };
    assert!(!has_careful(&ok_get(&app, "/v2/permission-modes").await));
    assert!(has_careful(
        &ok_get(&app, &format!("/v2/permission-modes?directory={b_q}")).await
    ));
    assert!(!has_careful(
        &ok_get(
            &app,
            &format!("/v2/permission-modes?session={plain_session}&directory={a_q}")
        )
        .await
    ));
    assert_eq!(
        get_json(&app, "/v2/permission-modes?session=not-a-session")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    for (directory, session) in [
        (String::new(), String::new()),
        (f.b.to_string_lossy().into_owned(), String::new()),
        (f.c.to_string_lossy().into_owned(), String::new()),
        (String::new(), project_session),
    ] {
        let q = format!("?directory={}&session={session}", enc(&directory));
        assert_eq!(
            grpc_json(
                grpc.list_permission_modes(tonic::Request::new(pb::ListPermissionModesRequest {
                    directory: directory.clone(),
                    session: session.clone()
                }))
                .await
            ),
            ok_get(&app, &format!("/v2/permission-modes{q}")).await
        );
        assert_eq!(
            grpc_json(
                grpc.list_bundle_apis(tonic::Request::new(pb::ListBundleApisRequest {
                    directory,
                    session
                }))
                .await
            ),
            ok_get(&app, &format!("/v2/bundle-apis{q}")).await
        );
    }
}
