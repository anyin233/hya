//! Integration tests for `hya-core`: every bind uses its session's catalog
//! scope (Project with fresh roots, else the workdir), scope-aware bundle
//! APIs, scope invalidation, and the scope cache limits.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::AgentRole;
use hya_core::{
    AgentSpec, ApiMethod, ApiPathTemplate, ApiScope, BundleApiCall, BundleApiError,
    BundleApiProvider, BundleApiReply, BundleApiRequest, CatalogScope, CatalogScopeCacheConfig,
    CoreError, CreateSession, EventBus, RuntimeCatalogRefresh, RuntimePermissionMode,
    RuntimeRegistry, RuntimeSource, RuntimeSourceId, ScopeKey, ScopeOverlay, SessionEngine,
    SourceApi,
};
use hya_proto::{AgentName, FinishReason, ModelRef, ProjectId, SessionId, SessionKind};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::{Value, json};
use support::TestDir;
use tokio_util::sync::CancellationToken;

/// The bundle a published scope overlay carries.
const SCOPE_BUNDLE: &str = "hya/test-scoped";

/// Records every scope it is asked to refresh; publishes an overlay (one
/// bundle `hya/test-scoped` with a session API and a permission mode) for
/// every non-global scope that has none.
#[derive(Default)]
struct RecordingRefresh {
    scopes: Mutex<Vec<CatalogScope>>,
}

impl RecordingRefresh {
    fn scopes(&self) -> Vec<CatalogScope> {
        self.scopes.lock().unwrap().clone()
    }

    fn last(&self) -> CatalogScope {
        self.scopes().pop().expect("a scope was refreshed")
    }
}

struct Answer;

#[async_trait]
impl BundleApiProvider for Answer {
    async fn request(&self, request: BundleApiRequest) -> Result<BundleApiReply, String> {
        Ok(BundleApiReply {
            status: 200,
            body: json!({ "session": request.session.map(|id| id.to_string()) }),
        })
    }
}

fn scoped_overlay() -> ScopeOverlay {
    let mut overlay = ScopeOverlay::new(support::test_catalog(&[("scoped", AgentRole::Main, &[])]));
    overlay.bundle_sources = vec![
        RuntimeSource::new(
            RuntimeSourceId::bundle(SCOPE_BUNDLE),
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
impl RuntimeCatalogRefresh for RecordingRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        self.scopes.lock().unwrap().push(scope.clone());
        if *scope == CatalogScope::Global || runtime.scope_overlay(&scope.key()).is_some() {
            return Ok(false);
        }
        runtime.publish_scope(scope.key(), scoped_overlay())?;
        Ok(true)
    }
}

struct Harness {
    engine: SessionEngine,
    refresh: Arc<RecordingRefresh>,
}

async fn harness() -> Harness {
    let router = Arc::new(
        ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(vec![vec![
            FakeStep::Finish(FinishReason::Stop),
        ]]))),
    );
    let refresh = Arc::new(RecordingRefresh::default());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
    .with_catalog_refresh(refresh.clone());
    Harness { engine, refresh }
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn subdir(dir: &TestDir, child: &str) -> PathBuf {
    let path = dir.path().join(child);
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn create(
    engine: &SessionEngine,
    parent: Option<SessionId>,
    workdir: &Path,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: text(workdir),
            project,
            kind,
        })
        .await
        .unwrap()
}

async fn project(engine: &SessionEngine, roots: &[&Path]) -> ProjectId {
    let roots = roots.iter().map(|root| text(root)).collect::<Vec<_>>();
    engine
        .store()
        .create_project("demo", &roots)
        .await
        .unwrap()
        .id
}

fn project_scope(id: ProjectId, roots: &[&Path]) -> CatalogScope {
    CatalogScope::Project {
        id,
        roots: roots.iter().map(|root| root.to_path_buf()).collect(),
    }
}

#[tokio::test]
async fn project_session_binds_its_project_and_a_roots_edit_applies_next_bind() {
    let dir = TestDir::new("scope-project");
    let (a, b, c) = (subdir(&dir, "a"), subdir(&dir, "b"), subdir(&dir, "c"));
    let h = harness().await;
    let id = project(&h.engine, &[&a, &b]).await;
    let session = create(&h.engine, None, &a, Some(id), SessionKind::Project).await;

    // Create-time bind already used the Project scope.
    assert_eq!(h.refresh.last(), project_scope(id, &[&a, &b]));
    let binding = h.engine.bind_session_runtime(session, &a).await.unwrap();
    assert_eq!(*binding.scope(), project_scope(id, &[&a, &b]));
    assert!(binding.resolve_agent("scoped").is_some(), "overlay bound");

    h.engine
        .store()
        .replace_project_roots(id, &[text(&a), text(&c)])
        .await
        .unwrap();
    let binding = h.engine.bind_session_runtime(session, &a).await.unwrap();
    assert_eq!(*binding.scope(), project_scope(id, &[&a, &c]));
    assert_eq!(
        h.engine.catalog_scope_for_session(session).await.unwrap(),
        project_scope(id, &[&a, &c])
    );
}

#[tokio::test]
async fn a_turn_binds_the_session_scope() {
    let dir = TestDir::new("scope-turn");
    let a = subdir(&dir, "a");
    let h = harness().await;
    let id = project(&h.engine, &[&a]).await;
    let session = create(&h.engine, None, &a, Some(id), SessionKind::Project).await;
    h.refresh.scopes.lock().unwrap().clear();

    h.engine
        .admit_user_prompt(session, "hi".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: a.clone(),
        reasoning: None,
    };
    let finish = h
        .engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let seen = h.refresh.scopes();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|scope| *scope == project_scope(id, &[&a])),
        "every admission/turn bind used the Project scope: {seen:?}"
    );
}

#[tokio::test]
async fn temporary_projectless_and_deleted_project_sessions_bind_their_workdir() {
    let dir = TestDir::new("scope-directory");
    let w = subdir(&dir, "w");
    let h = harness().await;
    let temporary = create(&h.engine, None, &w, None, SessionKind::Temporary).await;
    let legacy = create(&h.engine, None, &w, None, SessionKind::Project).await;
    let deleted = create(
        &h.engine,
        None,
        &w,
        Some(ProjectId::new()),
        SessionKind::Project,
    )
    .await;
    for session in [temporary, legacy, deleted] {
        let binding = h.engine.bind_session_runtime(session, &w).await.unwrap();
        assert_eq!(*binding.scope(), CatalogScope::Directory(w.clone()));
        assert_eq!(
            h.engine.catalog_scope_for_session(session).await.unwrap(),
            CatalogScope::Directory(w.clone())
        );
    }
}

#[tokio::test]
async fn a_child_session_inherits_its_parents_project_scope() {
    let dir = TestDir::new("scope-child");
    let (a, sub) = (subdir(&dir, "a"), subdir(&dir, "elsewhere"));
    let h = harness().await;
    let id = project(&h.engine, &[&a]).await;
    let parent = create(&h.engine, None, &a, Some(id), SessionKind::Project).await;
    let child = create(&h.engine, Some(parent), &sub, None, SessionKind::Temporary).await;

    let binding = h.engine.bind_session_runtime(child, &sub).await.unwrap();
    assert_eq!(*binding.scope(), project_scope(id, &[&a]));
    assert_eq!(binding.workdir(), sub.as_path());
}

#[tokio::test]
async fn a_directory_inside_a_project_resolves_to_the_project() {
    let dir = TestDir::new("scope-by-dir");
    let (a, b, out) = (
        subdir(&dir, "a"),
        subdir(&dir, "b/src"),
        subdir(&dir, "out"),
    );
    let b_root = dir.path().join("b");
    let h = harness().await;
    let id = project(&h.engine, &[&a, &b_root]).await;
    let expected = project_scope(id, &[&a, &b_root]);

    assert_eq!(
        h.engine.catalog_scope_for_directory(Some(&b)).await,
        expected
    );
    assert_eq!(
        h.engine.catalog_scope_for_directory(Some(&out)).await,
        CatalogScope::Directory(out.clone())
    );
    assert_eq!(
        h.engine.catalog_scope_for_directory(None).await,
        CatalogScope::Global
    );
    assert_eq!(
        h.engine
            .catalog_scope_for_directory(Some(Path::new(".")))
            .await,
        CatalogScope::Directory(PathBuf::from(".")),
        "an unresolvable path is a plain directory"
    );
    let binding = h.engine.bind_root_runtime(&b).await.unwrap();
    assert_eq!(*binding.scope(), expected);
    let global = h.engine.bind_global_runtime().await.unwrap();
    assert_eq!(*global.scope(), CatalogScope::Global);
    assert!(global.resolve_agent("scoped").is_none());
}

fn usage_call(session: Option<SessionId>) -> BundleApiCall {
    BundleApiCall {
        bundle: SCOPE_BUNDLE.into(),
        method: ApiMethod::Get,
        session,
        path: "/usage".into(),
        query: std::collections::BTreeMap::new(),
        body: Value::Null,
    }
}

#[tokio::test]
async fn session_scoped_bundle_apis_and_modes_resolve_in_the_session_scope() {
    let dir = TestDir::new("scope-api");
    let a = subdir(&dir, "a");
    let h = harness().await;
    let id = project(&h.engine, &[&a]).await;
    let session = create(&h.engine, None, &a, Some(id), SessionKind::Project).await;

    let outcome = h
        .engine
        .invoke_bundle_api(usage_call(Some(session)))
        .await
        .unwrap();
    assert_eq!(outcome.status, 200);
    assert_eq!(outcome.body, json!({ "session": session.to_string() }));
    let listed = h.engine.session_bundle_apis(session).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].bundle, SCOPE_BUNDLE);
    let modes = h.engine.session_permission_modes(session).await.unwrap();
    assert!(
        modes
            .iter()
            .any(|mode| mode.id == format!("{SCOPE_BUNDLE}/careful"))
    );
    h.engine
        .set_permission_mode(session, &format!("{SCOPE_BUNDLE}/careful"))
        .await
        .unwrap();

    // The global view stays base-only.
    let error = h.engine.invoke_bundle_api(usage_call(None)).await;
    assert!(
        matches!(error, Err(BundleApiError::NotFound { .. })),
        "{error:?}"
    );
    assert!(h.engine.bundle_apis().await.is_empty());
    assert!(
        !h.engine
            .permission_modes()
            .await
            .iter()
            .any(|mode| mode.source == SCOPE_BUNDLE)
    );
}

#[tokio::test]
async fn invalidating_a_project_drops_its_scope_and_notifies() {
    let dir = TestDir::new("scope-invalidate");
    let a = subdir(&dir, "a");
    let h = harness().await;
    let id = project(&h.engine, &[&a]).await;
    let mut notices = h.engine.subscribe_catalog_scope_invalidations();
    create(&h.engine, None, &a, Some(id), SessionKind::Project).await;
    let registry = h.engine.runtime_registry();
    assert!(registry.scope_keys().contains(&ScopeKey::Project(id)));

    h.engine.invalidate_catalog_scope(id);
    assert!(!registry.scope_keys().contains(&ScopeKey::Project(id)));
    assert_eq!(notices.try_recv().unwrap(), ScopeKey::Project(id));
}

#[tokio::test]
async fn the_least_recently_bound_idle_scope_is_evicted_past_the_cap() {
    let dir = TestDir::new("scope-lru");
    let (x, y, z) = (subdir(&dir, "x"), subdir(&dir, "y"), subdir(&dir, "z"));
    let h = harness().await;
    h.engine
        .set_catalog_scope_cache_config(CatalogScopeCacheConfig {
            max_scopes: 2,
            idle_ttl: Duration::from_secs(3600),
        });
    let registry = h.engine.runtime_registry();
    let key = |path: &Path| ScopeKey::Directory(path.to_path_buf());

    // `x` stays bound (a turn in flight): never evicted.
    let in_flight = h.engine.bind_root_runtime(&x).await.unwrap();
    drop(h.engine.bind_root_runtime(&y).await.unwrap());
    drop(h.engine.bind_root_runtime(&z).await.unwrap());
    let keys = registry.scope_keys();
    assert!(keys.contains(&key(&x)), "in-flight scope kept: {keys:?}");
    assert!(!keys.contains(&key(&y)), "LRU idle scope evicted: {keys:?}");
    assert!(keys.contains(&key(&z)), "just-bound scope kept: {keys:?}");
    drop(in_flight);

    // Rebinding an evicted scope republishes it; now `x` is the LRU.
    drop(h.engine.bind_root_runtime(&y).await.unwrap());
    let keys = registry.scope_keys();
    assert_eq!(keys, vec![key(&y), key(&z)]);
}

#[tokio::test]
async fn a_scope_idle_past_the_ttl_is_swept() {
    let dir = TestDir::new("scope-ttl");
    let x = subdir(&dir, "x");
    let h = harness().await;
    let config = CatalogScopeCacheConfig {
        max_scopes: 32,
        idle_ttl: Duration::from_millis(50),
    };
    let h = Harness {
        engine: h.engine.with_catalog_scope_cache(config),
        refresh: h.refresh,
    };
    assert_eq!(h.engine.catalog_scope_cache_config(), config);
    let registry = h.engine.runtime_registry();

    let held = h.engine.bind_root_runtime(&x).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine.sweep_catalog_scopes();
    assert_eq!(registry.scope_keys().len(), 1, "a bound scope is not swept");
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine.sweep_catalog_scopes();
    assert!(registry.scope_keys().is_empty());
}

#[tokio::test]
async fn a_project_turn_sees_skills_from_every_root_in_order() {
    let (a, b) = (
        TestDir::new("scope-skills-a"),
        TestDir::new("scope-skills-b"),
    );
    a.write_skill("shared");
    a.write_skill("only-a");
    b.write_skill("shared");
    b.write_skill("only-b");
    let h = harness().await;
    let id = project(&h.engine, &[a.path(), b.path()]).await;
    let session = create(&h.engine, None, a.path(), Some(id), SessionKind::Project).await;

    let binding = h
        .engine
        .bind_session_runtime(session, a.path())
        .await
        .unwrap();
    let skill = |name: &str| {
        binding
            .skills()
            .iter()
            .find(|skill| skill.name == name)
            .unwrap_or_else(|| panic!("skill {name} bound"))
            .dir
            .clone()
    };
    assert!(skill("only-a").starts_with(a.path()));
    assert!(skill("only-b").starts_with(b.path()));
    assert!(skill("shared").starts_with(a.path()), "first root wins");
}
