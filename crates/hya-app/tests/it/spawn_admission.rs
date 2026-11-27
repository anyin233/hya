//! Integration tests for `hya-app`: spawn admission, batch rollback,
//! `TurnBinding` pinning, triggering guidance propagation, and supervisor I/O.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_app::spawn_team_supervisor;
use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
};
use hya_engine::{
    AgentSpec, BoundSpawnSender, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor,
    RuntimeRegistry, SessionEngine, SubagentGovernor, SubagentLimits,
};
use hya_proto::{
    AgentName, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef, SessionId,
    SubagentMode, ToolCallId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, AgentDef, Mode, PermissionPlane, PermissionRules, Rule, SpawnError, SpawnMember,
    SpawnerPlane, ToolOperation, ToolRegistry,
};
use serde_json::json;
use sqlx::{Connection, SqliteConnection};
use tokio::sync::Notify;

const TRIGGER_GUIDANCE: &str = "TRIGGERING_TURN_GUIDANCE_MARKER";
const PROJECT_CONTEXT: &str = "RECORDED_PROJECT_CONTEXT_MARKER";
const POST_SPAWN_MUTATION: &str = "POST_SPAWN_SOURCE_MUTATION_MARKER";

struct AdmissionFixture {
    tasks: hya_app::runtime::RuntimeTaskOwner,
    engine: Arc<SessionEngine>,
    spawner: SpawnerPlane,
    sender: BoundSpawnSender,
    parent: SessionId,
    provider_calls: Arc<AtomicUsize>,
    resident: Arc<ResidentSupervisor>,
    agents: Arc<[AgentDef]>,
}

impl AdmissionFixture {
    fn scoped_spawner(&self) -> SpawnerPlane {
        self.spawner
            .for_session_with_agents(self.parent, self.agents.clone())
    }
}

struct AdmissionTempDb(String);

impl AdmissionTempDb {
    fn new() -> Self {
        Self(
            std::env::temp_dir()
                .join(format!("hya-app-spawn-admission-{}.db", SessionId::new()))
                .to_string_lossy()
                .into_owned(),
        )
    }
    fn path(&self) -> &str {
        &self.0
    }
}

impl Drop for AdmissionTempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0));
        }
    }
}

async fn wait_member_turn_done(engine: &SessionEngine, member: &str) {
    let session: SessionId = member.parse().unwrap();
    let (root, _) = engine.session_lineage(session).await.unwrap();
    for _ in 0..500 {
        let projection = engine.read_projection(root).await.unwrap();
        let turn_done = engine
            .read_projection(session)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .any(|m| m.role == hya_proto::Role::Assistant);
        let idle = hya_tool::extension::team_projection(&projection)
            .roster
            .values()
            .find(|e| e.session == session)
            .is_some_and(|e| e.status == hya_proto::RosterStatus::Idle);
        if turn_done && idle {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("spawned member {member} never completed its first turn");
}

struct ProviderGate {
    entered: Notify,
    release: Notify,
}

struct CountingProvider {
    calls: Arc<AtomicUsize>,
    inner: FakeProvider,
    gate: Option<Arc<ProviderGate>>,
}

#[async_trait::async_trait]
impl Provider for CountingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        if model.as_str().starts_with("hya/") {
            return None;
        }
        self.inner.capabilities(model)
    }
    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        Some(b"hya-test-counting-provider-identity-v1".to_vec())
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        self.inner.stream(request, session, message).await
    }
}

type ToolsBySession = Arc<Mutex<Vec<(SessionId, Vec<String>)>>>;

struct CaptureSystemsProvider<P = FakeProvider> {
    by_session: Arc<Mutex<Vec<(SessionId, String)>>>,
    tools_by_session: ToolsBySession,
    inner: P,
}

#[async_trait::async_trait]
impl<P: Provider> Provider for CaptureSystemsProvider<P> {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.inner.capabilities(model)
    }
    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        Some(b"hya-test-capture-systems-provider-identity-v1".to_vec())
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.by_session
            .lock()
            .unwrap()
            .push((session, request.system.clone().unwrap_or_default()));
        self.tools_by_session.lock().unwrap().push((
            session,
            request
                .tools
                .iter()
                .map(|t| t.name.as_str().to_string())
                .collect(),
        ));
        self.inner.stream(request, session, message).await
    }
}

/// One script per session, assigned in the order sessions first reach the
/// provider; later sessions (and turns past a script) get a bare stop. A single
/// shared queue would let concurrent sessions take each other's turns.
struct SessionScripts {
    seen: Mutex<Vec<SessionId>>,
    scripts: Vec<FakeProvider>,
    rest: FakeProvider,
}

#[async_trait::async_trait]
impl Provider for SessionScripts {
    fn id(&self) -> &str {
        self.rest.id()
    }
    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        self.rest.capabilities(model)
    }
    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let index = {
            let mut seen = self.seen.lock().unwrap();
            seen.iter().position(|s| *s == session).unwrap_or_else(|| {
                seen.push(session);
                seen.len() - 1
            })
        };
        self.scripts
            .get(index)
            .unwrap_or(&self.rest)
            .stream(request, session, message)
            .await
    }
}

fn operation() -> ToolOperation {
    ToolOperation::from_tool_call(ToolCallId::new())
}

fn temp_child_workdir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-app-guidance-{label}-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn admission_fixture_with(
    per_run_budget: u64,
    gate: Option<Arc<ProviderGate>>,
    store: SessionStore,
) -> AdmissionFixture {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CountingProvider {
        calls: provider_calls.clone(),
        inner: FakeProvider::scripted(Vec::new()),
        gate,
    })));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(2);
    let engine = Arc::new(
        SessionEngine::new(
            store,
            provider_router,
            support::test_runtime(
                Arc::new(hya_tool_loader::builtins()),
                &[
                    ("build", AgentRole::Main, &["quick"]),
                    ("general", AgentRole::Main, &[]),
                    ("quick", AgentRole::Subagent, &[]),
                ],
            ),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone())
        .with_governor(SubagentGovernor::new(SubagentLimits {
            per_run_budget,
            ..SubagentLimits::default()
        })),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "test".into(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    let tasks = spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        Arc::new(CategoryRegistry::from_entries(
            std::collections::HashMap::from([(
                "live".into(),
                hya_engine::CategoryEntry {
                    model: ModelRef::new("hya/offline"),
                    fallback: vec![ModelRef::new("fake")],
                },
            )]),
        )),
        resident.clone(),
    );
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name,
            model: agent.model,
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&std::env::temp_dir()).unwrap();
    let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    AdmissionFixture {
        tasks,
        engine,
        spawner,
        sender: spawn_sender,
        parent,
        provider_calls,
        resident,
        agents,
    }
}

async fn admission_fixture(per_run_budget: u64) -> AdmissionFixture {
    admission_fixture_with(
        per_run_budget,
        None,
        SessionStore::connect_memory().await.unwrap(),
    )
    .await
}

struct GuidanceFixture {
    _tasks: hya_app::runtime::RuntimeTaskOwner,
    engine: Arc<SessionEngine>,
    spawner: SpawnerPlane,
    parent: SessionId,
    agent: AgentSpec,
    agents: Arc<[AgentDef]>,
    by_session: Arc<Mutex<Vec<(SessionId, String)>>>,
}

impl GuidanceFixture {
    async fn wait_for_systems(&self, session: SessionId, count: usize) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                {
                    let captures = self.by_session.lock().unwrap();
                    let systems: Vec<_> = captures
                        .iter()
                        .filter(|(id, _)| *id == session)
                        .map(|(_, s)| s.clone())
                        .collect();
                    if systems.len() >= count {
                        return systems;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("session {session} did not reach {count} provider requests"))
    }
}

/// `scripts[i]` drives the i-th session to reach the provider.
async fn guidance_fixture(
    agents: &[(&str, AgentRole, &[&str])],
    scripts: Vec<FakeProvider>,
) -> GuidanceFixture {
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        by_session: by_session.clone(),
        tools_by_session: Arc::new(Mutex::new(Vec::new())),
        inner: SessionScripts {
            seen: Mutex::new(Vec::new()),
            scripts,
            rest: FakeProvider::scripted_turns(Vec::new()),
        },
    })));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Task, "*", Mode::Allow),
        Rule::new(Action::Read, "*", Mode::Allow),
    ]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router,
            support::test_runtime(Arc::new(hya_tool_loader::builtins()), agents),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone())
        .with_governor(SubagentGovernor::new(SubagentLimits {
            per_run_budget: 16,
            ..SubagentLimits::default()
        })),
    );
    let workdir = temp_child_workdir("parent");
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "parent base".into(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    let _tasks = spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        Arc::new(CategoryRegistry::default()),
        resident,
    );
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let roster = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    GuidanceFixture {
        _tasks,
        engine,
        spawner,
        parent,
        agent,
        agents: roster,
        by_session,
    }
}

#[tokio::test]
async fn inline_validation_overlay_execution_and_base_roster_spawn() {
    // 1. Unknown target and unsupported inline description fail pre-admission with zero side effects.
    let fixture = admission_fixture(2).await;
    let sessions_before = fixture.engine.store().list_sessions().await.unwrap();
    let events_before = fixture.engine.replay(fixture.parent).await.unwrap();

    assert!(matches!(
        fixture
            .scoped_spawner()
            .spawn(
                operation(),
                vec![SpawnMember {
                    description: "unknown".into(),
                    prompt: "run".into(),
                    subagent_type: "missing-agent".into(),
                    inline_agent: Some(hya_tool::InlineAgent {
                        name: "inline-name".into(),
                        prompt: "P".into(),
                        ..hya_tool::InlineAgent::default()
                    }),
                    ..SpawnMember::default()
                }],
                Default::default(),
            )
            .await,
        Err(SpawnError::UnknownAgentId { ref agent_id }) if agent_id == "missing-agent"
    ));

    let bad_inline = SpawnMember {
        description: "bad".into(),
        prompt: "no".into(),
        subagent_type: "quick".into(),
        inline_agent: Some(hya_tool::InlineAgent {
            name: "inline-with-description".into(),
            prompt: "P".into(),
            description: Some("unsupported".into()),
            ..hya_tool::InlineAgent::default()
        }),
        ..SpawnMember::default()
    };
    for members in [
        vec![bad_inline.clone()],
        vec![
            SpawnMember {
                description: "ok".into(),
                prompt: "no".into(),
                subagent_type: "quick".into(),
                ..SpawnMember::default()
            },
            bad_inline,
        ],
    ] {
        assert!(matches!(
            fixture
                .scoped_spawner()
                .spawn(operation(), members, Default::default())
                .await,
            Err(SpawnError::UnsupportedInlineAgentField {
                field: "description"
            })
        ));
    }
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap(),
        sessions_before
    );
    assert_eq!(
        fixture.engine.replay(fixture.parent).await.unwrap(),
        events_before
    );
    assert!(fixture.resident.team_cancel(fixture.parent).is_none());
    assert_eq!(fixture.provider_calls.load(Ordering::SeqCst), 0);

    // 2. Authorized inline overlay runs without mutating catalog and retains its base `can_spawn` roster.
    let fixture = guidance_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &["plan"]),
            ("plan", AgentRole::Subagent, &[]),
        ],
        // The inline child, then its nested `plan`.
        vec![FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "task".into(),
                    input: json!({
                        "description": "nested plan",
                        "prompt": "plan next",
                        "subagent_type": "plan"
                    }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ])],
    )
    .await;
    let out = fixture
        .spawner
        .for_session_with_agents(fixture.parent, fixture.agents.clone())
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "inline quick".into(),
                prompt: "delegate once".into(),
                subagent_type: "quick".into(),
                inline_agent: Some(hya_tool::InlineAgent {
                    name: "inline-quick".into(),
                    prompt: "INLINE QUICK".into(),
                    ..hya_tool::InlineAgent::default()
                }),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(out[0].status, "running");
    wait_member_turn_done(&fixture.engine, &out[0].session).await;

    let binding = fixture.engine.bind_runtime(&fixture.agent.workdir).unwrap();
    assert!(binding.resolve_agent("inline-quick").is_none());
    assert_eq!(
        binding.resolve_agent("quick").unwrap().prompt,
        Some("quick prompt")
    );
    let mut spawned_agents = Vec::new();
    for s in fixture.engine.store().list_sessions().await.unwrap() {
        spawned_agents.push(
            fixture
                .engine
                .read_projection(s.session)
                .await
                .unwrap()
                .session
                .agent,
        );
    }
    assert!(
        spawned_agents
            .iter()
            .flatten()
            .any(|a| a.as_str() == "plan")
    );
}

#[tokio::test]
async fn admission_overload_and_registration_failures_leave_zero_side_effects() {
    // 1. Budget overload rejects cleanly before child/event/team creation.
    let overloaded = admission_fixture(0).await;
    let before = overloaded.engine.replay(overloaded.parent).await.unwrap();
    assert!(matches!(
        overloaded
            .scoped_spawner()
            .spawn(
                operation(),
                vec![SpawnMember {
                    description: "over".into(),
                    prompt: "no".into(),
                    subagent_type: "quick".into(),
                    ..SpawnMember::default()
                }],
                Default::default(),
            )
            .await,
        Err(SpawnError::Overloaded)
    ));
    assert_eq!(
        overloaded
            .engine
            .store()
            .list_sessions()
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        overloaded.engine.replay(overloaded.parent).await.unwrap(),
        before
    );
    assert!(overloaded.resident.team_cancel(overloaded.parent).is_none());

    // 2. Root registration failure aborts without child or actor-claim side effects.
    let db = AdmissionTempDb::new();
    let fixture =
        admission_fixture_with(1, None, SessionStore::connect(db.path()).await.unwrap()).await;
    let mut conn = SqliteConnection::connect(&format!("sqlite://{}", db.path()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_root_reg BEFORE INSERT ON event_log
         WHEN json_extract(NEW.payload, '$.kind') = 'agent_registered'
           AND json_extract(NEW.payload, '$.payload.handle') = 'main'
           AND json_extract(NEW.payload, '$.payload.agent_session') = json_extract(NEW.payload, '$.session')
         BEGIN SELECT RAISE(ABORT, 'root reg failure'); END;",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    assert!(matches!(
        fixture
            .scoped_spawner()
            .spawn(
                operation(),
                vec![SpawnMember {
                    description: "fail root".into(),
                    prompt: "no".into(),
                    subagent_type: "quick".into(),
                    ..SpawnMember::default()
                }],
                Default::default(),
            )
            .await,
        Err(SpawnError::Unavailable)
    ));
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        1
    );
    use hya_engine::SessionStoreExt as _;
    assert!(
        fixture
            .engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(fixture.resident.team_cancel(fixture.parent).is_none());

    // 3. Mid-batch mail failure rolls back every member and lazy main registration.
    let db2 = AdmissionTempDb::new();
    let fixture2 =
        admission_fixture_with(2, None, SessionStore::connect(db2.path()).await.unwrap()).await;
    let before2 = fixture2.engine.replay(fixture2.parent).await.unwrap();
    let mut conn2 = SqliteConnection::connect(&format!("sqlite://{}", db2.path()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_second BEFORE INSERT ON event_log
         WHEN json_extract(NEW.payload, '$.kind') = 'mail_sent'
           AND json_extract(NEW.payload, '$.payload.body') = 'reject second'
         BEGIN SELECT RAISE(ABORT, 'second failed'); END;",
    )
    .execute(&mut conn2)
    .await
    .unwrap();
    assert!(matches!(
        fixture2
            .scoped_spawner()
            .spawn(
                operation(),
                ["first", "reject second"]
                    .into_iter()
                    .map(|p| SpawnMember {
                        description: p.into(),
                        prompt: p.into(),
                        subagent_type: "quick".into(),
                        ..SpawnMember::default()
                    })
                    .collect(),
                Default::default(),
            )
            .await,
        Err(SpawnError::Unavailable)
    ));
    assert_eq!(
        fixture2.engine.replay(fixture2.parent).await.unwrap(),
        before2
    );
    assert_eq!(
        fixture2.engine.store().list_sessions().await.unwrap().len(),
        1
    );
    assert_eq!(
        fixture2
            .engine
            .store()
            .live_member_count(fixture2.parent)
            .await
            .unwrap(),
        0
    );
    fixture2.tasks.shutdown().await;
    fixture2
        .resident
        .drain(hya_proto::FinishCause::Shutdown, hya_engine::DRAIN_DEADLINE)
        .await;
}

#[tokio::test]
async fn member_row_concurrent_batch_live_provider_and_supervisor_io_shutdown() {
    // 1. Concurrent batch children enter provider concurrently, carry call_id/description, and register as residents.
    let gate = Arc::new(ProviderGate {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let fixture = admission_fixture_with(
        3,
        Some(gate.clone()),
        SessionStore::connect_memory().await.unwrap(),
    )
    .await;
    let call = ToolCallId::new();
    let outcomes = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.scoped_spawner().spawn(
            ToolOperation::from_tool_call(call),
            ["survey repo", "check tests"]
                .into_iter()
                .map(|label| SpawnMember {
                    description: label.into(),
                    prompt: label.into(),
                    subagent_type: "quick".into(),
                    ..SpawnMember::default()
                })
                .collect(),
            Default::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(|o| o.status == "running"));

    let child0: SessionId = outcomes[0].session.parse().unwrap();
    let proj = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    let members = hya_tool::extension::members_projection(&proj);
    let row = members
        .iter()
        .find(|r| r.child == Some(child0))
        .unwrap();
    assert_eq!(row.tool_call, Some(call));
    assert_eq!(row.description, "survey repo");
    assert_eq!(
        hya_tool::extension::team_projection(&proj)
            .roster
            .values()
            .find(|e| e.session == child0)
            .unwrap()
            .mode,
        SubagentMode::Resident
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.provider_calls.load(Ordering::SeqCst) < 2 {
            gate.entered.notified().await;
        }
    })
    .await
    .unwrap();
    gate.release.notify_waiters();
    for o in &outcomes {
        wait_member_turn_done(&fixture.engine, &o.session).await;
    }

    // 2. Spawn picks up providers published after supervisor startup.
    fixture.engine.publish_provider_catalog(
        Arc::new(ProviderRouter::new().with(Arc::new(hya_provider::DevProvider::new()))),
        fixture.engine.provider_catalog_snapshot(),
    );
    fixture
        .engine
        .runtime_registry()
        .publish_agent_model_preferences(std::collections::BTreeMap::from([(
            "quick".into(),
            ModelRef::new("hya/offline"),
        )]));
    let binding = fixture.engine.bind_runtime(&std::env::temp_dir()).unwrap();
    let live = fixture
        .sender
        .for_binding(&binding)
        .for_session_with_agents(fixture.parent, fixture.agents.clone())
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "live provider".into(),
                prompt: "hello".into(),
                subagent_type: "quick".into(),
                category: Some("live".into()),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .engine
            .read_projection(live[0].session.parse().unwrap())
            .await
            .unwrap()
            .session
            .model,
        Some(ModelRef::new("hya/offline"))
    );

    // 3. Supervisor shutdown waits for tracked connection I/O tasks.
    let io = fixture.tasks.io_tasks.clone();
    let (release, waiting) = tokio::sync::oneshot::channel();
    let reader = io.spawn(async move { waiting.await.unwrap() });
    let mut shutdown = Box::pin(fixture.tasks.shutdown());
    tokio::select! {
        biased;
        () = &mut shutdown => panic!("shutdown returned before I/O completed"),
        () = async { while !io.is_closed() { tokio::task::yield_now().await; } } => {}
    }
    assert!(!reader.is_finished());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), shutdown)
        .await
        .unwrap();
    assert!(reader.is_finished() && io.is_empty());
}

#[tokio::test]
async fn queued_spawn_uses_parent_turn_binding_after_catalog_publication() {
    const OLD_PROMPT: &str = "quick prompt";
    const NEW_PROMPT: &str = "NEW_CATALOG_CHILD_PROMPT";

    let by_session = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        by_session: by_session.clone(),
        tools_by_session: Arc::new(Mutex::new(Vec::new())),
        inner: FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]),
    })));
    let runtime = support::test_runtime(
        Arc::new(hya_tool_loader::builtins()),
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
    );
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, mut queued_rx) = BoundSpawnSender::with_capacity(1);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            runtime.clone(),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone()),
    );
    let workdir = temp_child_workdir("queued-parent-binding");
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "parent base".into(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: base.name.clone(),
            model: base.model.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let old_binding = runtime.bind_turn(&workdir).unwrap();
    let old_gen = old_binding.generation();
    let old_roster = engine
        .agent_roster_for_binding(&old_binding, "build")
        .unwrap();
    let spawner = spawn_sender.for_binding(&old_binding);
    let queued_spawn = tokio::spawn({
        let scoped = spawner.for_session_with_agents(parent, old_roster);
        async move {
            scoped
                .spawn(
                    operation(),
                    vec![SpawnMember {
                        description: "queued quick".into(),
                        prompt: "run with parent binding".into(),
                        subagent_type: "quick".into(),
                        ..SpawnMember::default()
                    }],
                    Default::default(),
                )
                .await
        }
    });
    let queued_request = queued_rx.recv().await.unwrap();

    let mut bundles: Vec<_> = old_binding
        .bundle_catalog()
        .bundles()
        .iter()
        .map(|b| b.as_ref().clone())
        .collect();
    let quick = bundles
        .iter_mut()
        .flat_map(|b| &mut b.agents)
        .find(|a| a.id.as_str() == "quick")
        .unwrap();
    assert_eq!(quick.prompt.as_deref(), Some(OLD_PROMPT));
    quick.prompt = Some(NEW_PROMPT.into());
    let new_cat =
        hya_engine::AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&bundles).unwrap()))
            .unwrap();
    runtime.publish_catalog(Arc::new(new_cat)).unwrap();
    let fresh_gen = runtime.bind_turn(&workdir).unwrap().generation();
    assert_ne!(fresh_gen, old_gen);

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(queued_request).await.unwrap();
    drop(tx);
    let resident = ResidentSupervisor::start(engine.clone());
    let _tasks = spawn_team_supervisor(
        rx,
        engine.clone(),
        base,
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    let outcomes = tokio::time::timeout(Duration::from_secs(5), queued_spawn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let child: SessionId = outcomes[0].session.parse().unwrap();
    wait_member_turn_done(&engine, &outcomes[0].session).await;

    let gens: Vec<_> = engine
        .replay(child)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .collect();
    assert_eq!(gens, vec![old_gen]);
    let captures = by_session.lock().unwrap();
    let sys = captures
        .iter()
        .find_map(|(s, p)| (*s == child).then_some(p))
        .unwrap();
    assert!(sys.contains(OLD_PROMPT) && !sys.contains(NEW_PROMPT));
}

#[tokio::test]
async fn guidance_is_ephemeral_immutable_and_propagates_across_turns_and_nested_spawns() {
    // 1. Root turn -> `task` tool -> child spawns nested `plan`, plus second mail wake after disk mutation.
    let fixture = guidance_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &["plan"]),
            ("plan", AgentRole::Subagent, &[]),
        ],
        // Root, then child; later turns and the grandchild get a bare stop.
        vec![
            // Root: invoke real `task` tool.
            FakeProvider::scripted_turns(vec![vec![
                FakeStep::ToolCall {
                    name: "task".into(),
                    input: json!({"description": "child", "prompt": "spawn plan", "subagent_type": "quick"}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]]),
            // Child: spawn nested `plan`.
            FakeProvider::scripted_turns(vec![vec![
                FakeStep::ToolCall {
                    name: "task".into(),
                    input: json!({"description": "nested", "prompt": "work", "subagent_type": "plan"}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ]]),
        ],
    )
    .await;
    std::fs::write(fixture.agent.workdir.join("AGENTS.md"), PROJECT_CONTEXT).unwrap();

    // Invalid batch with guidance has zero side effects.
    assert!(matches!(
        fixture
            .spawner
            .for_session_with_agents_and_guidance(
                fixture.parent,
                fixture.agents.clone(),
                Some(Arc::from(TRIGGER_GUIDANCE)),
            )
            .spawn(
                operation(),
                vec![
                    SpawnMember {
                        description: "ok".into(),
                        prompt: "ok".into(),
                        subagent_type: "quick".into(),
                        ..SpawnMember::default()
                    },
                    SpawnMember {
                        description: "bad".into(),
                        prompt: "bad".into(),
                        subagent_type: "missing".into(),
                        ..SpawnMember::default()
                    },
                ],
                Default::default(),
            )
            .await,
        Err(SpawnError::UnknownAgentId { .. })
    ));
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        1
    );

    // Run real root turn with triggering guidance.
    fixture
        .engine
        .admit_user_prompt(fixture.parent, "spawn guided tree".into())
        .await
        .unwrap();
    let finish = fixture
        .engine
        .run_turn_with_external_dirs_and_guidance(
            fixture.parent,
            &fixture.agent,
            Default::default(),
            &[],
            Some(Arc::from(TRIGGER_GUIDANCE)),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    // Wait for child + nested grandchild to both hit provider.
    let (child, child_handle, grandchild) = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let proj = fixture
                .engine
                .read_projection(fixture.parent)
                .await
                .unwrap();
            let team = hya_tool::extension::team_projection(&proj);
            let c = team
                .roster
                .iter()
                .find(|(_, e)| e.agent_type.as_str() == "quick");
            let g = team
                .roster
                .iter()
                .find(|(_, e)| e.agent_type.as_str() == "plan");
            if let (Some((ch, ce)), Some((_, ge))) = (c, g) {
                break (ce.session, ch.clone(), ge.session);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    fixture.wait_for_systems(child, 2).await;
    fixture.wait_for_systems(grandchild, 1).await;

    // Mutate AGENTS.md on disk, then send a second mail wake to the child.
    std::fs::write(fixture.agent.workdir.join("AGENTS.md"), POST_SPAWN_MUTATION).unwrap();
    fixture
        .engine
        .mail_send(
            fixture.parent,
            MailEndpoint::Handle(child_handle),
            MailKind::Message,
            "second wake".into(),
        )
        .await
        .unwrap();
    let child_systems = fixture.wait_for_systems(child, 3).await;
    let grand_systems = fixture.wait_for_systems(grandchild, 1).await;

    for system in child_systems.iter().chain(&grand_systems) {
        assert_eq!(system.matches(TRIGGER_GUIDANCE).count(), 1, "{system}");
        assert!(!system.contains(POST_SPAWN_MUTATION), "{system}");
    }
    for system in &child_systems {
        assert_eq!(system.matches(PROJECT_CONTEXT).count(), 1, "{system}");
    }

    // Triggering guidance is ephemeral — never persisted in parent or child events.
    let blob = format!(
        "{:?}{:?}",
        fixture.engine.replay(fixture.parent).await.unwrap(),
        fixture.engine.replay(child).await.unwrap()
    );
    assert!(!blob.contains(TRIGGER_GUIDANCE));
}

fn divergence_bundle(id: &str, role: AgentRole, prompt: &str, can_spawn: &[&str]) -> Bundle {
    Bundle {
        check: None,
        tui: None,
        format_version: 4,
        identity: BundleIdentity {
            id: format!("hya/div-{id}"),
            version: "0.0.0".into(),
            publisher: "hya-tests".into(),
        },
        namespace: None,
        digest: format!("test-only-{id}"),
        agents: vec![PreparedAgent {
            private: false,
            id: AgentName::new(id),
            description: None,
            role,
            color: None,
            prompt: Some(prompt.into()),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            resource_view: ResourceView::default(),
            can_spawn: can_spawn.iter().map(|s| AgentName::new(*s)).collect(),
            hook_refs: Vec::new(),
            advertise: Vec::new(),
            handoff: hya_bundle::HandoffPolicy::Model,
        }],
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
        workflow: None,
        channels: vec![],
        schemas: vec![],
        process: None,
        apis: vec![],
        commands: Vec::new(),
        runtime: None,
        permission_modes: vec![],
    }
}

#[tokio::test]
async fn nested_resident_main_synthesis_keeps_root_definition_and_guidance() {
    const ROOT_PROMPT: &str = "ROOT_MAIN_BUNDLE_PROMPT_MARKER";
    const NESTED_PROMPT: &str = "NESTED_CALLER_BUNDLE_PROMPT_MARKER";

    let by_session = Arc::new(Mutex::new(Vec::new()));
    let tools_by_session = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        by_session: by_session.clone(),
        tools_by_session: tools_by_session.clone(),
        inner: FakeProvider::scripted(Vec::new()),
    })));
    let tools: Arc<ToolRegistry> = Arc::new(hya_tool_loader::builtins());
    let bundles = vec![
        divergence_bundle(
            "root-main",
            AgentRole::Main,
            ROOT_PROMPT,
            &["planner", "root-only-helper"],
        ),
        divergence_bundle("planner", AgentRole::Subagent, NESTED_PROMPT, &["quick"]),
        divergence_bundle("quick", AgentRole::Subagent, "quick prompt", &[]),
        divergence_bundle(
            "root-only-helper",
            AgentRole::Subagent,
            "helper prompt",
            &[],
        ),
    ];
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(
            hya_engine::AgentCatalog::new(Arc::new(
                BundleCatalog::from_prepared(&bundles).unwrap(),
            ))
            .unwrap(),
        ),
    ));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Task, "*", Mode::Allow),
        Rule::new(Action::Read, "*", Mode::Allow),
    ]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            runtime,
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone())
        .with_governor(SubagentGovernor::new(SubagentLimits {
            per_run_budget: 16,
            ..SubagentLimits::default()
        })),
    );
    let workdir = temp_child_workdir("nested-root");
    std::fs::write(workdir.join("AGENTS.md"), PROJECT_CONTEXT).unwrap();
    let base = AgentSpec {
        name: AgentName::new("root-main"),
        model: ModelRef::new("fake"),
        system_prompt: "harness base must not win".into(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    let _tasks = spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        base,
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("root-main"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    let root_roster = engine
        .agent_roster_for_binding(&binding, "root-main")
        .unwrap();
    let nested_roster = engine
        .agent_roster_for_binding(&binding, "planner")
        .unwrap();
    assert!(root_roster.iter().any(|a| a.name == "root-only-helper"));
    assert!(!nested_roster.iter().any(|a| a.name == "root-only-helper"));

    let out = spawner
        .for_session_with_agents_and_guidance(root, root_roster, Some(Arc::from(TRIGGER_GUIDANCE)))
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "planner".into(),
                prompt: "prep".into(),
                subagent_type: "planner".into(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .unwrap();
    let nested: SessionId = out[0].session.parse().unwrap();
    spawner
        .for_session_with_agents_and_guidance(
            nested,
            nested_roster,
            Some(Arc::from(TRIGGER_GUIDANCE)),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "quick".into(),
                prompt: "go".into(),
                subagent_type: "quick".into(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let proj = engine.read_projection(root).await.unwrap();
            let has_synth = proj.session.messages.iter().any(|m| {
                matches!(m.role, hya_proto::Role::User)
                    && m.parts.iter().any(|p| matches!(p, hya_proto::PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED")))
            });
            if has_synth && by_session.lock().unwrap().iter().any(|(s, _)| *s == root) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    let main_systems: Vec<_> = by_session
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, _)| *s == root)
        .map(|(_, p)| p.clone())
        .collect();
    for sys in &main_systems {
        assert!(sys.contains(ROOT_PROMPT) && !sys.contains(NESTED_PROMPT));
        assert_eq!(sys.matches(TRIGGER_GUIDANCE).count(), 1);
        assert_eq!(sys.matches(PROJECT_CONTEXT).count(), 1);
    }
    let main_tools: Vec<_> = tools_by_session
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, _)| *s == root)
        .map(|(_, t)| t.clone())
        .collect();
    assert!(
        main_tools
            .iter()
            .all(|ts| ts.iter().any(|n| n == "task" || n == "read"))
    );
}

#[tokio::test]
async fn missing_root_definition_fails_before_admission_for_resident_batch() {
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        by_session: by_session.clone(),
        tools_by_session: Arc::new(Mutex::new(Vec::new())),
        inner: FakeProvider::scripted(Vec::new()),
    })));
    let bundles = vec![
        divergence_bundle("planner", AgentRole::Subagent, "planner prompt", &["quick"]),
        divergence_bundle("quick", AgentRole::Subagent, "quick prompt", &[]),
    ];
    let runtime = Arc::new(RuntimeRegistry::from_snapshot(
        hya_tool_loader::builtins().snapshot(),
        Arc::new(
            hya_engine::AgentCatalog::new(Arc::new(
                BundleCatalog::from_prepared(&bundles).unwrap(),
            ))
            .unwrap(),
        ),
    ));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router,
            runtime,
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone()),
    );
    let workdir = temp_child_workdir("missing-root");
    let resident = ResidentSupervisor::start(engine.clone());
    let _tasks = spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        AgentSpec {
            name: AgentName::new("general"),
            model: ModelRef::new("fake"),
            system_prompt: "base".into(),
            workdir: workdir.clone(),
            reasoning: None,
        },
        Arc::new(CategoryRegistry::default()),
        resident,
    );
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("ghost-root"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let nested = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("planner"),
            model: ModelRef::new("fake"),
            workdir: workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let roster = engine
        .agent_roster_for_binding(&binding, "planner")
        .unwrap();

    let result = spawn_sender
        .for_binding(&binding)
        .for_session_with_agents_and_guidance(nested, roster, Some(Arc::from(TRIGGER_GUIDANCE)))
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "res".into(),
                prompt: "no".into(),
                subagent_type: "quick".into(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await;
    assert!(matches!(
        result,
        Err(SpawnError::UnknownAgentId { ref agent_id }) if agent_id == "ghost-root"
    ));
    assert_eq!(engine.store().list_sessions().await.unwrap().len(), 2);
    assert!(by_session.lock().unwrap().is_empty());
}
