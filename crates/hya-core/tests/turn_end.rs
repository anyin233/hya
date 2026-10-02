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
    ToolSchema,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Decision, InvocationPolicy, InvocationRule, Mode, PermissionModel, PermissionPlane,
    PermissionRules, PermissionTarget, Tool, ToolCtx, ToolError, ToolRegistry,
};
use serde_json::json;
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
    /// One complete tool-call round for the registered `gate_tool`, ending
    /// the round with the call open (the tool parks it).
    Tool,
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
            Some(Script::Tool) => Ok(tool_call_stream(session, message)),
            None => Ok(text_answer(session, message)),
        }
    }
}

/// One complete tool-call round against the registered `gate_tool`: the round
/// ends with the call open (the tool parks on its release gate), so the test
/// controls exactly when the turn reaches its next round boundary.
fn tool_call_stream(session: SessionId, message: MessageId) -> EventStream {
    let events = FakeProvider::materialize(
        &[
            FakeStep::ToolCall {
                name: "gate_tool".to_string(),
                input: json!({}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        session,
        message,
    );
    Box::pin(stream::iter(
        events.into_iter().map(Ok::<Event, ProviderError>),
    ))
}

/// A tool that parks until `release`, so a test can quiesce mid-tool and
/// decide when the turn's next round boundary arrives.
struct GatedTool {
    release: Notify,
}

impl GatedTool {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Notify::new(),
        })
    }
}

#[async_trait]
impl Tool for GatedTool {
    fn name(&self) -> &str {
        "gate_tool"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("gate_tool"),
            description: "gated marker".to_string(),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        _ctx: &ToolCtx,
        _input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        self.release.notified().await;
        Ok(json!({ "ok": true }))
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
    let (engine, agent) =
        scripted_engine(provider.clone(), Arc::new(ToolRegistry::builtins())).await;
    (engine, agent, provider)
}

/// A harness whose registry also carries [`GatedTool`], for tests that park a
/// real tool call mid-round and control when the next round boundary arrives.
async fn engine_with_gate_tool() -> (
    Arc<SessionEngine>,
    AgentSpec,
    Arc<ScriptProvider>,
    Arc<GatedTool>,
) {
    let provider = Arc::new(ScriptProvider::default());
    let gate = GatedTool::new();
    let tools = Arc::new(ToolRegistry::builtins());
    tools
        .register(gate.clone())
        .expect("register the gated tool");
    let (engine, agent) = scripted_engine(provider.clone(), tools).await;
    (engine, agent, provider, gate)
}

/// Build the scripted harness on `tools`, claiming the store's runtime owner
/// and wiring it into the engine (`with_runtime_owner`) exactly like the
/// production engine builder, so handoff checkpoints are writable.
async fn scripted_engine(
    provider: Arc<ScriptProvider>,
    tools: Arc<ToolRegistry>,
) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(provider));
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let owner = hya_proto::OwnerRunId::new();
    store.claim_runtime_owner(owner).unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_runtime_owner(owner)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
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
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
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
    let root = make_session(&engine, None, "hya-main").await;
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
    let a = make_session(&engine, Some(root), "hya-task").await;
    let b = make_session(&engine, Some(root), "hya-task").await;
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
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let scout = make_session(&engine, Some(root), "hya-task").await;
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
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
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
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
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
    let root = make_session(&engine, None, "hya-main").await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    register_main(&supervisor, &engine, &agent, root).await;
    let worker = make_session(&engine, Some(root), "hya-task").await;
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

/// An aborted restart handoff strands nothing: a turn that cannot reach its
/// round boundary in time (here: parked mid-provider-stream) is neither
/// cancelled nor closed, the quiesce lifts so the old harness keeps serving,
/// and the turn later closes like any user cancel — never as `handoff`.
#[tokio::test]
async fn handoff_aborts_rather_than_terminalizing_a_stranded_turn() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;

    provider.script(root, Script::Hang);
    engine
        .admit_user_prompt(root, "keep working".to_string())
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
        "the root turn is mid-stream with an open part"
    );

    let report = engine
        .handoff_turns(&agent, Duration::from_millis(300))
        .await;
    assert_eq!(
        report.stragglers,
        vec![root],
        "the parked turn rejects the restart"
    );
    assert_eq!(report.cancelled, Vec::<SessionId>::new());
    assert!(
        !engine.handoff_readiness().quiescing,
        "the abort lifts the quiesce"
    );
    assert!(engine.turn_active(root), "the live turn keeps running");

    // Nothing was closed and nothing is queued for a successor.
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert!(
        finishes.first().is_none_or(|(_, list)| list.is_empty()),
        "no MessageFinished was appended: {finishes:?}"
    );
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        open_tool_parts(&engine, root).await,
        1,
        "the mid-flight part stays open, not terminalized"
    );

    // The untouched turn closes like a user cancel, never as a handoff.
    assert!(engine.cancel_turn(root, FinishCause::UserCancel));
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("the turn returns after the cancel")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))],
        "the handoff never wrote a close for the stranded turn"
    );

    // The abort left nothing resumable.
    let resumed = engine.resume_handed_off_turns(&agent, None).await;
    assert!(resumed.is_empty(), "{resumed:?}");
    assert_eq!(provider.streams_for(root), 1, "no continuation was driven");
}

/// Suspend before the next model call: a quiesced root turn finishes its
/// current round (the gated tool completes — its side effect is kept), then
/// checkpoints atomically at the round boundary instead of requesting a
/// second stream, closing with `cause: handoff` and queueing one prompt-less
/// resume row. The successor's resume continues the same session at most
/// once: a new assistant turn, no new user prompt, no repeated side effect.
#[tokio::test]
async fn suspend_checkpoints_before_the_next_model_call_and_resumes_once() {
    let (engine, agent, provider, gate) = engine_with_gate_tool().await;
    let root = make_session(&engine, None, "hya-main").await;

    provider.script(root, Script::Tool);
    engine
        .admit_user_prompt(root, "go".to_string())
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
        "the turn is parked in the gated tool"
    );

    // Quiesce mid-tool: nothing is cancelled; the turn hands off only after
    // the tool completes, at its next round boundary.
    assert_eq!(engine.begin_handoff_quiesce(), vec![root]);
    assert!(engine.handoff_readiness().quiescing);
    gate.release.notify_one();

    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("the turn returns at the boundary")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
    assert_eq!(
        provider.streams_for(root),
        1,
        "no second model call was started after the quiesce"
    );

    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::Handoff))],
        "the checkpoint close is the resume marker"
    );
    assert_eq!(
        open_tool_parts(&engine, root).await,
        0,
        "the completed tool call was kept, not errored"
    );

    // Exactly one prompt-less resume row was queued.
    let rows = engine.store().list_pending_resumes().await.unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].session, root);
    assert!(rows[0].prompt.is_empty(), "the continuation is prompt-less");

    // The successor: one continuation turn, no new user prompt, no repeated
    // side effect.
    let resumed = engine.resume_handed_off_turns(&agent, None).await;
    assert_eq!(resumed, vec![root]);
    assert!(
        eventually(|| async {
            let messages = &engine.read_projection(root).await.unwrap().session.messages[..];
            messages.len() == 3
                && messages[0].role == Role::User
                && messages[2].role == Role::Assistant
                && messages[2].finish == Some(FinishReason::Stop)
        })
        .await,
        "the continuation turn finished"
    );
    let messages = &engine.read_projection(root).await.unwrap().session.messages[..];
    assert_eq!(
        finishes[0].0, messages[1].id,
        "the handoff message is stable"
    );
    assert_eq!(
        messages.iter().filter(|m| m.role == Role::User).count(),
        1,
        "no user message was added to the transcript"
    );
    assert_eq!(
        provider.streams_for(root),
        2,
        "exactly one continuation stream"
    );

    // At most once: the tail moved, another pass finds nothing to resume.
    let again = engine.resume_handed_off_turns(&agent, None).await;
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(provider.streams_for(root), 2, "no second continuation");
}

/// A handoff that partly drains and then aborts: the session that reached its
/// boundary checkpointed and is re-driven in-process (the same session, no
/// new prompt), a session that merely finished naturally during the window is
/// NOT reported as handed off, and the stranded session is left exactly as it
/// was — live, unclosed — for the old harness to keep serving.
#[tokio::test]
async fn aborted_handoff_re_drives_checkpointed_sessions_in_process() {
    let (engine, agent, provider, gate) = engine_with_gate_tool().await;
    let draining = make_session(&engine, None, "hya-main").await;
    let stranded = make_session(&engine, None, "hya-task").await;
    let natural = make_session(&engine, None, "hya-main").await;

    provider.script(draining, Script::Tool);
    provider.script(stranded, Script::Hang);
    provider.script(natural, Script::Gate);
    for session in [draining, stranded, natural] {
        engine
            .admit_user_prompt(session, "work".to_string())
            .await
            .unwrap();
    }
    let lead_draining = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(draining, &agent, CancellationToken::new())
                .await
        })
    };
    let lead_stranded = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(stranded, &agent, CancellationToken::new())
                .await
        })
    };
    let lead_natural = {
        let engine = engine.clone();
        let agent = agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(natural, &agent, CancellationToken::new())
                .await
        })
    };
    assert!(
        eventually(|| async {
            open_tool_parts(&engine, draining).await == 1
                && open_tool_parts(&engine, stranded).await == 1
                && provider.streams_for(natural) == 1
        })
        .await,
        "the two tool turns are parked mid-round and the natural turn is parked in its stream"
    );

    let quiesced = engine.begin_handoff_quiesce();
    assert_eq!(quiesced.len(), 3, "{quiesced:?}");
    assert!(
        quiesced.contains(&draining) && quiesced.contains(&stranded) && quiesced.contains(&natural)
    );
    gate.release.notify_one();
    provider.release.notify_one();
    assert!(
        eventually(|| async { !engine.turn_active(draining) }).await,
        "the drained session checkpointed and ended"
    );
    let natural_finish = tokio::time::timeout(Duration::from_secs(5), lead_natural)
        .await
        .expect("the natural turn finishes during the window")
        .unwrap()
        .unwrap();
    assert_eq!(natural_finish, FinishReason::Stop);

    let report = engine
        .handoff_turns(&agent, Duration::from_millis(300))
        .await;
    assert_eq!(report.stragglers, vec![stranded]);
    assert_eq!(
        report.cancelled,
        vec![draining],
        "only the session that reached a handoff boundary counts; the natural Stop is not a handoff"
    );
    assert!(!engine.handoff_readiness().quiescing);
    let natural_finishes = assistant_finishes(&engine.replay(natural).await.unwrap(), natural);
    assert_eq!(
        natural_finishes[0].1,
        vec![(FinishReason::Stop, None)],
        "the naturally finished turn was not rewritten into a handoff"
    );

    // The checkpointed session was re-driven in-process: same session, one
    // continuation stream, finished Stop, no new user prompt.
    assert!(
        eventually(|| async {
            let messages = &engine
                .read_projection(draining)
                .await
                .unwrap()
                .session
                .messages[..];
            messages.len() == 3 && messages[2].finish == Some(FinishReason::Stop)
        })
        .await,
        "the in-process continuation finished"
    );
    assert_eq!(provider.streams_for(draining), 2);
    let drain_messages = &engine
        .read_projection(draining)
        .await
        .unwrap()
        .session
        .messages[..];
    assert_eq!(
        drain_messages
            .iter()
            .filter(|m| m.role == Role::User)
            .count(),
        1,
        "the in-process continuation is prompt-less"
    );
    let _ = lead_draining
        .await
        .expect("the drained turn returns")
        .unwrap();

    // The stranded session was never touched by the handoff.
    assert!(engine.turn_active(stranded));
    assert_eq!(open_tool_parts(&engine, stranded).await, 1);
    let stranded_finishes = assistant_finishes(&engine.replay(stranded).await.unwrap(), stranded);
    assert!(
        stranded_finishes[0].1.is_empty(),
        "no close was written for the stranded turn"
    );
    assert!(engine.cancel_turn(stranded, FinishCause::UserCancel));
    let finish = tokio::time::timeout(Duration::from_secs(5), lead_stranded)
        .await
        .expect("the stranded turn returns after the cancel")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Cancelled);
    let stranded_finishes = assistant_finishes(&engine.replay(stranded).await.unwrap(), stranded);
    assert_eq!(
        stranded_finishes[0].1,
        vec![(FinishReason::Cancelled, Some(FinishCause::UserCancel))]
    );
}

/// A restart-handoff quiesce refuses new turns but cancels nothing: the
/// active turn runs to its natural end — the safe boundary the cutover waits
/// for — and no handoff marker is written when nothing had to be closed.
#[tokio::test]
async fn quiesce_waits_for_the_active_turn_and_refuses_new_ones() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;

    provider.script(root, Script::Gate);
    engine
        .admit_user_prompt(root, "parked".to_string())
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
        .expect("the turn reaches the provider");

    let active = engine.begin_handoff_quiesce();
    assert_eq!(active, vec![root]);

    // New turns are refused while the parked one is untouched.
    let refused = engine
        .run_turn(root, &agent, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(refused, FinishReason::Cancelled);
    let readiness = engine.handoff_readiness();
    assert_eq!(readiness.active, vec![root]);
    assert!(readiness.pending_asks.is_empty(), "{readiness:?}");
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert!(
        finishes.first().is_none_or(|(_, list)| list.is_empty()),
        "the parked turn is not closed: {finishes:?}"
    );

    // The parked turn reaches its natural end: the safe boundary.
    provider.release.notify_one();
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("the parked turn finishes")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let finishes = assistant_finishes(&engine.replay(root).await.unwrap(), root);
    assert_eq!(
        finishes[0].1,
        vec![(FinishReason::Stop, None)],
        "a natural end carries no harness cause"
    );
    let readiness = engine.handoff_readiness();
    assert!(readiness.active.is_empty(), "{readiness:?}");
}

/// The cutover must never strand a pending permission ask: a session parked
/// on a prompt stays listed in `handoff_readiness().pending_asks` until the
/// reply lands, and the turn then finishes normally.
#[tokio::test]
async fn handoff_readiness_names_sessions_with_pending_permission_asks() {
    let policy = InvocationPolicy::compile(
        PermissionModel::Default,
        vec![InvocationRule::new(
            PermissionTarget::Command,
            "^printf ",
            Mode::Ask,
        )],
    )
    .unwrap();
    let (permission, mut asks) =
        PermissionPlane::new_with_policy(PermissionRules::default(), policy);
    let turns = vec![
        vec![
            FakeStep::ToolCall {
                name: "bash".to_string(),
                input: json!({ "command": "printf one" }),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ],
        vec![
            FakeStep::Text("done".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
    ];
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted_turns(turns)))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let root = make_session(&engine, None, "hya-main").await;
    // The session's recorded model routes to the fake provider.
    engine
        .switch_model(root, ModelRef::new("fake"))
        .await
        .unwrap();
    let ask_agent = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("fake"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    engine
        .admit_user_prompt(root, "run".to_string())
        .await
        .unwrap();
    let lead = {
        let engine = engine.clone();
        let ask_agent = ask_agent.clone();
        tokio::spawn(async move {
            engine
                .run_turn(root, &ask_agent, CancellationToken::new())
                .await
        })
    };
    let request = tokio::time::timeout(Duration::from_secs(5), asks.recv())
        .await
        .expect("permission ask timeout")
        .expect("permission ask");
    assert_eq!(request.session, Some(root), "the ask carries its session");
    let readiness = engine.handoff_readiness();
    assert_eq!(
        readiness.pending_asks,
        vec![root],
        "the parked ask names its session"
    );

    // The reply reaches the turn at a safe boundary; the session clears.
    request.reply.send(Decision::AllowOnce).unwrap();
    let finish = tokio::time::timeout(Duration::from_secs(5), lead)
        .await
        .expect("the turn finishes after the reply")
        .unwrap()
        .unwrap();
    assert_eq!(finish, FinishReason::Stop);
    let readiness = engine.handoff_readiness();
    assert!(readiness.pending_asks.is_empty(), "{readiness:?}");
    assert!(readiness.active.is_empty(), "{readiness:?}");
}

/// Child and Workflow sessions a restart handoff closed are never auto-resumed:
/// an explicit non-resume outcome. A child is re-driven by its parent's resumed
/// turn (the handoff-errored `task` call is in the model's view); a Workflow
/// run is terminalized by `recover_nonterminal_workflows` on its own.
#[tokio::test]
async fn restart_handoff_does_not_auto_resume_child_or_workflow_sessions() {
    let (engine, agent, provider) = engine_with_script().await;
    let root = make_session(&engine, None, "hya-main").await;
    let child = make_session(&engine, Some(root), "hya-task").await;
    let workflow = make_session(&engine, None, "hya-main").await;

    // A child whose transcript ends with the handoff close.
    let child_message = MessageId::new();
    for event in [
        Event::MessageStarted {
            session: child,
            message: child_message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::MessageFinished {
            session: child,
            message: child_message,
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            tokens: None,
            cause: Some(FinishCause::Handoff),
        },
    ] {
        engine.store().append_event(child, &event).await.unwrap();
    }
    // A root whose last turn closed with `handoff` while a Workflow run is
    // active on it.
    let run = hya_proto::WorkflowRunId::new();
    let workflow_message = MessageId::new();
    for event in [
        Event::WorkflowRunStarted {
            session: workflow,
            run,
            workflow: hya_proto::WorkflowIdentity {
                source: hya_proto::WorkflowSourceId::new("test:restart-flow"),
                name: "restart-flow".to_string(),
                revision: hya_proto::WorkflowRevision::from_bytes([1; 32]),
            },
            request_hash: "inputs".to_string(),
            owner: hya_proto::OwnerRunId::new(),
            stages: vec![hya_proto::WorkflowStagePlan {
                id: "stage".to_string(),
                title: None,
                agent: AgentName::new("hya-task"),
                mode: "once".to_string(),
                level: 0,
                worker_model: None,
                selected_worker_model: None,
                verifier_model: None,
                selected_verifier_model: None,
            }],
        },
        Event::MessageStarted {
            session: workflow,
            message: workflow_message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
        Event::MessageFinished {
            session: workflow,
            message: workflow_message,
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            tokens: None,
            cause: Some(FinishCause::Handoff),
        },
    ] {
        engine.store().append_event(workflow, &event).await.unwrap();
    }

    // Both are handoff-marked, and the resume driver picks up neither.
    let resumed = engine.resume_handed_off_turns(&agent, None).await;
    assert!(resumed.is_empty(), "{resumed:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        provider.streams_for(child) + provider.streams_for(workflow),
        0,
        "no continuation stream was started for a child or workflow session"
    );
    assert!(
        !engine.turn_active(child) && !engine.turn_active(workflow),
        "nothing was driven"
    );
}
