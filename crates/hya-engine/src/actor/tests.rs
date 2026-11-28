#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::inbox::{recipient_channel_role, resident_delivery_allowed};
use super::*;
use crate::store_bridge::{RecoveredActorClaim, SessionStoreExt as _};
use crate::{CreateSession, EventBus};
use hya_proto::{AgentName, ChannelKind, MailKind, ModelRef, SessionKind, SubagentMode};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::extension::{ChannelProjection, ExtensionEvent, TeamProjection, team_projection};
use hya_tool::{PermissionPlane, PermissionRules};

async fn engine() -> Arc<SessionEngine> {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        crate::test_support::runtime(hya_tool_loader::builtins()),
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
            workdir: "/tmp".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap()
}

fn runtime_for(engine: &SessionEngine) -> (AgentSpec, ResolvedResidentRuntime) {
    let agent = AgentSpec {
        name: AgentName::new("hya-scout"),
        model: ModelRef::new("hya/offline"),
        workdir: "/tmp".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    (agent, (binding, Arc::from([]), resources, None))
}

async fn parked(sup: &ResidentSupervisor, parent: SessionId) -> (SessionId, String) {
    let (agent, rt) = runtime_for(&sup.engine);
    let (s, h, _) = sup
        .spawn_resident_parked(parent, agent, rt, "parked".into(), None, None)
        .await
        .unwrap();
    (s, h)
}

async fn mail_h(
    engine: &SessionEngine,
    from: SessionId,
    to: &str,
    body: &str,
) -> Result<hya_tool::MailReceipt, CoreError> {
    engine
        .mail_send(
            from,
            MailEndpoint::Handle(to.into()),
            MailKind::Message,
            body.into(),
        )
        .await
}

async fn act(
    sup: &ResidentSupervisor,
    root: SessionId,
    child: SessionId,
    body: &str,
    guidance: &str,
) -> Result<ActivationWaiter, CoreError> {
    sup.activate(
        root,
        child,
        runtime_for(&sup.engine),
        body.into(),
        Arc::from(guidance),
        None,
        None,
    )
    .await
}

async fn dm_of(engine: &SessionEngine, root: SessionId, a: &str, b: &str) -> String {
    crate::engine::dm_channel_between(&engine.read_projection(root).await.unwrap(), a, b).unwrap()
}

async fn active_ids(engine: &SessionEngine) -> Vec<SessionId> {
    engine.store().active_actor_ids().await.unwrap()
}

async fn archived_handles(engine: &SessionEngine, root: SessionId) -> Vec<String> {
    engine
        .replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match ExtensionEvent::from_event(&e.event) {
            Some(ExtensionEvent::AgentArchived { handle, .. }) => Some(handle),
            _ => None,
        })
        .collect()
}

async fn drain_sup(sup: &ResidentSupervisor) {
    sup.drain(FinishCause::Shutdown, DRAIN_DEADLINE).await;
}

struct PauseHook {
    on_start: bool,
    target: Mutex<Option<SessionId>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl PauseHook {
    fn chat_params() -> (Arc<Self>, Arc<Notify>, Arc<Notify>) {
        let (e, r) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        (
            Arc::new(Self {
                on_start: false,
                target: Mutex::new(None),
                entered: e.clone(),
                release: r.clone(),
            }),
            e,
            r,
        )
    }
    fn session_start() -> (Arc<Self>, Arc<Notify>, Arc<Notify>) {
        let (e, r) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        (
            Arc::new(Self {
                on_start: true,
                target: Mutex::new(None),
                entered: e.clone(),
                release: r.clone(),
            }),
            e,
            r,
        )
    }
}

#[async_trait::async_trait]
impl crate::hooks::NativeHook for PauseHook {
    fn id(&self) -> &str {
        "PauseHook"
    }
    fn hook_points(&self) -> &[hya_runtime_abi::HookPoint] {
        &[
            hya_runtime_abi::HookPoint::ChatParams,
            hya_runtime_abi::HookPoint::SessionStart,
        ]
    }

    async fn chat_params(
        &self,
        i: crate::hooks::NativeChatParamsInput,
    ) -> Result<serde_json::Value, hya_runtime_abi::HostError> {
        if !self.on_start {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(i.params)
    }
    async fn session_start(
        &self,
        i: crate::hooks::NativeSessionLifecycleInput,
    ) -> Result<(), hya_runtime_abi::HostError> {
        if self.on_start && *self.target.lock().unwrap() == Some(i.session) {
            self.entered.notify_one();
            self.release.notified().await;
        }
        Ok(())
    }
}

async fn paused_engine() -> (Arc<SessionEngine>, Arc<Notify>, Arc<Notify>) {
    let (hook, entered, release) = PauseHook::chat_params();
    let eng = Arc::new((*engine().await).clone().with_hooks(hook));
    (eng, entered, release)
}

#[tokio::test]
async fn mail_revival_reserves_team_and_falls_back_from_missing_root_agent() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let lead = eng.try_begin_turn(r).unwrap();
    let (child, handle) = parked(&sup, r).await;
    sup.archive_member(r, &handle, "pause").await.unwrap();

    // 1) Mail revival reserves team across retirement race before claim registration.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let mail = tokio::spawn({
        let (eng, h) = (eng.clone(), handle.clone());
        async move { mail_h(&eng, r, &h, "resume").await }
    });
    entered.notified().await;
    let team = sup.teams().get(&r).unwrap().clone();
    drop(lead);
    sup.retire_team(&team);
    release.notify_one();
    assert!(mail.await.unwrap().is_ok());
    assert!(sup.team_with_resident(child).is_some());

    // 2) Cold revival after teardown falls back to TASK_AGENT_ID when root agent vanished.
    sup.archive_member(r, &handle, "pause").await.unwrap();
    sup.teardown_root(r).await.unwrap();
    eng.store()
        .append_event(
            r,
            &Event::AgentSwitched {
                session: r,
                agent: AgentName::new("removed-bundle-agent"),
            },
        )
        .await
        .unwrap();
    mail_h(&eng, r, &handle, "resume").await.unwrap();
    assert!(sup.team_with_resident(child).is_some());
    assert_eq!(
        eng.read_projection(r)
            .await
            .unwrap()
            .session
            .agent
            .unwrap()
            .as_str(),
        crate::TASK_AGENT_ID
    );
    sup.drain(FinishCause::UserCancel, Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn concurrent_mail_revives_deduplicate_and_preserve_fast_archive() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let lead = eng.try_begin_turn(r).unwrap();
    let (child, handle) = parked(&sup, r).await;
    sup.archive_member(r, &handle, "pause").await.unwrap();

    // 1) Concurrent revivals install exactly one episode without awaiting the winner.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.registration_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let first = tokio::spawn({
        let (eng, h) = (eng.clone(), handle.clone());
        async move { mail_h(&eng, r, &h, "first").await }
    });
    entered.notified().await;
    let second = mail_h(&eng, r, &handle, "second").await.unwrap();
    release.notify_one();
    let first = tokio::time::timeout(Duration::from_secs(2), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!((second.recipients, first.recipients), (1, 2));
    assert_eq!(active_ids(&eng).await, vec![child]);
    assert_eq!(
        eng.replay(r)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::AgentRestarted { .. })
            ))
            .count(),
        1
    );
    assert_eq!(
        team_projection(&eng.read_projection(r).await.unwrap()).inboxes[&handle].len(),
        2
    );

    // 2) Fast archive before revival sender returns cannot be erased by late restart completion.
    sup.archive_member(r, &handle, "first ending")
        .await
        .unwrap();
    let (e2, r2) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.revive_ready_pause.lock().unwrap() = Some((e2.clone(), r2.clone()));
    let rev = tokio::spawn({
        let (eng, h) = (eng.clone(), handle.clone());
        async move { mail_h(&eng, r, &h, "new episode").await }
    });
    e2.notified().await;
    sup.archive_member(r, &handle, "fast archive")
        .await
        .unwrap();
    r2.notify_one();
    tokio::time::timeout(Duration::from_secs(5), rev)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let proj = eng.read_projection(r).await.unwrap();
    let team = team_projection(&proj);
    assert!(team.archived.contains_key(&handle) && !team.roster.contains_key(&handle));
    let evs = eng.replay(r).await.unwrap();
    let restarted = evs
        .iter()
        .rposition(|e| {
            matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::AgentRestarted { .. })
            )
        })
        .unwrap();
    let archived = evs
        .iter()
        .rposition(|e| {
            matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::AgentArchived { .. })
            )
        })
        .unwrap();
    assert!(restarted < archived && !active_ids(&eng).await.contains(&child));
    drop(lead);
    drain_sup(&sup).await;
}

#[tokio::test]
async fn list_channels_and_steer_count_incoming_only_ignoring_outgoing_copy() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let lead = eng.try_begin_turn(r).unwrap();
    let (child, handle) = parked(&sup, r).await;
    let proj = eng.read_projection(r).await.unwrap();
    let team = team_projection(&proj);
    let dm = team.resolve_dm(&handle, "^parent").unwrap();
    let announce = team
        .channels
        .iter()
        .find(|(_, c)| c.kind == ChannelKind::Group && c.members.contains(&handle))
        .unwrap()
        .0
        .clone();

    eng.mail_send(
        r,
        MailEndpoint::Channel(dm.clone()),
        MailKind::Message,
        "outgoing directive".into(),
    )
    .await
    .unwrap();
    let policy = crate::channel_policy::from_binding(
        &eng.bind_runtime(std::path::Path::new("/tmp")).unwrap(),
    )
    .unwrap()
    .snapshot_for("hya-main");
    assert!(
        eng.steer_mailbox_snapshot_with_policy(r, Some(policy))
            .await
            .drain(&eng)
            .await
            .unwrap()
            .is_none()
    );

    let (mailbox, rx) = hya_tool::MailboxPlane::new();
    let stop = CancellationToken::new();
    let svc = tokio::spawn(crate::run_mailbox_service(eng.clone(), rx, stop.clone()));
    let root_rows = mailbox.for_session(r).list_channels().await.unwrap();
    assert!(!root_rows.is_empty() && root_rows.iter().all(|row| row.unread == 0));

    let through =
        team_projection(&eng.read_projection(r).await.unwrap()).inboxes[&handle].len() as u64;
    eng.store()
        .append_event(
            r,
            &ExtensionEvent::MailConsumed {
                session: r,
                handle: handle.clone(),
                through,
            }
            .into_event(),
        )
        .await
        .unwrap();
    for (ch, group_unread) in [(&dm, 0), (&announce, 1)] {
        eng.store()
            .append_event(
                r,
                &ExtensionEvent::MailSent {
                    session: r,
                    from: "main".into(),
                    to: MailEndpoint::Channel(ch.clone()),
                    kind: MailKind::Message,
                    body: "update".into(),
                }
                .into_event(),
            )
            .await
            .unwrap();
        let rows = mailbox.for_session(child).list_channels().await.unwrap();
        assert_eq!(rows.iter().find(|x| x.id == dm).unwrap().unread, 1);
        assert_eq!(
            rows.iter().find(|x| x.id == announce).unwrap().unread,
            group_unread
        );
    }
    stop.cancel();
    svc.await.unwrap();
    drop(lead);
    drain_sup(&sup).await;
}

#[tokio::test]
async fn mandatory_parent_delivery_requires_actual_parent_and_own_dm() {
    let eng = engine().await;
    let policy = crate::channel_policy::from_binding(
        &eng.bind_runtime(std::path::Path::new("/tmp")).unwrap(),
    )
    .unwrap();
    let mut team = TeamProjection::default();
    for (id, members) in [
        ("own", vec!["main/lead", "main/lead/child"]),
        ("other", vec!["main/lead", "main/lead/other"]),
        (
            "three",
            vec!["main/lead", "main/lead/child", "main/lead/other"],
        ),
    ] {
        team.channels.insert(
            id.into(),
            ChannelProjection {
                kind: ChannelKind::Dm,
                members: members.into_iter().map(str::to_string).collect(),
                ..Default::default()
            },
        );
    }
    for (sender, to, want) in [
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
        let (kind, role) = recipient_channel_role(&team, &to, "main/lead/child", sender);
        assert_eq!(
            resident_delivery_allowed(&policy, "hya-scout", kind, role),
            want,
            "{sender} -> {to:?}"
        );
    }
}

#[tokio::test]
async fn stage_activation_keeps_one_supervisor_subscription() {
    let (eng, entered, release) = paused_engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let subs = eng.bus().subscriber_count();
    let binding = eng.bind_runtime(std::path::Path::new("/tmp")).unwrap();
    let base = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        workdir: "/tmp".into(),
        system_prompt: String::new(),
        reasoning: None,
    };
    let agents = eng.agent_roster_for_binding(&binding, "hya-task").unwrap();
    let resources = eng
        .agent_resource_policy_for_binding(&binding, "hya-task")
        .unwrap();
    let resolved = (base, (binding, agents, resources, None));
    let (child, _handle, _member) = sup
        .spawn_resident_parked(
            r,
            resolved.0.clone(),
            resolved.1.clone(),
            "stage".into(),
            None,
            None,
        )
        .await
        .unwrap();
    let task = tokio::spawn({
        let sup = sup.clone();
        async move {
            let mut waiter = sup
                .activate(
                    r,
                    child,
                    resolved,
                    "Answer with hello.".into(),
                    Arc::from(""),
                    None,
                    None,
                )
                .await
                .unwrap();
            waiter.wait().await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    let during = eng.bus().subscriber_count();
    release.notify_one();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        ActivationResult::Settled { .. }
    ));
    drain_sup(&sup).await;
    // Unread-mail steering runs via the `hya/agent-channels` NativeHook without
    // opening a per-turn bus subscription, so only the supervisor subscription exists.
    assert_eq!(during, subs);
}

#[tokio::test]
async fn activation_waiter_parked_settles_reports_revives_ends_and_records_dm_history() {
    let (eng, entered, release) = paused_engine().await;
    let r = root(&eng).await;
    let root_turn = eng.try_begin_turn(r).unwrap();
    let sup = ResidentSupervisor::start(eng.clone());
    let (child, handle) = parked(&sup, r).await;

    assert_eq!(sup.member_busy(r, child), Some(false));
    assert!(
        eng.read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    assert!(
        !team_projection(&eng.read_projection(r).await.unwrap())
            .inboxes
            .contains_key(&handle)
    );

    // 1) Settle + concurrent TurnAlreadyActive refusal.
    let mut first = act(&sup, r, child, "ACT_0", "g0").await.unwrap();
    entered.notified().await;
    assert!(first.done.try_recv().is_err());
    {
        let mut p = Box::pin(first.wait());
        assert!(futures::poll!(p.as_mut()).is_pending());
    }
    assert!(matches!(
        act(&sup, r, child, "bad", "g").await,
        Err(CoreError::TurnAlreadyActive { .. })
    ));
    assert_eq!(
        team_projection(&eng.read_projection(r).await.unwrap()).inboxes[&handle].len(),
        1
    );
    release.notify_one();
    assert!(
        matches!(first.wait().await.unwrap(), ActivationResult::Settled { final_text } if final_text.contains("ACT_0"))
    );
    assert_eq!(
        team_projection(&eng.read_projection(r).await.unwrap()).roster[&handle].resident_cursor,
        1
    );

    // 2) Reported activation archives and clears active claims.
    let mut second = act(&sup, r, child, "ACT_1", "g1").await.unwrap();
    entered.notified().await;
    sup.submit_report(r, &handle, ReportOutcome::Done, "terminal evidence".into())
        .await
        .unwrap();
    release.notify_one();
    assert_eq!(
        second.wait().await.unwrap(),
        ActivationResult::Reported {
            outcome: ReportOutcome::Done,
            text: "terminal evidence".into()
        }
    );
    assert!(active_ids(&eng).await.is_empty());

    // 3) Revived activation cancelled by end_member yields Ended(ArchivedByParent, Cancelled).
    let mut third = act(&sup, r, child, "ACT_2", "g2").await.unwrap();
    entered.notified().await;
    let cancel = sup.team_with_resident(child).unwrap().lock().residents[&child]
        .cancel
        .clone();
    let ending = tokio::spawn({
        let (sup, h) = (sup.clone(), handle.clone());
        async move { sup.end_member(r, &h).await }
    });
    cancel.cancelled().await;
    release.notify_one();
    ending.await.unwrap().unwrap();
    assert!(matches!(
        third.wait().await.unwrap(),
        ActivationResult::Ended {
            reason: ArchiveReason::ArchivedByParent,
            status: MemberRunStatus::Cancelled,
            ..
        }
    ));

    // All three directives were recorded in parent DM channel history.
    let proj = eng.read_projection(r).await.unwrap();
    let dm = crate::engine::dm_channel_between(&proj, "main", &handle).unwrap();
    let team = team_projection(&proj);
    let bodies: Vec<_> = team.channels[&dm]
        .log
        .iter()
        .map(|i| &team.mail[*i].body)
        .filter(|b| b.starts_with("ACT_"))
        .cloned()
        .collect();
    assert_eq!(bodies, ["ACT_0", "ACT_1", "ACT_2"]);
    assert_eq!(
        eng.replay(r)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::AgentRestarted { .. })
            ))
            .count(),
        1
    );
    drop(root_turn);
    drain_sup(&sup).await;
}

#[tokio::test]
async fn activation_rest_waits_for_followup_mail_live_children_and_skips_consumed_wake() {
    for with_child in [false, true] {
        let (eng, entered, release) = paused_engine().await;
        let r = root(&eng).await;
        let sup = ResidentSupervisor::start(eng.clone());
        let _drv = sup.root_driver_activity(r).await.unwrap();
        let (child, handle) = parked(&sup, r).await;
        let mut events = eng.bus().subscribe();
        let mut waiter = act(&sup, r, child, "FIRST", "g").await.unwrap();
        entered.notified().await;
        let desc = if with_child {
            Some(parked(&sup, child).await)
        } else {
            mail_h(&eng, r, &handle, "FOLLOW_UP").await.unwrap();
            None
        };
        release.notify_one();
        if with_child {
            tokio::time::timeout(Duration::from_secs(3), async {
                while !matches!(
                    ExtensionEvent::from_event(&events.recv().await.unwrap().event),
                    Some(ExtensionEvent::ResidentWorkFinished { handle: ref f, .. }) if f == &handle
                ) {}
            })
            .await
            .unwrap();
            let team = sup.team_with_resident(child).unwrap();
            let claim = team.lock().residents[&child].claim;
            team.settle_activation(child, claim.as_ref()).await.unwrap();
        } else {
            entered.notified().await;
        }
        assert!(waiter.done.try_recv().is_err(), "settled before true rest");
        if let Some((_, d)) = desc {
            sup.end_member(r, &d).await.unwrap();
        } else {
            release.notify_one();
        }
        assert!(matches!(
            waiter.wait().await.unwrap(),
            ActivationResult::Settled { .. }
        ));
        let mut next = act(&sup, r, child, "NEXT", "n").await.unwrap();
        entered.notified().await;
        release.notify_one();
        assert!(matches!(
            next.wait().await.unwrap(),
            ActivationResult::Settled { .. }
        ));
        sup.end_member(r, &handle).await.unwrap();
        drain_sup(&sup).await;
    }

    // Explicitly consumed trailing mail settles without another model turn.
    let (eng, entered, release) = paused_engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let _drv = sup.root_driver_activity(r).await.unwrap();
    let (child, handle) = parked(&sup, r).await;
    let mut waiter = act(&sup, r, child, "FIRST", "g").await.unwrap();
    entered.notified().await;
    let dm = dm_of(&eng, r, "main", &handle).await;
    eng.mail_send(
        r,
        MailEndpoint::Channel(dm.clone()),
        MailKind::Message,
        "already read".into(),
    )
    .await
    .unwrap();
    sup.team_with_resident(child)
        .unwrap()
        .on_mail("main", &MailEndpoint::Channel(dm.clone()))
        .await;
    eng.read_channel_history(None, child, &dm, None)
        .await
        .unwrap();
    release.notify_one();
    assert!(matches!(
        waiter.wait().await.unwrap(),
        ActivationResult::Settled { .. }
    ));
    let starts = eng
        .replay(child)
        .await
        .unwrap()
        .iter()
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
    sup.end_member(r, &handle).await.unwrap();
    drain_sup(&sup).await;
    assert_eq!(starts, 1);
}

#[tokio::test]
async fn activation_is_armed_before_bus_delivery_for_live_and_revive() {
    for live_revive in [false, true] {
        let eng = engine().await;
        let r = root(&eng).await;
        let sup = ResidentSupervisor::start(eng.clone());
        let drv = sup.root_driver_activity(r).await.unwrap();
        let (child, handle) = parked(&sup, r).await;
        sup.listener_stop.cancel();
        let listener = sup.listener.lock().unwrap().take().unwrap();
        listener.await.unwrap();
        drop(drv);
        let mut waiter = if live_revive {
            let (done, rx) = oneshot::channel();
            let dm = dm_of(&eng, r, "main", &handle).await;
            sup.revive_episode_with_activation(
                r,
                "main",
                &handle,
                dm,
                MailKind::Message,
                "PENDING".into(),
                None,
                None,
                Some(WorkflowActivation {
                    resolved: runtime_for(&eng),
                    guidance: Arc::from("g"),
                    route: None,
                    done,
                }),
            )
            .await
            .unwrap();
            ActivationWaiter { done: rx }
        } else {
            act(&sup, r, child, "PENDING", "g").await.unwrap()
        };
        let busy = sup.member_busy(r, child);
        let cancelled = eng.cancel_user_turn(child);
        let team = sup.team_with_resident(child).unwrap();
        let synthesis = {
            let mut st = team.lock();
            st.root_driver.autonomous.store(true, Ordering::Release);
            st.work_seq += 1;
            team.maybe_fire_quiescence(&mut st);
            st.residents[&r].synth_pending
        };
        let held = eng.stopped_by_user(child);
        sup.end_member(r, &handle).await.unwrap();
        waiter.wait().await.unwrap();
        drain_sup(&sup).await;
        assert_eq!(
            (busy, cancelled, held, synthesis),
            (Some(true), true, true, false)
        );
    }
}

#[tokio::test]
async fn activation_rejects_blank_revives_across_lock_race_and_rejects_roster_orphan() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    eng.store().claim_runtime_owner(sup.owner_run_id).unwrap();
    let _drv = sup.root_driver_activity(r).await.unwrap();
    let (child, handle) = parked(&sup, r).await;

    let before = eng.replay(r).await.unwrap();
    assert!(matches!(
        act(&sup, r, child, " \n\t ", "g").await,
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(eng.replay(r).await.unwrap(), before);

    // Archive racing before activation lock revives cleanly.
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.activation_lock_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let racing = tokio::spawn({
        let sup = sup.clone();
        async move { act(&sup, r, child, "RACE", "g").await }
    });
    entered.notified().await;
    sup.end_member(r, &handle).await.unwrap();
    release.notify_one();
    racing.await.unwrap().unwrap().wait().await.unwrap();

    // Concurrent roster orphan rejects activation before committing mail.
    sup.end_member(r, &handle).await.unwrap();
    let (e2, r2) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.registration_pause.lock().unwrap() = Some((e2.clone(), r2.clone()));
    let orphan_act = tokio::spawn({
        let sup = sup.clone();
        async move { act(&sup, r, child, "must not commit", "").await }
    });
    e2.notified().await;
    let dm = dm_of(&eng, r, "main", &handle).await;
    eng.store()
        .revive_episode(
            r,
            sup.owner_run_id,
            u64::MAX,
            None,
            child,
            "main".into(),
            dm,
            MailKind::Message,
            "prior".into(),
            hya_provider::COMPACT_CONTEXT_MARKER,
            None,
        )
        .await
        .unwrap();
    let before_orphan = eng.replay(r).await.unwrap();
    r2.notify_one();
    assert!(orphan_act.await.unwrap().is_err());
    assert_eq!(eng.replay(r).await.unwrap(), before_orphan);
    sup.end_member(r, &handle).await.unwrap();
    drain_sup(&sup).await;
}

#[tokio::test]
async fn ending_fences_parent_sweeps_descendants_racing_spawn_and_roster_orphans() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    eng.store().claim_runtime_owner(sup.owner_run_id).unwrap();
    let _drv = sup.root_driver_activity(r).await.unwrap();

    // 1) Descendant ends before parent; unrelated sibling stays live.
    let (stage, stage_h) = parked(&sup, r).await;
    let (_, desc_h) = parked(&sup, stage).await;
    let (other, other_h) = parked(&sup, r).await;
    sup.end_member(r, &stage_h).await.unwrap();
    assert_eq!(active_ids(&eng).await, [other]);
    assert_eq!(archived_handles(&eng, r).await, [desc_h, stage_h]);
    sup.end_member(r, &other_h).await.unwrap();

    // 2) Ending cancels parent before sweep and catches a child racing mid-ending.
    let (parent, parent_h) = parked(&sup, r).await;
    let lease = eng.try_begin_turn(parent).unwrap();
    let cancel = lease.bind_cancel(&CancellationToken::new());
    let (entered, release) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.ending_pause.lock().unwrap() = Some((entered.clone(), release.clone()));
    let ending = tokio::spawn({
        let (sup, h) = (sup.clone(), parent_h.clone());
        async move { sup.end_member(r, &h).await }
    });
    entered.notified().await;
    assert!(cancel.is_cancelled());
    let (agent, rt, claim) = {
        let team = sup.team_with_resident(parent).unwrap();
        let slot = &team.lock().residents[&parent];
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
    let (late, _, _) = sup
        .spawn_resident_parked(parent, agent, rt, "racing".into(), Some(&claim), None)
        .await
        .unwrap();
    drop(lease);
    release.notify_one();
    ending.await.unwrap().unwrap();
    assert!(!active_ids(&eng).await.contains(&late));

    // 3) Ending takes over and archives a roster-orphan descendant before its ancestor.
    let (anc, anc_h) = parked(&sup, r).await;
    let (orphan, orphan_h) = parked(&sup, anc).await;
    sup.end_member(r, &orphan_h).await.unwrap();
    let dm = dm_of(&eng, r, &anc_h, &orphan_h).await;
    eng.store()
        .revive_episode(
            r,
            sup.owner_run_id,
            u64::MAX,
            None,
            orphan,
            anc_h.clone(),
            dm,
            MailKind::Message,
            "orphan".into(),
            hya_provider::COMPACT_CONTEXT_MARKER,
            None,
        )
        .await
        .unwrap();
    assert!(sup.team_with_resident(orphan).is_none());
    sup.end_member(r, &anc_h).await.unwrap();
    assert!(!active_ids(&eng).await.contains(&orphan));
    let tail = archived_handles(&eng, r).await;
    assert_eq!(&tail[tail.len() - 2..], &[orphan_h, anc_h]);
    drain_sup(&sup).await;
}

#[tokio::test]
async fn registration_frames_precede_siblings_and_activation_mail() {
    let (hooks, entered, release) = PauseHook::session_start();
    let eng = Arc::new((*engine().await).clone().with_hooks(hooks.clone()));
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let _drv = sup.root_driver_activity(r).await.unwrap();
    let mut prepared = Vec::new();
    for _ in 0..2 {
        let (a, rt) = runtime_for(&eng);
        prepared.push(
            sup.prepare_resident(
                r,
                a,
                rt,
                ResidentSpawnContext::new("parked".into(), None, None, None),
            )
            .await
            .unwrap()
            .1,
        );
    }
    let first = prepared[0].session;
    *hooks.target.lock().unwrap() = Some(first);
    let mut bus = eng.bus().subscribe();
    let mut inits = sup.register_episodes(r, prepared, None).await.unwrap();
    entered.notified().await;
    inits.pop().unwrap().await.unwrap().unwrap();
    let mut activation = act(&sup, r, first, "work", "").await.unwrap();
    let expected: Vec<_> = eng
        .replay(r)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            matches!(
                ExtensionEvent::from_event(&e.event),
                Some(
                    ExtensionEvent::MemberSpawned { .. }
                        | ExtensionEvent::AgentRegistered { .. }
                        | ExtensionEvent::ChannelCreated { .. }
                        | ExtensionEvent::MailSent { .. }
                )
            )
        })
        .map(|e| e.seq)
        .collect();
    let mut observed = Vec::new();
    while let Ok(e) = bus.try_recv() {
        if e.event.session() == Some(r) && expected.contains(&e.seq) {
            observed.push(e.seq);
        }
    }
    release.notify_one();
    inits.pop().unwrap().await.unwrap().unwrap();
    activation.wait().await.unwrap();
    drain_sup(&sup).await;
    assert_eq!(observed, expected);
}

#[tokio::test]
async fn revival_keeps_preflighted_stage_spec_and_catalog() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let _drv = sup.root_driver_activity(r).await.unwrap();
    let agent = AgentSpec {
        name: AgentName::new("resident"),
        model: ModelRef::new("hya/offline"),
        workdir: "/tmp".into(),
        system_prompt: "PINNED_STAGE_PROMPT".into(),
        reasoning: Some(hya_provider::ReasoningEffort::High),
    };
    let binding = eng.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("resident").unwrap();
    let (child, handle, _) = sup
        .spawn_resident_parked(
            r,
            agent.clone(),
            (binding.clone(), Arc::from([]), resources.clone(), None),
            "parked".into(),
            None,
            None,
        )
        .await
        .unwrap();
    sup.end_member(r, &handle).await.unwrap();
    let mut a1 = sup
        .activate(
            r,
            child,
            (
                agent.clone(),
                (binding.clone(), Arc::from([]), resources.clone(), None),
            ),
            "revived".into(),
            Arc::from(""),
            None,
            None,
        )
        .await
        .unwrap();
    let actual = {
        let t = sup.team_with_resident(child).unwrap();
        let s = &t.lock().residents[&child];
        (s.agent.system_prompt.clone(), s.agent.reasoning)
    };
    sup.end_member(r, &handle).await.unwrap();
    a1.wait().await.unwrap();

    let empty = crate::AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
    ))
    .unwrap();
    eng.runtime_registry()
        .publish_catalog(Arc::new(empty))
        .unwrap();
    let revived = sup
        .activate(
            r,
            child,
            (agent.clone(), (binding, Arc::from([]), resources, None)),
            "pinned".into(),
            Arc::from(""),
            None,
            None,
        )
        .await;
    sup.end_member(r, &handle).await.unwrap();
    drain_sup(&sup).await;
    assert_eq!(actual, (agent.system_prompt, agent.reasoning));
    assert!(revived.is_ok());
}

#[tokio::test]
async fn root_driver_policy_survives_replacement_and_task_registration() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let drv = sup.root_driver_activity(r).await.unwrap();
    let lease = eng.try_begin_turn(r).unwrap();

    let main = sup.resolve_main_activation(r).await.unwrap();
    let (agent, runtime) = runtime_for(&eng);
    sup.spawn_residents_typed(
        r,
        vec![TaskResidentSpawn {
            agent,
            runtime,
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
    assert!(
        !sup.teams()
            .get(&r)
            .unwrap()
            .lock()
            .root_driver
            .autonomous
            .load(Ordering::Acquire)
    );

    let prev = sup.teams().get(&r).unwrap().clone();
    for done in prev.kill(Ending::Killed("team budget".into())).await {
        await_episode(done).await.unwrap();
    }
    let repl = sup.team_for(r).await.unwrap();
    {
        let mut st = repl.lock();
        st.work_seq += 1;
        repl.maybe_fire_quiescence(&mut st);
        assert!(!st.residents[&r].synth_pending);
    }
    drop(drv);
    assert!(!repl.lock().root_driver.autonomous.load(Ordering::Acquire));
    drop((lease, repl));
    drain_sup(&sup).await;
}

#[tokio::test]
async fn wait_stalls_forget_ended_episodes_deleted_callers_and_reject_late_or_revived() {
    let eng = engine().await;
    let (caller, other) = (root(&eng).await, root(&eng).await);
    let sup = ResidentSupervisor::start(eng.clone());
    let drv = sup.root_driver_activity(other).await.unwrap();
    let (child, handle) = parked(&sup, other).await;
    let claim = sup.member_claim(child).unwrap();

    sup.record_wait_stall(caller, claim, None);
    sup.record_wait_stall(other, claim, None);
    eng.delete_session(caller).await.unwrap();
    assert!(
        !sup.wait_stall_reported(caller, claim, None)
            && sup.wait_stall_reported(other, claim, None)
    );

    sup.end_member(other, &handle).await.unwrap();
    assert!(!sup.wait_stall_reported(other, claim, None));
    sup.record_wait_stall(other, claim, None);
    assert!(!sup.wait_stall_reported(other, claim, None));

    let lease = eng.try_begin_turn(child).unwrap();
    mail_h(&eng, other, &handle, "revive").await.unwrap();
    sup.record_wait_stall(other, claim, None);
    assert!(!sup.wait_stall_reported(other, sup.member_claim(child).unwrap(), None));
    eng.begin_drain(FinishCause::Shutdown);
    drop((lease, drv));
    drain_sup(&sup).await;
}

#[tokio::test]
async fn resident_mail_live_and_catch_up_deliver_only_incoming_work() {
    for catch_up in [false, true] {
        let eng = engine().await;
        let r = root(&eng).await;
        let sup = ResidentSupervisor::start(eng.clone());
        let drv = sup.root_driver_activity(r).await.unwrap();
        let (child, handle) = parked(&sup, r).await;
        let lease = eng.try_begin_turn(child).unwrap();
        let team = sup.team_with_resident(child).unwrap();
        let dm = dm_of(&eng, r, &handle, "main").await;
        let mut events = eng.bus().subscribe();
        for (from, body, want_pending) in [
            (handle.as_str(), "own mailbox post", false),
            ("main", "incoming mailbox work", true),
        ] {
            let mut committed = eng
                .store()
                .append_events(
                    r,
                    vec![
                        ExtensionEvent::MailSent {
                            session: r,
                            from: from.into(),
                            to: MailEndpoint::Channel(dm.clone()),
                            kind: MailKind::Message,
                            body: body.into(),
                        }
                        .into_event(),
                    ],
                )
                .await
                .unwrap();
            if catch_up {
                team.enqueue(TeamEvent::Recover);
            } else {
                sup.route_envelope(committed.pop().unwrap());
            }
            let (tx, rx) = oneshot::channel();
            team.enqueue(TeamEvent::Flush(tx));
            rx.await.unwrap();
            assert_eq!(team.lock().residents[&child].pending, want_pending);
        }
        drop(lease);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !matches!(
                ExtensionEvent::from_event(&events.recv().await.unwrap().event),
                Some(ExtensionEvent::ResidentWorkFinished { session, handle: ref f, inbox_through: 2, .. }) if session == r && f == &handle
            ) {}
        }).await.unwrap();
        let proj = eng.read_projection_shared(r).await.unwrap();
        let team = team_projection(&proj);
        assert_eq!(
            (
                team.inboxes[&handle].len(),
                team.roster[&handle].resident_cursor
            ),
            (2, 2)
        );
        let resp: String = eng
            .replay(child)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                Event::TextDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect();
        assert!(resp.contains("incoming mailbox work") && !resp.contains("own mailbox post"));
        drop(drv);
        drain_sup(&sup).await;
    }
}

#[tokio::test]
async fn slow_team_mail_does_not_block_another_root_and_queue_drops_payloads() {
    let eng = engine().await;
    let (first, second, third) = (root(&eng).await, root(&eng).await, root(&eng).await);
    let sup = ResidentSupervisor::start(eng.clone());
    let (d1, d2) = (
        sup.root_driver_activity(first).await.unwrap(),
        sup.root_driver_activity(second).await.unwrap(),
    );
    let (e1, r1) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.mail_pause.lock().unwrap() = Some((first, e1.clone(), r1.clone()));
    let emit = |target| {
        let eng = eng.clone();
        async move {
            eng.emit_for_actor(
                None,
                target,
                ExtensionEvent::MailSent {
                    session: target,
                    from: scope::HARNESS_HANDLE.into(),
                    to: MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
                    kind: MailKind::Message,
                    body: "q".into(),
                }
                .into_event(),
            )
            .await
            .unwrap();
        }
    };
    emit(first).await;
    e1.notified().await;
    let (e2, r2) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    *sup.mail_pause.lock().unwrap() = Some((second, e2.clone(), r2.clone()));
    emit(first).await;
    emit(second).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), e2.notified())
            .await
            .is_ok()
    );
    let t1 = sup.teams().get(&first).unwrap().clone();
    assert_eq!(t1.lock().messages_used, 0);
    let (tx, rx) = oneshot::channel();
    t1.enqueue(TeamEvent::Flush(tx));
    r1.notify_one();
    r2.notify_one();
    rx.await.unwrap();
    assert_eq!(t1.lock().messages_used, 2);
    drop((d1, d2));

    // Blocked team queue drops large payloads and keeps only routing fields.
    let (ev_tx, mut queue) = tokio::sync::mpsc::unbounded_channel();
    let t3 = Arc::new(TeamActor::new(
        third,
        eng.clone(),
        Arc::downgrade(&sup),
        Arc::new(RootDriverState {
            runs: AtomicUsize::new(0),
            autonomous: AtomicBool::new(true),
        }),
        sup.tasks.clone(),
        ev_tx,
    ));
    sup.teams().insert(third, t3.clone());
    for (seq, event) in [
        Event::TextDelta {
            session: third,
            message: hya_proto::MessageId::new(),
            part: hya_proto::PartId::new(),
            delta: "LARGE_ACTIVITY_PAYLOAD".repeat(10_000),
        },
        ExtensionEvent::MailSent {
            session: third,
            from: "harness".into(),
            to: MailEndpoint::Handle("main".into()),
            kind: MailKind::Message,
            body: "LARGE_MAIL_PAYLOAD".repeat(10_000),
        }
        .into_event(),
    ]
    .into_iter()
    .enumerate()
    {
        sup.route_envelope(hya_proto::Envelope {
            seq: hya_proto::EventSeq(seq as u64),
            ts_millis: 0,
            event,
        });
    }
    let (q1, q2) = (
        format!("{:?}", queue.try_recv().unwrap()),
        format!("{:?}", queue.try_recv().unwrap()),
    );
    assert!(!q1.contains("LARGE_ACTIVITY_PAYLOAD") && !q2.contains("LARGE_MAIL_PAYLOAD"));
    assert!(q1.contains(&format!("{third:?}")) && q2.contains("main"));
    assert_eq!(t3.lock().queued_wakes, 1);
    assert!(queue.try_recv().is_err());
    sup.teams().remove(&third);
    drain_sup(&sup).await;
}

#[tokio::test]
async fn supervisor_drop_drain_and_last_episode_retire_cleanly() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let (child, _) = parked(&sup, r).await;
    let team = sup.team_with_resident(child).unwrap();
    let done = team.lock().residents[&child].done.subscribe();
    let weak = Arc::downgrade(&sup);
    drop(sup);
    assert!(weak.upgrade().is_none() && eng.resident_supervisor().is_none());
    assert!(team.lock().phase.killed() && team.lock().residents[&child].ending.is_some());
    tokio::time::timeout(Duration::from_secs(5), await_episode(done))
        .await
        .unwrap()
        .unwrap();
    team.tasks.close();
    team.tasks.wait().await;
    assert!(team.lock().residents.is_empty());

    let eng2 = engine().await;
    let r2 = root(&eng2).await;
    let sup2 = ResidentSupervisor::start(eng2.clone());
    let drv = sup2.root_driver_activity(r2).await.unwrap();
    let (c2, h2) = parked(&sup2, r2).await;
    let t2 = sup2.team_with_resident(c2).unwrap();
    sup2.end_member(r2, &h2).await.unwrap();
    assert!(sup2.team_with_resident(c2).is_none());
    drop(drv);
    assert!(t2.cancel.is_cancelled() && sup2.teams().get(&r2).is_none());

    let drv2 = sup2.root_driver_activity(r2).await.unwrap();
    let (c3, _) = parked(&sup2, r2).await;
    sup2.team_with_resident(c3)
        .unwrap()
        .kill(Ending::Killed("replace".into()))
        .await;
    let _repl = sup2.team_for(r2).await.unwrap();
    drop((_repl, drv2));
    drain_sup(&sup2).await;
    assert!(
        sup2.listener.lock().unwrap().is_none()
            && sup2.tasks.is_empty()
            && sup2.episode_count() == 0
            && sup2.teams().is_empty()
    );
}

#[tokio::test]
async fn interactive_turn_release_retires_team_and_shutdown_does_not_retain_engine() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let turn = eng.try_begin_turn(r).unwrap();
    let (child, handle) = parked(&sup, r).await;
    let team = sup.team_with_resident(child).unwrap();
    sup.end_member(r, &handle).await.unwrap();
    eng.emit_for_actor(
        None,
        r,
        ExtensionEvent::MailSent {
            session: r,
            from: scope::HARNESS_HANDLE.into(),
            to: MailEndpoint::Handle(scope::ROOT_HANDLE.into()),
            kind: MailKind::Message,
            body: "child ended".into(),
        }
        .into_event(),
    )
    .await
    .unwrap();
    sup.listener_stop.cancel();
    let listener = sup.listener.lock().unwrap().take().unwrap();
    listener.await.unwrap();
    let (tx, rx) = oneshot::channel();
    team.enqueue(TeamEvent::Flush(tx));
    rx.await.unwrap();
    crate::member_wait::wait_for_members(
        None,
        &eng,
        &sup,
        r,
        hya_tool::WaitSpec::parse(&serde_json::json!({"timeout_secs": 0}), true).unwrap(),
        None,
    )
    .await
    .unwrap();
    assert!(sup.teams().contains_key(&r));
    drop(turn);
    tokio::time::timeout(Duration::from_secs(1), team.cancel.cancelled())
        .await
        .unwrap();
    sup.tasks.close();
    sup.tasks.wait().await;
    assert!(sup.tasks.is_empty() && team.lock().residents.is_empty() && sup.teams().is_empty());
    drain_sup(&sup).await;

    let eng2 = engine().await;
    let r2 = root(&eng2).await;
    let sup2 = ResidentSupervisor::start(eng2.clone());
    let weak = Arc::downgrade(&eng2);
    let lease = eng2.try_begin_turn(r2).unwrap();
    eng2.begin_drain(FinishCause::Shutdown);
    drop(lease);
    assert_eq!(Arc::strong_count(&sup2), 1);
    drain_sup(&sup2).await;
    drop((sup2, eng2));
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn member_name_reservations_release_on_commit_cancel_and_store_failure() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let drv = sup.root_driver_activity(r).await.unwrap();

    let (a, rt) = runtime_for(&eng);
    let (_, cancelled, _) = sup
        .prepare_resident(
            r,
            a,
            rt,
            ResidentSpawnContext::new("park".into(), None, None, None),
        )
        .await
        .unwrap();
    assert_eq!(eng.pending_member_names(r), 1);
    drop(cancelled);
    assert_eq!(eng.pending_member_names(r), 0);

    let (a, rt) = runtime_for(&eng);
    let (_, prep, _) = sup
        .prepare_resident(
            r,
            a,
            rt,
            ResidentSpawnContext::new("park".into(), None, None, None),
        )
        .await
        .unwrap();
    let leaf = prep.leaf.clone();
    assert_eq!(eng.pending_member_names(r), 1);
    sup.register_episodes(r, vec![prep], None).await.unwrap();
    assert_eq!(eng.pending_member_names(r), 0);
    assert!(
        crate::handle_naming::team_leaves(&eng.read_projection(r).await.unwrap()).contains(&leaf)
    );
    drop(drv);
    drain_sup(&sup).await;

    let eng2 = engine().await;
    let r2 = root(&eng2).await;
    let mut tx = eng2.store().begin_transaction().await.unwrap();
    sqlx::query("DROP TABLE event_log")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(matches!(
        eng2.mint_member_leaf(r2, "scout").await,
        Err(CoreError::Store(_))
    ));
    assert_eq!(eng2.pending_member_names(r2), 0);
}

#[tokio::test]
async fn recovered_episode_ignores_outgoing_dm_and_staged_claim_recovers_root_mail() {
    let eng = engine().await;
    let r = root(&eng).await;
    let sup = ResidentSupervisor::start(eng.clone());
    let _lead = eng.try_begin_turn(r).unwrap();
    let child = eng
        .create(CreateSession {
            parent: Some(r),
            agent: AgentName::new("hya-scout"),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".into(),
            project: None,
            kind: SessionKind::Project,
        })
        .await
        .unwrap();
    eng.store()
        .append_events(
            r,
            vec![
                ExtensionEvent::AgentRegistered {
                    session: r,
                    agent_session: child,
                    handle: "scout".into(),
                    parent: Some("main".into()),
                    agent_type: AgentName::new("hya-scout"),
                    mode: SubagentMode::Resident,
                }
                .into_event(),
                ExtensionEvent::ChannelCreated {
                    session: r,
                    channel: "DM-12345678".into(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec!["main".into(), "main/scout".into()],
                }
                .into_event(),
                ExtensionEvent::MailSent {
                    session: r,
                    from: "main/scout".into(),
                    to: MailEndpoint::Channel("DM-12345678".into()),
                    kind: MailKind::Message,
                    body: "outgoing status".into(),
                }
                .into_event(),
            ],
        )
        .await
        .unwrap();
    eng.store()
        .try_claim_new(r, child, OwnerRunId::new())
        .await
        .unwrap();
    eng.store().claim_runtime_owner(sup.owner_run_id).unwrap();
    let rec = eng
        .store()
        .recover_claim(child, sup.owner_run_id)
        .await
        .unwrap();
    let child_turn = eng.try_begin_turn(child).unwrap();
    let (a, rt) = runtime_for(&eng);
    sup.register_recovered_resident(r, "main/scout".into(), a, rt, rec, ResidentRecovery::Idle)
        .await
        .unwrap();
    drop(child_turn);
    assert_eq!(sup.member_busy(r, child), Some(false));
    drop(_lead);
    drain_sup(&sup).await;

    // Ended staged recovery claim still recovers unread root mail on startup.
    let eng2 = engine().await;
    let r2 = root(&eng2).await;
    let prev = ResidentSupervisor::start(eng2.clone());
    let drv = prev.root_driver_activity(r2).await.unwrap();
    let (c2, h2) = parked(&prev, r2).await;
    let claim = prev.team_with_resident(c2).unwrap().lock().residents[&c2]
        .claim
        .unwrap();
    prev.end_member(r2, &h2).await.unwrap();
    drop(drv);
    prev.listener_stop.cancel();
    let listener = prev.listener.lock().unwrap().take().unwrap();
    listener.await.unwrap();
    prev.tasks.close();
    prev.tasks.wait().await;
    drop(prev);

    let sup2 = ResidentSupervisor::start(eng2.clone());
    let (a2, rt2) = runtime_for(&eng2);
    sup2.defer_recovered_resident(
        r2,
        h2,
        a2,
        rt2,
        RecoveredActorClaim {
            claim,
            previous_epoch: claim.epoch,
        },
        ResidentRecovery::Idle,
    );
    eng2.store()
        .append_event(
            r2,
            &ExtensionEvent::MailSent {
                session: r2,
                from: scope::HARNESS_HANDLE.into(),
                to: MailEndpoint::Handle("main".into()),
                kind: MailKind::Message,
                body: "unread root mail".into(),
            }
            .into_event(),
        )
        .await
        .unwrap();
    assert_eq!(sup2.start_recovered().await.unwrap(), 0);
    sup2.tasks.close();
    tokio::time::timeout(Duration::from_secs(5), sup2.tasks.wait())
        .await
        .unwrap();
    let ok = eng2.replay(r2).await.unwrap().iter().any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("unread root mail")));
    drain_sup(&sup2).await;
    assert!(ok);
}
