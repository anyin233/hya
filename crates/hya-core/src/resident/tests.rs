#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{CreateSession, EventBus};
use hya_proto::{AgentName, MailKind, ModelRef, SessionKind};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine() -> Arc<SessionEngine> {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        crate::test_support::runtime(ToolRegistry::builtins()),
        permission,
        EventBus::default(),
    ))
}

async fn root(engine: &SessionEngine) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("hya/offline"),
            workdir: ".".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn parked(supervisor: &ResidentSupervisor, root: SessionId) -> (SessionId, String) {
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("hya/offline"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = supervisor.engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (session, handle, _) = supervisor
        .spawn_resident_parked(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            "parked".into(),
            None,
            None,
        )
        .await
        .unwrap();
    (session, handle)
}

#[tokio::test]
async fn mail_revival_reserves_the_team_before_claim_registration() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let mail = tokio::spawn({
        let engine = engine.clone();
        let handle = handle.clone();
        async move {
            engine
                .mail_send(
                    root,
                    MailEndpoint::Handle(handle),
                    MailKind::Message,
                    "resume".into(),
                )
                .await
        }
    });
    entered.notified().await;
    let team = supervisor.teams().get(&root).unwrap().clone();
    drop(lead_turn);
    // Force the last-main-turn retirement at the selected-team/claim-write gap.
    supervisor.retire_team(&team);
    release.notify_one();
    let result = mail.await.unwrap();
    assert!(
        result.is_ok(),
        "revival inherited retirement cancellation: {result:?}"
    );
    assert!(supervisor.team_with_resident(child).is_some());
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn cold_mail_revival_falls_back_from_missing_root_agent() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    supervisor.teardown_root(root).await.unwrap();
    drop(lead_turn);
    engine
        .store()
        .append_event(
            root,
            &Event::AgentSwitched {
                session: root,
                agent: AgentName::new("removed-bundle-agent"),
            },
        )
        .await
        .unwrap();
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "resume".into(),
        )
        .await
        .unwrap();
    assert!(supervisor.team_with_resident(child).is_some());
    assert_eq!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .agent
            .unwrap()
            .as_str(),
        crate::TASK_AGENT_ID
    );
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(5))
        .await;
}

struct StuckParams(Arc<Notify>);

#[async_trait::async_trait]
impl crate::hooks::HookDispatcher for StuckParams {
    fn dispatch_event(&self, _: &hya_proto::Envelope) {}
    async fn command_execute_before(
        &self,
        i: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(
        &self,
        i: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(
        &self,
        i: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        crate::hooks::NoopHookHost.message_user_before(i).await
    }
    async fn tool_execute_before(
        &self,
        i: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(
        &self,
        i: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::NoopHookHost.tool_execute_after(i).await
    }
    async fn chat_params(
        &self,
        _: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn drain_cause_reaches_the_member_straggler_close() {
    let entered = Arc::new(Notify::new());
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            crate::test_support::runtime(ToolRegistry::builtins()),
            permission,
            EventBus::default(),
        )
        .with_hooks(Arc::new(StuckParams(entered.clone()))),
    );
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = parked(&supervisor, root).await;
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "work".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    // The episode's five-second close must win, rather than the generic turn drain.
    supervisor
        .drain(FinishCause::UserCancel, Duration::from_secs(6))
        .await;
    let projection = engine.read_projection(child).await.unwrap();
    let answer = projection
        .session
        .messages
        .iter()
        .rev()
        .find(|message| message.role == hya_proto::Role::Assistant)
        .unwrap();
    assert_eq!(answer.cause, Some(FinishCause::UserCancel));
    assert!(
        engine
            .store()
            .list_pending_resumes()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn concurrent_mail_revives_install_one_episode_without_waiting_for_the_winner() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let first = tokio::spawn({
        let engine = engine.clone();
        let handle = handle.clone();
        async move {
            engine
                .mail_send(
                    root,
                    MailEndpoint::Handle(handle),
                    MailKind::Message,
                    "first caller".into(),
                )
                .await
        }
    });
    entered.notified().await;
    let second = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "second caller".into(),
        )
        .await;
    release.notify_one();
    let first = tokio::time::timeout(Duration::from_secs(2), first).await;
    assert!(second.is_ok(), "{second:?}");
    first
        .expect("concurrent sender must not await the winning episode")
        .unwrap()
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        engine.store().active_actor_ids().await.unwrap(),
        vec![child]
    );
    assert_eq!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|envelope| matches!(envelope.event, Event::AgentRestarted { .. }))
            .count(),
        1
    );
    assert_eq!(projection.team.inboxes[&handle].len(), 2);
    drop(lead_turn);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
}

#[tokio::test]
async fn mail_revival_fast_archive_cannot_be_erased_by_a_late_restart() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let engine = engine().await;
        let root = root(&engine).await;
        let supervisor = ResidentSupervisor::start(engine.clone());
        let lead = engine.try_begin_turn(root).unwrap();
        let (child, handle) = parked(&supervisor, root).await;
        supervisor
            .archive_member(root, &handle, "first ending")
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *supervisor.revive_ready_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
        let reviving = tokio::spawn({
            let engine = engine.clone();
            let handle = handle.clone();
            async move {
                engine
                    .mail_send(
                        root,
                        MailEndpoint::Handle(handle),
                        MailKind::Message,
                        "new episode".into(),
                    )
                    .await
            }
        });
        // Initialization is complete, but the sender has not returned yet.
        entered.notified().await;
        supervisor
            .archive_member(root, &handle, "fast archive")
            .await
            .unwrap();
        release.notify_one();
        reviving.await.unwrap().unwrap();
        let projection = engine.read_projection(root).await.unwrap();
        assert!(projection.team.archived.contains_key(&handle));
        assert!(!projection.team.roster.contains_key(&handle));
        let events = engine.replay(root).await.unwrap();
        let restarted = events
            .iter()
            .rposition(|envelope| matches!(envelope.event, Event::AgentRestarted { .. }))
            .unwrap();
        let archived = events
            .iter()
            .rposition(|envelope| matches!(envelope.event, Event::AgentArchived { .. }))
            .unwrap();
        assert!(restarted < archived);
        assert!(
            !engine
                .store()
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&child)
        );
        drop(lead);
        supervisor
            .drain(FinishCause::Shutdown, Duration::from_secs(2))
            .await;
    })
    .await
    .expect("fast archive must finish before the sender returns");
}

#[tokio::test]
async fn list_channels_ignores_the_senders_directive_copy() {
    outgoing_directive_reader(false).await;
}

#[tokio::test]
async fn steer_ignores_the_senders_directive_copy() {
    outgoing_directive_reader(true).await;
}

async fn outgoing_directive_reader(steer: bool) {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead = engine.try_begin_turn(root).unwrap();
    let (_, handle) = parked(&supervisor, root).await;
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(
                engine
                    .read_projection(root)
                    .await
                    .unwrap()
                    .team
                    .channels
                    .iter()
                    .find(|(_, channel)| {
                        channel.kind == ChannelKind::Dm && channel.members.contains(&handle)
                    })
                    .unwrap()
                    .0
                    .clone(),
            ),
            MailKind::Message,
            "outgoing directive".into(),
        )
        .await
        .unwrap();
    if steer {
        let mut steer = engine
            .steer_mailbox_snapshot_with_policy(
                root,
                Some(
                    ChannelPolicy::from_binding(
                        &engine.bind_runtime(std::path::Path::new(".")).unwrap(),
                    )
                    .unwrap()
                    .snapshot_for("hya-main"),
                ),
            )
            .await;
        assert!(
            steer.drain(&engine).await.unwrap().is_none(),
            "outgoing copy must not steer its author"
        );
    } else {
        let (mailbox, rx) = hya_tool::MailboxPlane::new();
        let stop = CancellationToken::new();
        let service = tokio::spawn(crate::run_mailbox_service(engine.clone(), rx, stop.clone()));
        let rows = mailbox.for_session(root).list_channels().await.unwrap();
        stop.cancel();
        service.await.unwrap();
        assert!(!rows.is_empty());
        assert!(
            rows.iter().all(|row| row.unread == 0),
            "outgoing directive must not show unread: {rows:?}"
        );
    }
    drop(lead);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
}

#[tokio::test]
async fn mandatory_parent_delivery_requires_the_actual_parent_and_own_dm() {
    let engine = engine().await;
    let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
    let policy = ChannelPolicy::from_binding(&binding).unwrap();
    let mut projection = hya_proto::Projection::default();
    for (id, members) in [
        ("own", vec!["main/lead", "main/lead/child"]),
        ("other", vec!["main/lead", "main/lead/other"]),
        (
            "three",
            vec!["main/lead", "main/lead/child", "main/lead/other"],
        ),
    ] {
        projection.team.channels.insert(
            id.into(),
            hya_proto::ChannelProjection {
                kind: ChannelKind::Dm,
                members: members.into_iter().map(str::to_string).collect(),
                ..Default::default()
            },
        );
    }
    for (sender, to, expected) in [
        ("main/lead", MailEndpoint::Channel("own".into()), true),
        (
            "main/lead",
            MailEndpoint::Handle("main/lead/child".into()),
            false,
        ),
        (
            "main/lead/other",
            MailEndpoint::Channel("own".into()),
            false,
        ),
        ("main", MailEndpoint::Channel("own".into()), false),
        ("main/lead", MailEndpoint::Channel("missing".into()), false),
        ("main/lead", MailEndpoint::Channel("other".into()), false),
        ("main/lead", MailEndpoint::Channel("three".into()), false),
        (
            "main/lead",
            MailEndpoint::Handle("main/lead/other".into()),
            false,
        ),
    ] {
        let (kind, role) = recipient_channel_role(&projection, &to, "main/lead/child", sender);
        assert_eq!(
            resident_delivery_allowed(&policy, "hya-scout", kind, role),
            expected,
            "sender {sender}, endpoint {to:?}"
        );
    }
}

#[tokio::test]
async fn parked_workflow_actor_has_no_pending_directive() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lead = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    assert_eq!(supervisor.member_busy(root, child), Some(false));
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    assert!(
        !engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .inboxes
            .contains_key(&handle)
    );
    drop(lead);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
}

struct PausedParams {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl crate::hooks::HookDispatcher for PausedParams {
    fn dispatch_event(&self, _: &hya_proto::Envelope) {}
    async fn command_execute_before(
        &self,
        i: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(
        &self,
        i: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(
        &self,
        i: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        crate::hooks::NoopHookHost.message_user_before(i).await
    }
    async fn tool_execute_before(
        &self,
        i: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(
        &self,
        i: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::NoopHookHost.tool_execute_after(i).await
    }
    async fn chat_params(
        &self,
        input: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        self.entered.notify_one();
        self.release.notified().await;
        crate::hooks::NoopHookHost.chat_params(input).await
    }
}

#[tokio::test]
async fn i20_stage_activation_keeps_one_supervisor_subscription() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let engine = engine().await;
    let engine = Arc::new((*engine).clone().with_hooks(Arc::new(PausedParams {
        entered: entered.clone(),
        release: release.clone(),
    })));
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let subscribers = engine.bus().subscriber_count();
    let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
    let base = AgentSpec {
        name: AgentName::new("hya-main"),
        model: ModelRef::new("hya/offline"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let workflow = hya_workflow::compile(hya_workflow::WorkflowSource::new(
        "stage.hya.md",
        r#"---
kind: Workflow
name: stage
description: Stage completion uses the episode rather than subscribing.
nodes:
  run:
    agent: hya-task
    directive: Answer with hello.
---
flowchart TD
  run
"#,
    ))
    .unwrap();
    let task = tokio::spawn({
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        async move {
            crate::run_workflow(
                engine,
                root,
                &workflow,
                crate::WorkflowRunContext {
                    binding,
                    caller: "hya-main".into(),
                    base_agent: base,
                    inputs: BTreeMap::new(),
                    resident_supervisor: Some(supervisor),
                    routing: None,
                },
                CancellationToken::new(),
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let during = engine.bus().subscriber_count();
    release.notify_one();
    let report = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        report.status,
        crate::WorkflowStatus::Completed,
        "{report:?}"
    );
    assert_eq!(
        during,
        subscribers + 1,
        "only the normal in-turn mail steering subscription may be added"
    );
}

#[tokio::test]
async fn activation_waiter_settles_reports_revives_and_ends_on_the_episode() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let engine = engine().await;
    let engine = Arc::new((*engine).clone().with_hooks(Arc::new(PausedParams {
        entered: entered.clone(),
        release: release.clone(),
    })));
    let root = root(&engine).await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, handle) = parked(&supervisor, root).await;
    let mut first = supervisor
        .activate(
            root,
            child,
            "FIRST_ACTIVATION".into(),
            Arc::from("first guidance"),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        first.done.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    {
        let mut pending = Box::pin(first.wait());
        assert!(futures::poll!(pending.as_mut()).is_pending());
    }
    let refused = supervisor
        .activate(
            root,
            child,
            "must not be mailed".into(),
            Arc::from("bad guidance"),
            None,
            None,
        )
        .await;
    assert!(matches!(refused, Err(CoreError::TurnAlreadyActive { .. })));
    assert_eq!(
        engine.read_projection(root).await.unwrap().team.inboxes[&handle].len(),
        1
    );
    release.notify_one();
    let first = tokio::time::timeout(Duration::from_secs(3), first.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(first, ActivationResult::Settled { final_text } if final_text.contains("FIRST_ACTIVATION"))
    );
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(projection.team.roster[&handle].resident_cursor, 1);
    assert!(projection.team.roster[&handle].resident_work.is_none());
    assert_eq!(
        engine.store().active_actor_ids().await.unwrap(),
        vec![child]
    );

    let mut second = supervisor
        .activate(
            root,
            child,
            "SECOND_ACTIVATION".into(),
            Arc::from("second guidance"),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    supervisor
        .submit_report(
            root,
            &handle,
            ReportOutcome::Done,
            "terminal evidence".into(),
        )
        .await
        .unwrap();
    release.notify_one();
    let second = tokio::time::timeout(Duration::from_secs(3), second.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second,
        ActivationResult::Reported {
            outcome: ReportOutcome::Done,
            text: "terminal evidence".into()
        }
    );
    assert!(engine.store().active_actor_ids().await.unwrap().is_empty());
    assert_eq!(
        engine.read_projection(root).await.unwrap().team.archived[&handle].session,
        child
    );

    let mut third = supervisor
        .activate(
            root,
            child,
            "THIRD_ACTIVATION".into(),
            Arc::from("revived guidance"),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let team = supervisor.team_with_resident(child).unwrap();
    let cancel = team.lock().residents[&child].cancel.clone();
    let ending = tokio::spawn({
        let supervisor = supervisor.clone();
        let handle = handle.clone();
        async move { supervisor.end_member(root, &handle).await }
    });
    tokio::time::timeout(Duration::from_secs(3), cancel.cancelled())
        .await
        .unwrap();
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), ending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let third = tokio::time::timeout(Duration::from_secs(3), third.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        third,
        ActivationResult::Ended {
            reason: ArchiveReason::ArchivedByParent,
            status: MemberRunStatus::Cancelled,
            ..
        }
    ));
    assert!(engine.store().active_actor_ids().await.unwrap().is_empty());
    assert_eq!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(e.event, Event::AgentRestarted { .. }))
            .count(),
        1
    );
    drop(root_turn);
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
}

async fn review_a1_rest_case(with_child: bool) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let engine = engine().await;
    let engine = Arc::new((*engine).clone().with_hooks(Arc::new(PausedParams {
        entered: entered.clone(),
        release: release.clone(),
    })));
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let mut events = engine.bus().subscribe();
    let mut waiter = supervisor
        .activate(
            root,
            child,
            "REVIEW_FIRST".into(),
            Arc::from("guidance"),
            None,
            None,
        )
        .await
        .unwrap();
    entered.notified().await;
    let descendant = if with_child {
        Some(parked(&supervisor, child).await)
    } else {
        engine
            .mail_send(
                root,
                MailEndpoint::Handle(handle.clone()),
                MailKind::Message,
                "REVIEW_FOLLOW_UP".into(),
            )
            .await
            .unwrap();
        None
    };
    release.notify_one();
    if with_child {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if matches!(events.recv().await.unwrap().event, Event::ResidentWorkFinished { handle: ref finished, .. } if finished == &handle) { break; }
            }
        }).await.unwrap();
        let team = supervisor.team_with_resident(child).unwrap();
        let claim = team.lock().residents[&child].claim;
        team.settle_activation(child, claim.as_ref()).await.unwrap();
    } else {
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
    }
    let premature = waiter.done.try_recv().is_ok();
    let raced_next = if premature && !with_child {
        Some(
            supervisor
                .activate(
                    root,
                    child,
                    "REVIEW_NEXT".into(),
                    Arc::from("next"),
                    None,
                    None,
                )
                .await
                .err(),
        )
    } else {
        None
    };
    if let Some((_, descendant)) = descendant {
        supervisor.end_member(root, &descendant).await.unwrap();
    } else {
        release.notify_one();
    }
    if !premature {
        let settled = tokio::time::timeout(Duration::from_secs(3), waiter.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(settled, ActivationResult::Settled { .. }));
        let mut next = supervisor
            .activate(
                root,
                child,
                "REVIEW_NEXT".into(),
                Arc::from("next"),
                None,
                None,
            )
            .await
            .unwrap();
        entered.notified().await;
        release.notify_one();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(3), next.wait())
                .await
                .unwrap()
                .unwrap(),
            ActivationResult::Settled { .. }
        ));
    }
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        !premature,
        "activation settled before true rest; next Stage: {raced_next:?}"
    );
}

#[tokio::test]
async fn review_a1_follow_up_mail_delays_settlement_and_next_stage_is_idle() {
    review_a1_rest_case(false).await;
}

#[tokio::test]
async fn review_a1_live_child_delays_settlement_until_its_ending() {
    review_a1_rest_case(true).await;
}

#[tokio::test]
async fn review_a2_activation_directives_are_in_the_parent_dm_history() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    for index in 0..3 {
        if index == 2 {
            supervisor.end_member(root, &handle).await.unwrap();
        }
        let mut waiter = supervisor
            .activate(
                root,
                child,
                format!("REVIEW_DM_{index}"),
                Arc::from("guidance"),
                None,
                None,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), waiter.wait())
            .await
            .unwrap()
            .unwrap();
    }
    let projection = engine.read_projection(root).await.unwrap();
    let dm = crate::engine::dm_channel_between(&projection, "main", &handle).unwrap();
    let bodies = projection.team.channels[&dm]
        .log
        .iter()
        .filter(|m| m.body.starts_with("REVIEW_DM_"))
        .map(|m| m.body.clone())
        .collect::<Vec<_>>();
    let targets = engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::MailSent { to, body, .. } if body.starts_with("REVIEW_DM_") => Some(to),
            _ => None,
        })
        .collect::<Vec<_>>();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(bodies, ["REVIEW_DM_0", "REVIEW_DM_1", "REVIEW_DM_2"]);
    assert!(
        targets
            .iter()
            .all(|to| to == &MailEndpoint::Channel(dm.clone()))
    );
}

async fn review_a3_pending_case(live_revive: bool) {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor.listener_stop.cancel();
    let listener = supervisor.listener.lock().unwrap().take().unwrap();
    listener.await.unwrap();
    drop(driver);
    let mut waiter = if live_revive {
        let (done, receiver) = oneshot::channel();
        let projection = engine.read_projection(root).await.unwrap();
        let dm = crate::engine::dm_channel_between(&projection, "main", &handle).unwrap();
        supervisor
            .revive_episode_with_activation(
                root,
                "main",
                &handle,
                dm,
                MailKind::Message,
                "REVIEW_PENDING".into(),
                None,
                None,
                Some(WorkflowActivation {
                    guidance: Arc::from("guidance"),
                    route: None,
                    done,
                }),
            )
            .await
            .unwrap();
        ActivationWaiter { done: receiver }
    } else {
        supervisor
            .activate(
                root,
                child,
                "REVIEW_PENDING".into(),
                Arc::from("guidance"),
                None,
                None,
            )
            .await
            .unwrap()
    };
    // This current-thread task has not yielded since the activation lease dropped.
    // The listener is stopped, so only installation can make the work observable.
    let busy = supervisor.member_busy(root, child);
    let cancelled = engine.cancel_user_turn(child);
    let team = supervisor.team_with_resident(child).unwrap();
    let synthesis = {
        let mut state = team.lock();
        state.main_autonomous = true;
        // A new unit of unrelated completed work makes synthesis eligible.
        state.work_seq = state.work_seq.saturating_add(1);
        team.maybe_fire_quiescence(&mut state);
        state.residents[&root].synth_pending
    };
    let held = engine.stopped_by_user(child);
    supervisor.end_member(root, &handle).await.unwrap();
    waiter.wait().await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        (busy, cancelled, held, synthesis),
        (Some(true), true, true, false)
    );
}

#[tokio::test]
async fn review_a3_live_activation_is_busy_cancellable_and_not_quiescent_before_bus_delivery() {
    review_a3_pending_case(false).await;
}

#[tokio::test]
async fn review_a3_live_revive_activation_is_armed_before_bus_delivery() {
    review_a3_pending_case(true).await;
}

#[tokio::test]
async fn review_a4_workflow_cleanup_ends_descendants_before_parent() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (stage, stage_handle) = parked(&supervisor, root).await;
    let (descendant, descendant_handle) = parked(&supervisor, stage).await;
    let (other, other_handle) = parked(&supervisor, root).await;
    supervisor.end_member(root, &stage_handle).await.unwrap();
    let live = engine.store().active_actor_ids().await.unwrap();
    let order = engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::AgentArchived { handle, .. } => Some(handle),
            _ => None,
        })
        .collect::<Vec<_>>();
    supervisor
        .end_member(root, &descendant_handle)
        .await
        .unwrap();
    supervisor.end_member(root, &other_handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        live,
        [other],
        "descendant {descendant} survived its Workflow parent"
    );
    assert_eq!(order, [descendant_handle, stage_handle]);
}

#[tokio::test]
async fn review_a5_blank_activation_has_no_lease_mail_or_turn_effects() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let before = engine.replay(root).await.unwrap();
    let result = supervisor
        .activate(
            root,
            child,
            " \n\t ".into(),
            Arc::from("guidance"),
            None,
            None,
        )
        .await;
    let refused = matches!(&result, Err(CoreError::Invalid(_)));
    let after = engine.replay(root).await.unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(refused, "blank activation was accepted");
    assert_eq!(before, after);
}

#[tokio::test]
async fn review_a5_archive_before_activation_lock_revives_the_member() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.activation_lock_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let activation = tokio::spawn({
        let supervisor = supervisor.clone();
        async move {
            supervisor
                .activate(
                    root,
                    child,
                    "REVIEW_RACE".into(),
                    Arc::from("guidance"),
                    None,
                    None,
                )
                .await
        }
    });
    entered.notified().await;
    supervisor.end_member(root, &handle).await.unwrap();
    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(3), activation)
        .await
        .unwrap()
        .unwrap();
    let succeeded = result.is_ok();
    if let Ok(mut waiter) = result {
        tokio::time::timeout(Duration::from_secs(3), waiter.wait())
            .await
            .unwrap()
            .unwrap();
    }
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        succeeded,
        "activation used a roster snapshot from before claim-write serialization"
    );
}

#[tokio::test]
async fn review_a1_consumed_trailing_wake_settles_without_another_model_turn() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let engine = engine().await;
    let engine = Arc::new((*engine).clone().with_hooks(Arc::new(PausedParams {
        entered: entered.clone(),
        release: release.clone(),
    })));
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let mut waiter = supervisor
        .activate(
            root,
            child,
            "REVIEW_FIRST".into(),
            Arc::from("guidance"),
            None,
            None,
        )
        .await
        .unwrap();
    entered.notified().await;
    let projection = engine.read_projection(root).await.unwrap();
    let dm = crate::engine::dm_channel_between(&projection, "main", &handle).unwrap();
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "already read in this turn".into(),
        )
        .await
        .unwrap();
    let team = supervisor.team_with_resident(child).unwrap();
    team.on_mail("main", &MailEndpoint::Channel(dm.clone()))
        .await;
    engine.read_channel_history(child, &dm, None).await.unwrap();
    release.notify_one();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), waiter.wait())
            .await
            .unwrap()
            .unwrap(),
        ActivationResult::Settled { .. }
    ));
    let starts = engine
        .replay(child)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            matches!(
                e.event,
                Event::MessageStarted {
                    role: hya_proto::Role::Assistant,
                    ..
                }
            )
        })
        .count();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        starts, 1,
        "the retained wake was empty after explicit consumption"
    );
}

#[tokio::test]
async fn recovered_episode_does_not_queue_an_outgoing_dm_copy() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _lead = engine.try_begin_turn(root).unwrap();
    let child = engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-scout"),
            model: ModelRef::new("hya/offline"),
            workdir: ".".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();
    engine
        .store()
        .append_events(
            root,
            vec![
                Event::AgentRegistered {
                    session: root,
                    agent_session: child,
                    handle: "scout".into(),
                    parent: Some("main".into()),
                    agent_type: AgentName::new("hya-scout"),
                    mode: SubagentMode::Resident,
                },
                Event::ChannelCreated {
                    session: root,
                    channel: "DM-12345678".into(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec!["main".into(), "main/scout".into()],
                },
                Event::MailSent {
                    session: root,
                    from: "main/scout".into(),
                    to: MailEndpoint::Channel("DM-12345678".into()),
                    kind: MailKind::Message,
                    body: "outgoing status".into(),
                },
            ],
        )
        .await
        .unwrap();
    engine
        .store()
        .try_claim_new(root, child, OwnerRunId::new())
        .await
        .unwrap();
    let recovered = engine
        .store()
        .recover_claim(child, supervisor.owner_run_id)
        .await
        .unwrap();
    let _child_turn = engine.try_begin_turn(child).unwrap();
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("hya/offline"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    supervisor
        .register_recovered_resident(
            root,
            "main/scout".into(),
            agent,
            (binding, Arc::from([]), resources, None),
            recovered,
            ResidentRecovery::Idle,
        )
        .await
        .unwrap();
    drop(_child_turn);
    let busy = supervisor.member_busy(root, child);
    drop(_lead);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;
    assert_eq!(
        busy,
        Some(false),
        "outgoing history must not owe a recovery wake"
    );
}

struct PausedStart {
    target: Mutex<Option<SessionId>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}
#[async_trait::async_trait]
impl crate::hooks::HookDispatcher for PausedStart {
    fn dispatch_event(&self, _: &hya_proto::Envelope) {}
    async fn command_execute_before(
        &self,
        i: crate::hooks::CommandExecuteBeforeInput,
    ) -> crate::hooks::CommandExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(
        &self,
        i: crate::hooks::TextCompleteInput,
    ) -> crate::hooks::TextCompleteOutcome {
        crate::hooks::NoopHookHost.text_complete(i).await
    }
    async fn message_user_before(
        &self,
        i: crate::hooks::MessageUserBeforeInput,
    ) -> crate::hooks::MessageUserBeforeOutcome {
        crate::hooks::NoopHookHost.message_user_before(i).await
    }
    async fn tool_execute_before(
        &self,
        i: crate::hooks::ToolExecuteBeforeInput,
    ) -> crate::hooks::ToolExecuteBeforeOutcome {
        crate::hooks::NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(
        &self,
        i: crate::hooks::ToolExecuteAfterInput,
    ) -> crate::hooks::ToolExecuteAfterOutcome {
        crate::hooks::NoopHookHost.tool_execute_after(i).await
    }
    async fn chat_params(
        &self,
        input: crate::hooks::ChatParamsInput,
    ) -> crate::hooks::ChatParamsOutcome {
        crate::hooks::NoopHookHost.chat_params(input).await
    }
    async fn session_start(&self, input: SessionLifecycleInput) {
        if *self.target.lock().unwrap() == Some(input.session) {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[tokio::test]
async fn review_r5_registration_frames_precede_siblings_and_activation_mail() {
    let hooks = Arc::new(PausedStart {
        target: Mutex::new(None),
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let engine = Arc::new((*engine().await).clone().with_hooks(hooks.clone()));
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let mut prepared = Vec::new();
    for _ in 0..2 {
        let agent = AgentSpec {
            name: AgentName::new("hya-scout"),
            model: ModelRef::new("hya/offline"),
            workdir: ".".into(),
            system_prompt: String::new(),
            reasoning: None,
        };
        let binding = engine.bind_runtime(&agent.workdir).unwrap();
        let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
        prepared.push(
            supervisor
                .prepare_resident(
                    root,
                    agent,
                    (binding, Arc::from([]), resources, None),
                    ResidentSpawnContext::new("parked".into(), None, None, None),
                )
                .await
                .unwrap()
                .1,
        );
    }
    let first = prepared[0].session;
    *hooks.target.lock().unwrap() = Some(first);
    let mut bus = engine.bus().subscribe();
    let mut initialized = supervisor
        .register_episodes(root, prepared, None)
        .await
        .unwrap();
    hooks.entered.notified().await;
    initialized.pop().unwrap().await.unwrap().unwrap();
    // Parked identity is usable even while its initializer remains blocked.
    let mut activation = supervisor
        .activate(root, first, "work".into(), Arc::from(""), None, None)
        .await
        .unwrap();
    let before_release = engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            matches!(
                e.event,
                Event::MemberSpawned { .. }
                    | Event::AgentRegistered { .. }
                    | Event::ChannelCreated { .. }
                    | Event::MailSent { .. }
            )
        })
        .map(|e| e.seq)
        .collect::<Vec<_>>();
    let mut observed = Vec::new();
    while let Ok(e) = bus.try_recv() {
        if e.event.session() == Some(root) && before_release.contains(&e.seq) {
            observed.push(e.seq);
        }
    }
    hooks.release.notify_one();
    initialized.pop().unwrap().await.unwrap().unwrap();
    activation.wait().await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        observed, before_release,
        "root registration must publish in commit order before activation mail, without waiting for session-start hooks"
    );
}

#[tokio::test]
async fn review_r2_concurrent_roster_orphan_rejects_activation_before_mail() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    engine
        .store()
        .claim_runtime_owner(supervisor.owner_run_id)
        .unwrap();
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor.end_member(root, &handle).await.unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let activation = tokio::spawn({
        let supervisor = supervisor.clone();
        async move {
            supervisor
                .activate(
                    root,
                    child,
                    "must not commit".into(),
                    Arc::from(""),
                    None,
                    None,
                )
                .await
        }
    });
    entered.notified().await;
    let projection = engine.read_projection(root).await.unwrap();
    let dm = crate::engine::dm_channel_between(&projection, "main", &handle).unwrap();
    // A concurrent revival committed but left no episode (e.g. failed terminal
    // persistence). The serialized roster, not the pre-lock snapshot, is live.
    engine
        .store()
        .revive_episode(
            root,
            supervisor.owner_run_id,
            u64::MAX,
            None,
            child,
            "main".into(),
            MailEndpoint::Channel(dm),
            MailKind::Message,
            "prior revival".into(),
            hya_provider::COMPACT_CONTEXT_MARKER,
            None,
        )
        .await
        .unwrap();
    let before = engine.replay(root).await.unwrap();
    release.notify_one();
    assert!(activation.await.unwrap().is_err());
    let after = engine.replay(root).await.unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        before, after,
        "rejected activation committed mail to a roster orphan"
    );
}
