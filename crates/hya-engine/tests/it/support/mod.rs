#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsStr;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
};
use hya_engine::hooks::{NativeToolAfterInput, NativeToolAfterOutcome, NativeToolResult};
use hya_engine::{
    AgentCatalog, AgentSpec, CreateSession, EventBus, NativeHook, RuntimeRegistry, SessionEngine,
};
use hya_proto::{
    AgentName, ModelRef, PartProjection, Role, SessionId, SessionKind, ToolName, ToolSchema,
};
use hya_provider::{Provider, ProviderRouter};
use hya_runtime_abi::NativeRegistrations;
use hya_store::SessionStore;
use hya_tool::{
    PermissionPlane, PermissionRules, SkillDiscovery, Tool, ToolCtx, ToolError, ToolRegistry,
};
use serde_json::{Value, json};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

/// Zero-delay retry schedule matching [`hya_engine::kernel::ROUND_RETRY_WAITS`] length
/// so retry/fallback integration tests run without wall-clock sleeps.
pub const ZERO_RETRY_WAITS: [Duration; 8] = [Duration::ZERO; 8];

/// Discover skills only from `workdir` and `roots` (plus bundled builtins),
/// never reading `$HOME`, so tests never mutate process-global env vars.
pub fn discover_local_skills(workdir: Option<&Path>, roots: &[PathBuf]) -> SkillDiscovery {
    let project_dirs = hya_engine::catalog_scope::inert_catalog_dirs(workdir, roots);
    let mut dirs = Vec::new();
    dirs.extend(project_dirs.iter().map(|d| d.join(".hya/skills")));
    dirs.extend(project_dirs.iter().map(|d| d.join(".agents/skills")));
    let mut discovery =
        hya_engine::skill_catalog::discover_skills_from_dirs_with_diagnostics(&dirs);
    discovery.skills = hya_engine::skill_catalog::merge_skill_catalog(discovery.skills);
    discovery
}

/// The first-party `hya/core-agents` package (`hya-main`, `hya-title`).
pub fn core_agents() -> &'static hya_bundle::PreparedCatalog {
    hya_bundle::first_party_bundle("hya/core-agents").expect("first-party hya/core-agents")
}

/// `bundles` plus the first-party `hya/core-agents`.
pub fn with_core_agents(bundles: &[Bundle]) -> Vec<Bundle> {
    let mut all = bundles.to_vec();
    all.extend(core_agents().bundles().iter().cloned());
    all
}

fn is_core_agent(stable_id: &str) -> bool {
    core_agents().bundles()[0]
        .agents()
        .iter()
        .any(|agent| agent.id.as_str() == stable_id)
}

/// One installed bundle per requested agent, over the first-party
/// `hya/core-agents` and (unless a fixture agent replaces one of its agents)
/// `hya/multi-agent`.
///
/// A bundle defines exactly one agent, so a fixture asking for N agents becomes
/// N single-agent bundles; a requested `hya-main` or `hya-title` is the
/// first-party one.
pub fn test_catalog(agents: &[(&str, AgentRole, &[&str])]) -> Arc<AgentCatalog> {
    let mut bundles = agents
        .iter()
        .filter(|(stable_id, _, _)| !is_core_agent(stable_id))
        .map(|(stable_id, role, can_spawn)| Bundle {
            check: None,
            format_version: 4,
            identity: BundleIdentity {
                id: format!("hya/test-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: format!("test-only-{stable_id}"),
            agents: vec![PreparedAgent {
                private: false,
                id: AgentName::new(*stable_id),
                description: None,
                role: *role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn
                    .iter()
                    .map(|agent| AgentName::new(*agent))
                    .collect(),
                hook_refs: Vec::new(),
                advertise: Vec::new(),
                handoff: hya_bundle::HandoffPolicy::Model,
            }],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
            tui: None,

            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            commands: Vec::new(),
            runtime: None,
            permission_modes: vec![],
            cli: Vec::new(),
            requires: Vec::new(),
            default_agent: None,
            overrides: None,
        })
        .collect::<Vec<_>>();
    let fixture_ids: std::collections::BTreeSet<&str> =
        agents.iter().map(|(id, _, _)| *id).collect();
    if let Ok(multi_agent) = hya_bundle::first_party_bundle("hya/multi-agent")
        && multi_agent.bundles().iter().all(|bundle| {
            bundle
                .agents()
                .iter()
                .all(|agent| !fixture_ids.contains(agent.id.as_str()))
        })
    {
        bundles.extend(multi_agent.bundles().iter().cloned());
    }
    bundles.extend(core_agents().bundles().iter().cloned());
    let catalog = BundleCatalog::from_prepared(&bundles);
    let Ok(catalog) = catalog else {
        panic!("test catalog must be valid: {catalog:?}");
    };
    let catalog = AgentCatalog::new(Arc::new(catalog));
    let Ok(catalog) = catalog else {
        panic!("test agent catalog must be valid: {catalog:?}");
    };
    Arc::new(catalog)
}

/// Agent catalog of the first-party `hya/core-agents` alone.
pub fn builtin_only_catalog() -> Arc<AgentCatalog> {
    let bundles =
        BundleCatalog::from_verified_catalogs(&[core_agents()]).expect("core-agents catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("builtin-only agent catalog"))
}

/// A runtime plus the `hya/agent-channels` registrations from the load that
/// produced its tools, so [`fixture_engine`] starts the service those tools hold.
pub struct TestRuntime {
    pub runtime: Arc<RuntimeRegistry>,
    pub channels: NativeRegistrations,
}

/// A custom registry plus a separate `hya/agent-channels` load: the hook, URI
/// handler, and reducer work, while channels tools in `runtime` (if any) hold
/// a service that is never started.
impl From<Arc<RuntimeRegistry>> for TestRuntime {
    fn from(runtime: Arc<RuntimeRegistry>) -> Self {
        let mut channels = hya_tool_loader::load_v2(
            "hya/agent-channels",
            Arc::new(hya_tool_loader::MinimalHostRuntime::empty()),
        )
        .expect("load hya/agent-channels");
        channels.tools.clear();
        Self { runtime, channels }
    }
}

fn multi_agent_catalog() -> Arc<AgentCatalog> {
    let multi_agent = hya_bundle::first_party_bundle("hya/multi-agent").expect("hya/multi-agent");
    let bundles = BundleCatalog::from_verified_catalogs(&[core_agents(), multi_agent])
        .expect("multi-agent catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("multi-agent agent catalog"))
}

/// Default runtime for root/main turn integration fixtures.
///
/// Uses the first-party `hya/core-agents` and `hya/multi-agent` rosters and
/// workdir/root-only skill discovery (never reading `$HOME`) so tests stay
/// hermetic and parallel-safe.
pub fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    test_runtime_with_catalog(tools, multi_agent_catalog())
}

/// [`test_runtime`] over every builtin tool bundle.
pub fn builtin_test_runtime() -> TestRuntime {
    builtin_test_runtime_with_catalog(multi_agent_catalog())
}

/// [`test_runtime_with_catalog`] over every builtin tool bundle.
pub fn builtin_test_runtime_with_catalog(catalog: Arc<AgentCatalog>) -> TestRuntime {
    let (tools, bundles) = hya_tool_loader::builtin_bundles().expect("builtin bundles");
    let channels = bundles
        .into_iter()
        .find_map(|(id, regs)| (id == "hya/agent-channels").then_some(regs))
        .expect("hya/agent-channels registrations");
    TestRuntime {
        runtime: test_runtime_with_catalog(Arc::new(tools), catalog),
        channels,
    }
}

/// Runtime backed by a custom agent catalog and workdir/root-only skill discovery.
pub fn test_runtime_with_catalog(
    tools: Arc<ToolRegistry>,
    catalog: Arc<AgentCatalog>,
) -> Arc<RuntimeRegistry> {
    Arc::new(
        RuntimeRegistry::from_snapshot(tools.snapshot(), catalog).with_skill_discovery(
            discover_local_skills,
            hya_engine::skill_catalog::builtin_skills,
        ),
    )
}

/// The production turn path with only the tools explicitly installed by the test.
pub fn kernel_engine(
    store: SessionStore,
    router: Arc<ProviderRouter>,
    permission: PermissionPlane,
    tools: &[Arc<dyn Tool>],
) -> SessionEngine {
    let registry = ToolRegistry::default();
    for tool in tools {
        let permission = match tool.name() {
            "read" | "grep" => hya_tool::ToolPermission::ReadOnly,
            "bash" => hya_tool::ToolPermission::Command,
            _ => hya_tool::ToolPermission::Tool,
        };
        registry.register_builtin(tool.clone(), permission, [], "hya/bare", None);
    }
    kernel_engine_with_registry(store, router, permission, registry)
}

pub fn kernel_engine_with_registry(
    store: SessionStore,
    router: Arc<ProviderRouter>,
    permission: PermissionPlane,
    registry: ToolRegistry,
) -> SessionEngine {
    let bundles = Arc::new(BundleCatalog::from_verified_catalogs(&[]).unwrap());
    let catalog = Arc::new(AgentCatalog::new(bundles).unwrap());
    let runtime = Arc::new(RuntimeRegistry::new(registry, catalog).with_pure_skills(true));
    SessionEngine::new(store, router, runtime, permission, EventBus::default())
        .with_pure_context(true)
}

/// Compose real native extensions for the fixtures that exercise them.
pub async fn fixture_engine(
    store: SessionStore,
    router: Arc<ProviderRouter>,
    runtime: impl Into<TestRuntime>,
    permission: PermissionPlane,
    bus: EventBus,
) -> SessionEngine {
    let TestRuntime { runtime, channels } = runtime.into();
    let regs = hya_tool_loader::load_v2(
        "hya/compaction",
        Arc::new(hya_tool_loader::MinimalHostRuntime::empty()),
    )
    .expect("load native rewriters");
    let engine =
        SessionEngine::new(store, router, runtime, permission, bus).with_rewriters(regs.rewriters);
    let host = engine.host_runtime();
    for service in &channels.services {
        service
            .start(Arc::clone(&host))
            .await
            .expect("start agent-channels service");
    }
    let engine = channels
        .hooks
        .into_iter()
        .fold(engine, SessionEngine::with_hooks)
        .with_uri_schema_handlers(channels.uri_schema_handlers, host)
        .with_legacy_coordination();
    engine.register_native_projection_reducers(regs.projection_reducers);
    engine.register_native_projection_reducers(channels.projection_reducers);
    engine
}

/// The `hya/compaction` rewriters with `keep_recent` set through the bundle's
/// own config (each load binds its own runtime, so tests do not share it).
pub fn compaction_rewriters(keep_recent: usize) -> Vec<Arc<dyn hya_context::rewrite::Rewriter>> {
    hya_tool_loader::load_v2(
        "hya/compaction",
        Arc::new(hya_tool_loader::MinimalHostRuntime::with_bundle_config(
            move |id| {
                if id == "hya/compaction" {
                    json!({ "keep_recent": keep_recent })
                } else {
                    Value::Null
                }
            },
        )),
    )
    .expect("load native rewriters")
    .rewriters
}

/// Compact `SessionEngine` with in-memory SQLite, builtin tools, and zero retry backoff.
pub async fn engine_with_provider(
    provider: Arc<dyn Provider>,
    rules: PermissionRules,
) -> SessionEngine {
    engine_with_router_and_runtime(
        Arc::new(ProviderRouter::new().with(provider)),
        builtin_test_runtime(),
        rules,
    )
    .await
}

/// Compact `SessionEngine` with custom router/runtime and zero retry backoff.
pub async fn engine_with_router_and_runtime(
    router: Arc<ProviderRouter>,
    runtime: impl Into<TestRuntime>,
    rules: PermissionRules,
) -> SessionEngine {
    let store = SessionStore::connect_memory().await.expect("memory store");
    let (permission, _rx) = PermissionPlane::new(rules);
    fixture_engine(store, router, runtime, permission, EventBus::default())
        .await
        .with_round_retry_waits(&ZERO_RETRY_WAITS)
}

/// The unread-mail notice the `hya/agent-channels` steer hook would show
/// `session` now, under the session's cached channel policy.
pub async fn steer_notice(engine: &SessionEngine, session: SessionId) -> Option<String> {
    hya_agent_channels_core::AgentChannelsService::prepare_unread_steer(
        engine.host_runtime().as_ref(),
        session,
    )
    .await
    .expect("prepare unread steer")
    .and_then(|prepared| prepared.notice)
}

/// Read `channel` as `session` through the `hya/agent-channels` service.
pub async fn read_channel(
    engine: &SessionEngine,
    session: SessionId,
    channel: &str,
    last: Option<usize>,
) -> Result<hya_agent_channels_core::ChannelHistoryRead, hya_runtime_abi::HostError> {
    hya_agent_channels_core::AgentChannelsService::read_channel_history(
        engine.host_runtime().as_ref(),
        None,
        session,
        channel,
        last,
    )
    .await
}

/// Create a root project session on `engine`.
pub async fn create_root_session(
    engine: &SessionEngine,
    agent: &str,
    model: &str,
    workdir: &Path,
) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new(agent),
            model: ModelRef::new(model),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .expect("create root session")
}

/// Build an ordinary `AgentSpec` for tests.
pub fn agent_spec(name: &str, model: &str, workdir: impl Into<PathBuf>) -> AgentSpec {
    AgentSpec {
        name: AgentName::new(name),
        model: ModelRef::new(model),
        system_prompt: String::new(),
        workdir: workdir.into(),
        reasoning: None,
    }
}

/// Join every assistant text part currently projected on `session`.
pub async fn assistant_text(engine: &SessionEngine, session: SessionId) -> String {
    let projection = engine
        .store()
        .read_projection(session)
        .await
        .expect("read projection");
    projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .flat_map(|m| &m.parts)
        .filter_map(|p| match p {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll `check` on a tight 5ms cadence until it returns `true` or `timeout` elapses.
pub async fn poll_until<F, Fut>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Run a test requiring process-global state (such as `hya_tool::deadline::install`
/// or `hya_store::bench::snapshot`) inside an isolated child invocation of the
/// current test binary when running under `cargo test`, or directly when already
/// isolated by `cargo nextest` or the child env marker.
pub async fn run_isolated_subproc<F, Fut>(key: &str, exact_test_name: &str, body: F)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    let in_subproc = std::env::var_os("HYA_TEST_SUBPROC").as_deref() == Some(OsStr::new(key));
    let in_nextest = std::env::var_os("NEXTEST_RUN_ID").is_some();
    if in_subproc || in_nextest {
        body().await;
        return;
    }
    let exe = std::env::current_exe().expect("current test binary path");
    let output = Command::new(exe)
        .env("HYA_TEST_SUBPROC", key)
        .args([
            "--exact",
            exact_test_name,
            "--test-threads=1",
            "--nocapture",
        ])
        .output()
        .expect("spawn isolated test subprocess");
    assert!(
        output.status.success(),
        "isolated test `{exact_test_name}` failed (status={:?})\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hya-runtime-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create test workdir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_skill(&self, name: &str) {
        self.write_skill_with(name, &format!("{name} skill"), &format!("{name} body\n"));
    }

    pub fn write_skill_with(&self, name: &str, description: &str, body: &str) {
        let dir = self.path.join(".hya/skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill directory");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .expect("write skill");
    }

    pub fn remove_skill(&self, name: &str) {
        std::fs::remove_dir_all(self.path.join(".hya/skills").join(name))
            .expect("remove skill directory");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub struct MarkerTool {
    name: String,
}

impl MarkerTool {
    pub fn new(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { name: name.into() })
    }
}

#[async_trait]
impl Tool for MarkerTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: format!("{} marker", self.name),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}

/// Replaces the first tool result with `{"masked": true}` and passes subsequent results through.
#[derive(Default)]
pub struct MaskFirstResult {
    masked: AtomicBool,
    captured_output_path: std::sync::Mutex<Option<String>>,
}

impl MaskFirstResult {
    pub fn captured_output_path(&self) -> Option<String> {
        self.captured_output_path.lock().unwrap().clone()
    }
}

#[async_trait]
impl NativeHook for MaskFirstResult {
    fn id(&self) -> &str {
        "MaskFirstResult"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[hya_runtime_abi::HookPoint::ToolExecuteAfter]
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, hya_runtime_abi::HostError> {
        if let NativeToolResult::Ok { output, .. } = &input.result
            && let Some(path) = output["metadata"]["outputPath"].as_str()
        {
            *self.captured_output_path.lock().unwrap() = Some(path.to_string());
        }
        Ok(NativeToolAfterOutcome::Continue {
            result: if self.masked.swap(true, Ordering::SeqCst) {
                input.result
            } else {
                NativeToolResult::Ok {
                    output: json!({ "masked": true }),
                    time_ms: 0,
                }
            },
        })
    }
}

pub mod resident;
