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

#[derive(Default)]
struct Hooks {
    seen: Mutex<Vec<(SessionId, &'static str)>>,
    pause_start: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    pause_spawn: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    requests: Arc<Mutex<Vec<hya_provider::CompletionRequest>>>,
}

struct RecordingProvider(Arc<Mutex<Vec<hya_provider::CompletionRequest>>>);

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
        self.0.lock().unwrap().push(request.clone());
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
        MessageUserBeforeOutcome::Continue {
            text: format!("{} [rewritten]", input.text),
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
        vec![hya_bundle::SourceFile::new("bundle.yaml", "kind: Plugin\nidentity: { id: test/registration-hooks, version: 1.0.0, publisher: test }\n")],
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
            Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider(requests)))),
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
async fn stale_parent_registration_has_no_child_hooks_or_memory() {
    let hooks = Arc::new(Hooks::default());
    let (engine, supervisor, agent, root) =
        fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let claim = engine
        .store()
        .try_claim_new(root, OwnerRunId::new())
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
async fn concurrent_siblings_share_one_group_and_all_receive_announce() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    // Root lazy registration is intentionally outside this change's scope.
    engine.resolve_handle(root, root).await.unwrap();
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
                .any(|mail| mail.body == "one announcement"),
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

async fn assert_cancelled_task(engine: &SessionEngine, child: SessionId, cause: FinishCause) {
    let projection = engine.read_projection(child).await.unwrap();
    let last = projection.session.messages.last().unwrap();
    assert_eq!(
        last.role,
        Role::Assistant,
        "ended episode left its initial user directive open"
    );
    assert_eq!(last.finish, Some(FinishReason::Cancelled));
    assert_eq!(last.cause, Some(cause));
    assert!(last.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("cancelled"))), "closure must survive the request mapper's empty-assistant filter");
}

#[tokio::test]
async fn archive_before_first_turn_closes_initial_task_before_revive() {
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
    assert_cancelled_task(&engine, child, FinishCause::Archived).await;
    supervisor
        .revive_child(root, "main", "main/worker", "do Y instead".into())
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
    let x = messages
        .iter()
        .position(|message| matches!(message, hya_proto::Message::User { .. }))
        .unwrap();
    assert!(matches!(
        &messages[x + 1],
        hya_proto::Message::Assistant {
            finish: Some(FinishReason::Cancelled),
            ..
        }
    ));
    supervisor.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn stop_before_first_turn_closes_initial_task() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    supervisor.stop_resident(root, "main/worker").await.unwrap();
    drop(lease);
    assert_cancelled_task(&engine, child, FinishCause::UserCancel).await;
}

#[tokio::test]
async fn initial_cancellation_rolls_back_with_the_terminal_event() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    let before = engine.replay(child).await.unwrap();
    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("CREATE TRIGGER reject_terminal BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'agent_activity_changed' AND json_extract(NEW.payload, '$.status') = 'failed' BEGIN SELECT RAISE(ABORT, 'terminal rejected'); END").execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    let error = supervisor
        .stop_resident(root, "main/worker")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("terminal rejected"));
    assert_eq!(engine.replay(child).await.unwrap(), before);
    assert!(
        engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let mut tx = engine.store().begin_transaction().await.unwrap();
    sqlx::query("DROP TRIGGER reject_terminal")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    supervisor.stop_resident(root, "main/worker").await.unwrap();
    drop(lease);
    assert_cancelled_task(&engine, child, FinishCause::UserCancel).await;
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
async fn sidecar_failure_before_first_turn_closes_initial_task() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let child = create(&engine, Some(root)).await;
    supervisor
        .register_existing_resident_with_sidecar(
            root,
            child,
            "worker".into(),
            agent,
            Some("do X".into()),
            Arc::new(FailingSidecar),
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
    assert_cancelled_task(&engine, child, FinishCause::Other).await;
}

#[tokio::test]
async fn team_turn_budget_kill_before_first_turn_closes_initial_task() {
    let (engine, supervisor, agent, root) = fixture(
        SubagentLimits {
            per_team_turn_budget: 1,
            ..SubagentLimits::default()
        },
        None,
    )
    .await;
    assert_eq!(
        engine.governor().unwrap().charge_team_turn(root),
        crate::TeamBudget::Ok
    );
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
    assert_cancelled_task(&engine, child, FinishCause::Other).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor.team_cancel(root).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    supervisor
        .revive_child(root, "main", &handle, "new task".into())
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
    assert_cancelled_task(&engine, child, FinishCause::Other).await;
}

#[tokio::test]
async fn terminal_archive_rechecks_mail_after_report_gate() {
    let (engine, supervisor, agent, root) = fixture(SubagentLimits::default(), None).await;
    let (child, lease) = armed_child(&engine, &supervisor, &agent, root).await;
    supervisor.report_gate(root, "main/worker").await.unwrap();
    engine
        .store()
        .append_harness_mail(root, "main/worker", "late directive".into())
        .await
        .unwrap();
    let claim = engine
        .store()
        .recover_claim(child, OwnerRunId::new())
        .await
        .unwrap()
        .claim;
    let result = crate::resident::archive_reported_agent(
        &engine,
        root,
        "main/worker",
        child,
        Some(&claim),
        hya_proto::ReportOutcome::Done,
        "finished".into(),
        hya_proto::ArchiveReason::Reported,
        false,
    )
    .await;
    assert!(result.is_err(), "late mail must refuse archive");
    engine.store().validate_actor_claim(&claim).await.unwrap();
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .handoff
            .is_none()
    );

    let projection = engine.read_projection(root).await.unwrap();
    assert!(projection.team.roster.contains_key("main/worker"));
    assert!(
        projection.team.inboxes["main/worker"]
            .iter()
            .any(|mail| mail.body == "late directive")
    );
    drop(lease);
    engine
        .store()
        .finalize_resident_stop(&claim, root, "main/worker")
        .await
        .unwrap();
    supervisor.team_cancel(root).unwrap().cancel();
}
