#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use hya_app::spawn_team_supervisor;
use hya_core::{
    AgentSpec, CategoryRegistry, CreateSession, EventBus, ResidentSupervisor, SessionEngine,
    SubagentGovernor, SubagentLimits,
};
use hya_proto::{AgentName, MemberRunStatus, MessageId, ModelRef, SessionId, SubagentMode};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    PermissionPlane, PermissionRules, SpawnError, SpawnMember, SpawnerPlane, ToolRegistry,
};

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
async fn admitted_background_transient_reserves_budget_once() {
    let fixture = admission_fixture(1).await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fixture
            .spawner
            .for_session(fixture.parent)
            .spawn_background(
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

    let mut sessions_before_retry: Vec<String> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session.to_string())
        .collect();
    sessions_before_retry.sort();
    let retry = fixture
        .spawner
        .for_session(fixture.parent)
        .spawn_background(
            vec![SpawnMember {
                description: "budget exhausted".to_string(),
                prompt: "must not start".to_string(),
                subagent_type: "quick".to_string(),
                ..SpawnMember::default()
            }],
            Default::default(),
        )
        .await;
    let mut sessions_after_retry: Vec<String> = fixture
        .engine
        .store()
        .list_sessions()
        .await
        .unwrap()
        .into_iter()
        .map(|info| info.session.to_string())
        .collect();
    sessions_after_retry.sort();

    assert!(matches!(retry, Err(SpawnError::Overloaded)));
    assert_eq!(sessions_before_retry, sessions_after_retry);
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

async fn background_overload_prevents_child_creation(resident_member: bool) {
    let fixture = admission_fixture(0).await;
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
}

async fn admission_fixture(per_run_budget: u64) -> AdmissionFixture {
    let store = SessionStore::connect_memory().await.unwrap();
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider_router = Arc::new(ProviderRouter::new().with(Arc::new(CountingProvider {
        calls: provider_calls.clone(),
        inner: FakeProvider::scripted(Vec::new()),
    })));
    let (permission, _permission_rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, spawn_rx) = SpawnerPlane::new();
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
