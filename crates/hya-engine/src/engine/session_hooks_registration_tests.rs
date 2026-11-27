#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use hya_proto::{AgentName, Envelope, FinishCause, FinishReason, ModelRef, Role, SessionId};
use hya_provider::{DevProvider, Provider, ProviderRouter};
use hya_store::{OwnerRunId, SessionStore};
use hya_tool::{PermissionPlane, PermissionRules};
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
    requests: Arc<Mutex<Vec<(SessionId, hya_provider::CompletionRequest)>>>,
}

struct RecordingProvider {
    requests: Arc<Mutex<Vec<(SessionId, hya_provider::CompletionRequest)>>>,
    pause: Arc<Mutex<Pause>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "hya"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<hya_provider::Capabilities> {
        DevProvider::new().capabilities(m)
    }
    async fn stream(
        &self,
        req: hya_provider::CompletionRequest,
        s: SessionId,
        m: hya_proto::MessageId,
    ) -> Result<hya_provider::EventStream, hya_provider::ProviderError> {
        self.requests.lock().unwrap().push((s, req.clone()));
        let pause = self.pause.lock().unwrap().take();
        if let Some((e, r)) = pause {
            e.notify_one();
            r.notified().await;
        }
        DevProvider::new().stream(req, s, m).await
    }
}

#[async_trait]
impl HookDispatcher for Hooks {
    fn dispatch_event(&self, e: &Envelope) {
        if let Some(s) = e.event.session() {
            self.seen.lock().unwrap().push((s, "event"));
        }
    }
    async fn session_start(&self, i: SessionLifecycleInput) {
        self.seen.lock().unwrap().push((i.session, "start"));
        let pause = self.pause_start.lock().unwrap().clone();
        if let Some((e, r)) = pause {
            e.notify_one();
            r.notified().await;
        }
    }
    async fn agent_spawn(&self, i: AgentSpawnInput) {
        self.seen.lock().unwrap().push((i.child, "spawn"));
        let pause = self.pause_spawn.lock().unwrap().clone();
        if let Some((e, r)) = pause {
            e.notify_one();
            r.notified().await;
        }
    }
    async fn message_user_before(&self, i: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        self.seen.lock().unwrap().push((i.session, "user"));
        self.user_texts.lock().unwrap().push(i.text.clone());
        MessageUserBeforeOutcome::Continue {
            text: if self.prepend {
                format!("HOOK_PREFIX {}", i.text)
            } else {
                format!("{} [rewritten]", i.text)
            },
        }
    }
    async fn command_execute_before(
        &self,
        i: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        NoopHookHost.command_execute_before(i).await
    }
    async fn text_complete(&self, i: TextCompleteInput) -> TextCompleteOutcome {
        NoopHookHost.text_complete(i).await
    }
    async fn chat_params(&self, i: ChatParamsInput) -> ChatParamsOutcome {
        NoopHookHost.chat_params(i).await
    }
    async fn tool_execute_before(&self, i: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        NoopHookHost.tool_execute_before(i).await
    }
    async fn tool_execute_after(&self, i: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        NoopHookHost.tool_execute_after(i).await
    }
}

fn upsert_hooks(runtime: &RuntimeRegistry, hooks: Arc<Hooks>, ids: [u8; 2]) {
    runtime
        .refresh(|c| {
            c.upsert_sources(vec![
                RuntimeSource::new(
                    RuntimeSourceId::plugin("registration-test"),
                    [ids[0]; 32],
                    Arc::new(()),
                    vec![],
                )
                .with_hooks(hooks.clone()),
                RuntimeSource::new(
                    RuntimeSourceId::bundle("test/registration-hooks"),
                    [ids[1]; 32],
                    Arc::new(()),
                    vec![],
                )
                .with_hooks(hooks),
            ])
        })
        .unwrap();
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
    let plugin = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "registration-hooks",
        vec![hya_bundle::SourceFile::new("bundle.yaml", "schema_version: 2\nkind: Bundle\nidentity: { id: test/registration-hooks, version: 1.0.0, publisher: test }\n")],
    )).unwrap();
    let catalog = AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_verified_catalogs(&[&plugin]).unwrap(),
    ))
    .unwrap();
    let runtime = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        Arc::new(catalog),
    ));
    let requests = hooks
        .as_ref()
        .map(|h| h.requests.clone())
        .unwrap_or_default();
    let pause = hooks
        .as_ref()
        .map(|h| h.pause_request.clone())
        .unwrap_or_default();
    if let Some(h) = hooks {
        upsert_hooks(&runtime, h, [1, 2]);
    }
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(RecordingProvider { requests, pause }))),
            runtime,
            perm,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(limits)),
    );
    let root = create(&engine, None).await;
    let sup = ResidentSupervisor::start(engine.clone());
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: String::new(),
        workdir: "/tmp".into(),
        reasoning: None,
    };
    (engine, sup, agent, root)
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
    sup: &ResidentSupervisor,
    eng: &SessionEngine,
    agent: &AgentSpec,
    parent: SessionId,
    claim: Option<&hya_proto::ActorClaim>,
) -> Result<(SessionId, String), crate::CoreError> {
    let binding = eng.bind_runtime(&agent.workdir).unwrap();
    let res = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    sup.spawn_resident(
        parent,
        agent.clone(),
        (binding, Arc::from([]), res, None),
        "do X".into(),
        claim,
        None,
    )
    .await
}

async fn ensure_main(
    sup: &ResidentSupervisor,
    eng: &SessionEngine,
    agent: &AgentSpec,
    root: SessionId,
) {
    let mut main = agent.clone();
    main.name = AgentName::new("hya-main");
    let b = eng.bind_runtime(&main.workdir).unwrap();
    let res = b.agent_resource_policy("hya-main").unwrap();
    sup.ensure_main(root, main, (b, Arc::from([]), res), None, None)
        .await
        .unwrap();
}

async fn mail(eng: &SessionEngine, from: SessionId, to: &str, body: &str) {
    eng.mail_send(
        from,
        hya_proto::MailEndpoint::parse(to),
        hya_proto::MailKind::Message,
        body.into(),
    )
    .await
    .unwrap();
}

async fn armed_child(
    eng: &SessionEngine,
    sup: &ResidentSupervisor,
    agent: &AgentSpec,
    root: SessionId,
) -> (SessionId, crate::engine::TurnLease) {
    let child = create(eng, Some(root)).await;
    let lease = eng.try_begin_turn(child).unwrap();
    sup.register_existing_resident(
        root,
        child,
        "worker".into(),
        agent.clone(),
        Some("do X".into()),
    )
    .await
    .unwrap();
    assert!(
        eng.read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .iter()
            .all(|m| m.role != Role::Assistant)
    );
    (child, lease)
}

async fn wait_idle_stop(
    eng: &SessionEngine,
    sup: &ResidentSupervisor,
    root: SessionId,
    child: SessionId,
) {
    let mut events = eng.bus().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let p = eng.read_projection(child).await.unwrap();
            if sup.member_busy(root, child) == Some(false)
                && p.session.messages.last().is_some_and(|m| {
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

async fn wait_inactive(eng: &SessionEngine, child: SessionId) {
    use crate::store_bridge::SessionStoreExt as _;
    tokio::time::timeout(Duration::from_secs(5), async {
        while eng
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
}

async fn wait_archived(eng: &SessionEngine, root: SessionId, handle: &str) {
    let mut bus = eng.bus().subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !hya_tool::extension::team_projection(&eng.read_projection(root).await.unwrap())
            .archived
            .contains_key(handle)
        {
            bus.recv().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn assert_pending_directive(eng: &SessionEngine, child: SessionId) {
    let p = eng.read_projection(child).await.unwrap();
    assert!(p.session.messages.is_empty());
    let doc = &hya_tool::extension::handoff_projection(&p).unwrap().doc;
    assert!(
        doc.contains("1. Goal - [mail from main] do X")
            && doc.contains("5. Pending tasks - [mail from main] do X")
    );
}

#[tokio::test]
async fn failed_registrations_roll_back_main_claims_hooks_and_memory() {
    let (ce, cs, agent, cr) = fixture(
        SubagentLimits {
            per_run_budget: 0,
            ..Default::default()
        },
        None,
    )
    .await;
    let rt = ce.try_begin_turn(cr).unwrap();
    let child = create(&ce, Some(cr)).await;
    let before = ce.replay(cr).await.unwrap();
    assert!(matches!(
        cs.register_existing_resident(cr, child, "worker".into(), agent, None)
            .await,
        Err(crate::CoreError::CapacityExceeded { .. })
    ));
    assert_eq!(ce.replay(cr).await.unwrap(), before);
    drop(rt);
    cs.drain(FinishCause::Shutdown, Duration::from_secs(2))
        .await;

    for trigger_abort in [false, true] {
        let hooks = Arc::new(Hooks::default());
        let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
        let claim = if trigger_abort {
            let mut tx = eng.store().begin_transaction().await.unwrap();
            sqlx::query("CREATE TRIGGER reject_prompt BEFORE INSERT ON event_log WHEN instr(NEW.payload, 'do X') > 0 BEGIN SELECT RAISE(ABORT, 'registration rejected'); END").execute(&mut *tx).await.unwrap();
            tx.commit().await.unwrap();
            None
        } else {
            use crate::store_bridge::SessionStoreExt as _;
            let c = eng
                .store()
                .try_claim_new(root, root, OwnerRunId::new())
                .await
                .unwrap();
            eng.store()
                .claim_runtime_owner(eng.runtime_owner())
                .unwrap();
            eng.store()
                .recover_claim(root, eng.runtime_owner())
                .await
                .unwrap();
            Some(c)
        };
        hooks.seen.lock().unwrap().clear();
        let err = spawn(&sup, &eng, &agent, root, claim.as_ref())
            .await
            .unwrap_err();
        use crate::store_bridge::SessionStoreExt as _;
        if trigger_abort {
            assert!(
                err.to_string().contains("registration rejected")
                    && eng.store().active_actor_ids().await.unwrap().is_empty()
            );
        } else {
            assert!(matches!(
                err,
                crate::CoreError::Store(hya_store::StoreError::StaleBundleClaim { .. })
            ));
            assert_eq!(eng.store().active_actor_ids().await.unwrap(), vec![root]);
        }
        assert!(
            eng.store()
                .list_sessions()
                .await
                .unwrap()
                .iter()
                .all(|s| s.session == root)
        );
        assert!(hooks.seen.lock().unwrap().iter().all(|(s, _)| *s == root));
        assert!(
            eng.session_bundle_hooks
                .read()
                .unwrap()
                .keys()
                .all(|s| *s == root)
        );
        assert!(
            eng.session_channel_policies
                .read()
                .unwrap()
                .keys()
                .all(|s| *s == root)
        );
    }
}

#[tokio::test]
async fn registered_session_start_precedes_hooks_and_batches_initial_mail_wake() {
    let hooks = Arc::new(Hooks::default());
    let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let (child, _) = spawn(&sup, &eng, &agent, root, None).await.unwrap();
    wait_idle_stop(&eng, &sup, root, child).await;
    let seen: Vec<_> = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, _)| *s == child)
        .map(|(_, k)| *k)
        .collect();
    assert_eq!(
        (
            seen.iter().take_while(|k| **k == "start").count(),
            seen.iter().filter(|k| **k == "start").count()
        ),
        (2, 2)
    );
    assert!(seen.contains(&"user"));
    assert!(eng.read_projection(child).await.unwrap().session.messages.iter().any(|m| m.parts.iter().any(|p| matches!(p, hya_proto::PartProjection::Text { text, .. } if text.contains("[rewritten]")))));
    sup.team_cancel(root).unwrap().cancel();

    let h2 = Arc::new(Hooks::default());
    let (e2, s2, a2, r2) = fixture(SubagentLimits::default(), Some(h2.clone())).await;
    let rt = e2.try_begin_turn(r2).unwrap();
    let (c2, ct) = armed_child(&e2, &s2, &a2, r2).await;
    mail(&e2, r2, "main/worker", "do Y too").await;
    assert!(h2.user_texts.lock().unwrap().is_empty());
    drop(ct);
    wait_idle_stop(&e2, &s2, r2, c2).await;
    let texts = h2.user_texts.lock().unwrap().clone();
    let wake = "[mail from main] do X\n\n[mail from main] do Y too";
    assert_eq!(texts.iter().filter(|t| t.as_str() == wake).count(), 1);
    assert!(texts.iter().all(|t| t.contains(wake)));
    drop(rt);
    s2.drain(FinishCause::Shutdown, crate::DRAIN_DEADLINE).await;
}

#[tokio::test]
async fn dropping_spawn_during_init_hooks_keeps_resident_driver() {
    for at_start in [true, false] {
        let hooks = Arc::new(Hooks::default());
        let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let pause = if at_start {
            &hooks.pause_start
        } else {
            &hooks.pause_spawn
        };
        *pause.lock().unwrap() = Some((entered.clone(), release.clone()));
        let task = tokio::spawn({
            let (eng, sup) = (eng.clone(), sup.clone());
            async move { spawn(&sup, &eng, &agent, root, None).await }
        });
        entered.notified().await;
        use crate::store_bridge::SessionStoreExt as _;
        let child = eng.store().active_actor_ids().await.unwrap()[0];
        assert!(sup.team_cancel(root).is_some());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        *pause.lock().unwrap() = None;
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !eng
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant && m.finish == Some(FinishReason::Stop))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        sup.team_cancel(root).unwrap().cancel();
    }
}

#[tokio::test]
async fn revived_resident_ignores_stale_init_hooks() {
    for at_start in [true, false] {
        let hooks = Arc::new(Hooks::default());
        let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
        let _lead = eng.try_begin_turn(root).unwrap();
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        let pause = if at_start {
            &hooks.pause_start
        } else {
            &hooks.pause_spawn
        };
        *pause.lock().unwrap() = Some((entered.clone(), release.clone()));
        let task = tokio::spawn({
            let (eng, sup) = (eng.clone(), sup.clone());
            async move { spawn(&sup, &eng, &agent, root, None).await }
        });
        entered.notified().await;
        *pause.lock().unwrap() = None;
        let proj = eng.read_projection(root).await.unwrap();
        let team = hya_tool::extension::team_projection(&proj);
        let (handle, entry) = team
            .roster
            .iter()
            .find(|(_, e)| e.session != root)
            .unwrap();
        let handle = handle.clone();
        let child = entry.session;
        sup.archive_member(root, &handle, "superseded")
            .await
            .unwrap();

        upsert_hooks(&eng.runtime, Arc::new(Hooks::default()), [3, 4]);
        let (running, finish) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *hooks.pause_request.lock().unwrap() = Some((running.clone(), finish.clone()));
        mail(&eng, root, &handle, "episode two").await;
        running.notified().await;
        let captured = eng.captured_session_hooks(child).unwrap();
        assert_eq!(sup.member_busy(root, child), Some(true));
        let mut events = eng.bus().subscribe();
        release.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();

        let stale = tokio::time::timeout(Duration::from_millis(30), async {
            loop {
                let event = events.recv().await.unwrap().event;
                if matches!(
                    event,
                    hya_proto::Event::SessionCreated { session, .. } if session == child
                ) {
                    break "late pub";
                }
                if let Some(hya_tool::extension::ExtensionEvent::AgentActivityChanged {
                    handle: ref a,
                    status: hya_proto::RosterStatus::Idle,
                    ..
                }) = hya_tool::extension::ExtensionEvent::from_event(&event)
                    && a == &handle
                {
                    break "premature idle";
                }
            }
        })
        .await;
        assert!(stale.is_err(), "{stale:?}");
        assert_eq!(sup.member_busy(root, child), Some(true));
        assert!(Arc::ptr_eq(
            &captured,
            &eng.captured_session_hooks(child).unwrap()
        ));
        finish.notify_one();
        wait_idle_stop(&eng, &sup, root, child).await;
        assert_eq!(hooks.requests.lock().unwrap().len(), 1);
        let activity: Vec<_> = eng
            .replay(root)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| match hya_tool::extension::ExtensionEvent::from_event(&e.event) {
                Some(hya_tool::extension::ExtensionEvent::AgentActivityChanged {
                    handle: a,
                    status,
                    ..
                }) if a == handle => Some(status),
                _ => None,
            })
            .collect();
        assert_eq!(
            &activity[activity.len() - 2..],
            &[hya_proto::RosterStatus::Busy, hya_proto::RosterStatus::Idle]
        );
        sup.end_member(root, &handle).await.unwrap();
        assert_eq!(sup.member_busy(root, child), None);
    }
}

#[tokio::test]
async fn concurrent_siblings_share_one_group_and_all_receive_announce() {
    let (eng, sup, agent, root) = fixture(SubagentLimits::default(), None).await;
    let children = futures::future::join_all((0..8).map(|_| spawn(&sup, &eng, &agent, root, None)))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let proj = eng.read_projection(root).await.unwrap();
    let team = hya_tool::extension::team_projection(&proj);
    assert_eq!(team.channels.values().filter(|c| c.kind == hya_proto::ChannelKind::Group && c.unit.as_deref() == Some("main")).count(), 1);
    eng.mail_announce(root, "one announcement".into())
        .await
        .unwrap();
    let proj = eng.read_projection(root).await.unwrap();
    let team = hya_tool::extension::team_projection(&proj);
    for (_, h) in children {
        assert!(
            team.inboxes[&h]
                .iter()
                .any(|i| team.mail[*i].body == "one announcement")
        );
    }
    sup.team_cancel(root).unwrap().cancel();
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
async fn archive_stop_and_sidecar_failure_before_first_wake_preserve_directive() {
    let hooks = Arc::new(Hooks::default());
    let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let (child, lease) = armed_child(&eng, &sup, &agent, root).await;
    let cancelled = lease.bind_cancel(&CancellationToken::new());
    let (res, ()) = tokio::join!(sup.archive_member(root, "worker", "superseded"), async {
        cancelled.cancelled().await;
        drop(lease);
    });
    res.unwrap();
    assert_pending_directive(&eng, child).await;
    mail(&eng, root, "main/worker", "do Y instead").await;
    wait_idle_stop(&eng, &sup, root, child).await;
    {
        // The root's quiescence synthesis may land after the child's request.
        let reqs = hooks.requests.lock().unwrap();
        let msgs = &reqs.iter().rfind(|(s, _)| *s == child).unwrap().1.messages;
        assert!(msgs.iter().rev().find_map(|m| match m {
            hya_proto::Message::User { parts, .. } => Some(parts.iter().any(|p| matches!(p, hya_proto::Part::Text { text, .. } if text.contains("do Y instead")))),
            _ => None,
        }).unwrap());
        assert_eq!(msgs.iter().filter(|m| matches!(m, hya_proto::Message::System { content, .. } if content.contains("PRIOR HANDOFF"))).count(), 1);
    }
    sup.team_cancel(root).unwrap().cancel();

    let (e2, s2, a2, r2) = fixture(SubagentLimits::default(), None).await;
    let (c2, l2) = armed_child(&e2, &s2, &a2, r2).await;
    s2.end_member(r2, "main/worker").await.unwrap();
    drop(l2);
    assert_pending_directive(&e2, c2).await;

    let (e3, s3, a3, r3) = fixture(SubagentLimits::default(), None).await;
    let b = e3.bind_runtime(&a3.workdir).unwrap();
    let (c3, _) = s3
        .spawn_resident(
            r3,
            a3,
            (
                b.clone(),
                Arc::from([]),
                b.agent_resource_policy("hya-task").unwrap(),
                Some(Arc::new(FailingSidecar)),
            ),
            "do X".into(),
            None,
            None,
        )
        .await
        .unwrap();
    wait_inactive(&e3, c3).await;
    assert_pending_directive(&e3, c3).await;
}

#[tokio::test]
async fn team_turn_and_message_budget_kills_before_first_wake_preserve_directive() {
    let (eng, sup, agent, root) = fixture(
        SubagentLimits {
            per_team_turn_budget: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    let _lead = eng.try_begin_turn(root).unwrap();
    let (first, _) = spawn(&sup, &eng, &agent, root, None).await.unwrap();
    wait_idle_stop(&eng, &sup, root, first).await;
    let (child, handle) = spawn(&sup, &eng, &agent, root, None).await.unwrap();
    wait_inactive(&eng, child).await;
    assert_pending_directive(&eng, child).await;
    mail(&eng, root, &handle, "new task").await;
    assert!(
        hya_tool::extension::team_projection(&eng.read_projection(root).await.unwrap())
            .roster
            .contains_key(&handle)
    );
    wait_idle_stop(&eng, &sup, root, child).await;
    sup.team_cancel(root).unwrap().cancel();

    let (me, ms, ma, mr) = fixture(
        SubagentLimits {
            per_team_message_budget: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    let (mc, lease) = armed_child(&me, &ms, &ma, mr).await;
    for i in 0..2 {
        me.mail_announce(mr, format!("budget {i}")).await.unwrap();
    }
    wait_inactive(&me, mc).await;
    drop(lease);
    assert_pending_directive(&me, mc).await;
}

#[tokio::test]
async fn mail_to_killed_team_waits_for_cleanup_and_restores_main() {
    let hooks = Arc::new(Hooks::default());
    let (eng, sup, agent, root) = fixture(
        SubagentLimits {
            per_team_message_budget: 2,
            ..Default::default()
        },
        Some(hooks.clone()),
    )
    .await;
    ensure_main(&sup, &eng, &agent, root).await;
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *hooks.pause_start.lock().unwrap() = Some((entered.clone(), release.clone()));
    let spawning = tokio::spawn({
        let (eng, sup) = (eng.clone(), sup.clone());
        async move { spawn(&sup, &eng, &agent, root, None).await }
    });
    entered.notified().await;
    let proj = eng.read_projection(root).await.unwrap();
    let team = hya_tool::extension::team_projection(&proj);
    let (handle, entry) = team
        .roster
        .iter()
        .find(|(p, _)| p.as_str() != "main")
        .unwrap();
    let (handle, child) = (handle.clone(), entry.session);
    for i in 0..3 {
        eng.mail_announce(root, format!("budget {i}"))
            .await
            .unwrap();
    }
    wait_archived(&eng, root, &handle).await;
    let mut revive = tokio::spawn({
        let (eng, h) = (eng.clone(), handle.clone());
        async move { mail(&eng, root, &h, "fresh task").await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut revive)
            .await
            .is_err()
    );
    *hooks.pause_start.lock().unwrap() = None;
    release.notify_waiters();
    assert_eq!(spawning.await.unwrap().unwrap(), (child, handle.clone()));
    revive.await.unwrap();
    wait_idle_stop(&eng, &sup, root, child).await;
    assert!(!sup.team_cancel(root).unwrap().is_cancelled());
    let mut bus = eng.bus().subscribe();
    mail(&eng, child, "main", "wake main").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !eng.read_projection(root).await.unwrap().session.messages.iter().any(|m| {
            m.role == Role::User && m.parts.iter().any(|p| matches!(p, hya_proto::PartProjection::Text { text, .. } if text.contains("wake main")))
        }) {
            bus.recv().await.unwrap();
        }
    }).await.unwrap();
    sup.end_member(root, &handle).await.unwrap();
    sup.team_cancel(root).unwrap().cancel();
}

#[tokio::test]
async fn wait_after_revived_kill_and_spawn_after_empty_team_kill_reset_state() {
    let (eng, sup, agent, root) = fixture(
        SubagentLimits {
            per_team_message_budget: 2,
            ..Default::default()
        },
        Some(Arc::new(Hooks::default())),
    )
    .await;
    ensure_main(&sup, &eng, &agent, root).await;
    let _lead = eng.try_begin_turn(root).unwrap();
    let (child, handle) = spawn(&sup, &eng, &agent, root, None).await.unwrap();
    wait_idle_stop(&eng, &sup, root, child).await;
    sup.submit_report(
        root,
        &handle,
        hya_tool::ReportOutcome::Done,
        "OLD REPORT".into(),
    )
    .await
    .unwrap();
    wait_archived(&eng, root, &handle).await;

    let child_turn = eng.try_begin_turn(child).unwrap();
    mail(&eng, root, &handle, "next task").await;
    eng.mail_announce(root, "kill before next turn".into())
        .await
        .unwrap();
    wait_archived(&eng, root, &handle).await;
    let (lifecycle, rx) = hya_tool::LifecyclePlane::new();
    tokio::spawn(crate::run_lifecycle_service(
        eng.clone(),
        sup.clone(),
        rx,
        CancellationToken::new(),
    ));
    let out = lifecycle
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
    let m = out
        .already_finished
        .iter()
        .chain(&out.finished)
        .find(|m| m.session == child)
        .unwrap();
    assert_eq!(
        (m.outcome.as_deref(), m.report.as_deref()),
        (Some("failed"), Some("per-team message budget exceeded"))
    );
    drop(child_turn);

    // Spawning after an already-empty team is killed resets budgets without blocking.
    let (e2, s2, a2, r2) = fixture(
        SubagentLimits {
            per_team_message_budget: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    ensure_main(&s2, &e2, &a2, r2).await;
    let _l2 = e2.try_begin_turn(r2).unwrap();
    let (fc, _) = spawn(&s2, &e2, &a2, r2, None).await.unwrap();
    wait_idle_stop(&e2, &s2, r2, fc).await;
    let killed = s2.team_cancel(r2).unwrap();
    e2.force_archive_team(r2).await.unwrap();
    assert!(s2.team_cancel(r2).is_none());
    e2.mail_announce(r2, "trip empty team budget".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), killed.cancelled())
        .await
        .unwrap();
    let (c2, h2) = tokio::time::timeout(Duration::from_secs(2), spawn(&s2, &e2, &a2, r2, None))
        .await
        .unwrap()
        .unwrap();
    assert!(!s2.team_cancel(r2).unwrap().is_cancelled());
    assert_eq!(
        hya_tool::extension::team_projection(&e2.read_projection(r2).await.unwrap()).roster[&h2]
            .session,
        c2
    );
    s2.end_member(r2, &h2).await.unwrap();
    s2.team_cancel(r2).unwrap().cancel();
}

#[tokio::test]
async fn revive_boundary_bypasses_user_hooks_keeps_latest_context_and_skips_spawn_hook() {
    let hooks = Arc::new(Hooks {
        prepend: true,
        ..Hooks::default()
    });
    let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
    let _rt = eng.try_begin_turn(root).unwrap();
    let (child, handle) = spawn(&sup, &eng, &agent, root, None).await.unwrap();
    wait_idle_stop(&eng, &sup, root, child).await;
    let spawned_before = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, k)| *s == child && *k == "spawn")
        .count();
    assert!(spawned_before > 0);

    let mut prior_ids = Vec::new();
    for dir in ["second episode", "third episode"] {
        prior_ids.extend(
            eng.read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .map(|m| m.id),
        );
        sup.archive_member(root, &handle, "next episode")
            .await
            .unwrap();
        mail(&eng, root, &handle, dir).await;
        wait_idle_stop(&eng, &sup, root, child).await;
    }
    let spawned_after = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(s, k)| *s == child && *k == "spawn")
        .count();
    let texts = hooks.user_texts.lock().unwrap().clone();
    let req = hooks.requests.lock().unwrap().last().unwrap().1.clone();
    let proj = eng.read_projection(child).await.unwrap();
    drop(_rt);
    sup.drain(FinishCause::Shutdown, crate::DRAIN_DEADLINE)
        .await;

    assert_eq!(spawned_after, spawned_before);
    assert!(texts.iter().all(|t| !t.contains("PRIOR HANDOFF")));
    let boundaries: Vec<_> = proj
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .collect();
    let bids: Vec<_> = boundaries.iter().map(|m| m.id).collect();
    assert!(!eng.replay(child).await.unwrap().iter().any(|e| matches!(e.event, hya_proto::Event::TextReplace { message, .. } if bids.contains(&message))));
    assert_eq!(boundaries.len(), 2);
    for b in boundaries {
        assert!(b.parts.iter().any(|p| matches!(p, hya_proto::PartProjection::Text { text, .. } if text.starts_with(hya_provider::COMPACT_CONTEXT_MARKER))));
    }
    assert!(req.messages.iter().all(|m| !prior_ids.contains(&m.id())));
    assert_eq!(req.messages.iter().filter(|m| matches!(m, hya_proto::Message::System { content, .. } if content.starts_with(hya_provider::COMPACT_CONTEXT_MARKER))).count(), 1);
}

#[tokio::test]
async fn registered_directive_before_bus_delivery_waits_cancels_and_blocks_quiescence() {
    for (cancel, release_lead) in [(false, false), (true, false), (false, true)] {
        let hooks = Arc::new(Hooks::default());
        let (eng, sup, agent, root) = fixture(SubagentLimits::default(), Some(hooks.clone())).await;
        let mut lead = Some(eng.try_begin_turn(root).unwrap());
        let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        *hooks.pause_start.lock().unwrap() = Some((entered.clone(), release.clone()));
        let spawning = tokio::spawn({
            let (eng, sup) = (eng.clone(), sup.clone());
            async move { spawn(&sup, &eng, &agent, root, None).await }
        });
        entered.notified().await;
        let child = hya_tool::extension::team_projection(&eng.read_projection(root).await.unwrap())
            .roster
            .values()
            .find(|e| e.session != root)
            .unwrap()
            .session;
        assert!(
            eng.read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .is_empty()
        );
        if release_lead {
            drop(lead.take());
            crate::TurnBoundaryObserver::turn_released(sup.as_ref(), root);
            assert_eq!(sup.member_busy(root, root), Some(false));
        }
        let spec =
            hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), false).unwrap();
        let outcome = if cancel {
            assert!(eng.cancel_user_turn(child) && eng.stopped_by_user(child));
            None
        } else {
            Some(
                crate::member_wait::wait_for_members(None, &eng, &sup, root, spec, None)
                    .await
                    .unwrap(),
            )
        };
        *hooks.pause_start.lock().unwrap() = None;
        release.notify_one();
        spawning.await.unwrap().unwrap();
        if let Some(o) = outcome {
            assert_eq!(o.woke_by, hya_tool::WaitWake::Timeout);
        } else {
            assert!(
                eng.read_projection(child)
                    .await
                    .unwrap()
                    .session
                    .messages
                    .is_empty()
            );
            eng.resume_after_user_stop(child);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let p = eng.read_projection(root).await.unwrap();
                let team = hya_tool::extension::team_projection(&p);
                let e = team.roster.values().find(|e| e.session == child).unwrap();
                if e.resident_cursor == 1
                    && sup.member_busy(root, child) == Some(false)
                    && eng.mail_mark(root).is_some_and(|m| m >= 1)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            eng.read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count(),
            1
        );
        assert_eq!(eng.replay(root).await.unwrap().iter().filter(|e| matches!(hya_tool::extension::ExtensionEvent::from_event(&e.event), Some(hya_tool::extension::ExtensionEvent::ResidentWorkStarted { actor_session, .. }) if actor_session == child)).count(), 1);
        drop(lead);
        sup.drain(FinishCause::Shutdown, Duration::from_secs(2))
            .await;
    }
}
