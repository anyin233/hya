#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hya_app::spawn_team_supervisor;
use hya_core::{
    AgentSpec, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor, SessionEngine,
    SubagentGovernor, SubagentLimits,
};
use hya_proto::{
    AgentName, MemberRunStatus, MessageId, ModelRef, SessionId, SubagentMode, ToolCallId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::{AdmissionTerminal, SessionStore};
use hya_tool::{
    PermissionPlane, PermissionRules, SpawnError, SpawnMember, SpawnerPlane, ToolOperation,
    ToolRegistry,
};
use tokio::sync::Notify;

struct AdmissionFixture {
    engine: Arc<SessionEngine>,
    spawner: SpawnerPlane,
    parent: SessionId,
    provider_calls: Arc<AtomicUsize>,
    resident: Arc<ResidentSupervisor>,
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
async fn admitted_background_transient_releases_its_exact_debit_on_completion() {
    let fixture = admission_fixture(1).await;
    let first_operation = operation();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .spawner
            .for_session(fixture.parent)
            .spawn_background(
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
    let child: SessionId = outcome[0].session.parse().expect("valid child session");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = fixture
                .engine
                .read_projection(fixture.parent)
                .await
                .unwrap();
            if let Some(member) = projection
                .session
                .members
                .iter()
                .find(|member| member.child == Some(child))
            {
                match member.status {
                    MemberRunStatus::Done => break,
                    MemberRunStatus::Failed | MemberRunStatus::Cancelled => {
                        panic!("admitted member did not finish: {}", member.summary)
                    }
                    MemberRunStatus::Spawning | MemberRunStatus::Running => {}
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("admitted member did not finish");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixture
                .engine
                .store()
                .admission(first_operation.operation_id())
                .await
                .unwrap()
                .unwrap();
            if record.state == hya_store::AdmissionState::Completed {
                assert!(record.logical_released);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("operation did not finalize");
    assert_eq!(
        fixture
            .engine
            .governor()
            .unwrap()
            .remaining_budget(fixture.parent),
        1
    );

    let retry = fixture
        .spawner
        .for_session(fixture.parent)
        .spawn_background(
            operation(),
            vec![SpawnMember {
                description: "budget exhausted".to_string(),
                prompt: "must not start".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await
        .expect("released capacity should admit the next operation");
    assert_eq!(retry.len(), 1);
}

#[tokio::test]
async fn admitted_background_resident_uses_the_common_pre_create_path() {
    let fixture = admission_fixture(1).await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .spawner
            .for_session(fixture.parent)
            .spawn_background(
                operation(),
                vec![SpawnMember {
                    description: "resident admission".to_string(),
                    prompt: "wait for mail".to_string(),
                    subagent_type: "quick".to_string(),
                    resident: true,
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
async fn foreground_completion_uses_one_debit_and_one_finalize() {
    let fixture = admission_fixture(1).await;
    let operation = operation();

    let outcome = fixture
        .spawner
        .for_session(fixture.parent)
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

#[tokio::test]
async fn conflicting_terminal_finalize_fails_closed_to_foreground_caller() {
    let gate = Arc::new(ProviderGate {
        entered: Notify::new(),
        release: Notify::new(),
    });
    let fixture = admission_fixture_with_gate(1, Some(gate.clone())).await;
    let operation = operation();
    let plane = fixture.spawner.for_session(fixture.parent);
    let spawn = tokio::spawn(async move {
        plane
            .spawn(
                operation,
                vec![SpawnMember {
                    description: "foreground terminal conflict".to_string(),
                    prompt: "wait for terminal conflict".to_string(),
                    subagent_type: "quick".to_string(),
                    ..SpawnMember::default()
                }],
                Default::default(),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified())
        .await
        .expect("provider did not start");
    fixture
        .engine
        .store()
        .finalize_admission(
            operation.operation_id(),
            AdmissionTerminal::Cancelled,
            "test terminal conflict",
        )
        .await
        .expect("test terminal transition");
    gate.release.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(5), spawn)
        .await
        .expect("foreground spawn timed out")
        .expect("spawn task panicked");
    assert!(matches!(result, Err(SpawnError::Unavailable)));
    let record = fixture
        .engine
        .store()
        .admission(operation.operation_id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, hya_store::AdmissionState::Cancelled);
}

async fn background_overload_prevents_child_creation(resident_member: bool) {
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
        fixture
            .spawner
            .for_session(fixture.parent)
            .spawn_background(
                operation,
                vec![SpawnMember {
                    description: "overloaded member".to_string(),
                    prompt: "must not start".to_string(),
                    subagent_type: "quick".to_string(),
                    resident: resident_member,
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
        .spawner
        .for_session(fixture.parent)
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
        .spawner
        .for_session(fixture.parent)
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
        .spawner
        .for_session(fixture.parent)
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
        .spawner
        .for_session(fixture.parent)
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
    let left_plane = fixture.spawner.for_session(fixture.parent);
    let right_plane = fixture.spawner.for_session(fixture.parent);

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
    admission_fixture_with_gate(per_run_budget, None).await
}

async fn admission_fixture_with_gate(
    per_run_budget: u64,
    gate: Option<Arc<ProviderGate>>,
) -> AdmissionFixture {
    let store = SessionStore::connect_memory().await.unwrap();
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CountingProvider {
        calls: provider_calls.clone(),
        inner: FakeProvider::scripted(Vec::new()),
        gate,
    })));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, spawn_rx) = SpawnerPlane::with_capacity(2);
    let engine = Arc::new(
        SessionEngine::new(
            store,
            provider_router.clone(),
            Arc::new(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_spawner(spawner.clone())
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
        false,
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
        })
        .await
        .unwrap();
    AdmissionFixture {
        engine,
        spawner,
        parent,
        provider_calls,
        resident,
    }
}
