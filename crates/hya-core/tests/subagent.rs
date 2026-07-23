#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentSpec, CreateSession, EventBus, MemberSpec, MemberStatus, SessionEngine, SubagentGovernor,
    SubagentLimits, TeamEvidenceEnvelope, project_envelope, run_team,
};
use hya_proto::{
    AgentName, Event, FinishReason, MemberId, MessageId, ModelRef, PartProjection, Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

struct SelectiveFakeProvider;

#[async_trait]
impl Provider for SelectiveFakeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("MEMBERTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

async fn engine() -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(SelectiveFakeProvider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        tools,
        perm,
        EventBus::default(),
    ));
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

/// A provider that records how many streams run concurrently, so a test can prove
/// the streaming-concurrency semaphore actually caps parallelism.
struct ConcurrencyProbeProvider {
    current: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for ConcurrencyProbeProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.current.fetch_sub(1, Ordering::SeqCst);
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("MEMBERTEXT".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

async fn governed_engine(
    limits: SubagentLimits,
    provider: Arc<dyn Provider>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(store, router, tools, perm, EventBus::default())
            .with_governor(SubagentGovernor::new(limits)),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

fn member(agent: &AgentSpec, directive: &str) -> MemberSpec {
    MemberSpec {
        id: MemberId::new(),
        agent: agent.clone(),
        directive: directive.to_string(),
        description: String::new(),
        session: None,
    }
}

#[tokio::test]
async fn governor_caps_streaming_concurrency() {
    let current = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(ConcurrencyProbeProvider {
        current: current.clone(),
        peak: peak.clone(),
    });
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_depth: 5,
            max_concurrency: 2,
            per_run_budget: 100,
            ..SubagentLimits::default()
        },
        provider,
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let specs: Vec<MemberSpec> = (0..6).map(|i| member(&agent, &format!("m{i}"))).collect();
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 6);
    assert!(evidence.iter().all(|e| e.status == MemberStatus::Done));
    assert!(
        peak.load(Ordering::SeqCst) <= 2,
        "peak concurrent streams {} exceeded max_concurrency 2",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn governor_rejects_members_beyond_budget() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_depth: 5,
            max_concurrency: 8,
            per_run_budget: 1,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let specs = vec![
        member(&agent, "a"),
        member(&agent, "b"),
        member(&agent, "c"),
    ];
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 3);
    let done = evidence
        .iter()
        .filter(|e| e.status == MemberStatus::Done)
        .count();
    let failed = evidence
        .iter()
        .filter(|e| e.status == MemberStatus::Failed)
        .count();
    assert_eq!(done, 1, "only the budgeted member runs");
    assert_eq!(failed, 2, "the rest are rejected");
    assert!(
        evidence
            .iter()
            .any(|e| e.summary.contains("budget exhausted")),
        "rejected members explain the budget"
    );
}

#[tokio::test]
async fn governor_rejects_spawn_beyond_max_depth() {
    let (engine, agent) = governed_engine(
        SubagentLimits {
            max_depth: 1,
            max_concurrency: 8,
            per_run_budget: 100,
            ..SubagentLimits::default()
        },
        Arc::new(SelectiveFakeProvider),
    )
    .await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    // A depth-1 child; its member would be depth 2 > max_depth 1.
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let evidence = run_team(
        engine.clone(),
        child,
        vec![member(&agent, "too deep")],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].status, MemberStatus::Failed);
    assert!(evidence[0].summary.contains("depth"));
}

#[tokio::test]
async fn run_team_records_member_lifecycle_on_lead() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let evidence = run_team(
        engine.clone(),
        lead,
        vec![member(&agent, "member one"), member(&agent, "member two")],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(evidence.len(), 2);

    // The lead projection now carries observable member lifecycle entries.
    let proj = engine.read_projection(lead).await.unwrap();
    assert_eq!(proj.session.members.len(), 2, "both members are tracked");
    assert!(
        proj.session
            .members
            .iter()
            .all(|m| matches!(m.status, hya_proto::MemberRunStatus::Done)),
        "members finished Done"
    );
    assert!(
        proj.session.members.iter().all(|m| m.child.is_some()),
        "each member links to its child session"
    );
}

#[tokio::test]
async fn team_evidence_envelope_has_no_transcript_leak() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let specs = vec![
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            directive: "do A".to_string(),
            description: String::new(),
            session: None,
        },
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            directive: "do B".to_string(),
            description: String::new(),
            session: None,
        },
    ];
    let evidence = run_team(engine.clone(), lead, specs, CancellationToken::new()).await;
    assert_eq!(evidence.len(), 2);
    assert!(evidence.iter().all(|e| e.status == MemberStatus::Done));

    project_envelope(&engine, lead, &TeamEvidenceEnvelope { members: evidence })
        .await
        .unwrap();

    let lead_proj = engine.read_projection(lead).await.unwrap();
    let has_envelope = lead_proj.session.messages.iter().any(|m| {
        matches!(m.role, Role::System)
            && m.parts.iter().any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains("TEAM EVIDENCE ENVELOPE")))
    });
    assert!(has_envelope, "lead must contain the evidence envelope");

    // The members ran in CHILD sessions: the lead transcript holds no replayed
    // assistant turns (no full-transcript leak into the lead context).
    let assistant_count = lead_proj
        .session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .count();
    assert_eq!(assistant_count, 0);
}

#[tokio::test]
async fn run_team_can_resume_existing_member_session() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            agent,
            directive: "continue prior work".to_string(),
            description: String::new(),
            session: Some(child),
        }],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].session, child.to_string());
    assert_eq!(evidence[0].status, MemberStatus::Done);
}

/// Restarting a failed/finished subagent via the same child session must not
/// grow the lead's member list or team roster. Duplicate members share one
/// session id and make the TUI roster multi-highlight on select.
#[tokio::test]
async fn run_team_resume_reuses_member_and_roster_handle() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(lead),
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let first = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            directive: "first attempt".to_string(),
            description: String::new(),
            session: Some(child),
        }],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, MemberStatus::Done);

    let after_first = engine.read_projection(lead).await.unwrap();
    let first_members: Vec<_> = after_first
        .session
        .members
        .iter()
        .filter(|m| m.child == Some(child))
        .collect();
    assert_eq!(first_members.len(), 1, "first spawn creates one member");
    let first_member_id = first_members[0].member;
    let first_handles: Vec<_> = after_first
        .team
        .roster
        .values()
        .filter(|e| e.session == child)
        .map(|e| e.handle.clone())
        .collect();
    assert_eq!(
        first_handles.len(),
        1,
        "first spawn creates one roster handle"
    );
    let first_handle = first_handles[0].clone();

    // Resume with a fresh MemberId (what the task tool does via task_id).
    let second = run_team(
        engine.clone(),
        lead,
        vec![MemberSpec {
            id: MemberId::new(),
            agent,
            directive: "restart after failure".to_string(),
            description: String::new(),
            session: Some(child),
        }],
        CancellationToken::new(),
    )
    .await;
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].status, MemberStatus::Done);
    assert_eq!(second[0].session, child.to_string());

    let after_resume = engine.read_projection(lead).await.unwrap();
    let resume_members: Vec<_> = after_resume
        .session
        .members
        .iter()
        .filter(|m| m.child == Some(child))
        .collect();
    assert_eq!(
        resume_members.len(),
        1,
        "resume must not add a second member row for the same child session"
    );
    assert_eq!(
        resume_members[0].member, first_member_id,
        "resume should upsert the original member id"
    );

    let resume_handles: Vec<_> = after_resume
        .team
        .roster
        .values()
        .filter(|e| e.session == child)
        .map(|e| e.handle.clone())
        .collect();
    assert_eq!(
        resume_handles.len(),
        1,
        "resume must not allocate a second roster handle for the same session"
    );
    assert_eq!(resume_handles[0], first_handle);
}

#[tokio::test]
async fn panic_in_one_member_is_isolated() {
    let panicker = tokio::spawn(async { panic!("member exploded") });
    let peer = tokio::spawn(async { 7u32 });

    let joined = panicker.await;
    assert!(joined.is_err());
    assert!(joined.unwrap_err().is_panic());

    // the supervisor and peers survive a member panic
    assert_eq!(peer.await.unwrap(), 7);
}

#[tokio::test]
async fn run_team_marks_failed_member_without_session_on_engine_error() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let healthy_id = MemberId::new();
    let failed_id = MemberId::new();
    let mut failing_agent = agent.clone();
    failing_agent.model = ModelRef::new("no-such-model");

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![
            MemberSpec {
                id: healthy_id,
                agent: agent.clone(),
                directive: "do healthy work".to_string(),
                description: String::new(),
                session: None,
            },
            MemberSpec {
                id: failed_id,
                agent: failing_agent,
                directive: "do failing work".to_string(),
                description: String::new(),
                session: None,
            },
        ],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(evidence.len(), 2);

    let healthy = evidence
        .iter()
        .find(|entry| entry.member == healthy_id.to_string())
        .unwrap();
    assert_eq!(healthy.status, MemberStatus::Done);

    let failed = evidence
        .iter()
        .find(|entry| entry.member == failed_id.to_string())
        .unwrap();
    assert_eq!(failed.status, MemberStatus::Failed);
    assert_eq!(failed.session, "-");
    assert!(!failed.summary.is_empty());
}

#[tokio::test]
async fn run_team_preserves_input_member_order_with_mixed_outcomes() {
    let (engine, agent) = engine().await;
    let lead = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap();

    let first = MemberId::new();
    let second = MemberId::new();
    let third = MemberId::new();
    let mut failing_agent = agent.clone();
    failing_agent.model = ModelRef::new("no-such-model");

    let evidence = run_team(
        engine.clone(),
        lead,
        vec![
            MemberSpec {
                id: first,
                agent: agent.clone(),
                directive: "first member".to_string(),
                description: String::new(),
                session: None,
            },
            MemberSpec {
                id: second,
                agent: failing_agent,
                directive: "second member fails".to_string(),
                description: String::new(),
                session: None,
            },
            MemberSpec {
                id: third,
                agent,
                directive: "third member".to_string(),
                description: String::new(),
                session: None,
            },
        ],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(
        evidence
            .iter()
            .map(|entry| entry.member.clone())
            .collect::<Vec<_>>(),
        vec![first.to_string(), second.to_string(), third.to_string()]
    );
    assert_eq!(
        evidence
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![MemberStatus::Done, MemberStatus::Failed, MemberStatus::Done]
    );
}
