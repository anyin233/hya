//! The `wait` lifecycle request (0.41.0): block the caller — typically inside
//! the lead's own active turn — until its subagents finish (report or are
//! archived; idle never counts), woken through the engine bus; optionally also
//! on new mail (the channel-tools override), including harness mail, which is
//! returned once and marked read; bounded by a timeout and aborted by the
//! caller's cancellation. Targets that finished before the call are listed as
//! `already_finished` and never wake it; a target that stops without a report
//! wakes it once as `stalled`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use hya_core::{
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
#[derive(Default)]
struct GateProvider {
    root: Mutex<Option<SessionId>>,
    gate_members: AtomicBool,
    gates: Mutex<HashMap<SessionId, Arc<Notify>>>,
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
        let is_root = *self.root.lock().unwrap() == Some(session);
        if !is_root && self.gate_members.load(Ordering::SeqCst) {
            let gate = self.gate(session);
            gate.notified().await;
        }
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("done with the unit".to_string()),
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

struct Team {
    engine: Arc<SessionEngine>,
    supervisor: Arc<ResidentSupervisor>,
    provider: Arc<GateProvider>,
    root: SessionId,
    lifecycle: LifecyclePlane,
}

async fn team() -> Team {
    let provider = Arc::new(GateProvider::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(provider.clone()));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let root = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
        })
        .await
        .unwrap();
    *provider.root.lock().unwrap() = Some(root);
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead = spec("build");
    let binding = engine.bind_runtime(&lead.workdir).unwrap();
    let resources = binding.agent_resource_policy("build").unwrap();
    supervisor
        .ensure_main(root, lead, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
    let (lifecycle, rx) = LifecyclePlane::new();
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        supervisor.clone(),
        rx,
    ));
    Team {
        engine,
        supervisor,
        provider,
        root,
        lifecycle,
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
    let agent = spec("explore");
    let binding = team.engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("explore").unwrap();
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
fn hold_lead_turn(team: &Team) -> hya_core::TurnLease {
    team.engine.try_begin_turn(team.root).unwrap()
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

/// Run-4 evidence (reviewer after 824 s, scout after a follow-up): a member
/// whose turn ends without `report` is NOT finished — no `members` wake, no
/// in-progress text surfaced as a report. It stopped for good (nothing is
/// queued for it), so the wait says so once (`stalled`) and a repeated wait
/// blocks instead of returning the same state again.
#[tokio::test]
async fn a_member_that_ends_its_turn_without_reporting_is_stalled_not_finished() {
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
        .expect("a stopped member must not leave the wait hanging")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Stalled, "{outcome:?}");
    assert!(outcome.finished.is_empty(), "idle is never finished");
    assert_eq!(outcome.running.len(), 1, "{outcome:?}");
    assert_eq!(outcome.running[0].handle, handle);
    assert_eq!(outcome.running[0].state, WaitMemberState::Idle);
    assert!(outcome.running[0].report.is_none(), "{outcome:?}");
    let rendered = outcome.to_tool_result().to_string();
    assert!(
        !rendered.contains("done with the unit"),
        "in-progress assistant text is never a report: {rendered}"
    );

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
    team.engine
        .mail_send(
            child,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "FRESH_STATUS".to_string(),
        )
        .await
        .unwrap();
    let notice = stale_steer
        .drain(&team.engine)
        .await
        .unwrap()
        .expect("mail on a channel created mid-turn is steered");
    assert!(notice.contains("FRESH_STATUS"), "{notice}");
    assert!(!notice.contains("RESEND_FULL_REPORT"), "{notice}");

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
    let envelope = team
        .engine
        .store()
        .append_harness_mail(team.root, &handle, "LEADER FAILED: wrap up".to_string())
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
    assert!(outcome.mail[0].preview.contains("LEADER FAILED"));
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
