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
            model: ModelRef::new("hya/echo"),
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
        model: ModelRef::new("hya/echo"),
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
            true,
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
