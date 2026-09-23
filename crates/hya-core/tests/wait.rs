//! The `wait` lifecycle request (0.41.0): block the caller — typically inside
//! the lead's own active turn — until its subagents finish their current
//! work, woken through the engine bus; optionally also on incoming mail (the
//! channel-tools override), including harness mail; bounded by a timeout and
//! aborted by the caller's cancellation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

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
    AgentName, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef, ReportOutcome,
    SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    LifecyclePlane, PermissionPlane, PermissionRules, ToolError, ToolRegistry, WaitMemberState,
    WaitOutcome, WaitSpec, WaitWake,
};
use serde_json::json;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// While `gate_members` is set, every stream of a non-root session parks
/// until `release` (set before the spawn, so the first turn cannot race it).
#[derive(Default)]
struct GateProvider {
    root: Mutex<Option<SessionId>>,
    gate_members: AtomicBool,
    release: Notify,
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
            self.release.notified().await;
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
    team.provider.release.notify_waiters();
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
}

#[tokio::test]
async fn wait_any_returns_when_a_busy_member_goes_idle() {
    let team = team().await;
    let _lead_turn = hold_lead_turn(&team);
    let (_a, a) = spawn(&team, team.root, true).await;
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
    team.provider.release.notify_one();
    let outcome = tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .expect("the idle transition wakes the wait")
        .unwrap()
        .unwrap();
    assert_eq!(outcome.woke_by, WaitWake::Members);
    assert_eq!(outcome.finished.len(), 1, "{outcome:?}");
    assert_eq!(outcome.finished[0].state, WaitMemberState::Idle);
    assert_eq!(outcome.running.len(), 1, "{outcome:?}");
    let handles = [&outcome.finished[0].handle, &outcome.running[0].handle];
    assert!(handles.contains(&&a) && handles.contains(&&b));
    team.provider.release.notify_waiters();
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
    team.provider.release.notify_waiters();
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
    team.provider.release.notify_waiters();
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
    team.provider.release.notify_waiters();
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
    team.provider.release.notify_waiters();
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
