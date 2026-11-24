//! The `wait` lifecycle request (0.41.0): block the caller — typically inside
//! the lead's own active turn — until its subagents finish (report or are
//! archived; idle never counts), woken through the engine bus; optionally also
//! on new mail (the channel-tools override), including harness mail, which is
//! returned once and marked read; bounded by a timeout and aborted by the
//! caller's cancellation. Targets that finished before the call are listed as
//! `already_finished` and never wake it; a target that ends its turn with a
//! final answer is reported with that answer, and one that stops without a
//! report or an answer wakes it once as `stalled`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_engine::{
    AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine, run_lifecycle_service,
};
use hya_proto::{
    AgentName, ChannelKind, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef,
    PartProjection, ReportOutcome, Role, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    ChannelPolicySnapshot, LifecyclePlane, PermissionPlane, PermissionRules, ToolError,
    ToolRegistry, WaitMemberState, WaitOutcome, WaitSpec, WaitWake,
};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// While `gate_members` is set, every stream of a non-root session parks on
/// its own gate until released (a release before the member reaches the gate
/// is kept, so the first turn cannot race it).
struct GateProvider {
    root: Mutex<Option<SessionId>>,
    gate_members: AtomicBool,
    gates: Mutex<HashMap<SessionId, Arc<Notify>>>,
    tool_batches: Mutex<HashMap<SessionId, Vec<FakeStep>>>,
    /// Every turn's final answer; empty leaves a member idle without a report.
    answer: Mutex<String>,
}

impl Default for GateProvider {
    fn default() -> Self {
        Self {
            root: Mutex::default(),
            gate_members: AtomicBool::default(),
            gates: Mutex::default(),
            tool_batches: Mutex::default(),
            answer: Mutex::new("done with the unit".to_string()),
        }
    }
}

impl GateProvider {
    fn gate(&self, session: SessionId) -> Arc<Notify> {
        self.gates
            .lock()
            .unwrap()
            .entry(session)
            .or_default()
            .clone()
    }

    /// Let one member's parked (or next) stream through.
    fn release(&self, session: SessionId) {
        self.gate(session).notify_one();
    }

    /// Let every member currently parked through.
    fn release_all(&self) {
        for gate in self.gates.lock().unwrap().values() {
            gate.notify_waiters();
        }
    }
}

#[async_trait]
impl Provider for GateProvider {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "fake").then_some(Capabilities {
            streaming_tool_calls: true,
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
        let is_root = *self.root.lock().unwrap() == Some(session);
        if !is_root && self.gate_members.load(Ordering::SeqCst) {
            let gate = self.gate(session);
            gate.notified().await;
        }
        let answer = self.answer.lock().unwrap().clone();
        let mut steps = Vec::new();
        if let Some(batch) = self.tool_batches.lock().unwrap().remove(&session) {
            steps = batch;
            steps.push(FakeStep::Finish(FinishReason::ToolCalls));
        } else {
            if !answer.is_empty() {
                steps.push(FakeStep::Text(answer));
            }
            steps.push(FakeStep::Finish(FinishReason::Stop));
        }
        let events = FakeProvider::materialize(&steps, session, message);
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<Event, ProviderError>),
        )))
    }
}

struct Team {
    engine: Arc<SessionEngine>,
    supervisor: Arc<ResidentSupervisor>,
    provider: Arc<GateProvider>,
    root: SessionId,
    lifecycle: LifecyclePlane,
    lead: AgentSpec,
}

async fn team() -> Team {
    team_with_limits(hya_engine::SubagentLimits::default()).await
}

async fn team_with_limits(limits: hya_engine::SubagentLimits) -> Team {
    team_with_runtime(
        limits,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        "hya-main",
    )
    .await
}

async fn team_with_runtime(
    limits: hya_engine::SubagentLimits,
    runtime: Arc<hya_engine::RuntimeRegistry>,
    agent: &str,
) -> Team {
    let provider = Arc::new(GateProvider::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let (lifecycle, rx) = LifecyclePlane::new();
    let engine = Arc::new(
        SessionEngine::new(store, router, runtime, permission, EventBus::default())
            .with_governor(hya_engine::SubagentGovernor::new(limits))
            .with_lifecycle(lifecycle.clone()),
    );
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new(agent),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    *provider.root.lock().unwrap() = Some(root);
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead = spec(agent);
    let binding = engine.bind_runtime(&lead.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent).unwrap();
    supervisor
        .ensure_main(
            root,
            lead.clone(),
            (binding, Arc::from([]), resources),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        supervisor.clone(),
        rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    Team {
        engine,
        supervisor,
        provider,
        root,
        lifecycle,
        lead,
    }
}

fn spec(name: &str) -> AgentSpec {
    AgentSpec {
        name: AgentName::new(name),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    }
}

/// Spawn a member under `parent`; `gated` parks its first turn until release.
async fn spawn(team: &Team, parent: SessionId, gated: bool) -> (SessionId, String) {
    spawn_agent(team, parent, gated, "hya-scout").await
}

async fn spawn_agent(
    team: &Team,
    parent: SessionId,
    gated: bool,
    name: &str,
) -> (SessionId, String) {
    let agent = spec(name);
    let binding = team.engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(name).unwrap();
    team.provider.gate_members.store(gated, Ordering::SeqCst);
    let (session, handle) = team
        .supervisor
        .spawn_resident(
            parent,
            agent,
            (binding, Arc::from([]), resources, None),
            "work".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    if gated {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if team.provider.gates.lock().unwrap().contains_key(&session) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("member must admit its directive mail before reporting");
    }
    (session, handle)
}

async fn wait(team: &Team, caller: SessionId, input: serde_json::Value, mail: bool) -> WaitOutcome {
    team.lifecycle
        .for_session(caller)
        .wait(
            WaitSpec::parse(&input, mail).unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap()
}

/// Hold the lead's own turn lease, as `exec`/`serve` does while the model's
/// `wait` tool call runs inside that turn.
fn hold_lead_turn(team: &Team) -> hya_engine::TurnLease {
    team.engine.try_begin_turn(team.root).unwrap()
}

#[tokio::test]
async fn wait_preserves_a_budget_kills_failure_and_reason() {
    let team = team_with_limits(hya_engine::SubagentLimits {
        per_team_message_budget: 1,
        ..hya_engine::SubagentLimits::default()
    })
    .await;
    let _lead = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    let mut bus = team.engine.bus().subscribe();
    team.engine
        .mail_send(
            team.root,
            MailEndpoint::parse(&handle),
            MailKind::Message,
            "one".into(),
        )
        .await
        .unwrap();
    team.engine
        .mail_send(
            team.root,
            MailEndpoint::parse(&handle),
            MailKind::Message,
            "two".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !team
            .engine
            .read_projection(team.root)
            .await
            .unwrap()
            .team
            .archived
            .contains_key(&handle)
        {
            bus.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    let outcome = wait(
        &team,
        team.root,
        json!({"targets": [handle], "timeout_secs": 1}),
        false,
    )
    .await;
    let member = outcome
        .finished
        .iter()
        .chain(&outcome.already_finished)
        .find(|member| member.session == child)
        .unwrap();
    assert_eq!(member.outcome.as_deref(), Some("failed"), "{outcome:?}");
    assert_eq!(
        member.report.as_deref(),
        Some("per-team message budget exceeded")
    );
}

#[tokio::test]
async fn wait_returns_when_a_member_reports_inside_the_lead_turn() {
    wait_returns_when_a_member_reports_inside_the_lead_turn_with_mail(false).await;
}

/// With the channel-aware wait the report's own mail (sent a moment before the
/// archive commits) must still resolve as a member finish, not a mail wake.
#[tokio::test]
async fn with_channels_a_report_still_wakes_as_a_finished_member() {
    wait_returns_when_a_member_reports_inside_the_lead_turn_with_mail(true).await;
}

async fn wait_returns_when_a_member_reports_inside_the_lead_turn_with_mail(mail: bool) {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (_child, handle) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        let handle = handle.clone();
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"targets": [handle]}), mail).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    // Mid-turn the member accepts its report; it executes after the turn.
    team.supervisor
        .submit_report(
            team.root,
            &handle,
            ReportOutcome::Done,
            "UNIT_SHIPPED".to_string(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiter.is_finished(), "the member is still mid-turn");
    team.provider.release_all();
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the report wakes the wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Members, "{outcome:?}");
    assert!(outcome.running.is_empty());
    let member = &outcome.finished[0];
    assert_eq!(member.handle, handle);
    assert_eq!(member.state, WaitMemberState::Reported, "{outcome:?}");
    assert_eq!(member.outcome.as_deref(), Some("done"));
    assert_eq!(member.report.as_deref(), Some("UNIT_SHIPPED"));
    assert_eq!(
        member.channel,
        Some(dm_channel(&team, &handle).await),
        "the full report stays readable on the parent's DM with the member"
    );
    assert!(
        outcome.mail.is_empty(),
        "the report mail is the member's finish, not separate mail: {outcome:?}"
    );
}

/// `any` returns on the first member that REPORTS; the other keeps running.
#[tokio::test]
async fn wait_any_returns_when_the_first_member_reports() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (a_session, a) = spawn(&team, team.root, true).await;
    let (_b, b) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"mode": "any"}), false).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiter.is_finished(), "both members are still working");
    team.supervisor
        .submit_report(team.root, &a, ReportOutcome::Done, "A_DONE".to_string())
        .await
        .unwrap();
    team.provider.release(a_session);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the report wakes the wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Members, "{outcome:?}");
    assert_eq!(outcome.finished.len(), 1, "{outcome:?}");
    assert_eq!(outcome.finished[0].handle, a);
    assert_eq!(outcome.finished[0].state, WaitMemberState::Reported);
    assert_eq!(outcome.running.len(), 1, "{outcome:?}");
    assert_eq!(outcome.running[0].handle, b);
    assert_eq!(outcome.running[0].state, WaitMemberState::Working);
    team.provider.release_all();
}

/// REPORT H8: a member that ends its turn with a final answer and no `report`
/// is finished with that answer as its `done` report — the parent's wait
/// wakes by `members`, not `stalled`.
#[tokio::test]
async fn a_member_that_ends_its_turn_with_an_answer_is_finished_with_it() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"timeout_secs": 30}), true).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    team.provider.release(child);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the answer finishes the member")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Members, "{outcome:?}");
    let member = &outcome.finished[0];
    assert_eq!(member.handle, handle);
    assert_eq!(member.state, WaitMemberState::Reported, "{outcome:?}");
    assert_eq!(member.outcome.as_deref(), Some("done"));
    assert_eq!(member.report.as_deref(), Some("done with the unit"));
    assert!(outcome.mail.is_empty(), "{outcome:?}");
}

/// A member whose turn ends with an answer while its own subagent is still
/// live is not reported (the report gate): it waits on that subagent, whose
/// report wakes it for a turn that then delivers the member's answer.
#[tokio::test]
async fn a_member_with_a_live_subagent_is_reported_only_after_it() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (member, handle) = spawn(&team, team.root, true).await;
    let (grandchild, _) = spawn(&team, member, true).await;
    team.provider.gate_members.store(false, Ordering::SeqCst);
    team.provider.release(member);
    for _ in 0..300 {
        if !team.engine.turn_active(member) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let now = wait(&team, team.root, json!({"timeout_secs": 0}), true).await;
    assert!(now.finished.is_empty(), "{now:?}");
    assert_eq!(now.running.len(), 1, "{now:?}");
    assert_eq!(now.running[0].handle, handle);
    assert_eq!(now.running[0].state, WaitMemberState::Working, "{now:?}");

    team.provider.release(grandchild);
    let mut report = None;
    for _ in 0..500 {
        report = team
            .engine
            .replay(team.root)
            .await
            .unwrap()
            .into_iter()
            .find_map(|envelope| match envelope.event {
                Event::SubagentReported { child, report, .. } if child == member => Some(report),
                _ => None,
            });
        if report.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        report.as_deref(),
        Some("done with the unit"),
        "the member reports once its subagent did"
    );
}

/// Run-4 evidence (reviewer after 824 s, scout after a follow-up): a member
/// whose turn ends without `report` and without an answer is NOT finished —
/// no `members` wake. It stopped for good (nothing is queued for it), so the
/// wait says so once (`stalled`) and a repeated wait blocks instead of
/// returning the same state again.
#[tokio::test]
async fn a_member_that_ends_its_turn_without_reporting_is_stalled_not_finished() {
    let team = team().await;
    team.provider.answer.lock().unwrap().clear();
    let _lead_turn = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"timeout_secs": 30}), true).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    team.provider.release(child);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("a stopped member must not leave the wait hanging")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Stalled, "{outcome:?}");
    assert!(outcome.finished.is_empty(), "idle is never finished");
    assert_eq!(outcome.running.len(), 1, "{outcome:?}");
    assert_eq!(outcome.running[0].handle, handle);
    assert_eq!(outcome.running[0].state, WaitMemberState::Idle);
    assert!(outcome.running[0].report.is_none(), "{outcome:?}");

    // Nothing new since: the next wait blocks until its timeout.
    let started = std::time::Instant::now();
    let again = wait(&team, team.root, json!({"timeout_secs": 1}), true).await;
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "a repeated wait on the same stopped member must block: {again:?}"
    );
    assert_eq!(again.woke_by, WaitWake::Timeout, "{again:?}");
    assert_eq!(again.running[0].state, WaitMemberState::Idle);
    assert!(again.finished.is_empty());
}

/// A member the user stopped mid-turn is not `stalled` (the lead is not
/// prompted to restart it) but it is nothing left to wait on: the wait returns
/// `nothing_to_wait_for` at once and lists it as stopped by the user.
#[tokio::test]
async fn a_wait_on_only_user_stopped_members_has_nothing_to_wait_for() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    for _ in 0..200 {
        if team.engine.turn_active(child) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut bus = team.engine.bus().subscribe();
    assert!(team.engine.cancel_user_turn(child), "it is mid-turn");
    let started = std::time::Instant::now();
    let outcome = wait(&team, team.root, json!({"timeout_secs": 30}), true).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a stopped member must not hold the wait to its timeout: {outcome:?}"
    );
    assert_eq!(outcome.woke_by, WaitWake::NothingToWaitFor, "{outcome:?}");
    assert_eq!(outcome.running.len(), 1, "{outcome:?}");
    assert_eq!(outcome.running[0].handle, handle);
    assert_eq!(outcome.running[0].state, WaitMemberState::StoppedByUser);
    let rendered = outcome.to_tool_result().to_string();
    assert!(rendered.contains("[stopped by the user:"), "{rendered}");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let projection = team.engine.read_projection(team.root).await.unwrap();
            if projection
                .team
                .roster
                .get(&handle)
                .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
            {
                assert!(!projection.team.archived.contains_key(&handle));
                break;
            }
            bus.recv().await.unwrap();
        }
    })
    .await
    .expect("a cancelled, live resident must become durably idle");
}

/// Run-4 evidence (scout): after its report the lead mails the member a
/// follow-up, which wakes it under the same handle. It is working again until
/// its NEXT report — the earlier report is neither `finished` nor
/// `already_finished` for a wait that starts after the wake.
#[tokio::test]
async fn a_member_woken_after_its_report_counts_as_working_until_its_next_report() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    team.supervisor
        .submit_report(
            team.root,
            &handle,
            ReportOutcome::Done,
            "FIRST_REPORT".to_string(),
        )
        .await
        .unwrap();
    team.provider.release(child);
    let first = wait(
        &team,
        team.root,
        json!({"targets": [handle.clone()], "timeout_secs": 10}),
        false,
    )
    .await;
    assert_eq!(first.woke_by, WaitWake::Members, "{first:?}");
    assert_eq!(first.finished[0].report.as_deref(), Some("FIRST_REPORT"));

    // Mail to the archived member wakes it (gated: its new turn is running).
    team.provider.gate_members.store(true, Ordering::SeqCst);
    team.engine
        .mail_send(
            team.root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "follow-up: verify the call sites".to_string(),
        )
        .await
        .unwrap();
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        let handle = handle.clone();
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"targets": [handle], "timeout_secs": 30}), true)
                        .unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !waiter.is_finished(),
        "the woken member is working; its previous report must not end the wait"
    );
    let mut accepted = false;
    for _ in 0..100 {
        if team
            .supervisor
            .submit_report(
                team.root,
                &handle,
                ReportOutcome::Done,
                "SECOND_REPORT".to_string(),
            )
            .await
            .is_ok()
        {
            accepted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(accepted, "the woken member can report again");
    team.provider.release(child);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the next report wakes the wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Members, "{outcome:?}");
    assert_eq!(outcome.finished.len(), 1, "{outcome:?}");
    assert_eq!(outcome.finished[0].state, WaitMemberState::Reported);
    assert_eq!(
        outcome.finished[0].report.as_deref(),
        Some("SECOND_REPORT"),
        "{outcome:?}"
    );
    assert!(outcome.already_finished.is_empty(), "{outcome:?}");
    assert!(outcome.mail.is_empty(), "{outcome:?}");
}

/// Targets that reported before the call are `already_finished`: returned at
/// once with `nothing_to_wait_for` (never a repeated `members` wake), and they
/// never satisfy `any` while another target is still running.
#[tokio::test]
async fn targets_that_already_reported_are_listed_once_as_already_finished() {
    let team = team().await;
    let (a_session, a) = spawn(&team, team.root, true).await;
    let (_b, b) = spawn(&team, team.root, true).await;
    team.supervisor
        .submit_report(team.root, &a, ReportOutcome::Done, "A_ONCE".to_string())
        .await
        .unwrap();
    team.provider.release(a_session);
    let first = wait(&team, team.root, json!({"targets": [a.clone()]}), true).await;
    assert_eq!(first.woke_by, WaitWake::Members, "{first:?}");
    assert_eq!(first.finished[0].report.as_deref(), Some("A_ONCE"));

    for _ in 0..2 {
        let started = std::time::Instant::now();
        let again = wait(&team, team.root, json!({"targets": [a.clone()]}), true).await;
        assert!(started.elapsed() < Duration::from_secs(1), "{again:?}");
        assert_eq!(again.woke_by, WaitWake::NothingToWaitFor, "{again:?}");
        assert!(again.finished.is_empty(), "{again:?}");
        assert!(again.mail.is_empty(), "{again:?}");
        assert_eq!(again.already_finished.len(), 1, "{again:?}");
        assert_eq!(again.already_finished[0].state, WaitMemberState::Reported);
        assert_eq!(again.already_finished[0].report.as_deref(), Some("A_ONCE"));
        let output = again.to_tool_result()["output"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(output.contains("already finished"), "{output}");
    }

    // `any` over [already finished, running] waits for the running one.
    let started = std::time::Instant::now();
    let mixed = wait(
        &team,
        team.root,
        json!({"targets": [a.clone(), b.clone()], "mode": "any", "timeout_secs": 1}),
        true,
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(900), "{mixed:?}");
    assert_eq!(mixed.woke_by, WaitWake::Timeout, "{mixed:?}");
    assert_eq!(mixed.already_finished.len(), 1, "{mixed:?}");
    assert_eq!(mixed.already_finished[0].handle, a);
    assert_eq!(mixed.running[0].handle, b);

    // Default targets are the live subagents only: the reported one is gone.
    let defaults = wait(&team, team.root, json!({"timeout_secs": 0}), true).await;
    assert_eq!(defaults.woke_by, WaitWake::Timeout, "{defaults:?}");
    assert!(defaults.already_finished.is_empty(), "{defaults:?}");
    assert_eq!(defaults.running.len(), 1, "{defaults:?}");
    assert_eq!(defaults.running[0].handle, b);
    team.provider.release_all();
}

/// The DM channel between the lead and a member, minted at registration.
async fn dm_channel(team: &Team, handle: &str) -> String {
    let projection = team.engine.read_projection(team.root).await.unwrap();
    projection
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == ChannelKind::Dm
                && channel.members.contains(handle)
                && channel.members.contains("main")
        })
        .map(|(id, _)| id.clone())
        .expect("registration mints the parent-child DM channel")
}

fn steer_everything() -> ChannelPolicySnapshot {
    ChannelPolicySnapshot {
        unit_leader: u8::MAX,
        unit_member: u8::MAX,
        dm_parent: u8::MAX,
        dm_child: u8::MAX,
    }
}

/// Run-4 evidence (the "RESEND — FULL REPORT" mail returned twice): mail a
/// wait returned is marked read durably — a second wait never returns it
/// again, and neither the steer notice of the lead's long-running turn (whose
/// channel list predates the member) nor a later resident wake of the lead
/// re-delivers it. New mail on that channel is still steered.
#[tokio::test]
async fn mail_a_wait_returned_is_never_delivered_again() {
    let team = team().await;
    let lead_turn = hold_lead_turn(&team);
    // The lead's turn began before the member (and its DM channel) existed.
    let mut stale_steer = team
        .engine
        .steer_mailbox_snapshot_with_policy(team.root, Some(steer_everything()))
        .await;
    let (child, handle) = spawn(&team, team.root, true).await;
    let mut steer = team
        .engine
        .steer_mailbox_snapshot_with_policy(team.root, Some(steer_everything()))
        .await;
    let dm = dm_channel(&team, &handle).await;
    team.engine
        .mail_send(
            child,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "RESEND_FULL_REPORT".to_string(),
        )
        .await
        .unwrap();
    let first = wait(&team, team.root, json!({"timeout_secs": 10}), true).await;
    assert_eq!(first.woke_by, WaitWake::Mail, "{first:?}");
    assert_eq!(first.mail.len(), 1, "{first:?}");
    assert!(first.mail[0].preview.contains("RESEND_FULL_REPORT"));
    assert_eq!(first.mail[0].channel.as_deref(), Some(dm.as_str()));

    let projection = team.engine.read_projection(team.root).await.unwrap();
    assert_eq!(
        projection.team.roster["main"].resident_cursor,
        projection.team.inboxes["main"].len() as u64,
        "the returned mail is consumed durably"
    );
    for mailbox in [&mut steer, &mut stale_steer] {
        let notice = mailbox.drain(&team.engine).await.unwrap();
        assert!(
            notice
                .as_deref()
                .is_none_or(|notice| !notice.contains("RESEND_FULL_REPORT")),
            "mail the wait returned must not be steered again: {notice:?}"
        );
    }

    let started = std::time::Instant::now();
    let second = wait(&team, team.root, json!({"timeout_secs": 1}), true).await;
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{second:?}"
    );
    assert_eq!(second.woke_by, WaitWake::Timeout, "{second:?}");
    assert!(second.mail.is_empty(), "{second:?}");

    // New mail on the channel minted after the turn began is still steered.
    let fresh_status = format!("FRESH_STATUS{}", "界".repeat(588));
    team.engine
        .mail_send(
            child,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            fresh_status.clone(),
        )
        .await
        .unwrap();
    let notice = stale_steer
        .drain(&team.engine)
        .await
        .unwrap()
        .expect("mail on a channel created mid-turn is steered");
    assert!(notice.contains(&fresh_status), "{notice}");
    assert!(!notice.contains("RESEND_FULL_REPORT"), "{notice}");

    // Preparing a steer notice does not deliver it. Let wait deliver the fresh
    // mail before releasing the turn lease.
    let fresh = wait(&team, team.root, json!({"timeout_secs": 0}), true).await;
    assert_eq!(fresh.mail.len(), 1);
    assert!(fresh.mail[0].preview.contains("FRESH_STATUS"));

    // The lead's turn ends: its resident wake must not re-inject either mail.
    drop(lead_turn);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let root = team.engine.read_projection(team.root).await.unwrap();
    let reinjected = root.session.messages.iter().any(|message| {
        message.role == Role::User
            && message.parts.iter().any(|part| {
                matches!(part, PartProjection::Text { text, .. }
                    if text.contains("RESEND_FULL_REPORT") || text.contains("FRESH_STATUS"))
            })
    });
    assert!(!reinjected, "delivered mail must not wake the lead again");
    team.provider.release_all();
}

/// `timeout_secs: 0` returns the current state at once; the mail it returns
/// is consumed, so the next snapshot is empty.
#[tokio::test]
async fn timeout_zero_returns_the_current_state_at_once() {
    let team = team().await;
    // Model `wait` runs inside the lead's active turn; keep its mail queued
    // instead of letting a resident wake deliver it before the snapshot.
    let _lead_turn = hold_lead_turn(&team);
    let (child, handle) = spawn(&team, team.root, true).await;
    team.engine
        .mail_send(
            child,
            MailEndpoint::Handle("^parent".to_string()),
            MailKind::Message,
            "SNAPSHOT_MAIL".to_string(),
        )
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let snapshot = wait(&team, team.root, json!({"timeout_secs": 0}), true).await;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(snapshot.woke_by, WaitWake::Mail, "{snapshot:?}");
    assert!(snapshot.mail[0].preview.contains("SNAPSHOT_MAIL"));
    assert_eq!(snapshot.running[0].handle, handle);
    assert_eq!(snapshot.running[0].state, WaitMemberState::Working);

    let started = std::time::Instant::now();
    let empty = wait(&team, team.root, json!({"timeout_secs": 0}), true).await;
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(empty.woke_by, WaitWake::Timeout, "{empty:?}");
    assert!(empty.mail.is_empty(), "{empty:?}");
    assert_eq!(empty.running[0].state, WaitMemberState::Working);
    team.provider.release_all();
}

#[tokio::test]
async fn wait_times_out_with_the_members_still_running() {
    let team = team().await;
    let (_child, handle) = spawn(&team, team.root, true).await;
    let started = std::time::Instant::now();
    let outcome = wait(&team, team.root, json!({"timeout_secs": 1}), false).await;
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(outcome.woke_by, WaitWake::Timeout);
    assert!(outcome.finished.is_empty());
    assert_eq!(outcome.running[0].handle, handle);
    assert_eq!(outcome.running[0].state, WaitMemberState::Working);
    team.provider.release_all();
}

#[tokio::test]
async fn cancelling_the_caller_aborts_the_wait_promptly() {
    let team = team().await;
    let _ = spawn(&team, team.root, true).await;
    let cancel = CancellationToken::new();
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        let cancel = cancel.clone();
        tokio::spawn(async move {
            lifecycle
                .wait(WaitSpec::parse(&json!({}), true).unwrap(), &cancel)
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("cancel aborts at once")
        .unwrap();
    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
    team.provider.release_all();
}

#[tokio::test]
async fn with_channels_mail_from_a_member_wakes_the_wait() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (child, _handle) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({}), true).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    team.engine
        .mail_send(
            child,
            MailEndpoint::Handle("^parent".to_string()),
            MailKind::Message,
            "PROGRESS: halfway".to_string(),
        )
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("mail wakes the channel-aware wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Mail, "{outcome:?}");
    assert!(outcome.mail[0].preview.contains("PROGRESS: halfway"));
    assert_eq!(outcome.running.len(), 1, "the member is still working");
    team.provider.release_all();
}

#[tokio::test]
async fn without_channels_mail_does_not_wake_the_wait() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (child, _handle) = spawn(&team, team.root, true).await;
    let waiter = {
        let lifecycle = team.lifecycle.for_session(team.root);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"timeout_secs": 1}), false).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    team.engine
        .mail_send(
            child,
            MailEndpoint::Handle("^parent".to_string()),
            MailKind::Message,
            "PROGRESS: halfway".to_string(),
        )
        .await
        .unwrap();
    let outcome = waiter.await.unwrap().unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Timeout, "{outcome:?}");
    assert!(outcome.mail.is_empty());
    team.provider.release_all();
}

/// A member's channel-aware wait wakes on harness mail (the LEADER FAILED
/// wrap-up notice), which bypasses the channel policy.
#[tokio::test]
async fn with_channels_harness_mail_wakes_a_member_wait() {
    let team = team().await;
    // No answer: the member stays live (an answer would be its report).
    team.provider.answer.lock().unwrap().clear();
    let (member, handle) = spawn(&team, team.root, false).await;
    // Let the member's own first turn finish; its wait then has no children.
    for _ in 0..300 {
        if !team.engine.turn_active(member) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let waiter = {
        let lifecycle = team.lifecycle.for_session(member);
        tokio::spawn(async move {
            lifecycle
                .wait(
                    WaitSpec::parse(&json!({"timeout_secs": 30}), true).unwrap(),
                    &CancellationToken::new(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let notice = format!("LEADER FAILED: {}", "界".repeat(5000));
    let expected = format!("{}…", notice.chars().take(600).collect::<String>());
    let envelope = team
        .engine
        .store()
        .append_harness_mail(team.root, &handle, notice)
        .await
        .unwrap();
    team.engine.bus().publish(envelope);
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("harness mail wakes the wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Mail, "{outcome:?}");
    assert_eq!(outcome.mail[0].from, hya_proto::HARNESS_HANDLE);
    assert_eq!(outcome.mail[0].preview, expected);
    assert!(provider_wait_output(&outcome.to_tool_result()).contains(&expected));
}

/// Encode the completed tool part exactly as a Responses provider request.
fn provider_wait_output(result: &serde_json::Value) -> String {
    let message = hya_proto::Message::Assistant {
        id: MessageId::new(),
        agent: "hya-scout".into(),
        model: "fake".into(),
        parts: vec![hya_proto::Part::Tool {
            id: hya_proto::PartId::new(),
            call_id: hya_proto::ToolCallId::new(),
            name: "wait".into(),
            state: hya_proto::ToolPartState::Completed {
                input: json!({}),
                output: result.clone(),
                time_ms: 0,
            },
            provider_data: None,
        }],
        finish: None,
        tokens: None,
        last_round: None,
        last_step_start: Some(0),
    };
    let items = hya_provider::encode_input_items(&[message], false).unwrap();
    assert_eq!(items[1]["type"], "function_call_output");
    let text = items[1]["output"].as_str().unwrap();
    assert!(text.chars().count() <= 4_500);
    text.to_owned()
}

#[tokio::test]
async fn direct_mail_survives_twenty_finished_subagent_reports() {
    let team = team().await;
    let _lead = hold_lead_turn(&team);
    let (parent, handle) = spawn(&team, team.root, true).await;
    let mut targets = Vec::new();
    let mut bus = team.engine.bus().subscribe();
    for _ in 0..20 {
        let (child, path) = spawn(&team, parent, true).await;
        team.supervisor
            .submit_report(team.root, &path, ReportOutcome::Done, "r".repeat(3000))
            .await
            .unwrap();
        team.provider.release(child);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !team
                .engine
                .read_projection(team.root)
                .await
                .unwrap()
                .team
                .archived
                .contains_key(&path)
            {
                bus.recv().await.unwrap();
            }
        })
        .await
        .unwrap();
        targets.push(path);
    }
    let notice = "LEADER FAILED: wrap up and report your work.";
    team.engine
        .store()
        .append_harness_mail(team.root, &handle, notice.into())
        .await
        .unwrap();
    let outcome = wait(
        &team,
        parent,
        json!({"targets":targets, "timeout_secs":0}),
        true,
    )
    .await;
    assert_eq!(outcome.already_finished.len(), 20);
    assert!(!outcome.more_mail);
    assert!(provider_wait_output(&outcome.to_tool_result()).contains(notice));
    team.provider.release(parent);
}

fn wait_call() -> FakeStep {
    FakeStep::ToolCall {
        name: "wait".into(),
        input: json!({"timeout_secs": 0}),
    }
}

/// Run the real mail-aware wait with one unread notice under the lead turn lease.
async fn run_wait_turn(
    team: &Team,
    engine: &SessionEngine,
    batch: Vec<FakeStep>,
) -> Result<FinishReason, hya_engine::CoreError> {
    let lease = hold_lead_turn(team);
    let prompt = engine
        .admit_user_prompt(team.root, "check for mail".into())
        .await
        .unwrap();
    super::report::mail_main_from_peer(engine, team.root, "direct notice").await;
    team.provider
        .tool_batches
        .lock()
        .unwrap()
        .insert(team.root, batch);
    engine
        .run_admitted_turn(
            lease,
            &team.lead,
            CancellationToken::new(),
            &[],
            None,
            None,
            prompt,
        )
        .await
}

/// Run two real waits under one outer tool-result transaction.
struct ReadPages;

#[async_trait]
impl hya_tool::Tool for ReadPages {
    fn name(&self) -> &str {
        "read_pages"
    }

    fn schema(&self) -> hya_proto::ToolSchema {
        hya_proto::ToolSchema {
            name: self.name().into(),
            description: "Read successive mail pages".into(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }
    }

    async fn execute(
        &self,
        ctx: &hya_tool::ToolCtx,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, ToolError> {
        let first = ctx
            .nested
            .call("receive", json!({"timeout_secs":0}))
            .await?;
        if input["fail"] == true {
            return Err(ToolError::Other("discard first page".into()));
        }
        let second = ctx
            .nested
            .call("receive", json!({"timeout_secs":0}))
            .await?;
        let pages = [first, second].map(|page| {
            page["output"]
                .as_str()
                .unwrap()
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("notice ")?
                        .split_once(' ')
                        .map(|(id, _)| id.to_owned())
                })
                .collect::<Vec<_>>()
        });
        Ok(json!({"pages": pages}))
    }
}

async fn mail_reader_team() -> Team {
    use hya_bundle::{AgentRole, BundleCatalog};
    use hya_engine::{AgentCatalog, RuntimeRegistry};

    let agent = "mail-reader";
    let fixture = support::test_catalog(&[(agent, AgentRole::Main, &[])]);
    let mut bundle = (*fixture.bundles().bundles()[0]).clone();
    let view = &mut bundle.agents[0].resource_view;
    view.allow = vec!["harness:tool/wait".into(), "harness:tool/read_pages".into()];
    view.aliases
        .insert("receive".into(), "harness:tool/wait".into());
    let bundles = BundleCatalog::from_prepared(&[bundle]).unwrap();
    let catalog = Arc::new(AgentCatalog::new(Arc::new(bundles)).unwrap());
    let tools = ToolRegistry::builtins();
    tools.register(Arc::new(ReadPages)).unwrap();
    team_with_runtime(
        Default::default(),
        Arc::new(RuntimeRegistry::new(tools, catalog)),
        agent,
    )
    .await
}

#[tokio::test]
async fn replaced_wait_result_leaves_mail_for_the_next_wait() {
    for name in ["wait", "receive"] {
        let team = if name == "wait" {
            team().await
        } else {
            mail_reader_team().await
        };
        let call = || FakeStep::ToolCall {
            name: name.into(),
            input: json!({"timeout_secs": 0}),
        };
        let engine = (*team.engine)
            .clone()
            .with_hooks(Arc::new(super::hooks_seam::MaskFirstResult::default()));
        run_wait_turn(&team, &engine, vec![call(), call()])
            .await
            .unwrap();
        let events = engine.replay(team.root).await.unwrap();
        let deliveries: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolResult { output, .. } if output == &json!({"masked": true}) => {
                    Some("masked")
                }
                Event::ToolResult { output, .. } => {
                    assert!(provider_wait_output(output).contains("direct notice"));
                    Some("delivered")
                }
                Event::MailConsumed { .. } => Some("ack"),
                _ => None,
            })
            .collect();
        assert_eq!(deliveries, ["masked", "delivered", "ack"]);
    }
}

#[tokio::test]
async fn wait_commits_mail_and_result_in_one_transaction() {
    for name in ["wait", "receive"] {
        for reject in [Some("tool_result"), Some("mail_consumed"), None] {
            let team = if name == "wait" {
                team().await
            } else {
                mail_reader_team().await
            };
            if let Some(tag) = reject {
                let mut tx = team.engine.store().begin_transaction().await.unwrap();
                sqlx::query(&format!("CREATE TRIGGER reject_delivery BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = '{tag}' BEGIN SELECT RAISE(ABORT, 'injected wait write failure'); END"))
                .execute(&mut *tx).await.unwrap();
                tx.commit().await.unwrap();
            }
            let result = run_wait_turn(
                &team,
                &team.engine,
                vec![FakeStep::ToolCall {
                    name: name.into(),
                    input: json!({"timeout_secs": 0}),
                }],
            )
            .await;
            let events = team.engine.replay(team.root).await.unwrap();
            let delivered = events
                .iter()
                .filter_map(|e| match &e.event {
                    Event::ToolResult { output, .. } => {
                        assert!(provider_wait_output(output).contains("direct notice"));
                        Some("result")
                    }
                    Event::MailConsumed { through, .. } => {
                        assert_eq!(*through, 1);
                        Some("ack")
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if reject.is_some() {
                let error = result.unwrap_err().to_string();
                assert!(error.contains("injected wait write failure"), "{error}");
                assert!(delivered.is_empty(), "{reject:?}: {delivered:?}");
            } else {
                result.unwrap();
                assert_eq!(delivered, ["result", "ack"]);
            }
        }
    }
}

#[tokio::test]
async fn nested_wait_pages_commit_with_the_surviving_outer_result() {
    let team = mail_reader_team().await;
    let notices: Vec<_> = (0..10)
        .map(|i| format!("notice {i} {}", "x".repeat(590)))
        .collect();
    let batch = [true, false].map(|fail| FakeStep::ToolCall {
        name: "read_pages".into(),
        input: json!({"fail":fail}),
    });
    let _lease = hold_lead_turn(&team);
    let (reader, handle) = spawn_agent(&team, team.root, true, "mail-reader").await;
    for notice in &notices {
        team.engine
            .store()
            .append_harness_mail(team.root, &handle, notice.clone())
            .await
            .unwrap();
    }
    let cursor = team
        .engine
        .read_projection(team.root)
        .await
        .unwrap()
        .team
        .inboxes[&handle]
        .len();
    let mut bus = team.engine.bus().subscribe();
    team.provider
        .tool_batches
        .lock()
        .unwrap()
        .insert(reader, batch.into());
    team.provider.gate_members.store(false, Ordering::SeqCst);
    team.provider.release(reader);
    let events = tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = Vec::new();
        loop {
            let e = bus.recv().await.unwrap();
            let done = matches!(&e.event, Event::MailConsumed {handle: recipient, ..} if recipient == &handle);
            if done || e.event.session() == Some(reader) {
                events.push(e);
            }
            if done { return events; }
        }
    }).await.unwrap();
    let deliveries: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolError { message_text, .. } => {
                assert!(
                    message_text.contains("discard first page"),
                    "{message_text}"
                );
                Some("error")
            }
            Event::ToolResult { output, .. } => {
                let pages = output["pages"].as_array().unwrap();
                assert_eq!(pages.len(), 2);
                let ids: Vec<_> = pages
                    .iter()
                    .flat_map(|page| {
                        let ids = page.as_array().unwrap();
                        assert!(!ids.is_empty());
                        ids.iter()
                            .map(|id| id.as_str().unwrap().parse::<usize>().unwrap())
                    })
                    .collect();
                assert_eq!(ids, (0..notices.len()).collect::<Vec<_>>());
                Some("result")
            }
            Event::MailConsumed { through, .. } => {
                assert_eq!(usize::try_from(*through).unwrap(), cursor);
                Some("ack")
            }
            _ => None,
        })
        .collect();
    assert_eq!(deliveries, ["error", "result", "ack"]);
}

#[tokio::test]
async fn direct_mail_pages_survive_tool_result_steering_and_provider_encoding() {
    let team = team().await;
    let _lead = hold_lead_turn(&team);
    let (member, handle) = spawn(&team, team.root, true).await;
    let notices: Vec<_> = (0..10)
        .map(|i| format!("notice {i} {}!", "界".repeat(590)))
        .collect();
    for notice in &notices {
        // Own channel posts occupy raw inbox indices but aren't returned as mail.
        team.engine
            .mail_send(
                member,
                MailEndpoint::Handle("^parent".into()),
                MailKind::Message,
                "progress".into(),
            )
            .await
            .unwrap();
        let envelope = team
            .engine
            .store()
            .append_harness_mail(team.root, &handle, notice.clone())
            .await
            .unwrap();
        team.engine.bus().publish(envelope);
    }
    let mut bus = team.engine.bus().subscribe();
    team.provider.tool_batches.lock().unwrap().insert(
        member,
        vec![
            wait_call(),
            FakeStep::ToolCall {
                name: "list_agents".into(),
                input: json!({}),
            },
            wait_call(),
        ],
    );
    team.provider.gate_members.store(false, Ordering::SeqCst);
    team.provider.release(member);
    let mut delivered = 0;
    for call in 0..3 {
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match bus.recv().await.unwrap().event {
                    Event::ToolResult {
                        session, output, ..
                    } if session == member => break output,
                    Event::ToolError {
                        session,
                        message_text,
                        ..
                    } if session == member => panic!("{message_text}"),
                    _ => {}
                }
            }
        })
        .await
        .expect("wait tool result");
        let text = provider_wait_output(&result);
        let rows = result["metadata"]["mail"].as_array();
        if call == 1 {
            assert!(result["agents"].is_array(), "{result}");
            assert!(
                !text.contains("NEW MAIL") && !text.contains("notice "),
                "{text}"
            );
            continue;
        }
        let count = rows.unwrap().len();
        assert!(count > 0);
        for (index, notice) in notices.iter().enumerate() {
            assert_eq!(notice.chars().count(), 600);
            assert_eq!(
                text.contains(notice),
                (delivered..delivered + count).contains(&index)
            );
        }
        delivered += count;
        assert_eq!(
            result["metadata"].get("more_mail"),
            (call == 0).then_some(&serde_json::Value::Bool(true))
        );
        assert_eq!(text.contains("call `wait` again"), call == 0);
    }
    assert_eq!(delivered, notices.len());
    let cursors: Vec<_> = team
        .engine
        .replay(team.root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::MailConsumed {
                handle: recipient,
                through,
                ..
            } if recipient == handle => Some(through),
            _ => None,
        })
        .collect();
    assert_eq!(cursors.len(), 2, "{cursors:?}");
    assert!(cursors[0] < cursors[1]);
}

#[tokio::test]
async fn wait_without_subagents_returns_nothing_to_wait_for() {
    let team = team().await;
    let outcome = wait(&team, team.root, json!({}), false).await;
    assert_eq!(outcome.woke_by, WaitWake::NothingToWaitFor);
    let unknown = team
        .lifecycle
        .for_session(team.root)
        .wait(
            WaitSpec::parse(&json!({"targets": ["ghost-1"]}), false).unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&unknown, ToolError::Input(message) if message.contains("ghost-1")),
        "{unknown:?}"
    );
}
