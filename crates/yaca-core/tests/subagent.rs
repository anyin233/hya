#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use yaca_core::{
    AgentSpec, CreateSession, EventBus, MemberSpec, MemberStatus, SessionEngine,
    TeamEvidenceEnvelope, project_envelope, run_team,
};
use yaca_proto::{AgentName, FinishReason, MemberId, ModelRef, PartProjection, Role};
use yaca_provider::{FakeProvider, FakeStep, ProviderRouter};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine() -> (Arc<SessionEngine>, AgentSpec) {
    let provider = FakeProvider::scripted_turns(vec![vec![
        FakeStep::Text("MEMBERTEXT".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
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
        },
        MemberSpec {
            id: MemberId::new(),
            agent: agent.clone(),
            directive: "do B".to_string(),
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
async fn panic_in_one_member_is_isolated() {
    let panicker = tokio::spawn(async { panic!("member exploded") });
    let peer = tokio::spawn(async { 7u32 });

    let joined = panicker.await;
    assert!(joined.is_err());
    assert!(joined.unwrap_err().is_panic());

    // the supervisor and peers survive a member panic
    assert_eq!(peer.await.unwrap(), 7);
}
