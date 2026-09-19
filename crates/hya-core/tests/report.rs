//! Unified lifecycle core (ADR-0015): report gate, handoff, archive, revive.
//!
//! These tests drive the `ResidentSupervisor` directly — the `report` tool and
//! the write-gate plane wire on top of these semantics later.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

mod support;

use async_trait::async_trait;
use futures::stream;
use hya_proto::{
    AgentName, ArchiveReason, Event, FinishReason, MailEndpoint, MailKind, MessageId, ModelRef,
    PartProjection, ReportOutcome, RosterStatus, SessionId,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

use hya_core::CoreError;
use hya_core::bus::EventBus;
use hya_core::engine::{AgentSpec, CreateSession, SessionEngine};
use hya_core::resident::ResidentSupervisor;

/// Minimal fake route: one text part, then stop — enough to drive a turn.
struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
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

async fn engine() -> Arc<SessionEngine> {
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(StubProvider)));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ))
}

fn agent_spec() -> AgentSpec {
    AgentSpec {
        name: AgentName::new("explore"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    }
}

async fn root_team(engine: &SessionEngine) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
        })
        .await
        .unwrap()
}

async fn ensure_main(supervisor: &ResidentSupervisor, engine: &SessionEngine, root: SessionId) {
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("build").unwrap();
    supervisor
        .ensure_main(root, agent, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
}

/// Spawn a resident under `parent`, armed with `directive`, and wait until its
/// first turn finished and the slot went idle.
async fn spawn_idle_resident(
    supervisor: &ResidentSupervisor,
    engine: &SessionEngine,
    parent: SessionId,
    directive: &str,
) -> (SessionId, String) {
    let agent = agent_spec();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, handle) = supervisor
        .spawn_resident(
            parent,
            agent,
            (binding, Arc::from([]), resources, None),
            directive.to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    let (root, _) = engine.session_lineage(parent).await.unwrap();
    wait_idle(engine, root, &handle, child).await;
    (child, handle)
}

async fn wait_idle(engine: &SessionEngine, root: SessionId, handle: &str, child: SessionId) {
    for _ in 0..300 {
        let projection = engine.read_projection(root).await.unwrap();
        let turn_done = engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .any(|message| message.role == hya_proto::Role::Assistant);
        if turn_done
            && projection
                .team
                .roster
                .get(handle)
                .is_some_and(|entry| entry.status == RosterStatus::Idle)
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let projection = engine.read_projection(root).await.unwrap();
    let rows: Vec<String> = projection
        .team
        .roster
        .iter()
        .map(|(path, entry)| format!("{path}={:?} task={:?}", entry.status, entry.current_task))
        .collect();
    panic!("resident {handle} never went idle; roster: {rows:?}");
}

#[tokio::test]
async fn report_gate_rejects_unread_mail() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) =
        spawn_idle_resident(&supervisor, &engine, root, "explore the tree").await;

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "one question first".to_string(),
        )
        .await
        .unwrap();

    let result = supervisor.report_gate(root, &handle).await;
    match result {
        Err(CoreError::Invalid(message)) => assert!(
            message.contains("unread"),
            "gate error should mention unread mail: {message}"
        ),
        other => panic!("expected unread-mail rejection, got {other:?}"),
    }
    // Nothing archived: the roster row is still live.
    let projection = engine.read_projection(root).await.unwrap();
    assert!(projection.team.roster.contains_key(&handle));
}

#[tokio::test]
async fn report_gate_rejects_live_children() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (lead, lead_handle) =
        spawn_idle_resident(&supervisor, &engine, root, "lead the work").await;
    let _ = spawn_idle_resident(&supervisor, &engine, lead, "do the sub-work").await;

    let result = supervisor.report_gate(root, &lead_handle).await;
    match result {
        Err(CoreError::Invalid(message)) => assert!(
            message.contains("child"),
            "gate error should mention live children: {message}"
        ),
        other => panic!("expected live-children rejection, got {other:?}"),
    }
}

#[tokio::test]
async fn report_archives_the_agent_end_to_end() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore the tree").await;

    supervisor
        .report_and_archive(
            root,
            &handle,
            ReportOutcome::Done,
            "found three call sites".to_string(),
        )
        .await
        .unwrap();

    // Roster row gone, archive history present.
    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(&handle));
    let archived = projection.team.archived.get(&handle).unwrap();
    assert_eq!(archived.reason, ArchiveReason::Reported);
    assert_eq!(archived.session, child);

    // The parent received the report as mail.
    let inbox = projection.team.inboxes.get("main").unwrap();
    assert!(
        inbox
            .iter()
            .any(|message| message.body.contains("found three call sites")),
        "report mail must reach the parent inbox"
    );

    // The member row on the parent log is terminal with the report summary.
    assert!(
        projection
            .session
            .members
            .iter()
            .any(|member| member.summary.contains("found three call sites")),
        "SubagentReported must mark the member terminal"
    );

    // The child carries a (here: degraded, no summarizer wired) handoff.
    let child_projection = engine.read_projection(child).await.unwrap();
    let handoff = child_projection.session.handoff.as_ref().unwrap();
    assert!(handoff.degraded);
    assert!(handoff.doc.contains("Goal"));

    // The durable log has the archive marker and the claim is released.
    let replay = engine.replay(root).await.unwrap();
    assert!(
        replay.iter().any(|envelope| matches!(
            &envelope.event,
            Event::AgentArchived {
                reason: ArchiveReason::Reported,
                ..
            }
        )),
        "AgentArchived must be on the root log"
    );
    assert!(
        !engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child),
        "archive must release the actor claim"
    );
}

#[tokio::test]
async fn revive_by_parent_mail_rearms_an_episode_from_the_handoff() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore the tree").await;
    supervisor
        .report_and_archive(
            root,
            &handle,
            ReportOutcome::Done,
            "partial answer".to_string(),
        )
        .await
        .unwrap();

    // A downward mail to the archived handle revives it.
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "continue: check the tests too".to_string(),
        )
        .await
        .unwrap();

    // The agent is live again and the restart is durable.
    let projection = engine.read_projection(root).await.unwrap();
    assert!(
        projection.team.roster.contains_key(&handle),
        "revive must re-register the roster row"
    );
    let replay = engine.replay(root).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::AgentRestarted { .. })),
        "AgentRestarted must be on the root log"
    );

    // The revived episode ran: the child transcript carries the follow-up mail.
    wait_idle(&engine, root, &handle, child).await;
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(
        child_projection
            .session
            .messages
            .iter()
            .any(|message| message.parts.iter().any(
                |part| matches!(part, PartProjection::Text { text, .. } if text
                .contains("check the tests too"))
            )),
        "the reviving mail must arm the new episode"
    );
}

#[tokio::test]
async fn revive_is_rejected_for_agents_the_caller_did_not_spawn() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (lead, lead_handle) = spawn_idle_resident(&supervisor, &engine, root, "lead").await;
    let (_worker, worker_handle) = spawn_idle_resident(&supervisor, &engine, lead, "work").await;
    // Both archive; the root then tries to revive the GRANDCHILD directly.
    supervisor
        .report_and_archive(root, &worker_handle, ReportOutcome::Done, String::new())
        .await
        .unwrap();
    // The worker's report mail wakes the lead to consume it; let that turn
    // settle before the lead reports its own terminal state. Retry briefly
    // with diagnostics: roster-Busy means a real re-arm; roster-Idle means a
    // slot-ordering bug.
    wait_idle(&engine, root, &lead_handle, lead).await;
    let mut lead_reported = false;
    for _ in 0..200 {
        match supervisor
            .report_and_archive(root, &lead_handle, ReportOutcome::Done, String::new())
            .await
        {
            Ok(()) => {
                lead_reported = true;
                break;
            }
            Err(CoreError::Invalid(message)) if message.contains("still working") => {
                let projection = engine.read_projection(root).await.unwrap();
                let entry = projection.team.roster.get(&lead_handle).unwrap();
                assert!(
                    entry.status == RosterStatus::Busy,
                    "slot busy but roster {:?}: ordering bug ({message})",
                    entry.status
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(other) => panic!("unexpected lead report error: {other:?}"),
        }
    }
    assert!(lead_reported, "lead never settled to report");

    let result = engine
        .mail_send(
            root,
            MailEndpoint::Handle(worker_handle.clone()),
            MailKind::Message,
            "skip levels".to_string(),
        )
        .await;
    assert!(
        result.is_err(),
        "revive must be limited to the caller's own direct children"
    );
    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(&worker_handle));
}

#[tokio::test]
async fn stop_still_terminates_a_resident_without_report() {
    // Sanity anchor: the pre-existing stop path keeps working alongside the
    // new archive machinery (engine-synthesized terminality lands on it).
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;
    supervisor.stop_resident(root, &handle).await.unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get(&handle).unwrap();
    assert_eq!(entry.status, RosterStatus::Failed);
}

#[tokio::test]
async fn submit_report_archives_an_idle_agent_after_the_turn() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    // The tool-shaped entry: gate feedback now, archive once the actor is at
    // rest (here: immediately, since the resident is idle).
    supervisor
        .submit_report(root, &handle, ReportOutcome::Done, "shipped".to_string())
        .await
        .unwrap();

    for _ in 0..300 {
        let projection = engine.read_projection(root).await.unwrap();
        if !projection.team.roster.contains_key(&handle) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("submit_report never archived the idle resident");
}

#[tokio::test]
async fn kill_archives_with_a_synthesized_failure_report() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;

    supervisor
        .kill_and_archive(root, &handle, "stuck")
        .await
        .unwrap();

    let projection = engine.read_projection(root).await.unwrap();
    assert!(
        !projection.team.roster.contains_key(&handle),
        "kill archives"
    );
    let archived = projection.team.archived.get(&handle).unwrap();
    assert_eq!(archived.reason, ArchiveReason::Killed);
    let inbox = projection.team.inboxes.get("main").unwrap();
    assert!(
        inbox.iter().any(|message| message.body.contains("stuck")),
        "the killer receives the synthesized failure report: {:?}",
        inbox
    );
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(
        child_projection
            .session
            .handoff
            .as_ref()
            .is_some_and(|handoff| handoff.degraded),
        "kill synthesizes a degraded handoff"
    );
}

#[tokio::test]
async fn root_teardown_force_archives_live_descendants() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (lead, lead_handle) = spawn_idle_resident(&supervisor, &engine, root, "lead").await;
    let (worker, worker_handle) = spawn_idle_resident(&supervisor, &engine, lead, "work").await;

    engine.force_archive_team(root).await.unwrap();

    let projection = engine.read_projection(root).await.unwrap();
    assert!(
        !projection.team.roster.keys().any(|path| path != "main"),
        "teardown empties the live roster below the root"
    );
    for handle in [&lead_handle, &worker_handle] {
        let archived = projection
            .team
            .archived
            .get(handle)
            .unwrap_or_else(|| panic!("`{handle}` must be archived"));
        assert_eq!(archived.reason, ArchiveReason::RootTeardown);
    }
    let active = engine.store().active_actor_ids().await.unwrap();
    assert!(
        !active.contains(&lead) && !active.contains(&worker),
        "claims released"
    );
}

#[tokio::test]
async fn registration_mints_the_unit_group_and_pair_dm_channels() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) =
        spawn_idle_resident(&supervisor, &engine, root, "explore the tree").await;

    let projection = engine.read_projection(root).await.unwrap();
    // One group channel for the root's unit: leader (main) + the child, unit
    // path recorded, minted id shape.
    let group = projection
        .team
        .channels
        .values()
        .find(|channel| {
            channel.kind == hya_proto::ChannelKind::Group && channel.unit.as_deref() == Some("main")
        })
        .expect("unit group channel minted");
    assert!(group.members.contains("main"));
    assert!(group.members.contains(&handle));
    assert!(
        projection
            .team
            .channels
            .keys()
            .any(|key| key.starts_with("announce-"))
    );

    // One DM channel for the pair, persisted with exactly the two members.
    let dm = projection
        .team
        .channels
        .values()
        .find(|channel| {
            channel.kind == hya_proto::ChannelKind::Dm
                && channel.members.contains("main")
                && channel.members.contains(&handle)
        })
        .expect("pair DM channel minted");
    assert_eq!(dm.members.len(), 2);
    assert!(dm.unit.is_none());
}

#[tokio::test]
async fn deleting_the_root_session_force_archives_descendants() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (lead, _lead_handle) = spawn_idle_resident(&supervisor, &engine, root, "lead").await;
    let (_worker, _worker_handle) = spawn_idle_resident(&supervisor, &engine, lead, "work").await;

    // Claims release is observable across the deletion: after teardown both
    // residents' actor claims are gone from the store.
    let active_before: Vec<_> = engine.store().active_actor_ids().await.unwrap();
    assert!(active_before.contains(&lead));
    engine.delete_session(root).await.unwrap();
    let active_after = engine.store().active_actor_ids().await.unwrap();
    assert!(
        !active_after.contains(&lead),
        "delete-driven teardown must release descendant claims: {active_after:?}"
    );
}

#[tokio::test]
async fn steer_surfaces_mail_inside_a_root_turn_tool_result() {
    // A provider that first issues one harmless tool call (ls) then finishes:
    // the steer notice must ride that tool result.
    struct OneToolProvider {
        called: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl Provider for OneToolProvider {
        fn id(&self) -> &str {
            "fake"
        }
        fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
            (model.as_str() == "fake").then_some(Capabilities {
                streaming_tool_calls: true,
                parallel_tool_calls: false,
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
            use std::sync::atomic::Ordering;
            let first = !self.called.swap(true, Ordering::SeqCst);
            let steps = if first {
                vec![
                    FakeStep::ToolCall {
                        name: "ls".to_string(),
                        input: serde_json::json!({"path": "."}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ]
            } else {
                vec![
                    FakeStep::Text("done".to_string()),
                    FakeStep::Finish(FinishReason::Stop),
                ]
            };
            let events = FakeProvider::materialize(&steps, session, message);
            Ok(Box::pin(futures::stream::iter(
                events.into_iter().map(Ok::<Event, ProviderError>),
            )))
        }
    }
    let (permission, _permission_rx) =
        PermissionPlane::new(PermissionRules::new(vec![hya_tool::Rule {
            action: hya_tool::Action::Read,
            resource_pattern: "*".to_string(),
            mode: hya_tool::Mode::Allow,
        }]));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(OneToolProvider {
            called: std::sync::atomic::AtomicBool::new(false),
        }))),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;

    engine
        .mail_send(
            root,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "steer payload for main".to_string(),
        )
        .await
        .unwrap();

    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: String::new(),
        workdir: PathBuf::from("."),
        reasoning: None,
    };
    engine
        .admit_user_prompt(root, "list the directory then stop".to_string())
        .await
        .unwrap();
    let finish = engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &agent,
            CancellationToken::new(),
            &[],
            None,
            None,
        )
        .await;
    let _ = finish;

    // The durable cursor for main advanced past the steered mail.
    let projection = engine.read_projection(root).await.unwrap();
    let entry = projection.team.roster.get("main").unwrap();
    assert!(
        entry.resident_cursor >= 1,
        "steer must advance main's durable cursor (cursor={})",
        entry.resident_cursor
    );
    let replay = engine.replay(root).await.unwrap();
    assert!(
        replay
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::MailConsumed { .. })),
        "MailConsumed must be committed"
    );
    // And the tool result carried the notice text.
    let tool_results: Vec<String> = replay
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => serde_json::to_string(output).ok(),
            _ => None,
        })
        .collect();
    assert!(
        tool_results
            .iter()
            .any(|text| text.contains("steer payload for main")),
        "some tool result must embed the steered mail; got {tool_results:?}"
    );
}

#[tokio::test]
async fn channel_read_returns_history_and_marks_inbox_seen() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    // Two broadcasts on the unit group channel; the child reads it.
    for body in ["first note", "second note"] {
        engine.mail_announce(root, body.to_string()).await.unwrap();
    }
    let projection = engine.read_projection(root).await.unwrap();
    let group = projection
        .team
        .channels
        .keys()
        .find(|key| key.starts_with("announce-"))
        .unwrap()
        .clone();
    let child: SessionId = projection.team.roster.get(&handle).unwrap().session;

    let latest = engine
        .read_channel_history(child, &group, None)
        .await
        .unwrap();
    assert_eq!(latest.messages.len(), 1, "no `last` reads exactly one");
    assert_eq!(
        latest.messages[0].1, "second note",
        "the latest message wins"
    );

    let history = engine
        .read_channel_history(child, &group, Some(10))
        .await
        .unwrap();
    assert_eq!(history.messages.len(), 2);
    assert_eq!(
        history.messages[0].1, "second note",
        "newest-first ordering"
    );
    assert_eq!(history.messages[1].1, "first note");

    // Reading marked the inbox seen: the durable cursor caught up.
    let projection = engine.read_projection(root).await.unwrap();
    let inbox_len = projection
        .team
        .inboxes
        .get(&handle)
        .map(Vec::len)
        .unwrap_or(0) as u64;
    let cursor = projection.team.roster.get(&handle).unwrap().resident_cursor;
    assert!(
        cursor >= inbox_len,
        "channel read must mark the inbox seen (cursor={cursor}, inbox={inbox_len})"
    );
    assert!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::MailConsumed { .. }))
    );
}

#[tokio::test]
async fn steer_notice_truncation_survives_multibyte_bodies() {
    // Regression: a raw byte slice at 600 panicked on Chinese text (the
    // run2 backend lost its tokio worker to this). Exercise drain directly.
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let long_body = "配置很长的中文邮件内容".repeat(120);
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            long_body,
        )
        .await
        .unwrap();
    let mut steer = engine.steer_mailbox_snapshot(root).await;
    let notice = steer.drain(&engine).await.unwrap().expect("notice");
    assert!(
        notice.contains("[mail from main]"),
        "notice carries the body"
    );
}
