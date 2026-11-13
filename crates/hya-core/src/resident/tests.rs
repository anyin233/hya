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

fn activation_runtime(engine: &SessionEngine) -> (AgentSpec, ResolvedResidentRuntime) {
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("hya/offline"),
        workdir: ".".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    (agent, (binding, Arc::from([]), resources, None))
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
    let first = first
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
    assert_eq!(
        second.unwrap().recipients,
        1,
        "the winner revived one recipient"
    );
    assert_eq!(
        first.recipients, 2,
        "the loser appended to both live DM members"
    );
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
            activation_runtime(&supervisor.engine),
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
            activation_runtime(&supervisor.engine),
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
            activation_runtime(&supervisor.engine),
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
            activation_runtime(&supervisor.engine),
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
            activation_runtime(&supervisor.engine),
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
                    activation_runtime(&supervisor.engine),
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
                activation_runtime(&supervisor.engine),
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
                activation_runtime(&supervisor.engine),
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
                    resolved: activation_runtime(&supervisor.engine),
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
                activation_runtime(&supervisor.engine),
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
        state.root_driver.autonomous.store(true, Ordering::Release);
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
            activation_runtime(&supervisor.engine),
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
                    activation_runtime(&supervisor.engine),
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
            activation_runtime(&supervisor.engine),
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
        .activate(
            root,
            first,
            activation_runtime(&supervisor.engine),
            "work".into(),
            Arc::from(""),
            None,
            None,
        )
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
                    activation_runtime(&supervisor.engine),
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
            dm,
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

#[tokio::test]
async fn review_r1_ending_fences_parent_and_sweeps_a_racing_spawn() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (stage, handle) = parked(&supervisor, root).await;
    let lease = engine.try_begin_turn(stage).unwrap();
    let cancel = lease.bind_cancel(&CancellationToken::new());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.ending_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let ending = tokio::spawn({
        let supervisor = supervisor.clone();
        let handle = handle.clone();
        async move { supervisor.end_member(root, &handle).await }
    });
    entered.notified().await;
    let cancelled_before_sweep = cancel.is_cancelled();
    // A task already in flight can still commit before its parent's claim ends.
    let (agent, runtime, claim) = {
        let team = supervisor.team_with_resident(stage).unwrap();
        let state = team.lock();
        let slot = &state.residents[&stage];
        (
            slot.agent.clone(),
            (
                slot.binding.clone(),
                slot.agents.clone().unwrap(),
                slot.resources.clone().unwrap(),
                slot.sidecar_factory.clone(),
            ),
            slot.claim.unwrap(),
        )
    };
    let (late, late_handle, _) = supervisor
        .spawn_resident_parked(
            stage,
            agent,
            runtime,
            "racing task".into(),
            Some(&claim),
            None,
        )
        .await
        .unwrap();
    drop(lease);
    release.notify_one();
    ending.await.unwrap().unwrap();
    let live = engine.store().active_actor_ids().await.unwrap();
    supervisor.end_member(root, &late_handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        cancelled_before_sweep,
        "parent's active turn was not cancelled before subtree cleanup"
    );
    assert!(
        !live.contains(&late),
        "a child committed during cleanup survived its parent"
    );
}

#[tokio::test]
async fn review_r1_ending_takes_over_a_roster_orphan_descendant() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    engine
        .store()
        .claim_runtime_owner(supervisor.owner_run_id)
        .unwrap();
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let (stage, handle) = parked(&supervisor, root).await;
    let (orphan, orphan_handle) = parked(&supervisor, stage).await;
    supervisor.end_member(root, &orphan_handle).await.unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let dm = crate::engine::dm_channel_between(&projection, &handle, &orphan_handle).unwrap();
    engine
        .store()
        .revive_episode(
            root,
            supervisor.owner_run_id,
            u64::MAX,
            None,
            orphan,
            handle.clone(),
            dm,
            MailKind::Message,
            "orphan".into(),
            hya_provider::COMPACT_CONTEXT_MARKER,
            None,
        )
        .await
        .unwrap();
    assert!(supervisor.team_with_resident(orphan).is_none());
    supervisor.end_member(root, &handle).await.unwrap();
    let live = engine.store().active_actor_ids().await.unwrap();
    let order = engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| {
            if let Event::AgentArchived { handle, .. } = e.event {
                Some(handle)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    supervisor.end_member(root, &orphan_handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        !live.contains(&orphan),
        "roster orphan retained its active claim"
    );
    assert_eq!(
        &order[1..],
        &[orphan_handle, handle],
        "orphan must end before its ancestor"
    );
}

#[tokio::test]
async fn review_r3_revival_keeps_the_preflighted_stage_spec_and_catalog() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let _driver = supervisor.root_driver_activity(root).await.unwrap();
    let agent = AgentSpec {
        name: AgentName::new("resident"),
        model: ModelRef::new("hya/offline"),
        workdir: ".".into(),
        system_prompt: "PINNED_STAGE_PROMPT".into(),
        reasoning: Some(hya_provider::ReasoningEffort::High),
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, handle, _) = supervisor
        .spawn_resident_parked(
            root,
            agent.clone(),
            (binding.clone(), Arc::from([]), resources.clone(), None),
            "parked".into(),
            None,
            None,
        )
        .await
        .unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    let mut activation = supervisor
        .activate(
            root,
            child,
            (
                agent.clone(),
                (binding.clone(), Arc::from([]), resources.clone(), None),
            ),
            "revived stage".into(),
            Arc::from(""),
            None,
            None,
        )
        .await
        .unwrap();
    let actual = {
        let team = supervisor.team_with_resident(child).unwrap();
        let state = team.lock();
        let slot = &state.residents[&child];
        (slot.agent.system_prompt.clone(), slot.agent.reasoning)
    };
    supervisor.end_member(root, &handle).await.unwrap();
    activation.wait().await.unwrap();
    // Remove the custom agent from the live catalog; the next activation is
    // still entitled to the Workflow's previously resolved runtime snapshot.
    let catalog = crate::AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
    ))
    .unwrap();
    engine
        .runtime_registry()
        .publish_catalog(Arc::new(catalog))
        .unwrap();
    let revived = supervisor
        .activate(
            root,
            child,
            (
                agent.clone(),
                (binding.clone(), Arc::from([]), resources.clone(), None),
            ),
            "pinned catalog".into(),
            Arc::from(""),
            None,
            None,
        )
        .await;
    let kept_catalog = revived.is_ok();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert_eq!(
        actual,
        (agent.system_prompt, agent.reasoning),
        "revival rebuilt the stage from the base agent"
    );
    assert!(
        kept_catalog,
        "revival resolved a preflighted Workflow agent against the live catalog"
    );
}

#[tokio::test]
async fn review_r4_root_driver_survives_team_replacement() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let previous = supervisor.teams().get(&root).unwrap().clone();
    for done in previous.kill(Ending::Killed("team budget".into())).await {
        await_episode(done).await.unwrap();
    }
    let replacement = supervisor.team_for(root).await.unwrap();
    {
        let mut state = replacement.lock();
        state.work_seq += 1;
        replacement.maybe_fire_quiescence(&mut state);
        assert!(
            !state.residents[&root].synth_pending,
            "replacement team synthesized during a root driver"
        );
    }
    drop(driver);
    drop(replacement);
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
}

#[tokio::test]
async fn review_r4_task_registration_preserves_headless_root_ownership() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let main = supervisor.resolve_main_activation(root).await.unwrap();
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
        .spawn_residents_typed(
            root,
            vec![TaskResidentSpawn {
                agent,
                runtime: (binding, Arc::from([]), resources, None),
                directive: "work".into(),
                task: TaskSpawnOrigin {
                    subagent_type: "hya-scout".into(),
                    description: "work".into(),
                    tool_call: None,
                },
                guidance: None,
            }],
            main,
            None,
        )
        .await
        .unwrap();
    let team = supervisor.teams().get(&root).unwrap().clone();
    let autonomous = team.lock().root_driver.autonomous.load(Ordering::Acquire);
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        !autonomous,
        "task registration transferred headless continuation ownership to the supervisor"
    );
}

#[tokio::test]
async fn review_r4_replacement_preserves_headless_policy_during_a_lead_turn() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    // run_lead acquires its root-driver guard before starting the lead's turn.
    let lease = engine.try_begin_turn(root).unwrap();
    let previous = supervisor.teams().get(&root).unwrap().clone();
    for done in previous.kill(Ending::Killed("team budget".into())).await {
        await_episode(done).await.unwrap();
    }
    let replacement = supervisor.team_for(root).await.unwrap();
    drop(driver);
    let autonomous = replacement
        .lock()
        .root_driver
        .autonomous
        .load(Ordering::Acquire);
    drop(lease);
    drop(replacement);
    supervisor
        .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
        .await;
    assert!(
        !autonomous,
        "a live headless lead turn was mistaken for autonomous continuation ownership"
    );
}

#[tokio::test]
async fn m412_drop_supervisor_releases_listener_without_shutdown() {
    let engine = engine().await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let weak = Arc::downgrade(&supervisor);
    let mut receiver = engine.bus().subscribe();
    drop(supervisor);
    assert!(
        weak.upgrade().is_none(),
        "listener or engine owns the supervisor"
    );
    let root = root(&engine).await;
    receiver.recv().await.unwrap();
    assert!(engine.resident_supervisor().is_none());
    assert!(engine.session_exists(root).await.unwrap());
}

#[tokio::test]
async fn m412_episode_end_forgets_wait_stalls() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor.record_wait_stall(root, child, None);
    supervisor.end_member(root, &handle).await.unwrap();
    let retained = supervisor.wait_stall_reported(root, child, None);
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(!retained, "ended episode retained its stall notice");
}

#[tokio::test]
async fn m412_delete_caller_forgets_only_its_wait_stalls() {
    let engine = engine().await;
    let caller = root(&engine).await;
    let other = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(other).await.unwrap();
    let (member, _) = parked(&supervisor, other).await;
    supervisor.record_wait_stall(caller, member, None);
    supervisor.record_wait_stall(other, member, None);
    engine.delete_session(caller).await.unwrap();
    let retained = supervisor.wait_stall_reported(caller, member, None);
    assert!(supervisor.wait_stall_reported(other, member, None));
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(!retained, "deleted caller retained its stall notice");
}

#[tokio::test]
async fn m412_slow_team_mail_does_not_block_another_root() {
    let engine = engine().await;
    let first = root(&engine).await;
    let second = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let first_driver = supervisor.root_driver_activity(first).await.unwrap();
    let second_driver = supervisor.root_driver_activity(second).await.unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *supervisor.mail_pause.lock().unwrap() = Some((first, entered.clone(), release.clone()));
    let publish = |root| {
        let engine = engine.clone();
        async move {
            engine
                .emit_for_actor(
                    None,
                    root,
                    Event::MailSent {
                        session: root,
                        from: scope::HARNESS_HANDLE.into(),
                        to: MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
                        kind: MailKind::Message,
                        body: "queue".into(),
                    },
                )
                .await
                .unwrap();
        }
    };
    publish(first).await;
    entered.notified().await;
    // Root two must receive its wake while root one's handler is held.
    let second_entered = Arc::new(Notify::new());
    let second_release = Arc::new(Notify::new());
    *supervisor.mail_pause.lock().unwrap() =
        Some((second, second_entered.clone(), second_release.clone()));
    publish(first).await;
    publish(second).await;
    let routed = tokio::time::timeout(Duration::from_secs(1), second_entered.notified()).await;
    let first_team = supervisor.teams().get(&first).unwrap().clone();
    let before_release = first_team.lock().messages_used;
    let (flushed, flush) = oneshot::channel();
    first_team.enqueue(TeamEvent::Flush(flushed));
    release.notify_one();
    second_release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), flush)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before_release, 0, "a later envelope overtook held mail");
    assert_eq!(first_team.lock().messages_used, 2, "queued mail was lost");
    drop(first_driver);
    drop(second_driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(routed.is_ok(), "slow root blocked another root's mail");
}

#[tokio::test]
async fn m412_drain_joins_listener_and_every_team_task() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, _) = parked(&supervisor, root).await;
    let old = supervisor.team_with_resident(child).unwrap();
    old.kill(Ending::Killed("replace".into())).await;
    let replacement = supervisor.team_for(root).await.unwrap();
    drop(replacement);
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(
        supervisor.listener.lock().unwrap().is_none(),
        "listener was not joined"
    );
    assert!(supervisor.tasks.is_empty(), "team futures survived drain");
    assert_eq!(supervisor.episode_count(), 0);
    assert!(supervisor.teams().is_empty());
}

#[tokio::test]
async fn m412_last_episode_retires_team_and_removes_member_index() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let team = supervisor.team_with_resident(child).unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    assert!(supervisor.team_with_resident(child).is_none());
    drop(driver);
    assert!(team.cancel.is_cancelled());
    assert!(supervisor.teams().get(&root).is_none());
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn m412_registration_releases_committed_handle_reservations() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (agent, runtime) = activation_runtime(&engine);
    let (_, prepared, _) = supervisor
        .prepare_resident(
            root,
            agent,
            runtime,
            ResidentSpawnContext::new("park".into(), None, None, None),
        )
        .await
        .unwrap();
    let leaf = prepared.leaf.clone();
    assert_eq!(engine.pending_member_names(root), 1);
    supervisor
        .register_episodes(root, vec![prepared], None)
        .await
        .unwrap();
    let reserved = engine.pending_member_names(root);
    let durable = engine.read_projection(root).await.unwrap();
    assert!(crate::handle_naming::team_leaves(&durable).contains(&leaf));
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert_eq!(
        reserved, 0,
        "committed identity retained a process reservation"
    );
}

#[tokio::test]
async fn m412_cancelled_preparation_releases_handle_reservation() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (agent, runtime) = activation_runtime(&engine);
    let (_, prepared, _) = supervisor
        .prepare_resident(
            root,
            agent,
            runtime,
            ResidentSpawnContext::new("park".into(), None, None, None),
        )
        .await
        .unwrap();
    assert_eq!(engine.pending_member_names(root), 1);
    drop(prepared);
    let reserved = engine.pending_member_names(root);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert_eq!(
        reserved, 0,
        "abandoned preparation retained its name reservation"
    );
}

#[tokio::test]
async fn m412_late_stall_record_cannot_retain_ended_episode() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let driver = supervisor.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor.record_wait_stall(root, child, None);
    let retained = supervisor.wait_stall_reported(root, child, None);
    drop(driver);
    supervisor
        .drain(FinishCause::Shutdown, Duration::from_secs(5))
        .await;
    assert!(
        !retained,
        "a wait snapshot recorded an already ended episode"
    );
}

#[tokio::test]
async fn m412_drop_supervisor_requests_live_episode_endings_before_cancellation() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (child, _) = parked(&supervisor, root).await;
    let team = supervisor.team_with_resident(child).unwrap();
    let done = team.lock().residents[&child].done.subscribe();
    let weak = Arc::downgrade(&supervisor);
    drop(supervisor);
    assert!(weak.upgrade().is_none());
    assert!(
        team.lock().phase.killed(),
        "Drop left a cancelled team without its ending owner"
    );
    assert!(team.lock().residents[&child].ending.is_some());
    tokio::time::timeout(Duration::from_secs(5), await_episode(done))
        .await
        .unwrap()
        .unwrap();
    team.tasks.close();
    team.tasks.wait().await;
    assert!(team.lock().residents.is_empty());
}

#[tokio::test]
async fn o8_name_allocation_does_not_reserve_after_a_store_failure() {
    let engine = engine().await;
    let root = root(&engine).await;
    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("DROP TABLE event_log")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reservation = engine.mint_member_leaf(root, "scout").await;
    assert_eq!(engine.pending_member_names(root), 0);
    assert!(matches!(reservation, Err(CoreError::Store(_))));
}

#[tokio::test]
async fn o2_interactive_turn_release_retires_the_finished_team() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = parked(&supervisor, root).await;
    let team = supervisor.team_with_resident(child).unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    engine.emit_for_actor(None, root, Event::MailSent {
        session: root,
        from: scope::HARNESS_HANDLE.into(),
        to: MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
        kind: MailKind::Message,
        body: "child ended".into(),
    }).await.unwrap();
    // Seal bus intake so a late terminal envelope cannot retire the team
    // on behalf of the turn boundary under test.
    supervisor.listener_stop.cancel();
    supervisor.listener.lock().unwrap().take().unwrap().await.unwrap();
    let (done, flushed) = oneshot::channel();
    team.enqueue(TeamEvent::Flush(done));
    flushed.await.unwrap();
    crate::member_wait::wait_for_members(
        &engine,
        &supervisor,
        root,
        hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), true).unwrap(),
    )
    .await
    .unwrap();
    assert!(supervisor.teams().contains_key(&root));
    drop(turn);
    let retired = tokio::time::timeout(Duration::from_secs(1), team.cancel.cancelled()).await;
    if retired.is_ok() {
        supervisor.tasks.close();
        supervisor.tasks.wait().await;
        assert!(supervisor.tasks.is_empty());
        assert!(team.lock().residents.is_empty());
    }
    supervisor.drain(FinishCause::Shutdown, Duration::from_secs(5)).await;
    assert!(retired.is_ok(), "root turn release retained its idle team");
    assert!(supervisor.teams().is_empty());
}

#[tokio::test]
async fn o3_turn_release_during_shutdown_cannot_retain_the_engine() {
    let engine = engine().await;
    let root = root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let weak = Arc::downgrade(&engine);
    let lease = engine.try_begin_turn(root).unwrap();
    engine.begin_drain(FinishCause::Shutdown);
    // A current-thread runtime keeps the release task queued until we yield.
    drop(lease);
    assert_eq!(Arc::strong_count(&supervisor), 1, "draining release queued a strong supervisor owner");
    supervisor.drain(FinishCause::Shutdown, Duration::from_secs(5)).await;
    drop(supervisor);
    drop(engine);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn o4_ended_staged_claim_still_recovers_its_roots_unread_mail() {
    let engine = engine().await;
    let root = root(&engine).await;
    let previous = ResidentSupervisor::start(engine.clone());
    let driver = previous.root_driver_activity(root).await.unwrap();
    let (child, handle) = parked(&previous, root).await;
    let claim = previous.team_with_resident(child).unwrap().lock().residents[&child].claim.unwrap();
    previous.end_member(root, &handle).await.unwrap();
    drop(driver);
    previous.listener_stop.cancel();
    previous.listener.lock().unwrap().take().unwrap().await.unwrap();
    previous.tasks.close();
    previous.tasks.wait().await;
    drop(previous);
    let supervisor = ResidentSupervisor::start(engine.clone());
    let (agent, runtime) = activation_runtime(&engine);
    supervisor.defer_recovered_resident(root, handle, agent, runtime,
        hya_store::RecoveredActorClaim { claim, previous_epoch: claim.epoch }, ResidentRecovery::Idle);
    // Persisted before startup; no live bus envelope can recover it for us.
    engine.store().append_event(root, &Event::MailSent {
        session: root, from: scope::HARNESS_HANDLE.into(),
        to: MailEndpoint::Handle("main".into()), kind: MailKind::Message,
        body: "unread root mail".into(),
    }).await.unwrap();
    assert_eq!(supervisor.start_recovered().await.unwrap(), 0);
    supervisor.tasks.close();
    tokio::time::timeout(Duration::from_secs(5), supervisor.tasks.wait()).await.unwrap();
    let replay = engine.replay(root).await.unwrap();
    let recovered = replay.iter().any(|envelope| matches!(&envelope.event,
        Event::TextDelta { delta, .. } if delta.contains("unread root mail")));
    supervisor.drain(FinishCause::Shutdown, Duration::from_secs(5)).await;
    assert!(recovered, "the stale claim retired its root before inbox recovery");
}
