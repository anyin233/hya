#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{AgentName, Envelope, FinishCause, FinishReason, ModelRef, Role, SessionId};
use hya_provider::{DevProvider, Provider, ProviderRouter};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::hooks::*;
use crate::{
    AgentCatalog, AgentSpec, CreateSession, EventBus, ResidentSupervisor, RuntimeRegistry,
    RuntimeSource, RuntimeSourceId, SessionEngine, SubagentGovernor, SubagentLimits,
};

type Pause = Option<(Arc<Notify>, Arc<Notify>)>;

#[derive(Default)]
struct Hooks {
    seen: Mutex<Vec<(SessionId, &'static str)>>,
    user_texts: Mutex<Vec<String>>,
    prepend: bool,
    pause_start: Mutex<Pause>,
    pause_spawn: Mutex<Pause>,
    pause_request: Arc<Mutex<Pause>>,
    requests: Arc<Mutex<Vec<hya_provider::CompletionRequest>>>,
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<hya_provider::CompletionRequest>>>,
    pause: Arc<Mutex<Pause>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "hya"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<hya_provider::Capabilities> {
        DevProvider::new().capabilities(model)
    }

    async fn stream(
        &self,
        request: hya_provider::CompletionRequest,
        session: SessionId,
        message: hya_proto::MessageId,
    ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
        self.requests.lock().unwrap().push(request.clone());
        let pause = self.pause.lock().unwrap().take();
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
        DevProvider::new().stream(request, session, message).await
    }
}

#[async_trait]
impl HookDispatcher for Hooks {
    fn dispatch_event(&self, envelope: &Envelope) {
        if let Some(session) = envelope.event.session() {
            self.seen.lock().unwrap().push((session, "event"));
        }
    }
    async fn session_start(&self, input: SessionLifecycleInput) {
        self.seen.lock().unwrap().push((input.session, "start"));
        let pause = self.pause_start.lock().unwrap().clone();
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
    }
    async fn agent_spawn(&self, input: AgentSpawnInput) {
        self.seen.lock().unwrap().push((input.child, "spawn"));
        let pause = self.pause_spawn.lock().unwrap().clone();
        if let Some((entered, release)) = pause {
            entered.notify_one();
            release.notified().await;
        }
    }
    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        self.seen.lock().unwrap().push((input.session, "user"));
        self.user_texts.lock().unwrap().push(input.text.clone());
        MessageUserBeforeOutcome::Continue {
            text: if self.prepend {
                format!("HOOK_PREFIX {}", input.text)
            } else {
                format!("{} [rewritten]", input.text)
            },
        }
    }
    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }
    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
    }
    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }
    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }
    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

async fn fixture(
    limits: SubagentLimits,
    hooks: Option<Arc<Hooks>>,
) -> (
    Arc<SessionEngine>,
    Arc<ResidentSupervisor>,
    AgentSpec,
    SessionId,
) {
    let tools = ToolRegistry::builtins();
    let plugin = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "registration-hooks",
        vec![hya_bundle::SourceFile::new("bundle.yaml", "kind: Bundle\nidentity: { id: test/registration-hooks, version: 1.0.0, publisher: test }\n")],
    )).unwrap();
    let catalog = AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_verified_catalogs(&[&plugin]).unwrap(),
    ))
    .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(tools, Arc::new(catalog)));
    let requests = hooks
        .as_ref()
        .map(|hooks| hooks.requests.clone())
        .unwrap_or_default();
    let pause = hooks
        .as_ref()
        .map(|hooks| hooks.pause_request.clone())
        .unwrap_or_default();
    if let Some(hooks) = hooks {
        runtime
            .refresh(|candidate| {
                candidate.upsert_sources(vec![
                    RuntimeSource::new(
                        RuntimeSourceId::plugin("registration-test"),
                        [1; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(hooks.clone()),
                    RuntimeSource::new(
                        RuntimeSourceId::bundle("test/registration-hooks"),
                        [2; 32],
                        Arc::new(()),
                        Vec::new(),
                    )
                    .with_hooks(hooks),
                ])
            })
            .unwrap();
    }
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { requests, pause }))),
            runtime,
            permission,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(limits)),
    );
    let root = create(&engine, None).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: String::new(),
        workdir: "/tmp".into(),
        reasoning: None,
    };
    (engine, supervisor, agent, root)
}

async fn create(engine: &SessionEngine, parent: Option<SessionId>) -> SessionId {
    engine
        .create(CreateSession {
            parent,
            agent: AgentName::new(if parent.is_some() {
                "hya-task"
            } else {
                "hya-main"
            }),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn spawn(
    supervisor: &ResidentSupervisor,
    engine: &SessionEngine,
    agent: &AgentSpec,
    parent: SessionId,
    claim: Option<&hya_proto::ActorClaim>,
) -> Result<(SessionId, String), crate::CoreError> {
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    supervisor
        .spawn_resident(
            parent,
            agent.clone(),
            (binding, Arc::from([]), resources, None),
            "do X".into(),
            claim,
            None,
        )
        .await
}

#[tokio::test]
async fn existing_registration_capacity_failure_does_not_register_main() {
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_run_budget: 0,
            ..SubagentLimits::default()
        },
        None,
    )
    .await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let child = create(&engine, Some(root)).await;
    let before = engine.replay(root).await.unwrap();
    let result = supervisor
        .register_existing_resident(root, child, "worker".into(), agent, None)
        .await;
    assert!(
        matches!(
            result,
            Err(crate::CoreError::Store(
                hya_store::StoreError::CapacityExceeded { .. }
            ))
        ),
        "{result:?}"
    );
    assert_eq!(
        engine.replay(root).await.unwrap(),
        before,
        "failed registration must roll back lazy main too"
    );
    drop(root_turn);
    supervisor
        .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(2))
        .await;
}

#[tokio::test]
async fn stale_parent_registration_has_no_child_hooks_or_memory() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let claim = engine
        .store()
        .try_claim_new(root, root, OwnerRunId::new())
        .await
        .unwrap();
    engine
        .store()
        .recover_claim(root, OwnerRunId::new())
        .await
        .unwrap();
    hooks.seen.lock().unwrap().clear();
    let result = spawn(&supervisor, &engine, &agent, root, Some(&claim)).await;
    assert!(matches!(
        result,
        Err(crate::CoreError::Store(
            hya_store::StoreError::StaleActorClaim { .. }
        ))
    ));
    assert!(
        engine
            .store()
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .all(|session| session.session == root)
    );
    assert_eq!(engine.store().active_actor_ids().await.unwrap(), vec![root]);
    assert!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .session
            .members
            .is_empty()
    );
    assert!(
        hooks
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|(session, _)| *session == root),
        "rolled-back child reached a plugin"
    );
    assert!(
        engine
            .session_bundle_hooks
            .read()
            .unwrap()
            .keys()
            .all(|session| *session == root)
    );
    assert!(
        engine
            .session_channel_policies
            .read()
            .unwrap()
            .keys()
            .all(|session| *session == root)
    );
}

#[tokio::test]
async fn failed_registration_has_no_child_hooks_or_memory() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_prompt BEFORE INSERT ON event_log WHEN instr(NEW.payload, 'do X') > 0 BEGIN SELECT RAISE(ABORT, 'registration rejected'); END").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    hooks.seen.lock().unwrap().clear();
    let error = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("registration rejected"));
    assert!(engine.store().active_actor_ids().await.unwrap().is_empty());
    assert!(
        engine
            .store()
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .all(|session| session.session == root)
    );
    assert!(
        hooks
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|(session, _)| *session == root)
    );
    assert!(
        engine
            .session_bundle_hooks
            .read()
            .unwrap()
            .keys()
            .all(|session| *session == root)
    );
    assert!(
        engine
            .session_channel_policies
            .read()
            .unwrap()
            .keys()
            .all(|session| *session == root)
    );
}

#[tokio::test]
async fn registered_session_start_precedes_initial_hook_and_events() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let (child, _) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    review_wait_idle(&engine, &supervisor, root, child).await;
    let seen = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, _)| *session == child)
        .map(|(_, kind)| *kind)
        .collect::<Vec<_>>();
    assert_eq!(
        seen.iter().take_while(|kind| **kind == "start").count(),
        2,
        "both global and scoped session.start must precede all child hooks/events: {seen:?}"
    );
    assert_eq!(seen.iter().filter(|kind| **kind == "start").count(), 2);
    assert!(seen.contains(&"user"));
    assert!(engine.read_projection(child).await.unwrap().session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("[rewritten]")))));
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn dropping_spawn_during_session_start_keeps_the_resident_driver() {
    dropping_spawn_during_hook(true).await;
}

#[tokio::test]
async fn dropping_spawn_during_agent_spawn_keeps_the_resident_driver() {
    dropping_spawn_during_hook(false).await;
}

async fn dropping_spawn_during_hook(at_start: bool) {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let pause = if at_start {
        &hooks.pause_start
    } else {
        &hooks.pause_spawn
    };
    *pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let task = tokio::spawn({
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        async move { spawn(&supervisor, &engine, &agent, root, None).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let child = *engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .first()
        .expect("commit must precede plugin RPC");
    assert!(
        supervisor.team_cancel(root).is_some(),
        "slot/task must exist before plugin RPC"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    *pause.lock().unwrap() = None;
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .any(|message| {
                    message.role == Role::Assistant && message.finish == Some(FinishReason::Stop)
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn revived_resident_ignores_stale_session_start() {
    revived_resident_ignores_stale_initialization(true).await;
}

#[tokio::test]
async fn revived_resident_ignores_stale_agent_spawn() {
    revived_resident_ignores_stale_initialization(false).await;
}

async fn revived_resident_ignores_stale_initialization(at_start: bool) {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let _lead = engine.try_begin_turn(root).unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let pause = if at_start {
        &hooks.pause_start
    } else {
        &hooks.pause_spawn
    };
    *pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let task = tokio::spawn({
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        async move { spawn(&supervisor, &engine, &agent, root, None).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    *pause.lock().unwrap() = None;
    let projection = engine.read_projection(root).await.unwrap();
    let (handle, entry) = projection
        .team
        .roster
        .iter()
        .find(|(_, e)| e.session != root)
        .unwrap();
    let child = entry.session;
    supervisor
        .archive_member(root, handle, "superseded")
        .await
        .unwrap();

    // The revived episode must retain its current hook chain even if an old
    // session.start finishes against the previous runtime snapshot.
    let fresh_hooks = Arc::new(Hooks::default());
    engine
        .runtime
        .refresh(|candidate| {
            candidate.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::plugin("registration-test"),
                    [3; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(fresh_hooks.clone()),
                RuntimeSource::new(
                    RuntimeSourceId::bundle("test/registration-hooks"),
                    [4; 32],
                    Arc::new(()),
                    Vec::new(),
                )
                .with_hooks(fresh_hooks),
            ])
        })
        .unwrap();
    let running = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    *hooks.pause_request.lock().unwrap() = Some((running.clone(), finish.clone()));
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::Handle(handle.into()),
            hya_proto::MailKind::Message,
            "episode two".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), running.notified())
        .await
        .unwrap();
    let captured = engine.captured_session_hooks(child).unwrap();
    assert_eq!(supervisor.member_busy(root, child), Some(true));
    let mut events = engine.bus().subscribe();
    release.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();

    // Episode two stays in the provider until released. A second driver would
    // take the shared slot through Busy -> Idle while that turn is still live.
    let stale_effect = tokio::time::timeout(Duration::from_millis(150), async {
        loop {
            let envelope = events.recv().await.unwrap();
            match envelope.event {
                hya_proto::Event::SessionCreated { session, .. } if session == child => {
                    break "late registration publication";
                }
                hya_proto::Event::AgentActivityChanged {
                    handle: ref actor,
                    status: hya_proto::RosterStatus::Idle,
                    ..
                } if actor == handle => break "premature idle",
                _ => {}
            }
        }
    })
    .await;
    assert!(
        stale_effect.is_err(),
        "stale episode affected its replacement: {stale_effect:?}"
    );
    assert_eq!(supervisor.member_busy(root, child), Some(true));
    assert!(
        Arc::ptr_eq(&captured, &engine.captured_session_hooks(child).unwrap()),
        "stale initialization replaced the revived hook chain"
    );
    finish.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(child).await.unwrap();
            if supervisor.member_busy(root, child) == Some(false)
                && projection
                    .session
                    .messages
                    .last()
                    .is_some_and(|message| message.finish == Some(FinishReason::Stop))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(hooks.requests.lock().unwrap().len(), 1);
    let activity = engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            hya_proto::Event::AgentActivityChanged {
                handle: actor,
                status,
                ..
            } if actor == *handle => Some(status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        &activity[activity.len() - 2..],
        &[hya_proto::RosterStatus::Busy, hya_proto::RosterStatus::Idle]
    );
    supervisor.end_member(root, handle).await.unwrap();
    assert_eq!(
        supervisor.member_busy(root, child),
        None,
        "revived slot must remain stoppable"
    );
}

#[tokio::test]
async fn concurrent_siblings_share_one_group_and_all_receive_announce() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let children =
        futures::future::join_all((0..8).map(|_| spawn(&supervisor, &engine, &agent, root, None)))
            .await;
    let children = children.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let groups = projection
        .team
        .channels
        .values()
        .filter(|channel| {
            channel.kind == hya_proto::ChannelKind::Group && channel.unit.as_deref() == Some("main")
        })
        .count();
    assert_eq!(
        groups, 1,
        "concurrent first siblings split the unit broadcast channel"
    );
    engine
        .mail_announce(root, "one announcement".into())
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    for (_, handle) in children {
        assert!(
            projection
                .team
                .inboxes
                .get(&handle)
                .unwrap()
                .iter()
                .any(|index| projection.team.mail[*index].body == "one announcement"),
            "{handle} missed unit announcement"
        );
    }
    supervisor.team_cancel(root).unwrap().cancel();
}

async fn armed_child(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    agent: &AgentSpec,
    root: SessionId,
) -> (SessionId, crate::engine::TurnLease) {
    let child = create(engine, Some(root)).await;
    let lease = engine.try_begin_turn(child).unwrap();
    supervisor
        .register_existing_resident(
            root,
            child,
            "worker".into(),
            agent.clone(),
            Some("do X".into()),
        )
        .await
        .unwrap();
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .all(|message| message.role != Role::Assistant)
    );
    (child, lease)
}

#[tokio::test]
async fn first_directive_and_queued_mail_are_hooked_as_one_wake() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let (child, child_turn) = armed_child(&engine, &supervisor, &agent, root).await;
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::Handle("main/worker".into()),
            hya_proto::MailKind::Message,
            "do Y too".into(),
        )
        .await
        .unwrap();
    assert!(
        hooks.user_texts.lock().unwrap().is_empty(),
        "registration must not hook the directive"
    );
    drop(child_turn);
    review_wait_idle(&engine, &supervisor, root, child).await;
    let texts = hooks.user_texts.lock().unwrap().clone();
    let wake = "[mail from main] do X\n\n[mail from main] do Y too";
    assert_eq!(
        texts.iter().filter(|text| text.as_str() == wake).count(),
        1,
        "{texts:?}"
    );
    assert!(texts.iter().all(|text| text.contains(wake)), "{texts:?}");
    drop(root_turn);
    supervisor
        .drain(FinishCause::Shutdown, crate::DRAIN_DEADLINE)
        .await;
}

async fn assert_pending_directive(engine: &SessionEngine, child: SessionId) {
    let projection = engine.read_projection(child).await.unwrap();
    assert!(
        projection.session.messages.is_empty(),
        "unstarted mail must not admit a transcript prompt"
    );
    let handoff = projection.session.handoff.unwrap();
    assert!(
        handoff.doc.contains("1. Goal - [mail from main] do X"),
        "{}",
        handoff.doc
    );
    assert!(
        handoff
            .doc
            .contains("5. Pending tasks - [mail from main] do X"),
        "{}",
        handoff.doc
    );
}

#[tokio::test]
async fn archive_before_first_wake_preserves_directive_in_handoff() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    let cancelled = lease.bind_cancel(&CancellationToken::new());
    let (result, ()) = tokio::join!(
        supervisor.archive_member(root, "worker", "superseded"),
        async {
            cancelled.cancelled().await;
            drop(lease);
        }
    );
    result.unwrap();
    assert_pending_directive(&engine, child).await;
    let ended_messages = engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::Handle("main/worker".into()),
            hya_proto::MailKind::Message,
            "do Y instead".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .any(|message| {
                    message.role == Role::Assistant && message.finish == Some(FinishReason::Stop)
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let requests = hooks.requests.lock().unwrap();
    let messages = &requests.last().unwrap().messages;
    assert!(messages.iter().rev().find_map(|message| match message {
        hya_proto::Message::User { parts, .. } => Some(parts.iter().any(|part| matches!(part, hya_proto::Part::Text { text, .. } if text.contains("do Y instead")))),
        _ => None,
    }).unwrap());
    assert!(
        messages
            .iter()
            .all(|message| !ended_messages.contains(&message.id()))
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message,
                hya_proto::Message::System { content, .. } if content.contains("PRIOR HANDOFF")
            ))
            .count(),
        1
    );
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn stop_before_first_wake_preserves_pending_directive() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    supervisor.end_member(root, "main/worker").await.unwrap();
    drop(lease);
    assert_pending_directive(&engine, child).await;
}

struct FailingSidecar;

#[async_trait]
impl crate::BoundSidecarFactory for FailingSidecar {
    async fn start(
        &self,
        _: crate::SidecarStart,
    ) -> Result<Box<dyn crate::SidecarHandle>, crate::CoreError> {
        Err(crate::CoreError::Invalid("sidecar start failure".into()))
    }
}

#[tokio::test]
async fn sidecar_failure_before_first_wake_preserves_pending_directive() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, _) = supervisor
        .spawn_resident(
            root,
            agent,
            (
                binding,
                Arc::from([]),
                resources,
                Some(Arc::new(FailingSidecar)),
            ),
            "do X".into(),
            None,
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_pending_directive(&engine, child).await;
}

#[tokio::test]
async fn team_turn_budget_kill_before_first_wake_preserves_pending_directive() {
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_team_turn_budget: 1,
            ..SubagentLimits::default()
        },
        None,
    )
    .await;
    let _lead = engine.try_begin_turn(root).unwrap();
    let (first, _) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.turn_active(first)
            || engine
                .read_projection(first)
                .await
                .unwrap()
                .session
                .messages
                .last()
                .is_none_or(|m| m.role != hya_proto::Role::Assistant || m.finish.is_none())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (child, handle) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_pending_directive(&engine, child).await;
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::parse(&handle),
            hya_proto::MailKind::Message,
            "new task".into(),
        )
        .await
        .unwrap();
    assert!(
        engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .roster
            .contains_key(&handle)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(child).await.unwrap();
            if projection.session.messages.last().is_some_and(|message| {
                message.role == Role::Assistant && message.finish == Some(FinishReason::Stop)
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn team_message_budget_kill_before_first_turn_closes_initial_task() {
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_team_message_budget: 1,
            ..SubagentLimits::default()
        },
        None,
    )
    .await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    for i in 0..2 {
        engine
            .mail_announce(root, format!("budget {i}"))
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(lease);
    assert_pending_directive(&engine, child).await;
}

#[tokio::test]
async fn mail_to_a_killed_team_waits_for_cleanup_and_restores_main() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            // Revival now contributes a real MailSent before the child replies.
            per_team_message_budget: 2,
            ..SubagentLimits::default()
        },
        Some(hooks.clone()),
    )
    .await;
    let mut main = agent.clone();
    main.name = AgentName::new("hya-main");
    let binding = engine.bind_runtime(&main.workdir).unwrap();
    let resources = binding.agent_resource_policy(main.name.as_str()).unwrap();
    supervisor
        .ensure_main(root, main, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *hooks.pause_start.lock().unwrap() = Some((entered.clone(), release.clone()));
    let spawning = tokio::spawn({
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        async move { spawn(&supervisor, &engine, &agent, root, None).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("child start hook");
    let projection = engine.read_projection(root).await.unwrap();
    let (handle, entry) = projection
        .team
        .roster
        .iter()
        .find(|(path, _)| path.as_str() != "main")
        .unwrap();
    let handle = handle.clone();
    let child = entry.session;
    let mut bus = engine.bus().subscribe();
    for i in 0..3 {
        engine
            .mail_announce(root, format!("budget {i}"))
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while !engine
            .read_projection(root)
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
    let mut revive = tokio::spawn({
        let engine = engine.clone();
        let handle = handle.clone();
        async move {
            engine
                .mail_send(
                    root,
                    hya_proto::MailEndpoint::parse(&handle),
                    hya_proto::MailKind::Message,
                    "fresh task".into(),
                )
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut revive)
            .await
            .is_err(),
        "revive must wait for the killed initializer"
    );
    *hooks.pause_start.lock().unwrap() = None;
    release.notify_waiters();
    let spawned = tokio::time::timeout(Duration::from_secs(5), spawning)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        spawned.unwrap(),
        (child, handle.clone()),
        "initialization cancellation must retain the committed identity"
    );
    tokio::time::timeout(Duration::from_secs(5), revive)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(child).await.unwrap();
            if projection
                .session
                .messages
                .last()
                .is_some_and(|message| message.finish == Some(FinishReason::Stop))
            {
                break;
            }
            bus.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!supervisor.team_cancel(root).unwrap().is_cancelled());
    engine
        .mail_send(
            child,
            hya_proto::MailEndpoint::parse("main"),
            hya_proto::MailKind::Message,
            "wake main".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(root).await.unwrap();
            if projection.session.messages.iter().any(|message| {
                message.role == Role::User && message.parts.iter().any(|part| {
                    matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("wake main"))
                })
            })
            {
                break;
            }
            bus.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn wait_after_a_revived_member_is_killed_before_its_first_turn_uses_this_episode() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_team_message_budget: 2,
            ..SubagentLimits::default()
        },
        Some(hooks.clone()),
    )
    .await;
    let mut main = agent.clone();
    main.name = AgentName::new("hya-main");
    let binding = engine.bind_runtime(&main.workdir).unwrap();
    let resources = binding.agent_resource_policy(main.name.as_str()).unwrap();
    supervisor
        .ensure_main(root, main, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
    let _lead = engine.try_begin_turn(root).unwrap();
    let (child, handle) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    let mut bus = engine.bus().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .roster
            .get(&handle)
            .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
            || engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .last()
                .is_none_or(|message| {
                    message.finish != Some(FinishReason::Stop) || message.role != Role::Assistant
                })
        {
            bus.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
    submit_and_wait(
        &engine,
        &supervisor,
        root,
        &handle,
        hya_proto::ReportOutcome::Done,
        "OLD REPORT".into(),
    )
    .await
    .unwrap();
    // Hold a real turn lease so the revived episode cannot begin its first wake.
    let child_turn = engine.try_begin_turn(child).unwrap();
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::parse(&handle),
            hya_proto::MailKind::Message,
            "next task".into(),
        )
        .await
        .unwrap();
    engine
        .mail_announce(root, "kill before the next turn".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !engine
            .read_projection(root)
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
    let (lifecycle, rx) = hya_tool::LifecyclePlane::new();
    tokio::spawn(crate::run_lifecycle_service(
        engine.clone(),
        supervisor.clone(),
        rx,
        tokio_util::sync::CancellationToken::new(),
    ));
    let outcome = lifecycle
        .for_session(root)
        .wait(
            hya_tool::WaitSpec::parse(
                &serde_json::json!({"targets": [handle], "timeout_secs": 0}),
                false,
            )
            .unwrap(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let member = outcome
        .already_finished
        .iter()
        .chain(&outcome.finished)
        .find(|member| member.session == child)
        .unwrap();
    assert_eq!(member.outcome.as_deref(), Some("failed"), "{outcome:?}");
    assert_eq!(
        member.report.as_deref(),
        Some("per-team message budget exceeded")
    );
    drop(child_turn);
}

#[tokio::test]
async fn spawn_after_an_empty_team_is_killed_resets_its_budgets() {
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_team_message_budget: 1,
            ..SubagentLimits::default()
        },
        None,
    )
    .await;
    let mut main = agent.clone();
    main.name = AgentName::new("hya-main");
    let binding = engine.bind_runtime(&main.workdir).unwrap();
    let resources = binding.agent_resource_policy(main.name.as_str()).unwrap();
    supervisor
        .ensure_main(root, main, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
    let _lead = engine.try_begin_turn(root).unwrap();
    let (first_child, _) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor.member_busy(root, first_child) != Some(false) {
            // The last durable event can precede the in-memory idle transition.
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let killed = supervisor.team_cancel(root).unwrap();
    engine.force_archive_team(root).await.unwrap();
    assert!(supervisor.team_cancel(root).is_none());
    engine
        .mail_announce(root, "trip the empty team's budget".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), killed.cancelled())
        .await
        .unwrap();
    let (child, handle) = tokio::time::timeout(
        Duration::from_secs(2),
        spawn(&supervisor, &engine, &agent, root, None),
    )
    .await
    .expect("an already-empty killed team must not wait for another slot removal")
    .unwrap();
    assert!(!supervisor.team_cancel(root).unwrap().is_cancelled());
    assert_eq!(
        engine.read_projection(root).await.unwrap().team.roster[&handle].session,
        child
    );
    supervisor.end_member(root, &handle).await.unwrap();
    supervisor.team_cancel(root).unwrap().cancel();
}

async fn submit_and_wait(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    root: SessionId,
    handle: &str,
    outcome: hya_proto::ReportOutcome,
    report: String,
) -> Result<(), crate::CoreError> {
    let mut events = engine.bus().subscribe();
    supervisor
        .submit_report(root, handle, outcome, report)
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(root).await?;
            if projection.team.archived.contains_key(handle) {
                return Ok(());
            }
            events
                .recv()
                .await
                .map_err(|error| crate::CoreError::Invalid(error.to_string()))?;
        }
    })
    .await
    .map_err(|_| crate::CoreError::Invalid("report did not archive".into()))?
}

async fn review_wait_idle(
    engine: &SessionEngine,
    supervisor: &ResidentSupervisor,
    root: SessionId,
    child: SessionId,
) {
    let mut events = engine.bus().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(child).await.unwrap();
            if supervisor.member_busy(root, child) == Some(false)
                && projection.session.messages.last().is_some_and(|m| {
                    m.role == Role::Assistant && m.finish == Some(FinishReason::Stop)
                })
            {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn m46_review_s4_revive_boundary_bypasses_user_hooks_and_keeps_latest_context() {
    let hooks = Arc::new(Hooks {
        prepend: true,
        ..Hooks::default()
    });
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let _root_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    review_wait_idle(&engine, &supervisor, root, child).await;
    let mut prior_ids = Vec::new();
    for directive in ["second episode", "third episode"] {
        prior_ids.extend(
            engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .map(|m| m.id),
        );
        supervisor
            .archive_member(root, &handle, "next episode")
            .await
            .unwrap();
        engine
            .mail_send(
                root,
                hya_proto::MailEndpoint::Handle(handle.clone()),
                hya_proto::MailKind::Message,
                directive.into(),
            )
            .await
            .unwrap();
        review_wait_idle(&engine, &supervisor, root, child).await;
    }
    let texts = hooks.user_texts.lock().unwrap().clone();
    let request = hooks.requests.lock().unwrap().last().unwrap().clone();
    let projection = engine.read_projection(child).await.unwrap();
    drop(_root_turn);
    supervisor
        .drain(FinishCause::Shutdown, crate::DRAIN_DEADLINE)
        .await;
    assert!(
        texts.iter().all(|text| !text.contains("PRIOR HANDOFF")),
        "user hook saw system boundary: {texts:?}"
    );
    let boundaries = projection
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .collect::<Vec<_>>();
    let boundary_ids = boundaries
        .iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
    assert!(!engine.replay(child).await.unwrap().iter().any(|envelope| matches!(envelope.event, hya_proto::Event::TextReplace { message, .. } if boundary_ids.contains(&message))), "initialization rewrote a system boundary");
    assert_eq!(boundaries.len(), 2);
    for boundary in boundaries {
        assert!(boundary.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.starts_with(hya_provider::COMPACT_CONTEXT_MARKER))));
    }
    assert!(
        request
            .messages
            .iter()
            .all(|message| !prior_ids.contains(&message.id())),
        "prior episode messages leaked: {:?}",
        request.messages
    );
    assert_eq!(request.messages.iter().filter(|m| matches!(m, hya_proto::Message::System { content, .. } if content.starts_with(hya_provider::COMPACT_CONTEXT_MARKER))).count(), 1);
}

#[tokio::test]
async fn m46_review_s5_agent_spawn_fires_for_creation_only() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let _root_turn = engine.try_begin_turn(root).unwrap();
    let (child, handle) = spawn(&supervisor, &engine, &agent, root, None)
        .await
        .unwrap();
    review_wait_idle(&engine, &supervisor, root, child).await;
    let spawned = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, kind)| *session == child && *kind == "spawn")
        .count();
    assert!(spawned > 0);
    supervisor
        .archive_member(root, &handle, "revive later")
        .await
        .unwrap();
    engine
        .mail_send(
            root,
            hya_proto::MailEndpoint::Handle(handle),
            hya_proto::MailKind::Message,
            "next episode".into(),
        )
        .await
        .unwrap();
    review_wait_idle(&engine, &supervisor, root, child).await;
    let after = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(session, kind)| *session == child && *kind == "spawn")
        .count();
    drop(_root_turn);
    supervisor
        .drain(FinishCause::Shutdown, crate::DRAIN_DEADLINE)
        .await;
    assert_eq!(after, spawned, "revival repeated creation hooks");
}

#[tokio::test]
async fn registered_directive_waits_before_bus_delivery() {
    registered_directive_before_bus(false, false).await;
}

#[tokio::test]
async fn registered_directive_can_be_user_stopped_before_bus_delivery() {
    registered_directive_before_bus(true, false).await;
}

#[tokio::test]
async fn registered_directive_blocks_quiescence_before_bus_delivery() {
    registered_directive_before_bus(false, true).await;
}

async fn registered_directive_before_bus(cancel: bool, release_lead: bool) {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let mut lead = Some(engine.try_begin_turn(root).unwrap());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    *hooks.pause_start.lock().unwrap() = Some((entered.clone(), release.clone()));
    let spawning = tokio::spawn({
        let engine = engine.clone();
        let supervisor = supervisor.clone();
        async move { spawn(&supervisor, &engine, &agent, root, None).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let child = projection
        .team
        .roster
        .values()
        .find(|entry| entry.session != root)
        .unwrap()
        .session;
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    if release_lead {
        drop(lead.take());
        crate::TurnBoundaryObserver::turn_released(supervisor.as_ref(), root);
        assert_eq!(
            supervisor.member_busy(root, root),
            Some(false),
            "pending member must hold back quiescence"
        );
    }
    let spec = hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), false).unwrap();
    let outcome = if cancel {
        assert!(
            engine.cancel_user_turn(child),
            "installed directive must be cancellable before its bus event"
        );
        assert!(engine.stopped_by_user(child));
        None
    } else {
        Some(
            crate::member_wait::wait_for_members(&engine, &supervisor, root, spec)
                .await
                .unwrap(),
        )
    };
    *hooks.pause_start.lock().unwrap() = None;
    release.notify_one();
    spawning.await.unwrap().unwrap();
    if let Some(outcome) = outcome {
        assert_eq!(
            outcome.woke_by,
            hya_tool::WaitWake::Timeout,
            "pending first work must not be reported stalled"
        );
    } else {
        assert!(
            engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .is_empty(),
            "late bus delivery must not run a user-stopped directive"
        );
        engine.resume_after_user_stop(child);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(root).await.unwrap();
            let entry = projection
                .team
                .roster
                .values()
                .find(|entry| entry.session == child)
                .unwrap();
            if entry.resident_cursor == 1
                && supervisor.member_busy(root, child) == Some(false)
                && engine.mail_mark(root).is_some_and(|mark| mark >= 1)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("directive must finish once after initialization or explicit resume");
    assert_eq!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert_eq!(engine.replay(root).await.unwrap().iter().filter(|envelope| matches!(envelope.event, hya_proto::Event::ResidentWorkStarted { actor_session, .. } if actor_session == child)).count(), 1, "late bus delivery must not admit the directive twice");
    drop(lead);
    supervisor
        .drain(FinishCause::Shutdown, std::time::Duration::from_secs(2))
        .await;
}
