//! Single-active-turn invariant: a session (one agent) runs at most ONE turn at
//! a time. Wakes that arrive while a turn is in flight — child mail to the lead,
//! the team-quiescence synthesis notice, a second `run_turn` — queue behind it
//! and are delivered at the turn boundary. Separate sessions (team members) keep
//! streaming concurrently.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_core::{
    AgentSpec, CoreError, CreateSession, EventBus, ResidentSupervisor, SessionEngine,
    SubagentGovernor, SubagentLimits,
};
use hya_proto::{
    AgentName, Envelope, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef,
    PartProjection, Role, RosterStatus, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::{Barrier, Notify};
use tokio_util::sync::CancellationToken;

/// Claims `hya/offline`. The first stream for a gated session parks until
/// `release`; everything else answers with one text part immediately. Members
/// listed in `rendezvous` must all be inside `stream` at the same time before
/// any of them answers (proves they stream concurrently).
#[derive(Default)]
struct GateProvider {
    gated: Mutex<HashSet<SessionId>>,
    entered: Notify,
    release: Notify,
    streams: Mutex<HashMap<SessionId, usize>>,
    rendezvous: Mutex<Option<(HashSet<SessionId>, Arc<Barrier>)>>,
    rendezvous_met: AtomicBool,
}

impl GateProvider {
    fn gate(&self, session: SessionId) {
        self.gated.lock().unwrap().insert(session);
    }

    fn streams_for(&self, session: SessionId) -> usize {
        self.streams
            .lock()
            .unwrap()
            .get(&session)
            .copied()
            .unwrap_or(0)
    }
}

#[async_trait]
impl Provider for GateProvider {
    fn id(&self) -> &str {
        "hya"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "hya/offline").then_some(Capabilities {
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
        *self.streams.lock().unwrap().entry(session).or_default() += 1;
        let gated = self.gated.lock().unwrap().remove(&session);
        if gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let barrier = self
            .rendezvous
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(members, _)| members.contains(&session))
            .map(|(_, barrier)| barrier.clone());
        if let Some(barrier) = barrier {
            match tokio::time::timeout(Duration::from_secs(5), barrier.wait()).await {
                Ok(_) => self.rendezvous_met.store(true, Ordering::SeqCst),
                Err(_) => {
                    return Err(ProviderError::Decode(
                        "members never streamed concurrently".to_string(),
                    ));
                }
            }
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("ok".to_string()),
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

async fn engine_with_gate() -> (Arc<SessionEngine>, AgentSpec, Arc<GateProvider>) {
    let provider = Arc::new(GateProvider::default());
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = AgentSpec {
        name: AgentName::new("general"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent, provider)
}

async fn make_session(engine: &SessionEngine, parent: Option<SessionId>, agent: &str) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(agent),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".to_string(),
        })
        .await
        .unwrap()
}

async fn register_main(
    supervisor: &ResidentSupervisor,
    engine: &SessionEngine,
    agent: &AgentSpec,
    root: SessionId,
) {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine.agent_roster_for_binding(&binding, "build").unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "build")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, agents, resources),
            None,
            None,
        )
        .await
        .unwrap();
}

async fn assistant_starts(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .replay(session)
        .await
        .unwrap()
        .iter()
        .filter(|envelope| {
            matches!(
                &envelope.event,
                Event::MessageStarted { session: s, role: Role::Assistant, .. } if *s == session
            )
        })
        .count()
}

/// The most assistant messages of `session` that were open at the same time.
fn max_open_assistant(events: &[Envelope], session: SessionId) -> usize {
    let mut open = HashSet::new();
    let mut max = 0;
    for envelope in events {
        match &envelope.event {
            Event::MessageStarted {
                session: s,
                message,
                role: Role::Assistant,
            } if *s == session => {
                open.insert(*message);
                max = max.max(open.len());
            }
            Event::MessageFinished {
                session: s,
                message,
                role: Role::Assistant,
                ..
            } if *s == session => {
                open.remove(message);
            }
            _ => {}
        }
    }
    max
}

async fn has_quiesced_notice(engine: &SessionEngine, root: SessionId) -> bool {
    engine
        .read_projection(root)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .any(|message| {
            matches!(message.role, Role::System)
                && message.parts.iter().any(|part| {
                    matches!(part, PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED"))
                })
        })
}

async fn roster_status(
    engine: &SessionEngine,
    root: SessionId,
    leaf: &str,
) -> Option<RosterStatus> {
    let team = engine.read_projection(root).await.unwrap().team;
    team.roster
        .get(&team.canonical_member(leaf))
        .map(|entry| entry.status)
}

async fn eventually<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..500 {
        if check().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Replay position of the TEAM QUIESCED system notice text.
fn system_notice_seq(events: &[Envelope], session: SessionId) -> Option<usize> {
    let mut system_messages = HashSet::new();
    for (index, envelope) in events.iter().enumerate() {
        match &envelope.event {
            Event::MessageStarted {
                session: s,
                message,
                role: Role::System,
            } if *s == session => {
                system_messages.insert(*message);
            }
            Event::TextDelta { message, delta, .. }
                if system_messages.contains(message) && delta.contains("TEAM QUIESCED") =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

/// Replay position of the first assistant `MessageFinished`.
fn first_assistant_finish_seq(events: &[Envelope], session: SessionId) -> Option<usize> {
    events
        .iter()
        .enumerate()
        .find_map(|(index, envelope)| match &envelope.event {
            Event::MessageFinished {
                session: s,
                role: Role::Assistant,
                ..
            } if *s == session => Some(index),
            _ => None,
        })
}

/// Reproduces the real-task #15 failure: a member goes idle while the lead's
/// own turn is still streaming. The quiescence wake must NOT open a second
/// assistant message on the lead; the notice lands after the first turn ends.
#[tokio::test]
async fn quiescence_wake_queues_behind_the_leads_active_turn() {
    let (engine, agent, provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.gate(root);

    engine
        .admit_user_prompt(root, "lead the team".to_string())
        .await
        .unwrap();
    let lead = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("lead turn reaches the provider");

    // Mid-turn, the lead spawns a member (what the `task` tool does).
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "general").await;
    supervisor
        .register_existing_resident(
            root,
            scout,
            "scout".to_string(),
            agent.clone(),
            Some("scout the repo".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            provider.streams_for(scout) == 1
                && roster_status(&engine, root, "scout").await == Some(RosterStatus::Idle)
        })
        .await,
        "the member runs its turn and goes idle while the lead is still streaming"
    );

    // Give an erroneous wake every chance to open a second lead turn.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        assistant_starts(&engine, root).await,
        1,
        "no second assistant message may start on the lead while its turn streams"
    );
    assert_eq!(provider.streams_for(root), 1);
    assert!(
        !has_quiesced_notice(&engine, root).await,
        "quiescence must not be declared while the lead itself is busy"
    );

    provider.release.notify_one();
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn finishes")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    // The deferred notice is delivered at the turn boundary.
    assert!(
        eventually(|| async {
            has_quiesced_notice(&engine, root).await && assistant_starts(&engine, root).await == 2
        })
        .await,
        "the quiesced notice is delivered once the lead's turn has ended"
    );
    assert!(
        eventually(|| async {
            let events = engine.replay(root).await.unwrap();
            max_open_assistant(&events, root) == 1
                && events.iter().any(|envelope| {
                    matches!(
                        &envelope.event,
                        Event::MessageFinished { session, role: Role::Assistant, .. }
                            if *session == root
                    )
                })
                && events
                    .iter()
                    .filter(|envelope| {
                        matches!(
                            &envelope.event,
                            Event::MessageFinished { session, role: Role::Assistant, .. }
                                if *session == root
                        )
                    })
                    .count()
                    == 2
        })
        .await,
        "both lead turns finish and never overlap"
    );
    let events = engine.replay(root).await.unwrap();
    assert_eq!(max_open_assistant(&events, root), 1);
    let notice = system_notice_seq(&events, root).expect("notice seq");
    let first_finish = first_assistant_finish_seq(&events, root).expect("first finish");
    assert!(
        notice > first_finish,
        "the quiesced notice ({notice}) must follow the first turn's finish ({first_finish})"
    );
}

/// Child mail to the lead while the lead's turn is in flight wakes the main
/// actor only after that turn ends — never as a concurrent second turn.
#[tokio::test]
async fn mail_to_the_lead_mid_turn_is_delivered_at_the_turn_boundary() {
    let (engine, agent, provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.gate(root);

    engine
        .admit_user_prompt(root, "lead the team".to_string())
        .await
        .unwrap();
    let lead = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("lead turn reaches the provider");

    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "general").await;
    supervisor
        .register_existing_resident(root, scout, "scout".to_string(), agent.clone(), None)
        .await
        .unwrap();
    engine
        .mail_send(
            scout,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "findings ready".to_string(),
        )
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        assistant_starts(&engine, root).await,
        1,
        "mail must not open a concurrent lead turn"
    );

    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn finishes")
        .unwrap()
        .unwrap();
    assert!(
        eventually(|| async {
            let events = engine.replay(root).await.unwrap();
            events
                .iter()
                .filter(|envelope| {
                    matches!(
                        &envelope.event,
                        Event::MessageFinished { session, role: Role::Assistant, .. }
                            if *session == root
                    )
                })
                .count()
                == 2
        })
        .await,
        "the queued mail wakes the lead for one follow-up turn"
    );
    let events = engine.replay(root).await.unwrap();
    assert_eq!(max_open_assistant(&events, root), 1);
}

/// Two `run_turn` calls on one session serialize: the second waits for the
/// first to end instead of streaming beside it.
#[tokio::test]
async fn concurrent_run_turn_calls_on_one_session_serialize() {
    let (engine, agent, provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    provider.gate(root);
    engine
        .admit_user_prompt(root, "first".to_string())
        .await
        .unwrap();
    let spawn_turn = || {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    let first = spawn_turn();
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("first turn reaches the provider");
    assert!(engine.turn_active(root));
    let second = spawn_turn();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(provider.streams_for(root), 1, "second turn must queue");
    assert_eq!(assistant_starts(&engine, root).await, 1);

    provider.release.notify_one();
    for turn in [first, second] {
        let finish = tokio::time::timeout(Duration::from_secs(5), turn)
            .await
            .expect("turn finishes")
            .unwrap()
            .unwrap();
        assert_eq!(finish, FinishReason::Stop);
    }
    let events = engine.replay(root).await.unwrap();
    assert_eq!(assistant_starts(&engine, root).await, 2);
    assert_eq!(max_open_assistant(&events, root), 1);
    assert!(!engine.turn_active(root));
}

/// A queued turn whose token is cancelled before the active turn ends never
/// starts.
#[tokio::test]
async fn a_queued_turn_can_be_cancelled_while_waiting() {
    let (engine, agent, provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    provider.gate(root);
    engine
        .admit_user_prompt(root, "first".to_string())
        .await
        .unwrap();
    let first = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &agent, CancellationToken::new())
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("first turn reaches the provider");
    let cancel = CancellationToken::new();
    let queued = {
        let engine = engine.clone();
        let agent = agent.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move { engine.run_turn(root, &agent, cancel).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    let queued = tokio::time::timeout(Duration::from_secs(5), queued)
        .await
        .expect("cancelled waiter returns")
        .unwrap();
    assert!(matches!(queued, Ok(FinishReason::Cancelled)));
    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first turn finishes")
        .unwrap()
        .unwrap();
    assert_eq!(assistant_starts(&engine, root).await, 1);
}

/// The invariant is observable: a second claim on a busy session is a typed
/// `TurnAlreadyActive`, and the claim releases on drop.
#[tokio::test]
async fn a_second_turn_claim_is_a_typed_error() {
    let (engine, _agent, _provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    let other = make_session(&engine, None, "build").await;
    let lease = engine.try_begin_turn(root).unwrap();
    assert!(engine.turn_active(root));
    match engine.try_begin_turn(root) {
        Err(CoreError::TurnAlreadyActive { session }) => assert_eq!(session, root),
        Err(other) => panic!("expected TurnAlreadyActive, got {other}"),
        Ok(_) => panic!("expected TurnAlreadyActive, got a second lease"),
    }
    // Other sessions are independent.
    let other_lease = engine.try_begin_turn(other).unwrap();
    drop(other_lease);
    drop(lease);
    assert!(!engine.turn_active(root));
    let again = engine.try_begin_turn(root).unwrap();
    drop(again);
}

/// Subagents are separate sessions: two members of one lead still stream at
/// the same time.
#[tokio::test]
async fn members_of_one_lead_stream_concurrently() {
    let (engine, agent, provider) = engine_with_gate().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let a = make_session(&engine, Some(root), "general").await;
    let b = make_session(&engine, Some(root), "general").await;
    *provider.rendezvous.lock().unwrap() = Some((HashSet::from([a, b]), Arc::new(Barrier::new(2))));
    for (session, handle) in [(a, "worker-a"), (b, "worker-b")] {
        supervisor
            .register_existing_resident(
                root,
                session,
                handle.to_string(),
                agent.clone(),
                Some("work".to_string()),
            )
            .await
            .unwrap();
    }
    assert!(
        eventually(|| async {
            roster_status(&engine, root, "worker-a").await == Some(RosterStatus::Idle)
                && roster_status(&engine, root, "worker-b").await == Some(RosterStatus::Idle)
        })
        .await,
        "both members finish"
    );
    assert!(
        provider.rendezvous_met.load(Ordering::SeqCst),
        "both members were inside the provider stream at the same time"
    );
}
