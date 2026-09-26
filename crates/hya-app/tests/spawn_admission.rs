//! Integration tests for `hya-app`: spawn admission.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_app::spawn_team_supervisor;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedAgentBundle,
    PreparedInstallableBundle, ResourceView,
};
use hya_core::{
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
use hya_store::{AdmissionState, SessionStore};
use hya_tool::{
    Action, AgentDef, Mode, PermissionPlane, PermissionRules, Rule, SpawnError, SpawnMember,
    SpawnerPlane, ToolOperation, ToolRegistry,
};
use serde_json::json;
use sqlx::{Connection, SqliteConnection};
use tokio::sync::Notify;

const TRIGGER_GUIDANCE: &str = "TRIGGERING_TURN_GUIDANCE_MARKER_0_34_8";
const CHILD_SCAN_POISON: &str = "CHILD_WORKDIR_SCAN_MUST_NOT_APPEAR";
const POST_SPAWN_MUTATION: &str = "POST_SPAWN_SOURCE_MUTATION_MARKER";

struct AdmissionFixture {
    engine: Arc<SessionEngine>,
    spawner: SpawnerPlane,
    parent: SessionId,
    provider_calls: Arc<AtomicUsize>,
    resident: Arc<ResidentSupervisor>,
    agents: Arc<[AgentDef]>,
}

struct AdmissionTempDb {
    path: String,
}

impl AdmissionTempDb {
    fn new() -> Self {
        let path = std::env::temp_dir()
            .join(format!("hya-app-spawn-admission-{}.db", SessionId::new()))
            .to_string_lossy()
            .into_owned();
        Self { path }
    }

    fn path(&self) -> &str {
        &self.path
    }
}

impl Drop for AdmissionTempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path));
        }
    }
}

impl AdmissionFixture {
    fn scoped_spawner(&self) -> SpawnerPlane {
        self.spawner
            .for_session_with_agents(self.parent, self.agents.clone())
    }
}

/// Wait until the spawned member (non-blocking handle reply, ADR-0015)
/// finishes its first resident turn: an assistant message exists and the
/// roster row is idle. Skips the roster's default-Idle pre-turn window.
async fn wait_member_turn_done(engine: &SessionEngine, member: &str) {
    let session: SessionId = member.parse().expect("member session id");
    let (root, _) = engine.session_lineage(session).await.unwrap();
    for _ in range_500() {
        let projection = engine.read_projection(root).await.unwrap();
        let turn_done = engine
            .read_projection(session)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .any(|message| message.role == hya_proto::Role::Assistant);
        let idle = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == session)
            .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle);
        if turn_done && idle {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("spawned member {member} never completed its first turn");
}

fn range_500() -> std::ops::Range<i32> {
    0..500
}

struct CountingProvider {
    calls: Arc<AtomicUsize>,
    inner: FakeProvider,
    gate: Option<Arc<ProviderGate>>,
}

struct ProviderGate {
    entered: Notify,
    release: Notify,
}

fn operation() -> ToolOperation {
    ToolOperation::from_tool_call(ToolCallId::new())
}

#[async_trait::async_trait]
impl Provider for CountingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
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

#[tokio::test]
async fn background_transient_overload_prevents_child_creation() {
    background_overload_prevents_child_creation(false).await;
}

#[tokio::test]
async fn background_resident_overload_prevents_child_creation() {
    background_overload_prevents_child_creation(true).await;
}

#[tokio::test]
async fn explicit_unknown_inline_target_creates_no_child() {
    let fixture = admission_fixture(1).await;
    let operation = operation();
    let result = fixture
        .scoped_spawner()
        .spawn_background(
            operation,
            vec![SpawnMember {
                description: "unknown target".to_string(),
                prompt: "run once".to_string(),
                subagent_type: "missing-agent".to_string(),
                inline_agent: Some(hya_tool::InlineAgent {
                    name: "inline-name".to_string(),
                    prompt: "INLINE PROMPT".to_string(),
                    ..hya_tool::InlineAgent::default()
                }),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await;

    assert!(matches!(
        result,
        Err(SpawnError::UnknownAgentId { ref agent_id }) if agent_id == "missing-agent"
    ));
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        1
    );
    assert!(
        fixture
            .engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .is_none(),
        "authorization failure must precede durable admission"
    );
}

#[tokio::test]
async fn inline_description_is_unsupported_before_admission_without_side_effects() {
    for foreground in [false, true] {
        let fixture = admission_fixture(if foreground { 2 } else { 1 }).await;
        let operation = operation();
        let sessions_before = fixture.engine.store().list_sessions().await.unwrap();
        let events_before = fixture.engine.replay(fixture.parent).await.unwrap();
        let projection_before = fixture
            .engine
            .read_projection(fixture.parent)
            .await
            .unwrap();
        let admission_counts_before = fixture.engine.store().admission_counts().await.unwrap();
        let remaining_budget_before = fixture
            .engine
            .governor()
            .unwrap()
            .remaining_budget(fixture.parent);
        let active_actors_before = fixture.engine.store().active_actor_ids().await.unwrap();
        let binding_before = fixture.engine.bind_runtime(&std::env::temp_dir()).unwrap();
        let quick_before = binding_before
            .resolve_agent("quick")
            .expect("quick must exist in fixture catalog")
            .clone();
        assert_eq!(fixture.provider_calls.load(Ordering::SeqCst), 0);
        assert!(fixture.resident.team_cancel(fixture.parent).is_none());

        let invalid = SpawnMember {
            description: "inline description rejected".to_string(),
            prompt: "must not start".to_string(),
            subagent_type: "quick".to_string(),
            inline_agent: Some(hya_tool::InlineAgent {
                name: "inline-with-description".to_string(),
                prompt: "INLINE WITH DESCRIPTION".to_string(),
                description: Some("unsupported request overlay description".to_string()),
                ..hya_tool::InlineAgent::default()
            }),
            ..SpawnMember::default()
        };
        let members = if foreground {
            vec![
                SpawnMember {
                    description: "valid foreground member".to_string(),
                    prompt: "must not start".to_string(),
                    subagent_type: "quick".to_string(),
                    ..SpawnMember::default()
                },
                invalid,
            ]
        } else {
            vec![invalid]
        };
        let result = if foreground {
            fixture
                .scoped_spawner()
                .spawn(operation, members, Default::default())
                .await
        } else {
            fixture
                .scoped_spawner()
                .spawn_background(operation, members, Default::default())
                .await
        };

        assert!(
            matches!(
                result,
                Err(SpawnError::UnsupportedInlineAgentField {
                    field: "description"
                })
            ),
            "expected typed UnsupportedInlineAgentField {{ field: description }}, got {result:?}"
        );

        let sessions_after = fixture.engine.store().list_sessions().await.unwrap();
        let events_after = fixture.engine.replay(fixture.parent).await.unwrap();
        let projection_after = fixture
            .engine
            .read_projection(fixture.parent)
            .await
            .unwrap();
        let admission_counts_after = fixture.engine.store().admission_counts().await.unwrap();
        let remaining_budget_after = fixture
            .engine
            .governor()
            .unwrap()
            .remaining_budget(fixture.parent);
        let active_actors_after = fixture.engine.store().active_actor_ids().await.unwrap();
        let binding_after = fixture.engine.bind_runtime(&std::env::temp_dir()).unwrap();
        let quick_after = binding_after
            .resolve_agent("quick")
            .expect("quick must remain in catalog");

        assert_eq!(
            sessions_after, sessions_before,
            "unsupported inline description must not create a child session"
        );
        assert_eq!(
            events_after, events_before,
            "unsupported inline description must not append parent/child events"
        );
        assert_eq!(
            projection_after, projection_before,
            "unsupported inline description must not change the parent projection"
        );
        assert_eq!(
            admission_counts_after, admission_counts_before,
            "unsupported inline description must not change durable admission counts"
        );
        assert_eq!(
            remaining_budget_after, remaining_budget_before,
            "unsupported inline description must not debit the governor"
        );
        assert_eq!(
            active_actors_after, active_actors_before,
            "unsupported inline description must not claim an actor"
        );
        assert!(
            fixture
                .engine
                .store()
                .admission(operation.operation_id())
                .await
                .unwrap()
                .is_none(),
            "unsupported inline description must precede durable admission"
        );
        assert!(
            fixture
                .engine
                .store()
                .admissions(operation.operation_id())
                .await
                .unwrap()
                .is_empty(),
            "unsupported inline description must leave no admission rows"
        );
        assert_eq!(
            fixture.provider_calls.load(Ordering::SeqCst),
            0,
            "unsupported inline description must not start a provider turn"
        );
        assert!(
            fixture.resident.team_cancel(fixture.parent).is_none(),
            "unsupported inline description must not create resident supervisor state"
        );
        assert!(
            binding_after
                .resolve_agent("inline-with-description")
                .is_none(),
            "request overlay name must not become a catalog entry"
        );
        assert_eq!(
            quick_after.prompt, quick_before.prompt,
            "catalog agent prompt must be unchanged"
        );
        assert_eq!(
            quick_after.stable_id, quick_before.stable_id,
            "catalog agent identity must be unchanged"
        );
        assert_eq!(
            quick_after.model_policy, quick_before.model_policy,
            "catalog agent model policy must be unchanged"
        );
    }
}

#[tokio::test]
async fn authorized_inline_overlay_executes_without_catalog_entry() {
    let fixture = admission_fixture(1).await;
    let result = fixture
        .scoped_spawner()
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "authorized inline".to_string(),
                prompt: "run inline".to_string(),
                subagent_type: "quick".to_string(),
                inline_agent: Some(hya_tool::InlineAgent {
                    name: "inline-one".to_string(),
                    prompt: "INLINE ONE".to_string(),
                    ..hya_tool::InlineAgent::default()
                }),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("authorized inline spawn");

    assert_eq!(result[0].status, "running", "{result:?}");
    wait_member_turn_done(&fixture.engine, &result[0].session).await;
    // Under the unified model the member's first episode is one provider
    // call; main-as-actor quiescence synthesis may add one more. Pin the
    // child's turn (>= 1) instead of the exact total.
    assert!(
        fixture.provider_calls.load(Ordering::SeqCst) >= 1,
        "the spawned inline member must run at least one provider turn"
    );
    let binding = fixture.engine.bind_runtime(&std::env::temp_dir()).unwrap();
    assert!(binding.resolve_agent("inline-one").is_none());
    assert_eq!(
        binding.resolve_agent("quick").unwrap().prompt,
        Some("quick prompt")
    );
}

#[tokio::test]
async fn inline_child_spawns_through_its_authorized_base_roster() {
    let store = SessionStore::connect_memory().await.unwrap();
    let provider = Arc::new(CountingProvider {
        calls: Arc::new(AtomicUsize::new(0)),
        inner: FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "task".to_string(),
                    input: json!({
                        "description": "nested plan",
                        "prompt": "plan the next step",
                        "subagent_type": "plan"
                    }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ]),
        gate: None,
    });
    let provider_router = Arc::new(ProviderRouter::new().with(provider));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Task,
        "*",
        Mode::Allow,
    )]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(2);
    let engine = Arc::new(
        SessionEngine::new(
            store,
            provider_router.clone(),
            support::test_runtime(
                Arc::new(ToolRegistry::builtins()),
                &[
                    ("build", AgentRole::Main, &["quick"]),
                    ("general", AgentRole::Main, &[]),
                    ("quick", AgentRole::Subagent, &["plan"]),
                    ("plan", AgentRole::Subagent, &[]),
                ],
            ),
            permission,
            EventBus::default(),
        )
        .with_spawn_sender(spawn_sender.clone()),
    );
    let base = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        base.clone(),
        provider_router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: base.name,
            model: base.model,
            workdir: base.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();

    let binding = engine.bind_runtime(&std::env::temp_dir()).unwrap();
    let root_agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    let outcome = spawner
        .for_session_with_agents(parent, root_agents)
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "inline quick".to_string(),
                prompt: "delegate once".to_string(),
                subagent_type: "quick".to_string(),
                inline_agent: Some(hya_tool::InlineAgent {
                    name: "inline-quick".to_string(),
                    prompt: "INLINE QUICK".to_string(),
                    ..hya_tool::InlineAgent::default()
                }),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("authorized inline spawn");
    assert_eq!(outcome[0].status, "running", "{outcome:?}");
    wait_member_turn_done(&engine, &outcome[0].session).await;

    let sessions = engine.store().list_sessions().await.unwrap();
    let mut agents = Vec::new();
    for session in &sessions {
        let projection = engine.read_projection(session.session).await.unwrap();
        agents.push(projection.session.agent);
    }
    assert!(
        agents
            .iter()
            .flatten()
            .any(|agent| agent.as_str() == "plan"),
        "inline child did not retain quick's authorized plan target: {agents:?}"
    );
}
/// Regression for the release window between member-journal finalize and the
/// owner's governor release.
///
/// A member task can publish `Completed + logical_released` before the owner
/// reaches its idempotent in-memory governor refund. A same-root spawn that
/// enters during that window must reconcile whole terminal operations and
/// retry its exact reservation instead of returning a false `Overloaded`.
///
/// The query deliberately requires every declared batch member to be terminal;
/// releasing after one member would return the operation's full cardinality
/// while siblings still run.
#[tokio::test]
async fn released_capacity_is_visible_to_a_concurrent_spawn_on_the_same_root() {
    const RUNS: usize = 20;
    for run in 0..RUNS {
        let fixture = admission_fixture(1).await;
        let first_operation = operation();
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture.scoped_spawner().spawn_background(
                first_operation,
                vec![SpawnMember {
                    description: "single reservation".to_string(),
                    prompt: "finish once".to_string(),
                    subagent_type: "quick".to_string(),
                    ..SpawnMember::default()
                }],
                Default::default(),
            ),
        )
        .await
        .expect("spawn timed out")
        .expect("first spawn should be admitted");

        // Wait for the member task to make the release durably visible, then spawn
        // again immediately -- the journal is the contract a caller can observe.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let record = fixture
                    .engine
                    .store()
                    .admission(first_operation.operation_id())
                    .await
                    .unwrap()
                    .unwrap();
                if record.state == AdmissionState::Completed && record.logical_released {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("journal did not finalize");

        let second = tokio::time::timeout(
            Duration::from_secs(5),
            fixture.scoped_spawner().spawn_background(
                operation(),
                vec![SpawnMember {
                    description: "concurrent".to_string(),
                    prompt: "second".to_string(),
                    subagent_type: "quick".to_string(),
                    ..SpawnMember::default()
                }],
                Default::default(),
            ),
        )
        .await
        .expect("second spawn timed out");
        assert!(
            second.is_ok(),
            "run {run}: capacity the journal reports as logically released must be \
             admissible on the same root, got {second:?}"
        );
    }
}

#[tokio::test]
async fn admitted_background_resident_uses_the_common_pre_create_path() {
    let fixture = admission_fixture(1).await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.scoped_spawner().spawn_background(
            operation(),
            vec![SpawnMember {
                description: "resident admission".to_string(),
                prompt: "wait for mail".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        ),
    )
    .await
    .expect("spawn timed out")
    .expect("resident spawn should be admitted");
    let child: SessionId = outcome[0].session.parse().expect("valid child session");

    assert_eq!(outcome[0].status, "running");
    assert!(fixture.resident.team_cancel(fixture.parent).is_some());
    let projection = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    let resident = projection
        .team
        .roster
        .values()
        .find(|entry| entry.session == child)
        .expect("resident must be registered in the root roster");
    assert_eq!(resident.mode, SubagentMode::Resident);
    assert!(
        fixture
            .engine
            .store()
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .any(|info| info.session == child)
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.provider_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admitted resident did not reach its provider turn");
}

#[tokio::test]
async fn resident_root_registration_failure_aborts_without_child_side_effects() {
    let database = AdmissionTempDb::new();
    let store = SessionStore::connect(database.path()).await.unwrap();
    let fixture = admission_fixture_with_store(1, store).await;
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", database.path()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER test_root_agent_registration_failure
         BEFORE INSERT ON event_log
         WHEN json_extract(NEW.payload, '$.type') = 'agent_registered'
           AND json_extract(NEW.payload, '$.handle') = 'main'
           AND json_extract(NEW.payload, '$.agent_session') = json_extract(NEW.payload, '$.session')
         BEGIN SELECT RAISE(ABORT, 'test root registration failure'); END;",
    )
    .execute(&mut connection)
    .await
    .unwrap();

    let operation = operation();
    let result = fixture
        .scoped_spawner()
        .spawn_background(
            operation,
            vec![SpawnMember {
                description: "root registration failure".to_string(),
                prompt: "must not run".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("resident spawn should return an outcome");

    assert_eq!(result.len(), 1, "{result:?}");
    assert_eq!(result[0].status, "failed", "{result:?}");

    let record = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixture
                .engine
                .store()
                .admission(operation.operation_id())
                .await
                .unwrap()
                .expect("admission record");
            if record.state.is_terminal() {
                break record;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admission did not finalize");

    let root_events = fixture.engine.replay(fixture.parent).await.unwrap();
    assert!(
        !root_events
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::AgentRegistered { .. }))
    );
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        1,
        "root registration failure must not create a child session"
    );
    assert_eq!(
        fixture.provider_calls.load(Ordering::SeqCst),
        0,
        "root registration failure must not poll a provider"
    );
    assert!(
        fixture.resident.team_cancel(fixture.parent).is_none(),
        "root registration failure must not create a resident team slot"
    );
    assert!(
        fixture
            .engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .is_empty(),
        "root registration failure must release any resident actor claim"
    );
    assert_eq!(record.state, hya_store::AdmissionState::Aborted);
    assert!(record.logical_released);
}

#[tokio::test]
async fn foreground_completion_uses_one_debit_and_one_finalize() {
    let fixture = admission_fixture(1).await;
    let operation = operation();

    let outcome = fixture
        .scoped_spawner()
        .spawn(
            operation,
            vec![SpawnMember {
                description: "foreground admission".to_string(),
                prompt: "finish".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("foreground spawn");

    assert_eq!(outcome.len(), 1);
    let record = fixture
        .engine
        .store()
        .admission(operation.operation_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, hya_store::AdmissionState::Completed);
    assert!(record.logical_released);
    assert_eq!(
        fixture
            .engine
            .governor()
            .unwrap()
            .remaining_budget(fixture.parent),
        1
    );
}
async fn background_overload_prevents_child_creation(_resident_member: bool) {
    let fixture = admission_fixture(0).await;
    let operation = operation();
    let sessions_before = fixture.engine.store().list_sessions().await.unwrap();
    let events_before = fixture.engine.replay(fixture.parent).await.unwrap();
    let projection_before = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    assert!(fixture.resident.team_cancel(fixture.parent).is_none());
    assert_eq!(fixture.provider_calls.load(Ordering::SeqCst), 0);

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.scoped_spawner().spawn_background(
            operation,
            vec![SpawnMember {
                description: "overloaded member".to_string(),
                prompt: "must not start".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        ),
    )
    .await
    .expect("spawn timed out");
    let sessions_after = fixture.engine.store().list_sessions().await.unwrap();
    let events_after = fixture.engine.replay(fixture.parent).await.unwrap();
    let projection_after = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();

    assert_eq!(
        sessions_after, sessions_before,
        "admission denial must not append a child session event"
    );
    assert_eq!(
        events_after, events_before,
        "admission denial must not append a parent member/roster event"
    );
    assert_eq!(
        projection_after, projection_before,
        "admission denial must not change the parent projection"
    );
    assert!(
        fixture.resident.team_cancel(fixture.parent).is_none(),
        "admission denial must not create resident supervisor state"
    );
    assert_eq!(
        fixture.provider_calls.load(Ordering::SeqCst),
        0,
        "admission denial must not start a provider turn"
    );
    assert!(matches!(result, Err(SpawnError::Overloaded)));
    let record = fixture
        .engine
        .store()
        .admission(operation.operation_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, hya_store::AdmissionState::Aborted);
    assert!(
        !record.logical_released,
        "an accepted-but-never-debited overload must not release capacity"
    );
}

#[tokio::test]
async fn duplicate_operation_never_dispatches_or_creates_a_second_child() {
    let fixture = admission_fixture(2).await;
    let operation = operation();
    let member = SpawnMember {
        description: "idempotent dispatch".to_string(),
        prompt: "run only once".to_string(),
        subagent_type: "quick".to_string(),
        ..SpawnMember::default()
    };

    fixture
        .scoped_spawner()
        .spawn_background(operation, vec![member.clone()], Default::default())
        .await
        .expect("first dispatch");
    let mut sessions_after_first: Vec<SessionId> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session)
        .collect();
    sessions_after_first.sort();

    let duplicate = fixture
        .scoped_spawner()
        .spawn_background(operation, vec![member], Default::default())
        .await;
    let mut sessions_after_duplicate: Vec<SessionId> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session)
        .collect();
    sessions_after_duplicate.sort();

    assert!(matches!(
        duplicate,
        Err(SpawnError::OperationAlreadyHandled)
    ));
    assert_eq!(sessions_after_duplicate, sessions_after_first);
}

#[tokio::test]
async fn reused_operation_with_different_request_fails_closed() {
    let fixture = admission_fixture(2).await;
    let operation = operation();
    fixture
        .scoped_spawner()
        .spawn_background(
            operation,
            vec![SpawnMember {
                description: "original".to_string(),
                prompt: "first immutable request".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("first request");
    let mut sessions_before_conflict: Vec<SessionId> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session)
        .collect();
    sessions_before_conflict.sort();

    let conflict = fixture
        .scoped_spawner()
        .spawn_background(
            operation,
            vec![SpawnMember {
                description: "changed".to_string(),
                prompt: "different immutable request".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await;
    let mut sessions_after_conflict: Vec<SessionId> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session)
        .collect();
    sessions_after_conflict.sort();

    assert!(matches!(conflict, Err(SpawnError::OperationIdConflict)));
    assert_eq!(sessions_after_conflict, sessions_before_conflict);
}

#[tokio::test]
async fn concurrent_retry_debits_and_dispatches_only_once() {
    let fixture = admission_fixture(2).await;
    let operation = operation();
    let member = SpawnMember {
        description: "concurrent retry".to_string(),
        prompt: "dispatch once".to_string(),
        subagent_type: "quick".to_string(),
        ..SpawnMember::default()
    };
    let left_plane = fixture.scoped_spawner();
    let right_plane = fixture.scoped_spawner();

    let (left, right) = tokio::join!(
        left_plane.spawn_background(operation, vec![member.clone()], Default::default()),
        right_plane.spawn_background(operation, vec![member], Default::default())
    );

    assert_eq!(
        [left.as_ref(), right.as_ref()]
            .into_iter()
            .filter(|result| result.is_ok())
            .count(),
        1
    );
    assert_eq!(
        [left.as_ref(), right.as_ref()]
            .into_iter()
            .filter(|result| matches!(result, Err(SpawnError::OperationAlreadyHandled)))
            .count(),
        1,
        "left={left:?}, right={right:?}"
    );
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        2,
        "one parent and one child only"
    );
}

async fn admission_fixture(per_run_budget: u64) -> AdmissionFixture {
    admission_fixture_with_store_and_gate(
        per_run_budget,
        None,
        SessionStore::connect_memory().await.unwrap(),
    )
    .await
}
async fn admission_fixture_with_store(
    per_run_budget: u64,
    store: SessionStore,
) -> AdmissionFixture {
    admission_fixture_with_store_and_gate(per_run_budget, None, store).await
}

async fn admission_fixture_with_store_and_gate(
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
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(2);
    let engine = Arc::new(
        SessionEngine::new(
            store,
            provider_router.clone(),
            support::test_runtime(
                Arc::new(ToolRegistry::builtins()),
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
        system_prompt: "test".to_string(),
        workdir: std::env::temp_dir(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        provider_router,
        Arc::new(CategoryRegistry::default()),
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
        engine,
        spawner,
        parent,
        provider_calls,
        resident,
        agents,
    }
}

/// Session-scoped tool-name captures used by resource-policy proofs.
type ToolsBySession = Arc<Mutex<Vec<(SessionId, Vec<String>)>>>;

/// Captures every provider request system prompt for guidance propagation proofs.
struct CaptureSystemsProvider {
    systems: Arc<Mutex<Vec<String>>>,
    /// Session-scoped captures for root-vs-child proofs.
    by_session: Arc<Mutex<Vec<(SessionId, String)>>>,
    /// Session-scoped tool names for resource-policy proofs on main synthesis.
    tools_by_session: ToolsBySession,
    inner: FakeProvider,
}

#[async_trait::async_trait]
impl Provider for CaptureSystemsProvider {
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
        let system = request.system.clone().unwrap_or_default();
        let tool_names: Vec<String> = request
            .tools
            .iter()
            .map(|t| t.name.as_str().to_string())
            .collect();
        self.systems.lock().unwrap().push(system.clone());
        self.by_session.lock().unwrap().push((session, system));
        self.tools_by_session
            .lock()
            .unwrap()
            .push((session, tool_names));
        self.inner.stream(request, session, message).await
    }
}

#[tokio::test]
async fn queued_spawn_uses_parent_turn_binding_after_catalog_publication() {
    const OLD_QUICK_PROMPT: &str = "quick prompt";
    const NEW_CATALOG_CHILD_PROMPT: &str = "NEW_CATALOG_CHILD_PROMPT";

    let systems = Arc::new(Mutex::new(Vec::new()));
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let tools_by_session = Arc::new(Mutex::new(Vec::new()));
    let router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        systems,
        by_session: by_session.clone(),
        tools_by_session,
        inner: FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]),
    })));
    let runtime = support::test_runtime(
        Arc::new(ToolRegistry::builtins()),
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
    );
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawn_sender, mut queued_rx) = BoundSpawnSender::with_capacity(1);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            router.clone(),
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
        system_prompt: "parent base".to_string(),
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
    let old_generation = old_binding.generation();
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
                        description: "queued quick".to_string(),
                        prompt: "run with the parent binding".to_string(),
                        subagent_type: "quick".to_string(),
                        ..SpawnMember::default()
                    }],
                    Default::default(),
                )
                .await
        }
    });
    let queued_request = queued_rx.recv().await.expect("queued spawn request");

    let mut published_bundles = old_binding.bundle_catalog().bundles().to_vec();
    let quick = published_bundles
        .iter_mut()
        .filter_map(|bundle| match bundle {
            PreparedInstallableBundle::Agent(bundle) => Some(&mut bundle.agent),
            PreparedInstallableBundle::AgentSet(_)
            | PreparedInstallableBundle::Workflow(_)
            | PreparedInstallableBundle::Plugin(_) => None,
        })
        .find(|agent| agent.id.as_str() == "quick")
        .expect("quick agent in old prepared catalog");
    assert_eq!(quick.prompt.as_deref(), Some(OLD_QUICK_PROMPT));
    quick.prompt = Some(NEW_CATALOG_CHILD_PROMPT.to_string());
    // Deliberately UNVERIFIED: this catalog exists only to prove the already-queued
    // request keeps using the parent's pinned TurnBinding (which came from the
    // verified `support::test_runtime` catalog above). Do not "fix" this to
    // `from_verified_catalogs` — it also cannot be: the bundles were hand-mutated
    // just above, and the verified constructors take `&[&PreparedCatalog]`.
    let published_catalog =
        BundleCatalog::from_prepared(&published_bundles).expect("complete replacement catalog");
    let published_catalog = hya_core::AgentCatalog::new(Arc::new(published_catalog))
        .expect("replacement agent catalog");
    runtime
        .publish_catalog(Arc::new(published_catalog))
        .expect("publish replacement catalog");

    assert_eq!(
        old_binding
            .resolve_agent("quick")
            .and_then(|agent| agent.prompt),
        Some(OLD_QUICK_PROMPT),
        "the parent TurnBinding must remain pinned"
    );
    let fresh_binding = runtime.bind_turn(&workdir).unwrap();
    let fresh_generation = fresh_binding.generation();
    assert_ne!(
        fresh_generation, old_generation,
        "catalog publication must advance the runtime generation"
    );
    assert_eq!(
        fresh_binding
            .resolve_agent("quick")
            .and_then(|agent| agent.prompt),
        Some(NEW_CATALOG_CHILD_PROMPT),
        "a fresh TurnBinding must observe the published catalog"
    );

    let (forward_tx, forward_rx) = tokio::sync::mpsc::channel(1);
    forward_tx
        .send(queued_request)
        .await
        .expect("forward retained queued request");
    drop(forward_tx);
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        forward_rx,
        engine.clone(),
        base,
        router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    // Bound the join itself: a supervisor that never replies must fail this test
    // rather than hang the whole binary.
    let outcomes = tokio::time::timeout(Duration::from_secs(5), queued_spawn)
        .await
        .expect("queued foreground spawn timed out")
        .expect("queued spawn task")
        .expect("queued foreground spawn");
    let child: SessionId = outcomes[0].session.parse().expect("child session id");
    wait_member_turn_done(&engine, &outcomes[0].session).await;
    let child_binding_generations: Vec<_> = engine
        .replay(child)
        .await
        .expect("replay child binding events")
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::TurnBindingRecorded { generation, .. } => Some(generation),
            _ => None,
        })
        .collect();
    assert_eq!(
        child_binding_generations.len(),
        1,
        "child execution must record exactly one turn binding: {child_binding_generations:?}"
    );
    assert_eq!(
        child_binding_generations[0], old_generation,
        "child execution must record the parent turn's retained generation"
    );
    assert_ne!(
        child_binding_generations[0], fresh_generation,
        "child execution must not record the post-publication generation"
    );
    let captures = by_session.lock().unwrap();
    let child_system = captures
        .iter()
        .find_map(|(session, system)| (*session == child).then_some(system))
        .expect("child provider system prompt");
    assert!(
        child_system.contains(OLD_QUICK_PROMPT),
        "queued child must use the parent binding's OLD prompt: {child_system}"
    );
    assert!(
        !child_system.contains(NEW_CATALOG_CHILD_PROMPT),
        "queued child must not rebind to the NEW catalog prompt: {child_system}"
    );
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

struct GuidanceSpawnFixture {
    engine: Arc<SessionEngine>,
    spawner: SpawnerPlane,
    parent: SessionId,
    /// Parent/base AgentSpec used for `run_turn` (same workdir as session).
    agent: AgentSpec,
    agents: Arc<[AgentDef]>,
    systems: Arc<Mutex<Vec<String>>>,
    by_session: Arc<Mutex<Vec<(SessionId, String)>>>,
}

async fn guidance_spawn_fixture(
    agents: &[(&str, AgentRole, &[&str])],
    scripted: FakeProvider,
) -> GuidanceSpawnFixture {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let tools_by_session = Arc::new(Mutex::new(Vec::new()));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        systems: systems.clone(),
        by_session: by_session.clone(),
        tools_by_session,
        inner: scripted,
    })));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Task, "*", Mode::Allow),
        Rule::new(Action::Read, "*", Mode::Allow),
    ]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router.clone(),
            support::test_runtime(Arc::new(ToolRegistry::builtins()), agents),
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
    // Poison file only on a *child* path used as AgentSpec.workdir below; the
    // base agent workdir is separate so root discovery is irrelevant.
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "parent base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        provider_router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );
    let parent = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let roster = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    GuidanceSpawnFixture {
        engine,
        spawner,
        parent,
        agent,
        agents: roster,
        systems,
        by_session,
    }
}

#[tokio::test]
async fn transient_child_uses_triggering_turn_guidance_once_without_child_scan() {
    // Two provider rounds on the child: guidance must appear once per system
    // string (composed once) and never include child-workdir scan content.
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "read".to_string(),
                    input: json!({"path": "noop.txt"}),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
        ]),
    )
    .await;

    // Child inherits base agent workdir; poison AGENTS.md before spawn so a
    // child filesystem discovery would pick it up.
    let parent_proj = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    let child_wd = parent_proj
        .session
        .workdir
        .as_deref()
        .expect("parent workdir");
    std::fs::write(PathBuf::from(child_wd).join("AGENTS.md"), CHILD_SCAN_POISON).unwrap();

    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    let outcomes = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance.clone()),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "guided child".to_string(),
                prompt: "do work".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("transient spawn");
    assert_eq!(outcomes[0].status, "running", "{outcomes:?}");
    wait_member_turn_done(&fixture.engine, &outcomes[0].session).await;

    let systems = fixture.systems.lock().unwrap().clone();
    assert!(
        systems.len() >= 2,
        "child must run multiple provider rounds: {systems:?}"
    );
    for system in &systems {
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "child system must carry triggering-turn guidance: {system}"
        );
        assert_eq!(
            system.matches(TRIGGER_GUIDANCE).count(),
            1,
            "guidance composed once into system: {system}"
        );
        assert!(
            !system.contains(CHILD_SCAN_POISON),
            "child must not discover workdir AGENTS.md: {system}"
        );
    }
    // Drop the original Arc; child still held clones for the turn.
    drop(guidance);
}

#[tokio::test]
async fn resident_activations_reuse_in_process_triggering_guidance() {
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted(Vec::new()),
    )
    .await;

    // Mutable source string used only to build the initial Arc; mutating the
    // source after spawn must not alter the in-process Arc text.
    let mut source = TRIGGER_GUIDANCE.to_string();
    let guidance: Arc<str> = Arc::from(source.as_str());

    let outcomes = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance.clone()),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "resident worker".to_string(),
                prompt: "initial resident directive".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("resident spawn");
    assert_eq!(outcomes[0].status, "running", "{outcomes:?}");
    let child: SessionId = outcomes[0].session.parse().unwrap();
    let handle = outcomes[0].member.clone();

    // Wait for first activation (initial directive).
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !fixture.systems.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first resident activation");

    // Mutate the original source and a disk file after spawn; neither may leak.
    source.push_str(POST_SPAWN_MUTATION);
    let child_proj = fixture.engine.read_projection(child).await.unwrap();
    if let Some(wd) = child_proj.session.workdir.as_deref() {
        let _ = std::fs::write(PathBuf::from(wd).join("AGENTS.md"), POST_SPAWN_MUTATION);
    }

    // Second activation via mail.
    fixture
        .engine
        .mail_send(
            fixture.parent,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "second wake".to_string(),
        )
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.systems.lock().unwrap().len() >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second resident activation");

    let systems = fixture.systems.lock().unwrap().clone();
    assert!(systems.len() >= 2, "expected >=2 activations: {systems:?}");
    for system in &systems {
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "every activation reuses in-process guidance: {system}"
        );
        assert!(
            !system.contains(POST_SPAWN_MUTATION),
            "post-spawn source/disk mutation must not alter guidance: {system}"
        );
        assert!(
            !system.contains(CHILD_SCAN_POISON),
            "resident must not rescan child workdir: {system}"
        );
    }
}

#[tokio::test]
async fn nested_spawn_inherits_same_immutable_guidance() {
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &["plan"]),
            ("plan", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted_turns(vec![
            // Child (quick): spawn nested plan, then stop.
            vec![
                FakeStep::ToolCall {
                    name: "task".to_string(),
                    input: json!({
                        "description": "nested plan",
                        "prompt": "plan next",
                        "subagent_type": "plan"
                    }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![FakeStep::Finish(FinishReason::Stop)],
            // Grandchild (plan).
            vec![FakeStep::Finish(FinishReason::Stop)],
        ]),
    )
    .await;

    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    let outcomes = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "nested root child".to_string(),
                prompt: "spawn a plan child".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("nested parent spawn");
    assert_eq!(outcomes[0].status, "running", "{outcomes:?}");
    wait_member_turn_done(&fixture.engine, &outcomes[0].session).await;

    let systems = fixture.systems.lock().unwrap().clone();
    assert!(
        systems.len() >= 2,
        "child and grandchild must each call provider: {systems:?}"
    );
    for system in &systems {
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "nested spawn must inherit the same immutable guidance Arc text: {system}"
        );
        assert_eq!(
            system.matches(TRIGGER_GUIDANCE).count(),
            1,
            "guidance once per activation system: {system}"
        );
    }
}

#[tokio::test]
async fn resident_guidance_is_ephemeral_not_persisted_in_events() {
    // Characterization: triggering guidance lives only in the in-process slot.
    // Durable replay must not contain the guidance text, so process-loss recovery
    // cannot invent it from the event log (register_recovered_resident sets None).
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted(Vec::new()),
    )
    .await;
    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    let outcomes = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "ephemeral guidance".to_string(),
                prompt: "initial".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("resident spawn");
    let child: SessionId = outcomes[0].session.parse().unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !fixture.systems.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resident activation");

    let parent_events = fixture.engine.replay(fixture.parent).await.unwrap();
    let child_events = fixture.engine.replay(child).await.unwrap();
    let blob = format!("{parent_events:?}{child_events:?}");
    assert!(
        !blob.contains(TRIGGER_GUIDANCE),
        "guidance must not be written into durable events: {blob}"
    );
    // Activation still used the in-process Arc.
    let systems = fixture.systems.lock().unwrap().clone();
    assert!(
        systems.iter().any(|s| s.contains(TRIGGER_GUIDANCE)),
        "in-process activation must still apply guidance: {systems:?}"
    );
}

#[tokio::test]
async fn batch_invalid_member_with_guidance_has_zero_durable_side_effects() {
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted(Vec::new()),
    )
    .await;
    let sessions_before = fixture.engine.store().list_sessions().await.unwrap().len();
    let events_before = fixture.engine.replay(fixture.parent).await.unwrap();
    let operation = operation();
    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);

    let result = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance),
        )
        .spawn(
            operation,
            vec![
                SpawnMember {
                    description: "ok member".to_string(),
                    prompt: "would run".to_string(),
                    subagent_type: "quick".to_string(),
                    ..SpawnMember::default()
                },
                SpawnMember {
                    description: "bad member".to_string(),
                    prompt: "must not run".to_string(),
                    subagent_type: "missing-agent".to_string(),
                    ..SpawnMember::default()
                },
            ],
            Default::default(),
        )
        .await;

    assert!(
        matches!(
            result,
            Err(SpawnError::UnknownAgentId { ref agent_id }) if agent_id == "missing-agent"
        ),
        "{result:?}"
    );
    assert_eq!(
        fixture.engine.store().list_sessions().await.unwrap().len(),
        sessions_before,
        "invalid batch member must create zero child sessions even with guidance"
    );
    assert!(
        fixture
            .engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .is_none(),
        "authorization failure must precede durable admission when guidance is present"
    );
    let events_after = fixture.engine.replay(fixture.parent).await.unwrap();
    assert_eq!(
        events_after, events_before,
        "no durable parent events on batch auth failure even when guidance is present"
    );
    assert!(
        fixture.systems.lock().unwrap().is_empty(),
        "no provider rounds when batch fails before admission"
    );
}

/// True end-to-end path: parent `run_turn_with_external_dirs_and_guidance` →
/// provider issues `task` → TurnExecution ToolCtx SpawnerPlane → app supervisor →
/// child provider request carries the immutable guidance marker once (no child
/// AGENTS.md scan, no AgentSpec/catalog overlay).
#[tokio::test]
async fn root_turn_task_tool_propagates_guidance_to_child_provider_once() {
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted_turns(vec![
            // Parent round 1: issue the existing task tool (real tool path).
            vec![
                FakeStep::ToolCall {
                    name: "task".to_string(),
                    input: json!({
                        "description": "e2e guided child",
                        "prompt": "do the work",
                        "subagent_type": "quick"
                    }),
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            // Child activation (blocking spawn runs before parent continues).
            vec![FakeStep::Finish(FinishReason::Stop)],
            // Parent round 2 after tool result.
            vec![FakeStep::Finish(FinishReason::Stop)],
        ]),
    )
    .await;

    // Poison the shared session workdir so a child filesystem discovery would
    // inject this marker into the child system prompt.
    let parent_proj = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    let child_wd = parent_proj
        .session
        .workdir
        .as_deref()
        .expect("parent workdir");
    std::fs::write(PathBuf::from(child_wd).join("AGENTS.md"), CHILD_SCAN_POISON).unwrap();

    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    fixture
        .engine
        .admit_user_prompt(fixture.parent, "spawn a guided child".to_string())
        .await
        .unwrap();
    let finish = fixture
        .engine
        .run_turn_with_external_dirs_and_guidance(
            fixture.parent,
            &fixture.agent,
            Default::default(),
            &[],
            Some(guidance),
            None,
        )
        .await
        .expect("parent turn with task tool");
    assert_eq!(finish, FinishReason::Stop);

    let mut by_session = fixture.by_session.lock().unwrap().clone();
    for _ in 0..500 {
        by_session = fixture.by_session.lock().unwrap().clone();
        if by_session
            .iter()
            .any(|(session, _)| *session != fixture.parent)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_systems: Vec<&String> = by_session
        .iter()
        .filter(|(session, _)| *session != fixture.parent)
        .map(|(_, system)| system)
        .collect();
    assert!(
        !child_systems.is_empty(),
        "child provider must be invoked via real task spawn path: {by_session:?}"
    );
    for system in &child_systems {
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "child system must carry triggering-turn guidance from root turn: {system}"
        );
        assert_eq!(
            system.matches(TRIGGER_GUIDANCE).count(),
            1,
            "guidance marker exactly once on child request: {system}"
        );
        assert!(
            !system.contains(CHILD_SCAN_POISON),
            "child must not scan workdir AGENTS.md: {system}"
        );
    }
}

/// Quiescence wakes main through the ordinary resident_task activation path.
/// `ensure_main` must seed the root slot with the triggering-turn guidance so
/// synthesis reuses it once (in-process only).
#[tokio::test]
async fn quiescence_main_synthesis_uses_triggering_guidance_once() {
    let fixture = guidance_spawn_fixture(
        &[
            ("build", AgentRole::Main, &["quick"]),
            ("general", AgentRole::Main, &[]),
            ("quick", AgentRole::Subagent, &[]),
        ],
        FakeProvider::scripted(Vec::new()),
    )
    .await;

    let parent_proj = fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap();
    if let Some(wd) = parent_proj.session.workdir.as_deref() {
        std::fs::write(PathBuf::from(wd).join("AGENTS.md"), CHILD_SCAN_POISON).unwrap();
    }

    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    let outcomes = fixture
        .spawner
        .for_session_with_agents_and_guidance(
            fixture.parent,
            fixture.agents.clone(),
            Some(guidance),
        )
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "resident then quiesce".to_string(),
                prompt: "finish quickly".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("resident spawn");
    assert_eq!(outcomes[0].status, "running", "{outcomes:?}");

    // Wait for resident idle + main synthesis directive + main provider call.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let proj = fixture
                .engine
                .read_projection(fixture.parent)
                .await
                .unwrap();
            let has_synth = proj.session.messages.iter().any(|m| {
                matches!(m.role, hya_proto::Role::System)
                    && m.parts.iter().any(|p| {
                        matches!(
                            p,
                            hya_proto::PartProjection::Text { text, .. }
                                if text.contains("TEAM QUIESCED")
                        )
                    })
            });
            let main_systems = fixture
                .by_session
                .lock()
                .unwrap()
                .iter()
                .filter(|(session, _)| *session == fixture.parent)
                .count();
            if has_synth && main_systems >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiescence must wake main synthesis with a provider request");

    let main_systems: Vec<String> = fixture
        .by_session
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, _)| *session == fixture.parent)
        .map(|(_, system)| system.clone())
        .collect();
    assert!(
        !main_systems.is_empty(),
        "main synthesis must call the provider: {main_systems:?}"
    );
    for system in &main_systems {
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "main synthesis must reuse triggering guidance: {system}"
        );
        assert_eq!(
            system.matches(TRIGGER_GUIDANCE).count(),
            1,
            "guidance once on main synthesis system: {system}"
        );
        assert!(
            !system.contains(CHILD_SCAN_POISON),
            "main synthesis must not rediscover workdir AGENTS.md: {system}"
        );
    }
}

const ROOT_MAIN_BUNDLE_PROMPT: &str = "ROOT_MAIN_BUNDLE_PROMPT_MARKER";
const NESTED_CALLER_BUNDLE_PROMPT: &str = "NESTED_CALLER_BUNDLE_PROMPT_MARKER";
const ROOT_ONLY_SPAWN_TARGET: &str = "root-only-helper";

/// Build a catalog where root and nested definitions differ in Bundle prompt
/// and can_spawn roster.
///
/// The harness plane is no longer part of the divergence: it is derived from
/// origin, so every installed bundle agent here shares the clamped plane.
fn nested_root_divergence_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    let bundle = |stable_id: &str, role: AgentRole, prompt: &str, can_spawn: &[&str]| {
        PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
            format_version: 2,
            identity: BundleIdentity {
                id: format!("hya/nested-root-divergence-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: format!("test-only-{stable_id}"),
            agent: PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role,
                color: None,
                prompt: Some(prompt.to_string()),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                legacy_spawn_lifecycle: None,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
                hook_refs: Vec::new(),
            },
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        }))
    };
    let bundles = vec![
        // Root main: root-only can_spawn target.
        bundle(
            "root-main",
            AgentRole::Main,
            ROOT_MAIN_BUNDLE_PROMPT,
            &["planner", ROOT_ONLY_SPAWN_TARGET],
        ),
        // Nested caller: only resident quick.
        bundle(
            "planner",
            AgentRole::Subagent,
            NESTED_CALLER_BUNDLE_PROMPT,
            &["quick"],
        ),
        bundle("quick", AgentRole::Subagent, "quick prompt", &[]),
        bundle(
            ROOT_ONLY_SPAWN_TARGET,
            AgentRole::Subagent,
            "root-only helper prompt",
            &[],
        ),
    ];
    let catalog = BundleCatalog::from_prepared(&bundles).expect("nested-root catalog");
    let catalog =
        hya_core::AgentCatalog::new(Arc::new(catalog)).expect("nested-root agent catalog");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}

/// Nested caller is first to produce a resident: main quiescence synthesis must
/// bind the team root stable AgentName definition, root roster/resource policy,
/// and inherited guidance — never the nested caller's.
#[tokio::test]
async fn nested_first_resident_main_synthesis_uses_root_definition_not_caller() {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let tools_by_session = Arc::new(Mutex::new(Vec::new()));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        systems: systems.clone(),
        by_session: by_session.clone(),
        tools_by_session: tools_by_session.clone(),
        inner: FakeProvider::scripted(Vec::new()),
    })));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Task, "*", Mode::Allow),
        Rule::new(Action::Read, "*", Mode::Allow),
    ]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    let tools = Arc::new(ToolRegistry::builtins());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router.clone(),
            nested_root_divergence_runtime(tools),
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
    let base = AgentSpec {
        name: AgentName::new("root-main"),
        model: ModelRef::new("fake"),
        system_prompt: "harness base must not win over root Bundle prompt".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        base.clone(),
        provider_router,
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
    // Nested subagent is the spawn parent (first resident-producing spawn).
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
    let spawner = spawn_sender.for_binding(&binding);
    let nested_roster = engine
        .agent_roster_for_binding(&binding, "planner")
        .unwrap();
    assert!(
        nested_roster.iter().any(|a| a.name == "quick"),
        "nested can_spawn is quick-only: {nested_roster:?}"
    );
    assert!(
        !nested_roster
            .iter()
            .any(|a| a.name == ROOT_ONLY_SPAWN_TARGET),
        "nested must not list root-only can_spawn target"
    );
    let root_roster = engine
        .agent_roster_for_binding(&binding, "root-main")
        .unwrap();
    assert!(
        root_roster.iter().any(|a| a.name == ROOT_ONLY_SPAWN_TARGET),
        "root can_spawn must include root-only target: {root_roster:?}"
    );

    let guidance: Arc<str> = Arc::from(TRIGGER_GUIDANCE);
    let outcomes = spawner
        .for_session_with_agents_and_guidance(nested, nested_roster, Some(guidance))
        .spawn(
            operation(),
            vec![SpawnMember {
                description: "nested first resident".to_string(),
                prompt: "finish quickly".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("nested resident spawn");
    assert_eq!(outcomes[0].status, "running", "{outcomes:?}");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let proj = engine.read_projection(root).await.unwrap();
            let has_synth = proj.session.messages.iter().any(|m| {
                matches!(m.role, hya_proto::Role::System)
                    && m.parts.iter().any(|p| {
                        matches!(
                            p,
                            hya_proto::PartProjection::Text { text, .. }
                                if text.contains("TEAM QUIESCED")
                        )
                    })
            });
            let main_systems = by_session
                .lock()
                .unwrap()
                .iter()
                .filter(|(session, _)| *session == root)
                .count();
            if has_synth && main_systems >= 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("quiescence must wake main synthesis on the team root");

    let main_systems: Vec<String> = by_session
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, _)| *session == root)
        .map(|(_, system)| system.clone())
        .collect();
    assert!(
        !main_systems.is_empty(),
        "main synthesis must call the provider: {main_systems:?}"
    );
    for system in &main_systems {
        assert!(
            system.contains(ROOT_MAIN_BUNDLE_PROMPT),
            "main synthesis must use root Bundle prompt, got: {system}"
        );
        assert!(
            !system.contains(NESTED_CALLER_BUNDLE_PROMPT),
            "main synthesis must not use nested caller Bundle prompt: {system}"
        );
        assert!(
            system.contains(TRIGGER_GUIDANCE),
            "main synthesis must reuse inherited triggering guidance: {system}"
        );
        assert_eq!(
            system.matches(TRIGGER_GUIDANCE).count(),
            1,
            "guidance once on main synthesis: {system}"
        );
    }

    // Root resource policy is Full (has harness tools); nested is None (no tools).
    let main_tool_sets: Vec<Vec<String>> = tools_by_session
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, _)| *session == root)
        .map(|(_, tools)| tools.clone())
        .collect();
    assert!(
        !main_tool_sets.is_empty(),
        "main synthesis must expose a tool list"
    );
    for tools in &main_tool_sets {
        assert!(
            tools.iter().any(|name| name == "task" || name == "read"),
            "main synthesis must compile root Full resource policy (harness tools), got: {tools:?}"
        );
    }
}

/// Unresolvable team-root definition fails closed before durable admission when
/// the batch contains a resident member (no admission row, no child session/event).
#[tokio::test]
async fn missing_root_definition_fails_before_admission_for_resident_batch() {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let by_session = Arc::new(Mutex::new(Vec::new()));
    let tools_by_session = Arc::new(Mutex::new(Vec::new()));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CaptureSystemsProvider {
        systems: systems.clone(),
        by_session: by_session.clone(),
        tools_by_session: tools_by_session.clone(),
        inner: FakeProvider::scripted(Vec::new()),
    })));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Task, "*", Mode::Allow),
        Rule::new(Action::Read, "*", Mode::Allow),
    ]));
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(4);
    // Catalog has nested + resident targets, but deliberately omits the root
    // session's stable AgentName so root main activation cannot resolve.
    let runtime = {
        let agent = |stable_id: &str, role: AgentRole, can_spawn: &[&str]| -> PreparedAgent {
            PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                legacy_spawn_lifecycle: None,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
                hook_refs: Vec::new(),
            }
        };
        // `general` is a compiled-in built-in, so only the two bundle agents
        // need bundles here.
        let bundles = [
            ("planner", AgentRole::Subagent, &["quick"][..]),
            ("quick", AgentRole::Subagent, &[][..]),
        ]
        .into_iter()
        .map(|(stable_id, role, can_spawn)| {
            PreparedInstallableBundle::Agent(Box::new(PreparedAgentBundle {
                format_version: 2,
                identity: BundleIdentity {
                    id: format!("hya/missing-root-def-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                namespace: None,
                digest: format!("test-only-{stable_id}"),
                agent: agent(stable_id, role, can_spawn),
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            }))
        })
        .collect::<Vec<_>>();
        let catalog = BundleCatalog::from_prepared(&bundles).expect("missing-root catalog");
        let catalog =
            hya_core::AgentCatalog::new(Arc::new(catalog)).expect("missing-root agent catalog");
        Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            Arc::new(catalog),
        ))
    };
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            provider_router.clone(),
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
    let workdir = temp_child_workdir("missing-root");
    let base = AgentSpec {
        name: AgentName::new("general"),
        model: ModelRef::new("fake"),
        system_prompt: "base".to_string(),
        workdir: workdir.clone(),
        reasoning: None,
    };
    let resident = ResidentSupervisor::start(engine.clone());
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        base,
        provider_router,
        Arc::new(CategoryRegistry::default()),
        resident,
    );

    // Root session AgentName is not in the catalog.
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

    let sessions_before = engine.store().list_sessions().await.unwrap().len();
    let root_events_before = engine.replay(root).await.unwrap();
    let nested_events_before = engine.replay(nested).await.unwrap();
    let operation = operation();
    let binding = engine.bind_runtime(&workdir).unwrap();
    let spawner = spawn_sender.for_binding(&binding);
    let nested_roster = engine
        .agent_roster_for_binding(&binding, "planner")
        .unwrap();

    let result = spawner
        .for_session_with_agents_and_guidance(
            nested,
            nested_roster,
            Some(Arc::from(TRIGGER_GUIDANCE)),
        )
        .spawn(
            operation,
            vec![SpawnMember {
                description: "resident under missing root".to_string(),
                prompt: "must not run".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await;

    assert!(
        matches!(
            result,
            Err(SpawnError::UnknownAgentId { ref agent_id }) if agent_id == "ghost-root"
        ),
        "missing root definition must fail typed before admission: {result:?}"
    );
    assert_eq!(
        engine.store().list_sessions().await.unwrap().len(),
        sessions_before,
        "no child session on missing-root fail-closed"
    );
    assert!(
        engine
            .store()
            .admission(operation.operation_id())
            .await
            .unwrap()
            .is_none(),
        "missing root must precede durable admission"
    );
    assert_eq!(
        engine.replay(root).await.unwrap(),
        root_events_before,
        "no durable root events"
    );
    assert_eq!(
        engine.replay(nested).await.unwrap(),
        nested_events_before,
        "no durable nested events"
    );
    assert!(
        systems.lock().unwrap().is_empty(),
        "no provider rounds when root resolution fails before admission"
    );
}

/// A `task` spawn links its member row to the spawning tool call and the
/// task's short description (not the prompt), and the first resident turn
/// moves the row from `spawning` to `running`.
#[tokio::test]
async fn task_spawn_member_row_carries_call_id_description_and_runs() {
    let fixture = admission_fixture(1).await;
    let call = ToolCallId::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.scoped_spawner().spawn(
            ToolOperation::from_tool_call(call),
            vec![SpawnMember {
                description: "survey the repo".to_string(),
                prompt: "Walk every crate and list the public entry points you find.".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        ),
    )
    .await
    .expect("spawn timed out")
    .expect("resident spawn should be admitted");
    let child: SessionId = outcome[0].session.parse().expect("valid child session");

    let row = |projection: &hya_proto::Projection| {
        projection
            .session
            .members
            .iter()
            .find(|row| row.child == Some(child))
            .cloned()
            .expect("member row on the parent log")
    };
    let spawned = row(&fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap());
    assert_eq!(spawned.tool_call, Some(call));
    assert_eq!(spawned.description, "survey the repo");
    assert_eq!(spawned.subagent_type.as_str(), "quick");

    wait_member_turn_done(&fixture.engine, &outcome[0].session).await;
    let running = row(&fixture
        .engine
        .read_projection(fixture.parent)
        .await
        .unwrap());
    assert_eq!(running.status, hya_proto::MemberRunStatus::Running);
    assert_eq!(running.member, spawned.member);
}
