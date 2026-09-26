//! Catalog rpcs follow the Project of the requested directory: a directory
//! inside a registered Project lists the Project's catalog (every root's
//! skills and commands, first root wins, plus its bundle overlay); a
//! directory in no Project lists only its own inert tiers; no directory is
//! the global view. Project mutations that change a Project's catalog emit
//! one `catalogUpdated {projectId}`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use http_body_util::BodyExt;
use hya_api::v1 as pb;
use hya_core::{
    AgentSpec, ApiMethod, ApiPathTemplate, ApiScope, BundleApiProvider, BundleApiReply,
    BundleApiRequest, CatalogScope, CoreError, EventBus, RuntimeCatalogRefresh,
    RuntimePermissionMode, RuntimeRegistry, RuntimeSource, RuntimeSourceId, ScopeOverlay,
    SessionEngine, SourceApi,
};
use hya_proto::{AgentName, FinishReason, ModelRef};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_server::{AppState, V1Grpc, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use support::AgentFixture;
use tower::ServiceExt;

/// The Project bundle a Project scope's overlay carries (agent `scoped`).
const SCOPED_BUNDLE: &str = "hya/server-tests-scoped";

struct Answer;

#[async_trait]
impl BundleApiProvider for Answer {
    async fn request(&self, _request: BundleApiRequest) -> Result<BundleApiReply, String> {
        Ok(BundleApiReply {
            status: 200,
            body: json!({}),
        })
    }
}

/// Stands in for the app's Project bundle tier: every Project scope gets
/// an overlay with agent `scoped`, a session API, and a permission mode;
/// Directory and Global scopes get none (inert tiers only).
struct ProjectBundleRefresh;

fn project_overlay() -> ScopeOverlay {
    let mut agents = support::test_agents();
    agents.push(AgentFixture::main("scoped"));
    let mut overlay = ScopeOverlay::new(support::agent_catalog(&agents));
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
    overlay
}

#[async_trait]
impl RuntimeCatalogRefresh for ProjectBundleRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
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
        runtime.publish_scope(scope.key(), project_overlay())?;
        Ok(true)
    }
}

async fn state() -> AppState {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("ok".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        providers,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        perm,
        EventBus::default(),
    )
    .with_catalog_refresh(Arc::new(ProjectBundleRefresh));
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

async fn send(app: &axum::Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
    let body = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(app: &axum::Router, uri: &str) -> Value {
    let (status, reply) = send(app, Method::GET, uri, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}: {reply}");
    reply
}

fn scratch(label: &str) -> PathBuf {
    std::fs::canonicalize(support::tempdir(&format!("catalog-scope-{label}"))).unwrap()
}

fn subdir(parent: &Path, child: &str) -> PathBuf {
    let path = parent.join(child);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn encode(path: &Path) -> String {
    url_escape(&text(path))
}

fn url_escape(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn write_skill(root: &Path, name: &str, description: &str) {
    let dir = root.join(".hya/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{name} body\n"),
    )
    .unwrap();
}

fn write_command(root: &Path, name: &str, body: &str) {
    let dir = root.join(".hya/commands");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.md")), body).unwrap();
}

/// Rows of `list[key]` as `(name, field)` pairs.
fn rows(reply: &Value, key: &str, field: &str) -> Vec<(String, String)> {
    reply[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key}: {reply}"))
        .iter()
        .map(|row| {
            (
                row["name"].as_str().unwrap_or_default().to_owned(),
                row[field].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn field_of<'a>(rows: &'a [(String, String)], name: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(row, _)| row == name)
        .map(|(_, value)| value.as_str())
}

fn names(reply: &Value, key: &str) -> Vec<String> {
    rows(reply, key, "name")
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Two roots `a` and `b` with skills and commands each (`shared` in both),
/// plus a directory `c` in no Project with its own skill.
struct Fixture {
    a: PathBuf,
    b: PathBuf,
    c: PathBuf,
}

fn fixture(label: &str) -> Fixture {
    let dir = scratch(label);
    let (a, b, c) = (subdir(&dir, "a"), subdir(&dir, "b"), subdir(&dir, "c"));
    write_skill(&a, "alpha", "alpha skill");
    write_skill(&a, "shared", "from a");
    write_skill(&b, "beta", "beta skill");
    write_skill(&b, "shared", "from b");
    write_skill(&c, "gamma", "gamma skill");
    write_command(&a, "from-a", "a command");
    write_command(&a, "dup", "dup from a");
    write_command(&b, "from-b", "b command");
    write_command(&b, "dup", "dup from b");
    write_command(&c, "from-c", "c command");
    Fixture { a, b, c }
}

async fn create_project(app: &axum::Router, roots: &[&Path]) -> String {
    let roots = roots.iter().map(|root| text(root)).collect::<Vec<_>>();
    let (status, reply) = send(
        app,
        Method::POST,
        "/v1/projects",
        json!({"name": "demo", "roots": roots}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["id"].as_str().unwrap().to_owned()
}

/// A session in `workdir` (its Project), or a temporary one (no Project).
async fn create_session(app: &axum::Router, workdir: Option<&Path>) -> String {
    let body = match workdir {
        Some(workdir) => json!({"agent": "build", "model": "fake", "workdir": text(workdir)}),
        None => json!({"agent": "build", "model": "fake", "kind": "SESSION_KIND_TEMPORARY"}),
    };
    let (status, reply) = send(app, Method::POST, "/v1/sessions", body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    reply["session"]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_directory_inside_a_two_root_project_lists_both_roots_first_root_wins() {
    let app = router(state().await);
    let f = fixture("two-roots");
    create_project(&app, &[&f.a, &f.b]).await;
    let inside = subdir(&f.a, "nested");

    let skills = get(&app, &format!("/v1/skills?directory={}", encode(&inside))).await;
    let skills = rows(&skills, "skills", "description");
    for name in ["alpha", "beta", "shared"] {
        assert!(field_of(&skills, name).is_some(), "{name}: {skills:?}");
    }
    assert_eq!(field_of(&skills, "shared"), Some("from a"), "{skills:?}");
    assert!(field_of(&skills, "gamma").is_none(), "{skills:?}");

    let commands = get(&app, &format!("/v1/commands?directory={}", encode(&inside))).await;
    let commands = rows(&commands, "commands", "template");
    assert_eq!(field_of(&commands, "from-a"), Some("a command"));
    assert_eq!(field_of(&commands, "from-b"), Some("b command"));
    assert_eq!(
        field_of(&commands, "dup"),
        Some("dup from a"),
        "{commands:?}"
    );
    assert!(field_of(&commands, "from-c").is_none(), "{commands:?}");

    // The requested directory comes first: from root `b`, `b` wins.
    let skills = get(&app, &format!("/v1/skills?directory={}", encode(&f.b))).await;
    let skills = rows(&skills, "skills", "description");
    assert_eq!(field_of(&skills, "shared"), Some("from b"), "{skills:?}");
    assert!(field_of(&skills, "alpha").is_some(), "{skills:?}");

    // Agents come from the Project's bundle overlay.
    let agents = get(&app, &format!("/v1/agents?directory={}", encode(&inside))).await;
    assert!(names(&agents, "agents").contains(&"scoped".to_owned()));

    // Bootstrap answers from the same scope.
    let boot = get(
        &app,
        &format!("/v1/bootstrap?directory={}", encode(&inside)),
    )
    .await;
    assert!(
        names(&boot, "agents").contains(&"scoped".to_owned()),
        "{boot}"
    );
    assert!(
        names(&boot, "skills").contains(&"beta".to_owned()),
        "{boot}"
    );
    assert!(names(&boot, "commands").contains(&"from-b".to_owned()));
}

#[tokio::test]
async fn an_unresolved_directory_lists_its_own_tiers_but_no_project_bundle() {
    let app = router(state().await);
    let f = fixture("unresolved");
    create_project(&app, &[&f.a, &f.b]).await;

    let skills = names(
        &get(&app, &format!("/v1/skills?directory={}", encode(&f.c))).await,
        "skills",
    );
    assert!(skills.contains(&"gamma".to_owned()), "{skills:?}");
    assert!(!skills.contains(&"alpha".to_owned()), "{skills:?}");
    let commands = names(
        &get(&app, &format!("/v1/commands?directory={}", encode(&f.c))).await,
        "commands",
    );
    assert!(commands.contains(&"from-c".to_owned()), "{commands:?}");
    assert!(!commands.contains(&"from-a".to_owned()), "{commands:?}");
    let agents = names(
        &get(&app, &format!("/v1/agents?directory={}", encode(&f.c))).await,
        "agents",
    );
    assert!(agents.contains(&"build".to_owned()), "{agents:?}");
    assert!(!agents.contains(&"scoped".to_owned()), "{agents:?}");
}

#[tokio::test]
async fn no_directory_lists_the_global_view() {
    let app = router(state().await);
    let f = fixture("global");
    create_project(&app, &[&f.a, &f.b]).await;

    let agents = names(&get(&app, "/v1/agents").await, "agents");
    assert!(agents.contains(&"build".to_owned()), "{agents:?}");
    assert!(!agents.contains(&"scoped".to_owned()), "{agents:?}");
    let skills = names(&get(&app, "/v1/skills").await, "skills");
    for name in ["alpha", "beta", "gamma"] {
        assert!(!skills.contains(&name.to_owned()), "{skills:?}");
    }
    let commands = names(&get(&app, "/v1/commands").await, "commands");
    assert!(!commands.contains(&"from-a".to_owned()), "{commands:?}");
    let boot = get(&app, "/v1/bootstrap").await;
    assert!(!names(&boot, "agents").contains(&"scoped".to_owned()));
}

/// Every `catalogUpdated` frame on the gRPC global stream within `window`,
/// as its `project_id`.
async fn catalog_notices_during<F: std::future::Future<Output = ()>>(
    state: &AppState,
    window: Duration,
    action: F,
) -> Vec<String> {
    use pb::events_server::Events as _;
    let grpc = V1Grpc::new(state.clone());
    let mut stream = grpc
        .stream_global_events(tonic::Request::new(pb::StreamGlobalEventsRequest::default()))
        .await
        .unwrap()
        .into_inner();
    action.await;
    let mut notices = Vec::new();
    let _ = tokio::time::timeout(window, async {
        while let Some(frame) = stream.next().await {
            if let Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                payload: Some(pb::stream_event::Payload::CatalogUpdated(notice)),
                ..
            })) = frame.unwrap().frame
            {
                notices.push(notice.project_id);
            }
        }
    })
    .await;
    notices
}

#[tokio::test]
async fn update_project_roots_emits_one_catalog_updated_and_the_listing_follows() {
    let state = state().await;
    let app = router(state.clone());
    let f = fixture("update-roots");
    let id = create_project(&app, &[&f.a]).await;
    let uri = format!("/v1/skills?directory={}", encode(&f.a));
    assert!(!names(&get(&app, &uri).await, "skills").contains(&"gamma".to_owned()));

    let notices = catalog_notices_during(&state, Duration::from_millis(400), async {
        let (status, reply) = send(
            &app,
            Method::PATCH,
            &format!("/v1/projects/{id}"),
            json!({"roots": [text(&f.a), text(&f.c)]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    })
    .await;
    assert_eq!(notices, vec![id.clone()]);
    assert!(names(&get(&app, &uri).await, "skills").contains(&"gamma".to_owned()));

    // A rename or the same roots changes no catalog: no notice.
    let notices = catalog_notices_during(&state, Duration::from_millis(300), async {
        let (status, reply) = send(
            &app,
            Method::PATCH,
            &format!("/v1/projects/{id}"),
            json!({"name": "renamed", "roots": [text(&f.a), text(&f.c)]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    })
    .await;
    assert!(notices.is_empty(), "{notices:?}");
}

#[tokio::test]
async fn delete_project_emits_one_catalog_updated_and_the_directory_turns_plain() {
    let state = state().await;
    let app = router(state.clone());
    let f = fixture("delete");
    let id = create_project(&app, &[&f.a, &f.b]).await;
    let agents_uri = format!("/v1/agents?directory={}", encode(&f.a));
    assert!(names(&get(&app, &agents_uri).await, "agents").contains(&"scoped".to_owned()));

    let notices = catalog_notices_during(&state, Duration::from_millis(400), async {
        let (status, reply) = send(
            &app,
            Method::DELETE,
            &format!("/v1/projects/{id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    })
    .await;
    assert_eq!(notices, vec![id]);
    assert!(!names(&get(&app, &agents_uri).await, "agents").contains(&"scoped".to_owned()));
    let skills = names(
        &get(&app, &format!("/v1/skills?directory={}", encode(&f.a))).await,
        "skills",
    );
    assert!(!skills.contains(&"beta".to_owned()), "{skills:?}");
}

#[tokio::test]
async fn provider_catalog_notices_keep_an_empty_project_id() {
    let state = state().await;
    let notices = catalog_notices_during(&state, Duration::from_millis(300), async {
        state.notify_catalog_updated();
    })
    .await;
    assert_eq!(notices, vec![String::new()]);
    let json = serde_json::to_value(pb::CatalogUpdated {
        project_id: "p".to_owned(),
    })
    .unwrap();
    assert_eq!(json, json!({"projectId": "p"}));
}

fn mode_ids(reply: &Value) -> Vec<String> {
    reply["modes"]
        .as_array()
        .unwrap_or_else(|| panic!("{reply}"))
        .iter()
        .map(|mode| mode["id"].as_str().unwrap().to_owned())
        .collect()
}

fn api_bundles(reply: &Value) -> Vec<String> {
    reply["apis"]
        .as_array()
        .map(|apis| {
            apis.iter()
                .map(|api| api["bundle"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn permission_modes_and_bundle_apis_follow_session_then_directory_then_global() {
    let app = router(state().await);
    let f = fixture("modes");
    create_project(&app, &[&f.a, &f.b]).await;
    let project_session = create_session(&app, Some(&f.a)).await;
    let plain_session = create_session(&app, None).await;
    let careful = format!("{SCOPED_BUNDLE}/careful");

    let global = mode_ids(&get(&app, "/v1/permission-modes").await);
    assert!(global.contains(&"manual".to_owned()), "{global:?}");
    assert!(!global.contains(&careful), "{global:?}");
    let dir = mode_ids(
        &get(
            &app,
            &format!("/v1/permission-modes?directory={}", encode(&f.b)),
        )
        .await,
    );
    assert!(dir.contains(&careful), "{dir:?}");
    let plain = mode_ids(
        &get(
            &app,
            &format!("/v1/permission-modes?directory={}", encode(&f.c)),
        )
        .await,
    );
    assert!(!plain.contains(&careful), "{plain:?}");
    let session = mode_ids(
        &get(
            &app,
            &format!("/v1/permission-modes?session={project_session}"),
        )
        .await,
    );
    assert!(session.contains(&careful), "{session:?}");
    // The session wins over the directory.
    let session = mode_ids(
        &get(
            &app,
            &format!(
                "/v1/permission-modes?session={plain_session}&directory={}",
                encode(&f.a)
            ),
        )
        .await,
    );
    assert!(!session.contains(&careful), "{session:?}");

    let bundle = SCOPED_BUNDLE.to_owned();
    assert!(!api_bundles(&get(&app, "/v1/bundle-apis").await).contains(&bundle));
    assert!(
        api_bundles(&get(&app, &format!("/v1/bundle-apis?directory={}", encode(&f.a))).await)
            .contains(&bundle)
    );
    assert!(
        !api_bundles(&get(&app, &format!("/v1/bundle-apis?directory={}", encode(&f.c))).await)
            .contains(&bundle)
    );
    assert!(
        api_bundles(&get(&app, &format!("/v1/bundle-apis?session={project_session}")).await)
            .contains(&bundle)
    );
    assert!(
        !api_bundles(&get(&app, &format!("/v1/bundle-apis?session={plain_session}")).await)
            .contains(&bundle)
    );

    let (status, reply) = send(
        &app,
        Method::GET,
        "/v1/permission-modes?session=not-a-session",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
}

#[tokio::test]
async fn grpc_matches_http_for_scoped_permission_modes_and_bundle_apis() {
    use pb::bundle_api_server::BundleApi as _;
    use pb::catalog_server::Catalog as _;
    let state = state().await;
    let app = router(state.clone());
    let grpc = V1Grpc::new(state);
    let f = fixture("grpc");
    create_project(&app, &[&f.a, &f.b]).await;
    let session = create_session(&app, Some(&f.a)).await;

    for (directory, session) in [
        (String::new(), String::new()),
        (text(&f.b), String::new()),
        (text(&f.c), String::new()),
        (String::new(), session.clone()),
    ] {
        let query = format!("?directory={}&session={session}", url_escape(&directory));
        let http = get(&app, &format!("/v1/permission-modes{query}")).await;
        let reply = grpc
            .list_permission_modes(tonic::Request::new(pb::ListPermissionModesRequest {
                directory: directory.clone(),
                session: session.clone(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(serde_json::to_value(reply).unwrap(), http, "{query}");

        let http = get(&app, &format!("/v1/bundle-apis{query}")).await;
        let reply = grpc
            .list_bundle_apis(tonic::Request::new(pb::ListBundleApisRequest {
                directory,
                session,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(serde_json::to_value(reply).unwrap(), http, "{query}");
    }
}

#[tokio::test]
async fn a_command_turn_expands_from_every_root_of_the_sessions_project() {
    let app = router(state().await);
    let f = fixture("expand");
    create_project(&app, &[&f.a, &f.b]).await;
    let session = create_session(&app, Some(&f.a)).await;
    let (status, turn) = send(
        &app,
        Method::POST,
        &format!("/v1/sessions/{session}/turns"),
        json!({"command": {"command": "from-b", "arguments": ""}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{turn}");
    let mut text = String::new();
    for _ in 0..100 {
        text = get(&app, &format!("/v1/sessions/{session}/messages"))
            .await
            .to_string();
        if text.contains("b command") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(text.contains("b command"), "{text}");
}
