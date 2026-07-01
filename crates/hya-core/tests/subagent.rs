#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentSpec, CreateSession, EventBus, MemberSpec, MemberStatus, SessionEngine,
    TeamEvidenceEnvelope, project_envelope, run_team,
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
            session: None,
        },
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            directive: "do B".to_string(),
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
            session: Some(child),
        }],
        CancellationToken::new(),
    )
    .await;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].session, child.to_string());
    assert_eq!(evidence[0].status, MemberStatus::Done);
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
                session: None,
            },
            MemberSpec {
                id: failed_id,
                agent: failing_agent,
                directive: "do failing work".to_string(),
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
                session: None,
            },
            MemberSpec {
                id: second,
                agent: failing_agent,
                directive: "second member fails".to_string(),
                session: None,
            },
            MemberSpec {
                id: third,
                agent,
                directive: "third member".to_string(),
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
