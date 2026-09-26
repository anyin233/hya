//! Integration tests for `hya-core`: a session's captured lifecycle/event
//! hook chain follows its current catalog scope binding. A respawned scope
//! plugin takes over at the session's next bind, a retired process is not
//! kept alive by idle sessions, and scope invalidation or eviction releases
//! captured chains at once.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::AgentRole;
use hya_core::{
    CatalogScope, CatalogScopeCacheConfig, ChatParamsInput, ChatParamsOutcome,
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, CoreError, CreateSession, EventBus,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, RuntimeCatalogRefresh,
    RuntimeRegistry, RuntimeSource, RuntimeSourceId, ScopeKey, ScopeOverlay, SessionEngine,
    SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome,
};
use hya_proto::{AgentName, Envelope, ModelRef, ProjectId, SessionId, SessionKind};
use hya_provider::ProviderRouter;
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use support::TestDir;

/// What one fake hook process observed, and whether it is still alive.
#[derive(Default)]
struct Probe {
    events: Mutex<HashMap<SessionId, usize>>,
    starts: Mutex<HashMap<SessionId, usize>>,
    ends: Mutex<HashMap<SessionId, usize>>,
    dropped: AtomicBool,
}

impl Probe {
    fn events(&self, session: SessionId) -> usize {
        self.events
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }

    fn starts(&self, session: SessionId) -> usize {
        self.starts
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }

    fn ends(&self, session: SessionId) -> usize {
        self.ends
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }

    fn alive(&self) -> bool {
        !self.dropped.load(Ordering::SeqCst)
    }
}

/// A fake hook dispatcher standing in for a plugin process: dropping the
/// last handle "kills" it.
struct FakeProcess(Arc<Probe>);

impl Drop for FakeProcess {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

fn bump(map: &Mutex<HashMap<SessionId, usize>>, session: SessionId) {
    *map.lock().unwrap().entry(session).or_default() += 1;
}

#[async_trait]
impl HookDispatcher for FakeProcess {
    fn dispatch_event(&self, envelope: &Envelope) {
        if let Some(session) = envelope.event.session() {
            bump(&self.0.events, session);
        }
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        bump(&self.0.starts, input.session);
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        bump(&self.0.ends, input.session);
    }
}

/// Publishes, for every non-global scope, an overlay whose one project
/// plugin carries a fresh [`FakeProcess`]; republishes (a respawn) whenever
/// `version` moved past the scope's published version or the scope has no
/// overlay (it was invalidated or evicted).
#[derive(Default)]
struct PluginRefresh {
    version: AtomicUsize,
    published: Mutex<HashMap<ScopeKey, usize>>,
    processes: Mutex<Vec<Arc<Probe>>>,
}

impl PluginRefresh {
    /// Simulate a `plugin.toml` edit: the next bind respawns the plugin.
    fn edit_manifest(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    fn process(&self, index: usize) -> Arc<Probe> {
        Arc::clone(&self.processes.lock().unwrap()[index])
    }

    fn spawned(&self) -> usize {
        self.processes.lock().unwrap().len()
    }
}

#[async_trait]
impl RuntimeCatalogRefresh for PluginRefresh {
    async fn refresh_if_changed(&self, _runtime: &RuntimeRegistry) -> Result<bool, CoreError> {
        Ok(false)
    }

    async fn refresh_scope(
        &self,
        runtime: &RuntimeRegistry,
        scope: &CatalogScope,
    ) -> Result<bool, CoreError> {
        if *scope == CatalogScope::Global {
            return Ok(false);
        }
        let key = scope.key();
        let version = self.version.load(Ordering::SeqCst);
        let current = self.published.lock().unwrap().get(&key).copied();
        if current == Some(version) && runtime.scope_overlay(&key).is_some() {
            return Ok(false);
        }
        let probe = Arc::new(Probe::default());
        self.processes.lock().unwrap().push(Arc::clone(&probe));
        let mut overlay =
            ScopeOverlay::new(support::test_catalog(&[("scoped", AgentRole::Main, &[])]));
        overlay.plugin_sources = vec![
            RuntimeSource::new(
                RuntimeSourceId::plugin("project-plugin"),
                [9; 32],
                Arc::new(()),
                Vec::new(),
            )
            .with_hooks(Arc::new(FakeProcess(probe))),
        ];
        runtime.publish_scope(key.clone(), overlay)?;
        self.published.lock().unwrap().insert(key, version);
        Ok(true)
    }
}

struct Harness {
    engine: SessionEngine,
    refresh: Arc<PluginRefresh>,
    /// A process-wide (config) plugin: sees every session regardless of scope.
    config: Arc<Probe>,
}

async fn harness(cache: Option<CatalogScopeCacheConfig>) -> Harness {
    let refresh = Arc::new(PluginRefresh::default());
    let config = Arc::new(Probe::default());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let mut engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new()),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    )
    .with_hooks(Arc::new(FakeProcess(Arc::clone(&config))))
    .with_catalog_refresh(refresh.clone());
    if let Some(cache) = cache {
        engine = engine.with_catalog_scope_cache(cache);
    }
    Harness {
        engine,
        refresh,
        config,
    }
}

fn subdir(dir: &TestDir, child: &str) -> PathBuf {
    let path = dir.path().join(child);
    std::fs::create_dir_all(&path).unwrap();
    path
}

async fn project(engine: &SessionEngine, root: &Path) -> ProjectId {
    engine
        .store()
        .create_project("demo", &[root.to_string_lossy().into_owned()])
        .await
        .unwrap()
        .id
}

async fn create(engine: &SessionEngine, root: &Path, project: ProjectId) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: root.to_string_lossy().into_owned(),
            project: Some(project),
            kind: SessionKind::Project,
        })
        .await
        .unwrap()
}

/// Admit a prompt: binds the session (the swap point) and then publishes
/// session events with no turn running (so the captured chain receives them).
async fn admit(engine: &SessionEngine, session: SessionId) {
    engine
        .admit_user_prompt(session, "hi".to_string())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_respawned_plugin_takes_over_session_hooks_at_the_next_bind() {
    let dir = TestDir::new("hook-follow-respawn");
    let root = subdir(&dir, "a");
    let h = harness(None).await;
    let id = project(&h.engine, &root).await;
    let session = create(&h.engine, &root, id).await;
    let old = h.refresh.process(0);
    assert_eq!(old.starts(session), 1, "created session starts the plugin");
    admit(&h.engine, session).await;
    assert!(old.events(session) > 0);

    // Unchanged scope: rebinding never re-starts or swaps.
    admit(&h.engine, session).await;
    assert_eq!(h.refresh.spawned(), 1);
    assert_eq!(old.starts(session), 1);

    h.refresh.edit_manifest();
    let old_events = old.events(session);
    let config_events = h.config.events(session);
    admit(&h.engine, session).await;
    assert_eq!(h.refresh.spawned(), 2, "the manifest edit respawned");
    let new = h.refresh.process(1);
    assert!(!old.alive(), "the retired process exits at the swap");
    assert_eq!(old.events(session), old_events, "nothing after the swap");
    assert_eq!(new.starts(session), 1, "the new process sees session.start");
    assert_eq!(
        new.events(session),
        h.config.events(session) - config_events,
        "every event after the swap reaches the new process exactly once"
    );
    assert_eq!(old.ends(session), 0, "a swap is not a session end");

    // Later binds keep the new chain and never re-start it.
    admit(&h.engine, session).await;
    assert_eq!(new.starts(session), 1);
    assert_eq!(h.config.starts(session), 1, "config plugins unaffected");
    assert!(h.engine.delete_session(session).await.unwrap());
    assert_eq!(new.ends(session), 1);
    assert_eq!(h.config.ends(session), 1);
}

#[tokio::test]
async fn another_sessions_bind_releases_the_retired_process_from_an_idle_session() {
    let dir = TestDir::new("hook-follow-idle");
    let root = subdir(&dir, "a");
    let h = harness(None).await;
    let id = project(&h.engine, &root).await;
    let idle = create(&h.engine, &root, id).await;
    let busy = create(&h.engine, &root, id).await;
    let old = h.refresh.process(0);
    assert!(old.starts(idle) == 1 && old.starts(busy) == 1);

    h.refresh.edit_manifest();
    admit(&h.engine, busy).await;
    let new = h.refresh.process(1);
    assert!(
        !old.alive(),
        "the idle session must not keep the retired process alive"
    );
    assert_eq!(new.starts(busy), 1);
    assert_eq!(new.starts(idle), 0, "the idle session rebinds lazily");

    // The idle session's own next bind captures the new process once.
    admit(&h.engine, idle).await;
    assert_eq!(new.starts(idle), 1);
    assert!(new.events(idle) > 0);
    admit(&h.engine, idle).await;
    assert_eq!(new.starts(idle), 1);
    assert_eq!(new.starts(busy), 1);
}

#[tokio::test]
async fn invalidating_a_project_releases_idle_session_hooks_at_once() {
    let dir = TestDir::new("hook-follow-invalidate");
    let root = subdir(&dir, "a");
    let h = harness(None).await;
    let id = project(&h.engine, &root).await;
    let session = create(&h.engine, &root, id).await;
    let old = h.refresh.process(0);
    assert!(old.alive());

    h.engine.invalidate_catalog_scope(id);
    assert!(!old.alive(), "released without waiting for a bind");

    admit(&h.engine, session).await;
    let new = h.refresh.process(1);
    assert_eq!(new.starts(session), 1);
    assert!(new.events(session) > 0);
    assert_eq!(old.ends(session), 0);
    assert_eq!(h.config.starts(session), 1, "config plugins unaffected");
}

#[tokio::test]
async fn evicting_an_idle_scope_releases_its_session_hooks() {
    let dir = TestDir::new("hook-follow-evict");
    let root = subdir(&dir, "a");
    let h = harness(Some(CatalogScopeCacheConfig {
        max_scopes: 32,
        idle_ttl: Duration::from_millis(50),
    }))
    .await;
    let id = project(&h.engine, &root).await;
    let session = create(&h.engine, &root, id).await;
    let old = h.refresh.process(0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.engine.sweep_catalog_scopes();
    assert!(
        !h.engine
            .runtime_registry()
            .scope_keys()
            .contains(&ScopeKey::Project(id))
    );
    assert!(
        !old.alive(),
        "an idle session does not pin an evicted scope"
    );

    admit(&h.engine, session).await;
    let new = h.refresh.process(1);
    assert_eq!(new.starts(session), 1);
}
