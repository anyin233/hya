//! End-event invariant: every assistant message ends with exactly one
//! `MessageFinished`, every non-terminal tool part reaches a terminal state,
//! and every member reaches a terminal status — on a graceful drain, a user
//! cancel, and a lead failure (which also broadcasts a wrap-up notice to the
//! team and never archives the lead).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt as _, stream};
use hya_core::{
    AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine, SubagentGovernor,
    SubagentLimits,
};
use hya_proto::{
    AgentName, ArchiveReason, Envelope, Event, FinishCause, FinishReason, MessageId, ModelRef,
    PartId, PartProjection, Role, RosterStatus, SessionId, ToolCallId, ToolName, ToolPartState,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// How a scripted session's next stream behaves.
#[derive(Clone, Copy)]
enum Script {
    /// Open a tool part, then stream nothing more until cancelled.
    Hang,
    /// Park until `release`, then answer with one text part.
    Gate,
    /// Fail the stream outright (a provider error).
    Fail,
}

/// Claims `hya/offline`. Sessions without a script answer immediately.
#[derive(Default)]
struct ScriptProvider {
    scripts: Mutex<HashMap<SessionId, Script>>,
    entered: Notify,
    release: Notify,
    streams: Mutex<HashMap<SessionId, usize>>,
}

impl ScriptProvider {
    fn script(&self, session: SessionId, script: Script) {
        self.scripts.lock().unwrap().insert(session, script);
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
impl Provider for ScriptProvider {
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
        let script = self.scripts.lock().unwrap().remove(&session);
        match script {
            Some(Script::Hang) => {
                self.entered.notify_one();
                let open = Event::ToolInputStart {
                    session,
                    message,
                    part: PartId::new(),
                    call: ToolCallId::new(),
                    name: ToolName::new("bash"),
                };
                Ok(Box::pin(
                    stream::iter(vec![Ok::<Event, ProviderError>(open)]).chain(stream::pending()),
                ))
            }
            Some(Script::Fail) => Err(ProviderError::Decode(
                "upstream exploded mid-turn".to_string(),
            )),
            Some(Script::Gate) => {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(text_answer(session, message))
            }
            None => Ok(text_answer(session, message)),
        }
    }
}

fn text_answer(session: SessionId, message: MessageId) -> EventStream {
    let events = FakeProvider::materialize(
        &[
            FakeStep::Text("ok".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        session,
        message,
    );
    Box::pin(stream::iter(
        events.into_iter().map(Ok::<Event, ProviderError>),
    ))
}

async fn engine_with_script() -> (Arc<SessionEngine>, AgentSpec, Arc<ScriptProvider>) {
    let provider = Arc::new(ScriptProvider::default());
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
            project: None,
            kind: hya_proto::SessionKind::Project,
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

/// One assistant message's finish events: `(finish, cause)` in log order.
type Finishes = Vec<(FinishReason, Option<FinishCause>)>;

/// Every assistant message of `session` with its finish events, in order.
fn assistant_finishes(events: &[Envelope], session: SessionId) -> Vec<(MessageId, Finishes)> {
    let mut order = Vec::new();
    let mut finishes: HashMap<MessageId, Vec<(FinishReason, Option<FinishCause>)>> = HashMap::new();
    for envelope in events {
        match &envelope.event {
            Event::MessageStarted {
                session: s,
                message,
                role: Role::Assistant,
                ..
            } if *s == session => {
                order.push(*message);
                finishes.entry(*message).or_default();
            }
            Event::MessageFinished {
                session: s,
                message,
                role: Role::Assistant,
                finish,
                cause,
                ..
            } if *s == session => {
                finishes
                    .entry(*message)
                    .or_default()
                    .push((*finish, *cause));
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .map(|message| {
            let list = finishes.remove(&message).unwrap_or_default();
            (message, list)
        })
        .collect()
}

async fn open_tool_parts(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter(|part| {
            matches!(
                part,
                PartProjection::Tool {
                    state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                    ..
                }
            )
        })
        .count()
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

async fn harness_mail_to(engine: &SessionEngine, root: SessionId, leaf: &str) -> Vec<String> {
    let team = engine.read_projection(root).await.unwrap().team;
    team.inboxes
        .get(&team.canonical_member(leaf))
        .map(|inbox| {
            inbox
                .iter()
                .filter(|mail| mail.from == hya_proto::HARNESS_HANDLE)
                .map(|mail| mail.body.clone())
                .collect()
        })
        .unwrap_or_default()
}

async fn quiesced_notices(engine: &SessionEngine, root: SessionId) -> usize {
    engine
        .read_projection(root)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|message| {
            message.role == Role::System
                && message.parts.iter().any(|part| {
                    matches!(part, PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED"))
                })
        })
        .count()
}

async fn archived(engine: &SessionEngine, root: SessionId) -> Vec<String> {
    engine
        .replay(root)
        .await
        .unwrap()
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::AgentArchived { handle, .. } => Some(handle.clone()),
            _ => None,
        })
        .collect()
}

/// A lead streaming a turn with two members streaming theirs: a graceful
/// drain closes all three turns — one `MessageFinished { cancelled, cause:
/// shutdown }` each, the open tool parts errored — and makes both members
/// terminal while the lead stays on the roster.
#[tokio::test]
async fn drain_closes_every_session_turn_with_a_cause() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    provider.script(root, Script::Hang);
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
    let a = make_session(&engine, Some(root), "general").await;
    let b = make_session(&engine, Some(root), "general").await;
    for (session, handle) in [(a, "worker-a"), (b, "worker-b")] {
        provider.script(session, Script::Hang);
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
            engine.turn_active(a)
                && engine.turn_active(b)
                && open_tool_parts(&engine, a).await == 1
                && open_tool_parts(&engine, b).await == 1
                && open_tool_parts(&engine, root).await == 1
        })
        .await,
        "all three sessions are mid-turn with an open tool part"
    );

    let report = supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(report.stragglers.is_empty(), "{report:?}");
    assert_eq!(
        report.cancelled.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([root, a, b])
    );
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn returns")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);

    for session in [root, a, b] {
        let events = engine.replay(session).await.unwrap();
        let finishes = assistant_finishes(&events, session);
        assert_eq!(finishes.len(), 1, "one assistant turn in {session}");
        assert_eq!(
            finishes[0].1,
            vec![(FinishReason::Cancelled, Some(FinishCause::Shutdown))],
            "exactly one finish with cause shutdown in {session}"
        );
        assert_eq!(open_tool_parts(&engine, session).await, 0);
        assert!(!engine.turn_active(session));
    }
    // A drain archives every member (reason `shutdown`) so a later run on
    // the same database can wake it with mail; the lead is never archived.
    for leaf in ["worker-a", "worker-b"] {
        assert_eq!(
            roster_status(&engine, root, leaf).await,
            None,
            "{leaf} left the live roster"
        );
    }
    let team = engine.read_projection(root).await.unwrap().team;
    for leaf in ["worker-a", "worker-b"] {
        let entry = team
            .archived
            .get(&format!("main/{leaf}"))
            .unwrap_or_else(|| panic!("{leaf} must be archived: {:?}", team.archived));
        assert_eq!(entry.reason, ArchiveReason::Shutdown);
    }
    assert_eq!(archived(&engine, root).await.len(), 2);
    assert!(
        roster_status(&engine, root, "main").await.is_some(),
        "the lead stays on the roster"
    );

    // Draining refuses new turns.
    engine
        .admit_user_prompt(root, "again".to_string())
        .await
        .unwrap();
    let refused = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(refused, FinishReason::Cancelled);
    assert_eq!(
        assistant_finishes(&engine.replay(root).await.unwrap(), root).len(),
        1,
        "no turn starts after the drain began"
    );
}

/// A user abort of the lead's turn records `cause: user_cancel` and sends no
/// leader-failed notice.
#[tokio::test]
async fn user_cancel_records_the_cause_and_does_not_broadcast() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "general").await;
    supervisor
        .register_existing_resident(root, scout, "scout".to_string(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Hang);
    engine
        .admit_user_prompt(root, "lead".to_string())
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
    assert!(
        eventually(|| async { open_tool_parts(&engine, root).await == 1 }).await,
        "lead is mid-turn"
    );
    assert!(engine.cancel_turn(root, FinishCause::UserCancel));
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("lead turn returns")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
    assert_eq!(open_tool_parts(&engine, root).await, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        harness_mail_to(&engine, root, "scout").await.is_empty(),
        "a user cancel is not a leader failure"
    );
    assert!(!engine.cancel_turn(root, FinishCause::UserCancel));
}

/// The lead's turn fails with a provider error while a member runs: the turn
/// ends `finish: error, cause: provider_error`, the member gets the harness
/// wrap-up mail, the lead is not archived, and no synthesis turn starts on
/// the dead lead even after the team goes quiet.
#[tokio::test]
async fn lead_failure_broadcasts_wrap_up_and_never_archives_the_lead() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "general").await;
    provider.script(worker, Script::Gate);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".to_string(),
            agent.clone(),
            Some("build the thing".to_string()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), provider.entered.notified())
        .await
        .expect("member is streaming");

    provider.script(root, Script::Fail);
    engine
        .admit_user_prompt(root, "lead".to_string())
        .await
        .unwrap();
    let failed = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await;
    assert!(failed.is_err(), "the lead's turn fails: {failed:?}");
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(finishes.len(), 1);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Error, Some(FinishCause::ProviderError))]
    );

    let notices = harness_mail_to(&engine, root, "worker").await;
    assert_eq!(notices.len(), 1, "exactly one wrap-up notice");
    assert!(notices[0].contains("LEADER FAILED"), "{}", notices[0]);

    // The member finishes its turn, is woken by the notice, and goes idle:
    // the team is quiet, but the dead lead gets no synthesis turn.
    provider.release.notify_one();
    assert!(
        eventually(|| async {
            provider.streams_for(worker) >= 2
                && roster_status(&engine, root, "worker").await == Some(RosterStatus::Idle)
        })
        .await,
        "the member handles the wrap-up notice and idles"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(quiesced_notices(&engine, root).await, 0);
    assert_eq!(
        provider.streams_for(root),
        1,
        "no harness-started lead turn"
    );
    assert!(
        archived(&engine, root).await.is_empty(),
        "the lead is never archived"
    );
    assert!(roster_status(&engine, root, "main").await.is_some());

    // The user resumes the lead: its session is live and runs normally.
    engine
        .admit_user_prompt(root, "resume".to_string())
        .await
        .unwrap();
    let resumed = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resumed, FinishReason::Stop);
}

/// Real-task run 1: a harness-started lead turn (a mail wake of `main`) fails
/// with a provider error. The lead used to be reported, handed off, and
/// archived like a failed member; now it stays live, the failure is the
/// turn's `finish: error`, and the team gets the wrap-up notice.
#[tokio::test]
async fn a_failed_lead_wake_does_not_archive_main() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "general").await;
    supervisor
        .register_existing_resident(root, worker, "worker".to_string(), agent.clone(), None)
        .await
        .unwrap();
    provider.script(root, Script::Fail);
    engine
        .mail_send(
            worker,
            hya_proto::MailEndpoint::Handle("main".to_string()),
            hya_proto::MailKind::Message,
            "status: halfway".to_string(),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
            finishes.len() == 1 && !finishes[0].1.is_empty()
        })
        .await,
        "the woken lead turn runs and closes"
    );
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Error, Some(FinishCause::ProviderError))]
    );
    assert!(
        eventually(|| async { !harness_mail_to(&engine, root, "worker").await.is_empty() }).await,
        "the member is told to wrap up"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        archived(&engine, root).await.is_empty(),
        "main is never archived"
    );
    let reported_main = engine.replay(root).await.unwrap().iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::SubagentReported { handle, .. } | Event::HandoffCommitted { handle, .. }
                if handle == "main"
        )
    });
    assert!(!reported_main, "no report/handoff for the lead");
    assert!(roster_status(&engine, root, "main").await.is_some());
    assert_eq!(
        provider.streams_for(root),
        1,
        "no retry wake of the dead lead"
    );
}

/// `archive` on a member that is mid-turn cancels that turn — its message
/// closes with `finish: cancelled, cause: archived`, open tool parts error —
/// and then archives it (reason `archived_by_parent`). The lead's own turn is
/// untouched.
#[tokio::test]
async fn archive_cancels_a_busy_member_turn_with_cause_archived() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "build").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "general").await;
    provider.script(worker, Script::Hang);
    supervisor
        .register_existing_resident(
            root,
            worker,
            "worker".to_string(),
            agent.clone(),
            Some("work".to_string()),
        )
        .await
        .unwrap();
    assert!(
        eventually(|| async {
            engine.turn_active(worker) && open_tool_parts(&engine, worker).await == 1
        })
        .await,
        "the member is mid-turn with an open tool part"
    );

    let receipt = supervisor
        .archive_member(root, "worker", "no longer needed")
        .await
        .unwrap();
    assert_eq!(receipt.handle, "main/worker");
    assert_eq!(receipt.session, worker);
    assert!(receipt.cancelled_turn, "{receipt:?}");

    assert!(!engine.turn_active(worker));
    let finishes = assistant_finishes(&engine.replay(worker).await.unwrap(), worker);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::Archived))],
        "the member's turn closes once with cause archived"
    );
    assert_eq!(open_tool_parts(&engine, worker).await, 0);
    let team = engine.read_projection(root).await.unwrap().team;
    assert!(!team.roster.contains_key("main/worker"));
    assert_eq!(
        team.archived.get("main/worker").map(|entry| entry.reason),
        Some(ArchiveReason::ArchivedByParent)
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&worker),
        "the member's actor claim is released"
    );
}
