//! Consolidated integration contracts for report gates, archiving/revival,
//! handoff policy, steer injection, channel reads/policies, report-ends-turn,
//! and the `wait` lifecycle — zero real wall-clock sleeps.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream;
use hya_agent_channels_core::AgentChannelsService;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleSource, ChannelCapability, ChannelParticipantRole, SourceFile,
    prepare_package,
};
use hya_engine::{
    AgentCatalog, AgentSpec, ChannelPolicy, CoreError, EventBus, ResidentSupervisor, SessionEngine,
    SessionStoreExt as _, SubagentGovernor, SubagentLimits, SummarizeOptions, Summarizer,
    TerminalHandoffOptions, TurnBinding, TurnLease, run_lifecycle_service,
};
use hya_proto::{
    ChannelKind, Event, FinishCause, FinishReason, MailEndpoint, MailKind, MemberId,
    MemberRunStatus, MessageId, ModelRef, OwnerRunId, PartId, PartProjection, Role, RosterStatus,
    SessionId, ToolCallId, ToolName, ToolPartState, ToolSchema, UsagePurpose, scope,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::extension::{
    ExtensionEvent, MemberProjection, handoff_projection, members_projection, team_projection,
};
use hya_tool::{
    Action, ChannelPolicySnapshot, LifecyclePlane, Mode, PermissionPlane, PermissionRules, Rule,
    Tool, ToolCtx, ToolError, WaitMemberState, WaitMode, WaitOutcome, WaitSpec, WaitWake,
};
use hya_tool::{ArchiveReason, ReportOutcome};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::support;

fn empty_workdir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let p =
            std::env::temp_dir().join(format!("hya-it-report-wait-empty-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    })
}

fn fake_caps(m: &ModelRef) -> Option<Capabilities> {
    (m.as_str() == "fake").then_some(Capabilities {
        streaming_tool_calls: true,
        usage_reporting: true,
        max_context: 200_000,
        ..Capabilities::default()
    })
}

fn stream_steps(steps: &[FakeStep], s: SessionId, m: MessageId) -> EventStream {
    Box::pin(stream::iter(
        FakeProvider::materialize(steps, s, m)
            .into_iter()
            .map(Ok::<Event, ProviderError>),
    ))
}

struct EmptyStub;
#[async_trait]
impl Provider for EmptyStub {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        Ok(stream_steps(&[FakeStep::Finish(FinishReason::Stop)], s, m))
    }
}

#[derive(Default)]
struct ParkedMembers {
    root: Mutex<Option<SessionId>>,
    release: Notify,
}
#[async_trait]
impl Provider for ParkedMembers {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if *self.root.lock().unwrap() != Some(s) {
            self.release.notified().await;
        }
        EmptyStub.stream(req, s, m).await
    }
}

struct FailingMembers(SessionId);
#[async_trait]
impl Provider for FailingMembers {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m)
    }
    async fn stream(
        &self,
        req: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if s == self.0 {
            EmptyStub.stream(req, s, m).await
        } else {
            Err(ProviderError::Incompatible("member model exploded".into()))
        }
    }
}

#[derive(Default)]
struct ParkedHandoff {
    entered: Notify,
    release: Notify,
}
#[async_trait]
impl Summarizer for ParkedHandoff {
    async fn summarize(
        &self,
        _: &[hya_proto::Message],
        _: SummarizeOptions,
    ) -> Result<String, CoreError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok("1. Goal - real goal\n2. Current state - ready\n3. Files and code - none\n4. Decisions - none\n5. Pending tasks - real pending\n6. Next step - wait".into())
    }
}

#[derive(Default)]
struct CountingHandoff(AtomicUsize);
#[async_trait]
impl Summarizer for CountingHandoff {
    async fn summarize(
        &self,
        _: &[hya_proto::Message],
        opts: SummarizeOptions,
    ) -> Result<String, CoreError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if let Some(u) = &opts.usage {
            u.record(
                ModelRef::new("fake"),
                hya_proto::TokenUsage {
                    input: 10,
                    output: 5,
                    ..Default::default()
                },
            );
        }
        Ok("1. Goal - model goal\n2. Current state - done\n3. Files and code - none\n4. Decisions - none\n5. Pending tasks - none\n6. Next step - none".into())
    }
}

// ---- Shared report/team helpers ----

async fn stub_engine() -> Arc<SessionEngine> {
    Arc::new(support::engine_with_provider(Arc::new(EmptyStub), PermissionRules::default()).await)
}

async fn restricted_engine(channels: &str) -> Arc<SessionEngine> {
    let prepared = prepare_package(BundleSource::new(
        "channel-restriction",
        vec![SourceFile::new(
            "bundle.yaml",
            format!("schema_version: 2\nkind: Bundle\nidentity: {{ id: acme/channel-restriction, version: 1.0.0, publisher: acme }}\nchannels:\n{channels}\n"),
        )],
    ))
    .unwrap();
    let multi_agent = hya_bundle::first_party_bundle("hya/multi-agent").unwrap();
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&[&prepared, multi_agent]).unwrap(),
        ))
        .unwrap(),
    );
    Arc::new(
        support::engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(Arc::new(EmptyStub))),
            support::builtin_test_runtime_with_catalog(catalog),
            PermissionRules::default(),
        )
        .await,
    )
}

async fn root_team(engine: &SessionEngine) -> SessionId {
    support::create_root_session(engine, "hya-main", "fake", empty_workdir()).await
}

async fn ensure_main(sup: &ResidentSupervisor, engine: &SessionEngine, root: SessionId) {
    let agent = support::agent_spec("hya-main", "fake", empty_workdir());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-main").unwrap();
    sup.ensure_main(root, agent, (binding, Arc::from([]), resources), None, None)
        .await
        .unwrap();
}

async fn send_mail(engine: &SessionEngine, from: SessionId, to: &str, body: &str) {
    engine
        .mail_send(
            from,
            MailEndpoint::Handle(to.into()),
            MailKind::Message,
            body.into(),
        )
        .await
        .unwrap();
}

async fn is_active(engine: &SessionEngine, s: SessionId) -> bool {
    engine
        .store()
        .active_actor_ids()
        .await
        .unwrap()
        .contains(&s)
}

async fn wait_idle(engine: &SessionEngine, root: SessionId, handle: &str, child: SessionId) {
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let p = engine.read_projection(root).await.unwrap();
            let done = engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant);
            done && team_projection(&p)
                .roster
                .get(handle)
                .is_some_and(|e| e.status == RosterStatus::Idle)
        })
        .await
    );
}

async fn spawn_idle(
    sup: &ResidentSupervisor,
    engine: &SessionEngine,
    parent: SessionId,
    directive: &str,
) -> (SessionId, String) {
    let agent = support::agent_spec("hya-scout", "fake", empty_workdir());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy("hya-scout").unwrap();
    let (child, handle) = sup
        .spawn_resident(
            parent,
            agent,
            (binding, Arc::from([]), resources, None),
            directive.into(),
            None,
            None,
        )
        .await
        .unwrap();
    let (root, _) = engine.session_lineage(parent).await.unwrap();
    wait_idle(engine, root, &handle, child).await;
    (child, handle)
}

async fn wait_archived(engine: &SessionEngine, root: SessionId, handle: &str) {
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            team_projection(&engine.read_projection(root).await.unwrap())
                .archived
                .contains_key(handle)
        })
        .await
    );
}

async fn submit_and_wait(
    engine: &SessionEngine,
    sup: &ResidentSupervisor,
    root: SessionId,
    handle: &str,
    report: &str,
) {
    sup.submit_report(root, handle, ReportOutcome::Done, report.into())
        .await
        .unwrap();
    wait_archived(engine, root, handle).await;
}

async fn mail_main_from_peer(engine: &SessionEngine, root: SessionId, body: &str) {
    engine
        .store()
        .append_events(
            root,
            vec![
                ExtensionEvent::ChannelCreated {
                    session: root,
                    channel: "DM-12345678".into(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec!["main/peer-1".into(), "main".into()],
                }
                .into_event(),
                ExtensionEvent::MailSent {
                    session: root,
                    from: "main/peer-1".into(),
                    to: MailEndpoint::Channel("DM-12345678".into()),
                    kind: MailKind::Message,
                    body: body.into(),
                }
                .into_event(),
            ],
        )
        .await
        .unwrap();
}

async fn member_row(
    engine: &SessionEngine,
    parent: SessionId,
    child: SessionId,
) -> MemberProjection {
    members_projection(&engine.read_projection(parent).await.unwrap())
        .into_iter()
        .find(|r| r.child == Some(child))
        .unwrap()
}

async fn assistant_turns(engine: &SessionEngine, s: SessionId) -> usize {
    engine
        .read_projection(s)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
        .count()
}

async fn transcript_has(engine: &SessionEngine, s: SessionId, needle: &str) -> bool {
    engine
        .read_projection(s)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .any(|m| {
            m.parts
                .iter()
                .any(|p| matches!(p, PartProjection::Text { text, .. } if text.contains(needle)))
        })
}

async fn dm_for(engine: &SessionEngine, root: SessionId, handle: &str) -> String {
    team_projection(&engine.read_projection(root).await.unwrap())
        .channels
        .iter()
        .find(|(_, c)| {
            c.kind == ChannelKind::Dm && c.members.contains(handle) && c.members.contains("main")
        })
        .unwrap()
        .0
        .clone()
}

// ---- Section A: Report gate, archive, revive, handoff, steer, channel read ----

#[tokio::test]
async fn report_gate_rejects_unread_mail_live_children_and_accepts_current_wake_mail() {
    let provider = Arc::new(ParkedMembers::default());
    let engine =
        Arc::new(support::engine_with_provider(provider.clone(), PermissionRules::default()).await);
    let root = root_team(&engine).await;
    *provider.root.lock().unwrap() = Some(root);
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let agent = support::agent_spec("hya-scout", "fake", empty_workdir());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let (_, handle) = sup
        .spawn_resident(
            root,
            agent,
            (
                binding.clone(),
                Arc::from([]),
                binding.agent_resource_policy("hya-scout").unwrap(),
                None,
            ),
            "explore".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            team_projection(&engine.read_projection(root).await.unwrap()).roster[&handle].status
                == RosterStatus::Busy
        })
        .await
    );
    send_mail(&engine, root, &handle, "one question first").await;
    let dm = dm_for(&engine, root, &handle).await;
    let Err(CoreError::Invalid(msg)) = sup.report_gate(root, &handle).await else {
        panic!("expected unread-mail rejection");
    };
    for needle in [
        "unread mail",
        &format!("#{dm}"),
        &format!("`read channel://{dm}`"),
        "`list_channel`",
        "`send`",
        "`wait`",
    ] {
        assert!(msg.contains(needle), "missing {needle}: {msg}");
    }
    assert!(
        team_projection(&engine.read_projection(root).await.unwrap())
            .roster
            .contains_key(&handle)
    );
    provider.release.notify_waiters();

    // Live children rejection & current-wake inbox_through acceptance.
    let e2 = stub_engine().await;
    let r2 = root_team(&e2).await;
    let s2 = ResidentSupervisor::start(e2.clone());
    ensure_main(&s2, &e2, r2).await;
    let (lead, lead_h) = spawn_idle(&s2, &e2, r2, "lead").await;
    let _ = spawn_idle(&s2, &e2, lead, "sub").await;
    let Err(CoreError::Invalid(child_msg)) = s2.report_gate(r2, &lead_h).await else {
        panic!("expected live-children rejection");
    };
    assert!(child_msg.contains("child"));

    let (scout, scout_h) = spawn_idle(&s2, &e2, r2, "scout").await;
    send_mail(&e2, r2, &scout_h, "follow-up question").await;
    e2.store()
        .append_event(
            r2,
            &ExtensionEvent::ResidentWorkStarted {
                session: r2,
                actor_session: scout,
                handle: scout_h.clone(),
                epoch: hya_proto::ActorEpoch::INITIAL,
                inbox_through: 2,
            }
            .into_event(),
        )
        .await
        .unwrap();
    assert!(
        team_projection(&e2.read_projection(r2).await.unwrap()).roster[&scout_h].resident_cursor
            < 2
    );
    s2.report_gate(r2, &scout_h).await.unwrap();
}

#[tokio::test]
async fn refused_report_is_dropped_until_member_reports_again() {
    let summary = Arc::new(ParkedHandoff::default());
    let engine = Arc::new(
        support::engine_with_provider(Arc::new(EmptyStub), PermissionRules::default())
            .await
            .with_terminal_handoff(summary.clone(), TerminalHandoffOptions::default()),
    );
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let _lead = engine.try_begin_turn(root).unwrap();
    let (child, handle) = spawn_idle(&sup, &engine, root, "work").await;

    sup.submit_report(root, &handle, ReportOutcome::Done, "STALE REPORT".into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), summary.entered.notified())
        .await
        .unwrap();
    send_mail(&engine, root, &handle, "late directive").await;
    summary.release.notify_one();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            engine
                .read_projection(child)
                .await
                .unwrap()
                .session
                .messages
                .iter()
                .filter(|m| m.role == Role::Assistant && m.finish.is_some())
                .count()
                >= 2
        })
        .await
    );

    let (lifecycle, rx) = LifecyclePlane::new();
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        sup.clone(),
        rx,
        CancellationToken::new(),
    ));
    let res = lifecycle
        .for_session(root)
        .wait(
            WaitSpec {
                targets: vec![handle.clone()],
                mode: WaitMode::All,
                timeout: Duration::from_millis(25),
                wake_on_mail: false,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(res.finished.is_empty() && res.already_finished.is_empty());
    assert!(
        team_projection(&engine.read_projection(root).await.unwrap())
            .roster
            .contains_key(&handle)
    );
    assert!(handoff_projection(&engine.read_projection(child).await.unwrap()).is_none());

    sup.submit_report(root, &handle, ReportOutcome::Done, "FRESH REPORT".into())
        .await
        .unwrap();
    summary.entered.notified().await;
    summary.release.notify_one();
    wait_archived(&engine, root, &handle).await;
    assert_eq!(
        member_row(&engine, root, child).await.summary,
        "FRESH REPORT"
    );
}

#[tokio::test]
async fn report_archives_end_to_end_mints_channels_and_revives_via_handle_and_dm() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (child, handle) = spawn_idle(&sup, &engine, root, "explore the tree").await;

    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    let (gid, group) = team
        .channels
        .iter()
        .find(|(_, c)| c.kind == ChannelKind::Group && c.unit.as_deref() == Some("main"))
        .unwrap();
    assert!(
        gid.starts_with("announce-")
            && group.members.contains("main")
            && group.members.contains(&handle)
    );
    let dm = dm_for(&engine, root, &handle).await;
    assert_eq!(
        (team.channels[&dm].members.len(), &team.channels[&dm].unit),
        (2, &None)
    );

    submit_and_wait(&engine, &sup, root, &handle, "found three call sites").await;
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(!team.roster.contains_key(&handle));
    assert_eq!(
        (
            team.archived[&handle].reason,
            team.archived[&handle].session
        ),
        (ArchiveReason::Reported, child)
    );
    assert!(
        team.inboxes["main"]
            .iter()
            .any(|i| team.mail[*i].body.contains("found three call sites"))
    );
    assert!(
        members_projection(&proj)
            .iter()
            .any(|m| m.summary.contains("found three call sites"))
    );
    let handoff = handoff_projection(&engine.read_projection(child).await.unwrap()).unwrap();
    assert!(handoff.degraded && handoff.doc.contains("Goal"));
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::AgentArchived {
            reason: ArchiveReason::Reported,
            ..
        })
    )));
    assert!(!is_active(&engine, child).await);

    // Revive via handle, then archive and revive via DM channel id.
    send_mail(&engine, root, &handle, "continue: check the tests too").await;
    assert!(
        team_projection(&engine.read_projection(root).await.unwrap())
            .roster
            .contains_key(&handle)
    );
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::AgentRestarted { .. })
    )));
    wait_idle(&engine, root, &handle, child).await;
    assert!(transcript_has(&engine, child, "check the tests too").await);

    sup.archive_member(root, &handle, "pause").await.unwrap();
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(dm),
            MailKind::Message,
            "wake via DM".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        team_projection(&engine.read_projection(root).await.unwrap()).roster[&handle].session,
        child
    );
    wait_idle(&engine, root, &handle, child).await;
    assert!(transcript_has(&engine, child, "wake via DM").await);
}

#[tokio::test]
async fn revive_rejects_non_parent_and_claim_conflict_leaves_child_unchanged() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let _lead_turn = engine.try_begin_turn(root).unwrap();
    let (lead, lead_h) = spawn_idle(&sup, &engine, root, "lead").await;
    let (_worker, worker_h) = spawn_idle(&sup, &engine, lead, "work").await;

    // 1) Root cannot revive a grandchild it did not spawn.
    submit_and_wait(&engine, &sup, root, &worker_h, "").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let p = engine.read_projection(root).await.unwrap();
            let team = team_projection(&p);
            let e = &team.roster[&lead_h];
            let through = team.inboxes.get(&lead_h).map_or(0, Vec::len) as u64;
            e.resident_cursor >= through
                && e.resident_work.is_none()
                && e.status == RosterStatus::Idle
        })
        .await
    );
    submit_and_wait(&engine, &sup, root, &lead_h, "").await;
    assert!(
        engine
            .mail_send(
                root,
                MailEndpoint::Handle(worker_h.clone()),
                MailKind::Message,
                "skip".into()
            )
            .await
            .is_err()
    );
    assert!(
        !team_projection(&engine.read_projection(root).await.unwrap())
            .roster
            .contains_key(&worker_h)
    );

    // 2) Competing claim rejects revival without mutating child transcript (no PRIOR HANDOFF).
    let (child, handle) = spawn_idle(&sup, &engine, root, "work").await;
    submit_and_wait(&engine, &sup, root, &handle, "done").await;
    let other = engine
        .store()
        .try_claim_new(root, child, OwnerRunId::new())
        .await
        .unwrap();
    let before = engine.replay(child).await.unwrap();
    let res = engine
        .mail_send(
            root,
            MailEndpoint::parse(&handle),
            MailKind::Message,
            "competing".into(),
        )
        .await;
    assert!(matches!(
        res,
        Err(CoreError::Store(hya_store::StoreError::BundleClaimConflict { claim_key, .. })) if claim_key == child.to_string()
    ));
    assert_eq!(engine.replay(child).await.unwrap(), before);
    engine.store().release_claim(&other).await.unwrap();
}

#[tokio::test]
async fn forced_stop_handoff_carries_prior_facts_and_is_searchable() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (child, handle) = spawn_idle(&sup, &engine, root, "quartz investigation").await;
    sup.end_member(root, &handle).await.unwrap();
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(!team.roster.contains_key(&handle));
    assert_eq!(
        team.archived[&handle].reason,
        ArchiveReason::ArchivedByParent
    );
    let h = handoff_projection(&engine.read_projection(child).await.unwrap()).unwrap();
    assert!(h.doc.starts_with("1. Goal - ") && h.doc.contains("5. Pending tasks - "));

    let rows = hya_multi_agent_core::MultiAgentService::search_archived(
        engine.host_runtime().as_ref(),
        root,
        "quartz",
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].session, child.to_string());
    assert!(!rows[0].goal.is_empty() && rows[0].pending.contains("quartz investigation"));

    // Forced stop on revived episode carries prior generation facts into degraded handoff.
    let summary = Arc::new(ParkedHandoff::default());
    let s_engine = Arc::new(
        support::engine_with_provider(Arc::new(EmptyStub), PermissionRules::default())
            .await
            .with_terminal_handoff(summary.clone(), TerminalHandoffOptions::default()),
    );
    let s_root = root_team(&s_engine).await;
    let s_sup = ResidentSupervisor::start(s_engine.clone());
    ensure_main(&s_sup, &s_engine, s_root).await;
    let _lead = s_engine.try_begin_turn(s_root).unwrap();
    let (sc, sh) = spawn_idle(&s_sup, &s_engine, s_root, "work").await;
    summary.release.notify_one();
    submit_and_wait(&s_engine, &s_sup, s_root, &sh, "FIRST REPORT").await;
    let orig = handoff_projection(&s_engine.read_projection(sc).await.unwrap()).unwrap();
    assert!(!orig.degraded);
    send_mail(&s_engine, s_root, &sh, "next task").await;
    s_sup.end_member(s_root, &sh).await.unwrap();
    let stopped = handoff_projection(&s_engine.read_projection(sc).await.unwrap()).unwrap();
    assert!(
        stopped.degraded && stopped.doc != orig.doc && stopped.generation == orig.generation + 1
    );
    for needle in [
        "1. Goal - [mail from main] next task",
        "3. Files and code - (from generation 1) none",
        "4. Decisions - (from generation 1) none",
        "5. Pending tasks - [mail from main] next task",
    ] {
        assert!(
            stopped.doc.contains(needle),
            "missing {needle}: {}",
            stopped.doc
        );
    }
}

#[tokio::test]
async fn archive_idle_member_actionable_errors_and_user_steer_lifecycle() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (child, handle) = spawn_idle(&sup, &engine, root, "work").await;
    let leaf = handle.rsplit('/').next().unwrap().to_string();

    assert!(
        sup.archive_member(root, "main", "x")
            .await
            .unwrap_err()
            .to_string()
            .contains("team lead")
    );
    assert!(
        sup.archive_member(root, &root.to_string(), "x")
            .await
            .unwrap_err()
            .to_string()
            .contains("team lead")
    );
    let unk = sup
        .archive_member(root, "nobody-7", "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(unk.contains("nobody-7") && unk.contains(&handle));

    let receipt = sup
        .archive_member(root, &child.to_string(), "done for now")
        .await
        .unwrap();
    assert_eq!(
        (receipt.handle.as_str(), receipt.cancelled_turn),
        (handle.as_str(), false)
    );
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(
        !team.roster.contains_key(&handle)
            && team.archived[&handle].reason == ArchiveReason::ArchivedByParent
    );
    assert!(team.inboxes.get("main").is_none_or(|i| {
        i.iter()
            .all(|idx| !team.mail[*idx].body.contains("done for now"))
    }));
    assert!(
        handoff_projection(&engine.read_projection(child).await.unwrap())
            .as_ref()
            .is_some_and(|h| h.degraded)
    );
    let again = sup
        .archive_member(root, &handle, "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(again.contains("already archived") && again.contains("mail"));

    // User steer revives archived subagent under same session; nested steer routes via parent.
    engine
        .steer_subagent(child, "one more thing")
        .await
        .unwrap();
    assert_eq!(
        team_projection(&engine.read_projection(root).await.unwrap()).roster[&handle].session,
        child
    );
    wait_idle(&engine, root, &handle, child).await;
    assert!(transcript_has(&engine, child, "[user] one more thing").await);

    let (worker, worker_h) = spawn_idle(&sup, &engine, child, "sub").await;
    assert_eq!(
        engine
            .steer_subagent(worker, "  focus on the parser  ")
            .await
            .unwrap()
            .from,
        handle
    );
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&engine, worker).await >= 2
        })
        .await
    );
    wait_idle(&engine, root, &worker_h, worker).await;
    assert!(transcript_has(&engine, worker, "[user] focus on the parser").await);
    assert_eq!(
        engine.steer_subagent(child, "wrap up").await.unwrap().from,
        "main"
    );
    assert!(
        engine
            .steer_subagent(worker, "   ")
            .await
            .unwrap_err()
            .to_string()
            .contains("needs text")
    );
    assert!(
        engine
            .steer_subagent(root, "hi")
            .await
            .unwrap_err()
            .to_string()
            .contains("not a subagent")
    );
    sup.archive_member(root, &worker_h, "x").await.unwrap();
    sup.archive_member(root, &leaf, "x").await.unwrap();
}

#[tokio::test]
async fn root_teardown_and_delete_session_cancel_busy_and_archive_descendants() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (lead, lead_h) = spawn_idle(&sup, &engine, root, "lead").await;
    let (worker, worker_h) = spawn_idle(&sup, &engine, lead, "work").await;
    engine.force_archive_team(root).await.unwrap();
    let proj = engine.read_projection(root).await.unwrap();
    let team = team_projection(&proj);
    assert!(!team.roster.keys().any(|p| p != "main"));
    assert_eq!(team.archived[&lead_h].reason, ArchiveReason::RootTeardown);
    assert_eq!(team.archived[&worker_h].reason, ArchiveReason::RootTeardown);
    assert!(!is_active(&engine, lead).await && !is_active(&engine, worker).await);

    let provider = Arc::new(ParkedMembers::default());
    let d_engine =
        Arc::new(support::engine_with_provider(provider.clone(), PermissionRules::default()).await);
    let d_root = root_team(&d_engine).await;
    *provider.root.lock().unwrap() = Some(d_root);
    let d_sup = ResidentSupervisor::start(d_engine.clone());
    ensure_main(&d_sup, &d_engine, d_root).await;
    let _lead_turn = d_engine.try_begin_turn(d_root).unwrap();
    let agent = support::agent_spec("hya-scout", "fake", empty_workdir());
    let binding = d_engine.bind_runtime(&agent.workdir).unwrap();
    let (busy, _) = d_sup
        .spawn_resident(
            d_root,
            agent,
            (
                binding.clone(),
                Arc::from([]),
                binding.agent_resource_policy("hya-scout").unwrap(),
                None,
            ),
            "work".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            d_engine.turn_active(busy)
        })
        .await
    );
    assert!(is_active(&d_engine, busy).await);
    tokio::time::timeout(Duration::from_secs(2), d_engine.delete_session(d_root))
        .await
        .unwrap()
        .unwrap();
    assert!(!d_engine.turn_active(busy) && !is_active(&d_engine, busy).await);
}

#[tokio::test]
async fn steer_tool_result_injection_non_object_shapes_and_transactional_rollback() {
    struct OutputTool(Value);
    #[async_trait]
    impl Tool for OutputTool {
        fn name(&self) -> &str {
            "ls"
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("ls"),
                description: "scripted".into(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
            }
        }
        async fn execute(&self, _: &ToolCtx, _: Value) -> Result<Value, ToolError> {
            Ok(self.0.clone())
        }
    }
    struct OneToolProvider(AtomicBool);
    #[async_trait]
    impl Provider for OneToolProvider {
        fn id(&self) -> &str {
            "fake"
        }
        fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
            fake_caps(m)
        }
        async fn stream(
            &self,
            _: CompletionRequest,
            s: SessionId,
            m: MessageId,
        ) -> Result<EventStream, ProviderError> {
            let steps = if !self.0.swap(true, Ordering::SeqCst) {
                vec![
                    FakeStep::ToolCall {
                        name: "ls".into(),
                        input: json!({"path": "."}),
                    },
                    FakeStep::Finish(FinishReason::ToolCalls),
                ]
            } else {
                vec![
                    FakeStep::Text("done".into()),
                    FakeStep::Finish(FinishReason::Stop),
                ]
            };
            Ok(stream_steps(&steps, s, m))
        }
    }

    for (output, reject) in [
        (json!({"output": "ok"}), None),
        (json!("ok"), None),
        (json!([1, 2]), None),
        (Value::Null, None),
        (json!({"output": "ok"}), Some("tool_result")),
        (json!({"output": "ok"}), Some("mail_consumed")),
    ] {
        let tools = hya_tool_loader::builtins();
        tools.remove("ls");
        tools
            .register(Arc::new(OutputTool(output.clone())))
            .unwrap();
        let engine = Arc::new(
            support::engine_with_router_and_runtime(
                Arc::new(
                    ProviderRouter::new().with(Arc::new(OneToolProvider(AtomicBool::new(false)))),
                ),
                support::test_runtime(Arc::new(tools)),
                PermissionRules::new(vec![Rule::new(Action::Read, "*", Mode::Allow)]),
            )
            .await,
        );
        let root = root_team(&engine).await;
        let sup = ResidentSupervisor::start(engine.clone());
        ensure_main(&sup, &engine, root).await;
        mail_main_from_peer(&engine, root, "steer payload for main").await;

        if let Some(tag) = reject {
            let mut tx = engine.store().begin_transaction().await.unwrap();
            sqlx::query(&format!(
                "CREATE TRIGGER reject_steer BEFORE INSERT ON event_log \
                 WHEN instr(NEW.payload, '{tag}') > 0 BEGIN SELECT RAISE(ABORT, 'reject steer result'); END"
            ))
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        let agent = support::agent_spec("hya-main", "fake", empty_workdir());
        engine
            .admit_user_prompt(root, "list then stop".into())
            .await
            .unwrap();
        let res = engine
            .run_turn_with_external_dirs_and_guidance(
                root,
                &agent,
                CancellationToken::new(),
                &[],
                None,
                None,
                None,
            )
            .await;
        let replay = engine.replay(root).await.unwrap();
        if reject.is_some() {
            assert!(res.is_err());
            assert!(!replay.iter().any(|e| {
                matches!(e.event, Event::ToolResult { .. })
                    || matches!(
                        ExtensionEvent::from_event(&e.event),
                        Some(ExtensionEvent::MailConsumed { .. })
                    )
            }));
            assert_eq!(
                team_projection(&engine.read_projection(root).await.unwrap()).roster["main"]
                    .resident_cursor,
                0
            );
        } else {
            res.unwrap();
            assert!(
                team_projection(&engine.read_projection(root).await.unwrap()).roster["main"]
                    .resident_cursor
                    >= 1
            );
            assert!(replay.iter().any(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::MailConsumed { .. })
            )));
            let delivered = replay
                .iter()
                .find_map(|e| match &e.event {
                    Event::ToolResult { output, .. } => Some(output),
                    _ => None,
                })
                .unwrap();
            if !output.is_object() {
                assert_eq!(delivered["result"], output);
            }
            assert!(delivered.to_string().contains("steer payload for main"));
        }
    }
}

#[tokio::test]
async fn steer_notice_multibyte_channel_id_and_harness_notice() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let _lead = engine.try_begin_turn(root).unwrap();
    mail_main_from_peer(&engine, root, &"配置很长的中文邮件内容".repeat(120)).await;
    let notice = support::steer_notice(&engine, root).await.unwrap();
    assert!(notice.contains("[mail from main/peer-1 @DM-12345678]") && notice.contains("配置很长"));

    let (child, handle) = spawn_idle(&sup, &engine, root, "hya-scout").await;
    let dm = dm_for(&engine, root, &handle).await;
    send_mail(&engine, child, "main", "handle-addressed status update").await;
    let n2 = support::steer_notice(&engine, root).await.unwrap();
    assert!(n2.contains(&format!("@{dm}")) && n2.contains("handle-addressed status update"));

    engine
        .store()
        .append_harness_mail(root, &handle, "notice from harness".into())
        .await
        .unwrap();
    let h_notice = support::steer_notice(&engine, child).await.unwrap();
    assert!(
        h_notice.contains("[mail from harness] notice from harness") && !h_notice.contains("@DM-")
    );
}

#[tokio::test]
async fn channel_read_history_normalization_handle_dm_and_heartbeats_status() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (child, handle) = spawn_idle(&sup, &engine, root, "hya-scout").await;

    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            team_projection(&engine.read_projection(root).await.unwrap()).roster[&handle]
                .heartbeat_ms
                > 0
        })
        .await
    );
    assert_eq!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .filter(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::AgentHeartbeat { handle: m, .. }) if m == handle))
            .count(),
        2
    );
    let rows = AgentChannelsService::team_status(engine.host_runtime().as_ref(), root)
        .await
        .unwrap();
    assert_eq!(
        (rows.len(), rows[0].handle.as_str(), rows[0].status.as_str()),
        (1, handle.as_str(), "idle")
    );
    assert!(
        rows[0].last_active_seconds.is_some()
            && AgentChannelsService::team_status(engine.host_runtime().as_ref(), child)
                .await
                .unwrap()
                .is_empty()
    );

    for body in ["first note", "second note"] {
        engine.mail_announce(root, body.into()).await.unwrap();
    }
    let proj = engine.read_projection(root).await.unwrap();
    let group = team_projection(&proj)
        .channels
        .keys()
        .find(|k| k.starts_with("announce-"))
        .unwrap()
        .clone();
    let dm = dm_for(&engine, root, &handle).await;

    let latest = support::read_channel(&engine, child, &group, None)
        .await
        .unwrap();
    assert_eq!(
        (
            latest.messages.len(),
            latest.messages[0].1.as_str(),
            latest.warning.as_deref()
        ),
        (1, "second note", None)
    );
    let hist = support::read_channel(&engine, child, &group, Some(10))
        .await
        .unwrap();
    assert_eq!(
        (
            hist.messages.len(),
            hist.messages[0].1.as_str(),
            hist.messages[1].1.as_str()
        ),
        (2, "second note", "first note")
    );
    let p = engine.read_projection(root).await.unwrap();
    let team_p = team_projection(&p);
    assert!(team_p.roster[&handle].resident_cursor >= team_p.inboxes[&handle].len() as u64);
    assert!(engine.replay(root).await.unwrap().iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::MailConsumed { .. })
    )));

    for (raw, want) in [
        (
            format!("##{group}"),
            format!("normalized channel id `##{group}` → `{group}`"),
        ),
        (
            format!("  {group}  "),
            format!("normalized channel id `  {group}  ` → `{group}`"),
        ),
    ] {
        let r = support::read_channel(&engine, child, &raw, None)
            .await
            .unwrap();
        assert_eq!(
            (r.channel.as_str(), r.warning.as_deref()),
            (group.as_str(), Some(want.as_str()))
        );
    }
    assert!(
        support::read_channel(&engine, child, "no-such-channel", None)
            .await
            .is_err()
    );

    for spelling in [
        format!("#{handle}"),
        handle.clone(),
        scope::leaf(&handle).into(),
    ] {
        let r = support::read_channel(&engine, root, &spelling, None)
            .await
            .unwrap();
        let w = r.warning.unwrap_or_default();
        assert_eq!(r.channel, dm);
        assert!(w.contains("member handle") && w.contains(&dm));
    }
    assert_eq!(
        support::read_channel(&engine, root, &dm, None)
            .await
            .unwrap()
            .warning,
        None
    );
    let err = support::read_channel(&engine, root, "#main/nobody-here", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown channel `#main/nobody-here`")
            && err.contains(&dm)
            && err.contains("list_channel")
    );
}

#[tokio::test]
async fn channel_policy_denials_for_steer_report_public_send_and_unit_mail() {
    // 1) Steer & report denials.
    let engine = restricted_engine("  - { id: dm, kind: parent_dm, participants: [{kind: role, role: parent}], capabilities: [], scope: vertical, retention: team_session }").await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let _lead = engine.try_begin_turn(root).unwrap();
    let (child, _) = spawn_idle(&sup, &engine, root, "hya-scout").await;
    send_mail(&engine, child, "main", "must stay hidden").await;
    assert!(support::steer_notice(&engine, root).await.is_none());
    assert_eq!(
        team_projection(&engine.read_projection(root).await.unwrap()).roster["main"]
            .resident_cursor,
        0
    );
    let (plane, rx) = LifecyclePlane::new();
    tokio::spawn(run_lifecycle_service(
        engine,
        sup,
        rx,
        CancellationToken::new(),
    ));
    assert!(matches!(
        plane.for_session(child).with_channel_policy(ChannelPolicySnapshot::default()).report(ReportOutcome::Done, "blocked".into()).await,
        Err(ToolError::Input(m)) if m.contains("channel policy denies report")
    ));

    // 2) Public mail_send enforces captured policy on both live & revival sends.
    let dm_deny = restricted_engine("  - { id: dm, kind: parent_dm, participants: [{kind: role, role: parent}], capabilities: [], scope: vertical, retention: team_session }").await;
    let dm_root = root_team(&dm_deny).await;
    let dm_sup = ResidentSupervisor::start(dm_deny.clone());
    ensure_main(&dm_sup, &dm_deny, dm_root).await;
    let (_, dm_h) = spawn_idle(&dm_sup, &dm_deny, dm_root, "hya-scout").await;
    assert!(matches!(
        dm_deny.mail_send(dm_root, MailEndpoint::Handle(dm_h.clone()), MailKind::Message, "blocked".into()).await,
        Err(CoreError::MailAuthorizationRejected { sender, channel, .. }) if sender == "main" && !channel.is_empty()
    ));
    submit_and_wait(&dm_deny, &dm_sup, dm_root, &dm_h, "done").await;
    assert!(matches!(
        dm_deny
            .mail_send(
                dm_root,
                MailEndpoint::Handle(dm_h.clone()),
                MailKind::Message,
                "revive".into()
            )
            .await,
        Err(CoreError::MailAuthorizationRejected { .. })
    ));
    assert!(
        team_projection(&dm_deny.read_projection(dm_root).await.unwrap())
            .archived
            .contains_key(&dm_h)
    );

    // 3) Unit resident_mail denial (single + batched) and follow_up denial advance cursor with 0 wakes.
    for (caps, batched) in [
        ("[send, report, steer, follow_up]", true),
        ("[send, report, steer, resident_mail]", false),
    ] {
        let u = restricted_engine(&format!("  - {{ id: dm, kind: unit, participants: [{{kind: role, role: direct_reports}}], capabilities: {caps}, scope: unit, retention: team_session }}")).await;
        let ur = root_team(&u).await;
        let us = ResidentSupervisor::start(u.clone());
        ensure_main(&us, &u, ur).await;
        let (uc, uh) = spawn_idle(&us, &u, ur, "initial").await;
        let before = u.replay(uc).await.unwrap();
        u.mail_announce(ur, "denied".into()).await.unwrap();
        if batched {
            let group = team_projection(&u.read_projection(ur).await.unwrap())
                .channels
                .iter()
                .find(|(_, c)| c.kind == ChannelKind::Group)
                .unwrap()
                .0
                .clone();
            let batch = u
                .store()
                .append_events(
                    ur,
                    ["first denied", "second denied"]
                        .into_iter()
                        .map(|b| {
                            ExtensionEvent::MailSent {
                                session: ur,
                                from: "main".into(),
                                to: MailEndpoint::Channel(group.clone()),
                                kind: MailKind::Message,
                                body: b.into(),
                            }
                            .into_event()
                        })
                        .collect(),
                )
                .await
                .unwrap();
            for env in batch {
                u.bus().publish(env);
            }
        }
        let want_len = if batched { 4 } else { 2 };
        assert!(
            support::poll_until(Duration::from_secs(5), || async {
                let p = u.read_projection(ur).await.unwrap();
                let team = team_projection(&p);
                team.roster[&uh].resident_cursor == want_len as u64
                    && team.inboxes[&uh].len() == want_len
            })
            .await
        );
        assert_eq!(u.replay(uc).await.unwrap(), before);
        assert_eq!(assistant_turns(&u, uc).await, 1);
    }
}

#[tokio::test]
async fn resident_member_row_lifecycle_cancel_failed_turn_and_failed_shutdown_rollback() {
    let engine = stub_engine().await;
    let root = root_team(&engine).await;
    let sup = ResidentSupervisor::start(engine.clone());
    ensure_main(&sup, &engine, root).await;
    let (child, handle) = spawn_idle(&sup, &engine, root, "explore").await;
    let row = member_row(&engine, root, child).await;
    assert_eq!(row.status, MemberRunStatus::Running);
    async fn running_count(e: &SessionEngine, root: SessionId, m: MemberId) -> usize {
        e.replay(root).await.unwrap().iter().filter(|ev| matches!(ExtensionEvent::from_event(&ev.event), Some(ExtensionEvent::MemberStatusChanged { member, status: MemberRunStatus::Running, .. }) if member == m)).count()
    }
    assert_eq!(running_count(&engine, root, row.member).await, 1);

    send_mail(&engine, root, &handle, "one more thing").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            assistant_turns(&engine, child).await >= 2
        })
        .await
    );
    wait_idle(&engine, root, &handle, child).await;
    assert_eq!(running_count(&engine, root, row.member).await, 1);

    submit_and_wait(&engine, &sup, root, &handle, "found it").await;
    let done = member_row(&engine, root, child).await;
    assert_eq!(
        (done.status, done.summary.as_str()),
        (MemberRunStatus::Done, "found it")
    );

    send_mail(&engine, root, &handle, "continue").await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            member_row(&engine, root, child).await.status == MemberRunStatus::Running
        })
        .await
    );
    assert_eq!(running_count(&engine, root, row.member).await, 2);

    sup.end_member(root, &handle).await.unwrap();
    sup.end_member(root, &handle).await.unwrap();
    assert_eq!(
        member_row(&engine, root, child).await.status,
        MemberRunStatus::Cancelled
    );
    assert_eq!(engine.replay(root).await.unwrap().iter().filter(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::MemberFinished { member, .. }) if member == row.member)).count(), 1);

    // Failing resident turn marks member row Failed & emits SubagentReported(Failed).
    let store = SessionStore::connect_memory().await.unwrap();
    let root_id = {
        let (perm, _) = PermissionPlane::new(PermissionRules::default());
        let tmp = support::fixture_engine(
            store.clone(),
            Arc::new(ProviderRouter::new().with(Arc::new(EmptyStub))),
            support::builtin_test_runtime(),
            perm,
            EventBus::default(),
        )
        .await;
        root_team(&tmp).await
    };
    let (perm, _) = PermissionPlane::new(PermissionRules::default());
    let f_engine = Arc::new(
        support::fixture_engine(
            store,
            Arc::new(ProviderRouter::new().with(Arc::new(FailingMembers(root_id)))),
            support::builtin_test_runtime(),
            perm,
            EventBus::default(),
        )
        .await
        .with_round_retry_waits(&support::ZERO_RETRY_WAITS),
    );
    let f_sup = ResidentSupervisor::start(f_engine.clone());
    ensure_main(&f_sup, &f_engine, root_id).await;
    let agent = support::agent_spec("hya-scout", "fake", empty_workdir());
    let binding = f_engine.bind_runtime(&agent.workdir).unwrap();
    let (fc, _) = f_sup
        .spawn_resident(
            root_id,
            agent,
            (
                binding.clone(),
                Arc::from([]),
                binding.agent_resource_policy("hya-scout").unwrap(),
                None,
            ),
            "impossible".into(),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            member_row(&f_engine, root_id, fc).await.status == MemberRunStatus::Failed
        })
        .await
    );
    assert!(f_engine.replay(root_id).await.unwrap().iter().any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::SubagentReported { outcome: ReportOutcome::Failed, child, .. }) if child == fc)));

    // Failed shutdown archive rolls back all terminal effects without a second write path.
    let dir = support::TestDir::new("failed-terminal-transition");
    let path = dir.path().join("session.db");
    let file_store = SessionStore::connect_runtime(path.to_str().unwrap())
        .await
        .unwrap();
    file_store.claim_runtime_owner(OwnerRunId::new()).unwrap();
    let faults = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    let (perm2, _) = PermissionPlane::new(PermissionRules::default());
    let t_engine = Arc::new(
        support::fixture_engine(
            file_store,
            Arc::new(ProviderRouter::new().with(Arc::new(EmptyStub))),
            support::builtin_test_runtime(),
            perm2,
            EventBus::default(),
        )
        .await
        .with_round_retry_waits(&support::ZERO_RETRY_WAITS),
    );
    let t_root = root_team(&t_engine).await;
    let t_sup = ResidentSupervisor::start(t_engine.clone());
    ensure_main(&t_sup, &t_engine, t_root).await;
    let (tc, th) = spawn_idle(&t_sup, &t_engine, t_root, "work").await;
    sqlx::query("CREATE TRIGGER reject_handoff BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.kind') = 'handoff_committed' BEGIN SELECT RAISE(ABORT, 'injected terminal failure'); END")
        .execute(&faults)
        .await
        .unwrap();
    t_sup
        .drain(FinishCause::Shutdown, Duration::from_millis(100))
        .await;
    assert_eq!(
        member_row(&t_engine, t_root, tc).await.status,
        MemberRunStatus::Running
    );
    let tp = t_engine.read_projection(t_root).await.unwrap();
    let tp_team = team_projection(&tp);
    assert!(
        tp_team.roster.contains_key(&th)
            && !tp_team.archived.contains_key(&th)
            && is_active(&t_engine, tc).await
    );
    sqlx::query("DROP TRIGGER reject_handoff")
        .execute(&faults)
        .await
        .unwrap();
    t_sup.end_member(t_root, &th).await.unwrap();
    faults.close().await;
}

#[tokio::test]
async fn handoff_policy_deterministic_skips_summarizer_while_default_invokes_it() {
    for (field, want_calls, want_degraded, want_usage) in [
        (", handoff: deterministic", 0usize, true, 0usize),
        ("", 1, false, 1),
    ] {
        let prepared = prepare_package(BundleSource::new(
            "handoff-policy",
            vec![SourceFile::new(
                "bundle.yaml",
                format!("schema_version: 2\nkind: Bundle\nidentity: {{ id: acme/handoff-policy, version: 1.0.0, publisher: acme }}\nagents: [{{ id: quick-scout, role: subagent{field} }}]\n"),
            )],
        ))
        .unwrap();
        let catalog = Arc::new(
            AgentCatalog::new(Arc::new(
                BundleCatalog::from_prepared(prepared.bundles()).unwrap(),
            ))
            .unwrap(),
        );
        let summary = Arc::new(CountingHandoff::default());
        let engine = Arc::new(
            support::engine_with_router_and_runtime(
                Arc::new(ProviderRouter::new().with(Arc::new(EmptyStub))),
                support::builtin_test_runtime_with_catalog(catalog),
                PermissionRules::default(),
            )
            .await
            .with_terminal_handoff(summary.clone(), TerminalHandoffOptions::default()),
        );
        let root = root_team(&engine).await;
        let sup = ResidentSupervisor::start(engine.clone());
        ensure_main(&sup, &engine, root).await;
        let agent = support::agent_spec("quick-scout", "fake", empty_workdir());
        let binding = engine.bind_runtime(&agent.workdir).unwrap();
        let (child, handle) = sup
            .spawn_resident(
                root,
                agent,
                (
                    binding.clone(),
                    Arc::from([]),
                    binding.agent_resource_policy("quick-scout").unwrap(),
                    None,
                ),
                "inspect".into(),
                None,
                None,
            )
            .await
            .unwrap();
        wait_idle(&engine, root, &handle, child).await;
        submit_and_wait(&engine, &sup, root, &handle, "tree inspected").await;

        assert_eq!(summary.0.load(Ordering::SeqCst), want_calls);
        let (rlog, clog) = (
            engine.replay(root).await.unwrap(),
            engine.replay(child).await.unwrap(),
        );
        assert_eq!(
            clog.iter()
                .find_map(|e| match ExtensionEvent::from_event(&e.event) {
                    Some(ExtensionEvent::HandoffCommitted { degraded, .. }) => Some(degraded),
                    _ => None,
                }),
            Some(want_degraded)
        );
        assert_eq!(
            clog.iter()
                .chain(&rlog)
                .filter(|e| matches!(
                    e.event,
                    Event::UsageRecorded {
                        purpose: UsagePurpose::Compaction,
                        ..
                    }
                ))
                .count(),
            want_usage
        );
        assert!(rlog.iter().any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::SubagentReported { report, .. }) if report.contains("tree inspected"))));
        assert!(rlog.iter().any(
            |e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::MailSent { body, .. }) if body.contains("tree inspected"))
        ));
        assert!(rlog.iter().any(|e| matches!(
            ExtensionEvent::from_event(&e.event),
            Some(ExtensionEvent::AgentArchived {
                reason: ArchiveReason::Reported,
                ..
            })
        )));
    }
}

// ---- Section B: Accepted report ends turn & final-answer auto-reporting ----

struct ReportTurnProvider {
    root: Mutex<Option<SessionId>>,
    rounds: Mutex<HashMap<SessionId, usize>>,
    script: Box<dyn Fn(usize) -> Vec<FakeStep> + Send + Sync>,
    engine: OnceLock<Weak<SessionEngine>>,
    mail_in_first_round: AtomicBool,
}
impl ReportTurnProvider {
    fn new(script: impl Fn(usize) -> Vec<FakeStep> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            root: Mutex::new(None),
            rounds: Mutex::new(HashMap::new()),
            script: Box::new(script),
            engine: OnceLock::new(),
            mail_in_first_round: AtomicBool::new(false),
        })
    }
    fn rounds(&self, s: SessionId) -> usize {
        self.rounds.lock().unwrap().get(&s).copied().unwrap_or(0)
    }
}
#[async_trait]
impl Provider for ReportTurnProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let steps = if *self.root.lock().unwrap() == Some(s) {
            vec![
                FakeStep::Text("lead done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ]
        } else {
            let round = {
                let mut map = self.rounds.lock().unwrap();
                let c = map.entry(s).or_default();
                *c += 1;
                *c - 1
            };
            if round == 0 && self.mail_in_first_round.load(Ordering::SeqCst) {
                let eng = self.engine.get().and_then(Weak::upgrade).unwrap();
                let root = self.root.lock().unwrap().unwrap();
                let handle = team_projection(&eng.read_projection(root).await.unwrap())
                    .roster
                    .values()
                    .find(|e| e.session == s)
                    .unwrap()
                    .handle
                    .clone();
                send_mail(&eng, root, &handle, "also check the tests").await;
            }
            (self.script)(round)
        };
        Ok(stream_steps(&steps, s, m))
    }
}

struct ReportTurnTeam {
    engine: Arc<SessionEngine>,
    sup: Arc<ResidentSupervisor>,
    root: SessionId,
    binding: TurnBinding,
}

async fn report_turn_team(provider: Arc<ReportTurnProvider>) -> ReportTurnTeam {
    const BUNDLE: &str = "schema_version: 2\nkind: Bundle\nidentity: { id: acme/report-turn, version: 1.0.0, publisher: acme }\nagents:\n  - id: lead\n    role: main\n    can_spawn: [scout]\n    resource_view: { allow: [harness:tool/grep] }\n  - id: scout\n    role: subagent\n    resource_view: { allow: [harness:tool/grep, harness:tool/glob] }\n";
    let prepared = prepare_package(BundleSource::new(
        "report-turn",
        vec![SourceFile::new("bundle.yaml", BUNDLE)],
    ))
    .unwrap();
    let multi_agent = hya_bundle::first_party_bundle("hya/multi-agent").unwrap();
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(
            BundleCatalog::from_verified_catalogs(&[&prepared, multi_agent]).unwrap(),
        ))
        .unwrap(),
    );
    let (lifecycle, l_rx) = LifecyclePlane::new();
    let engine = Arc::new(
        support::engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(provider.clone())),
            support::builtin_test_runtime_with_catalog(catalog),
            PermissionRules::new(vec![Rule::new(Action::Read, "*", Mode::Allow)]),
        )
        .await
        .with_lifecycle(lifecycle)
        .with_governor(SubagentGovernor::new(SubagentLimits::default())),
    );
    let sup = ResidentSupervisor::start(engine.clone());
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        sup.clone(),
        l_rx,
        CancellationToken::new(),
    ));
    let root = support::create_root_session(&engine, "lead", "fake", empty_workdir()).await;
    *provider.root.lock().unwrap() = Some(root);
    provider.engine.set(Arc::downgrade(&engine)).unwrap();
    let binding = engine.bind_runtime(empty_workdir()).unwrap();
    let lead_spec = support::agent_spec("lead", "fake", empty_workdir());
    sup.ensure_main(
        root,
        lead_spec.clone(),
        (
            binding.clone(),
            Arc::from([]),
            binding.agent_resource_policy("lead").unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    engine
        .admit_user_prompt(root, "coordinate".into())
        .await
        .unwrap();
    engine
        .run_turn_with_external_dirs_and_guidance(
            root,
            &lead_spec,
            CancellationToken::new(),
            &[],
            None,
            None,
            None,
        )
        .await
        .unwrap();
    ReportTurnTeam {
        engine,
        sup,
        root,
        binding,
    }
}

async fn spawn_scout(t: &ReportTurnTeam) -> (SessionId, String) {
    t.sup
        .spawn_resident(
            t.root,
            support::agent_spec("scout", "fake", empty_workdir()),
            (
                t.binding.clone(),
                Arc::from([]),
                t.binding.agent_resource_policy("scout").unwrap(),
                None,
            ),
            "find the marker".into(),
            None,
            None,
        )
        .await
        .unwrap()
}

async fn reported_bodies(e: &SessionEngine, root: SessionId) -> Vec<String> {
    e.replay(root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|ev| match ExtensionEvent::from_event(&ev.event) {
            Some(ExtensionEvent::SubagentReported { report, .. }) => Some(report),
            _ => None,
        })
        .collect()
}

async fn wait_reported(t: &ReportTurnTeam, child: SessionId, handle: &str, count: usize) {
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            team_projection(&t.engine.read_projection(t.root).await.unwrap())
                .archived
                .get(handle)
                .is_some_and(|e| e.reason == ArchiveReason::Reported)
                && reported_bodies(&t.engine, t.root).await.len() >= count
                && !t.engine.turn_active(child)
        })
        .await
    );
    tokio::task::yield_now().await;
}

fn report_call(result: &str) -> FakeStep {
    FakeStep::ToolCall {
        name: "report".into(),
        input: json!({"result": result}),
    }
}

fn answer_steps(text: &str, finish: FinishReason) -> Vec<FakeStep> {
    let mut s = Vec::new();
    if !text.is_empty() {
        s.push(FakeStep::Text(text.into()));
    }
    s.push(FakeStep::Finish(finish));
    s
}

#[tokio::test]
async fn accepted_report_ends_turn_after_batch_rejects_duplicate_and_revives() {
    let provider = ReportTurnProvider::new(|round| {
        vec![
            report_call(&format!("report of episode {round}")),
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let team = report_turn_team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, child, &handle, 1).await;
    assert_eq!(provider.rounds(child), 1);
    let results: Vec<_> = team
        .engine
        .replay(child)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ToolResult { output, .. } => Some(output.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1);
    assert!(results[0].contains("Report submitted; it takes effect when this turn ends."));
    assert!(results[0].contains("you will receive a harness notice and must report again"));

    send_mail(&team.engine, team.root, &handle, "follow-up").await;
    wait_reported(&team, child, &handle, 2).await;
    assert_eq!(provider.rounds(child), 2);
    assert_eq!(
        reported_bodies(&team.engine, team.root).await,
        ["report of episode 0", "report of episode 1"]
    );

    let batch_provider = ReportTurnProvider::new(|_| {
        vec![
            FakeStep::ToolCall {
                name: "grep".into(),
                input: json!({"pattern": "zzz_no_such_marker_zzz", "path": "."}),
            },
            report_call("first report"),
            report_call("second report"),
            FakeStep::ToolCall {
                name: "glob".into(),
                input: json!({"pattern": "*.toml"}),
            },
            FakeStep::Finish(FinishReason::ToolCalls),
        ]
    });
    let bt = report_turn_team(batch_provider.clone()).await;
    let (bc, bh) = spawn_scout(&bt).await;
    wait_reported(&bt, bc, &bh, 1).await;
    assert_eq!(batch_provider.rounds(bc), 1);
    let blog = bt.engine.replay(bc).await.unwrap();
    let res_n = blog
        .iter()
        .filter(|e| matches!(e.event, Event::ToolResult { .. }))
        .count();
    let errs: Vec<_> = blog
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolError { message_text, .. } => Some(message_text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(res_n + errs.len(), 4);
    assert!(
        errs.iter()
            .any(|e| e.contains("already accepted") && e.contains("do not call `report` again"))
    );
    assert_eq!(reported_bodies(&bt.engine, bt.root).await, ["first report"]);
}

#[tokio::test]
async fn final_answer_delivers_report_and_revives_while_unread_mail_blocks_until_handled() {
    let provider = ReportTurnProvider::new(|r| {
        answer_steps(&format!("answer of episode {r}"), FinishReason::Stop)
    });
    let team = report_turn_team(provider.clone()).await;
    let (child, handle) = spawn_scout(&team).await;
    wait_reported(&team, child, &handle, 1).await;
    assert_eq!(provider.rounds(child), 1);
    assert!(team.engine.replay(team.root).await.unwrap().iter().any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::SubagentReported { child: r, outcome: ReportOutcome::Done, .. }) if r == child)));
    send_mail(&team.engine, team.root, &handle, "follow-up").await;
    wait_reported(&team, child, &handle, 2).await;
    assert_eq!(provider.rounds(child), 2);
    assert_eq!(
        reported_bodies(&team.engine, team.root).await,
        ["answer of episode 0", "answer of episode 1"]
    );

    let mp = ReportTurnProvider::new(|r| {
        answer_steps(&format!("answer of round {r}"), FinishReason::Stop)
    });
    mp.mail_in_first_round.store(true, Ordering::SeqCst);
    let mt = report_turn_team(mp.clone()).await;
    let (mc, mh) = spawn_scout(&mt).await;
    wait_reported(&mt, mc, &mh, 1).await;
    assert_eq!(mp.rounds(mc), 2);
    assert_eq!(
        reported_bodies(&mt.engine, mt.root).await,
        ["answer of round 1"]
    );
}

#[tokio::test]
async fn empty_or_length_cut_final_answer_leaves_member_idle_unreported() {
    for (text, finish) in [
        ("", FinishReason::Stop),
        ("the marker is in", FinishReason::Length),
    ] {
        let team =
            report_turn_team(ReportTurnProvider::new(move |_| answer_steps(text, finish))).await;
        let (child, handle) = spawn_scout(&team).await;
        assert!(
            support::poll_until(Duration::from_secs(5), || async {
                team.engine.replay(child).await.unwrap().iter().any(|e| {
                    matches!(
                        e.event,
                        Event::MessageFinished {
                            role: Role::Assistant,
                            ..
                        }
                    )
                }) && !team.engine.turn_active(child)
                    && team_projection(&team.engine.read_projection(team.root).await.unwrap())
                        .roster
                        .get(&handle)
                        .is_some_and(|e| e.status == RosterStatus::Idle)
            })
            .await
        );
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(
            team_projection(&team.engine.read_projection(team.root).await.unwrap())
                .roster
                .contains_key(&handle)
        );
        assert!(reported_bodies(&team.engine, team.root).await.is_empty());
        let finishes: Vec<_> = team
            .engine
            .replay(child)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                Event::MessageFinished {
                    role: Role::Assistant,
                    finish,
                    ..
                } => Some(finish),
                _ => None,
            })
            .collect();
        assert_eq!(finishes, vec![finish]);
    }
}

// ---- Section C: `wait` lifecycle (zero wall-clock sleeps) ----

struct GateProvider {
    root: Mutex<Option<SessionId>>,
    gate_members: AtomicBool,
    gates: Mutex<HashMap<SessionId, Arc<Notify>>>,
    tool_batches: Mutex<HashMap<SessionId, Vec<FakeStep>>>,
    answer: Mutex<String>,
}
impl Default for GateProvider {
    fn default() -> Self {
        Self {
            root: Default::default(),
            gate_members: Default::default(),
            gates: Default::default(),
            tool_batches: Default::default(),
            answer: Mutex::new("done with the unit".into()),
        }
    }
}
impl GateProvider {
    fn gate(&self, s: SessionId) -> Arc<Notify> {
        self.gates.lock().unwrap().entry(s).or_default().clone()
    }
    fn release(&self, s: SessionId) {
        self.gate(s).notify_one();
    }
    fn release_all(&self) {
        for g in self.gates.lock().unwrap().values() {
            g.notify_waiters();
        }
    }
}
#[async_trait]
impl Provider for GateProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn capabilities(&self, m: &ModelRef) -> Option<Capabilities> {
        fake_caps(m)
    }
    async fn stream(
        &self,
        _: CompletionRequest,
        s: SessionId,
        m: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if *self.root.lock().unwrap() != Some(s) && self.gate_members.load(Ordering::SeqCst) {
            self.gate(s).notified().await;
        }
        let mut steps = Vec::new();
        if let Some(batch) = self.tool_batches.lock().unwrap().remove(&s) {
            steps = batch;
            steps.push(FakeStep::Finish(FinishReason::ToolCalls));
        } else {
            let ans = self.answer.lock().unwrap().clone();
            if !ans.is_empty() {
                steps.push(FakeStep::Text(ans));
            }
            steps.push(FakeStep::Finish(FinishReason::Stop));
        }
        Ok(stream_steps(&steps, s, m))
    }
}

struct WaitTeam {
    engine: Arc<SessionEngine>,
    sup: Arc<ResidentSupervisor>,
    provider: Arc<GateProvider>,
    root: SessionId,
    lifecycle: LifecyclePlane,
    lead: AgentSpec,
}

async fn wait_team() -> WaitTeam {
    wait_team_with(
        SubagentLimits::default(),
        support::builtin_test_runtime(),
        "hya-main",
    )
    .await
}

async fn wait_team_with(
    limits: SubagentLimits,
    runtime: support::TestRuntime,
    agent: &str,
) -> WaitTeam {
    let provider = Arc::new(GateProvider::default());
    let (lifecycle, rx) = LifecyclePlane::new();
    let engine = Arc::new(
        support::engine_with_router_and_runtime(
            Arc::new(ProviderRouter::new().with(provider.clone())),
            runtime,
            PermissionRules::default(),
        )
        .await
        .with_governor(SubagentGovernor::new(limits))
        .with_lifecycle(lifecycle.clone()),
    );
    let root = support::create_root_session(&engine, agent, "fake", empty_workdir()).await;
    *provider.root.lock().unwrap() = Some(root);
    let sup = ResidentSupervisor::start(engine.clone());
    let lead = support::agent_spec(agent, "fake", empty_workdir());
    let binding = engine.bind_runtime(&lead.workdir).unwrap();
    sup.ensure_main(
        root,
        lead.clone(),
        (
            binding.clone(),
            Arc::from([]),
            binding.agent_resource_policy(agent).unwrap(),
        ),
        None,
        None,
    )
    .await
    .unwrap();
    tokio::spawn(run_lifecycle_service(
        engine.clone(),
        sup.clone(),
        rx,
        CancellationToken::new(),
    ));
    WaitTeam {
        engine,
        sup,
        provider,
        root,
        lifecycle,
        lead,
    }
}

async fn spawn_gated_as(
    t: &WaitTeam,
    parent: SessionId,
    gated: bool,
    name: &str,
) -> (SessionId, String) {
    let agent = support::agent_spec(name, "fake", empty_workdir());
    let binding = t.engine.bind_runtime(&agent.workdir).unwrap();
    t.provider.gate_members.store(gated, Ordering::SeqCst);
    let (session, handle) = t
        .sup
        .spawn_resident(
            parent,
            agent,
            (
                binding.clone(),
                Arc::from([]),
                binding.agent_resource_policy(name).unwrap(),
                None,
            ),
            "work".into(),
            None,
            None,
        )
        .await
        .unwrap();
    if gated {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !t.provider.gates.lock().unwrap().contains_key(&session) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    (session, handle)
}

async fn spawn_gated(t: &WaitTeam, parent: SessionId, gated: bool) -> (SessionId, String) {
    spawn_gated_as(t, parent, gated, "hya-scout").await
}

fn spec_ms(targets: Vec<String>, mode: WaitMode, timeout_ms: u64, wake_on_mail: bool) -> WaitSpec {
    WaitSpec {
        targets,
        mode,
        timeout: Duration::from_millis(timeout_ms),
        wake_on_mail,
    }
}

async fn wait_now(t: &WaitTeam, caller: SessionId, spec: WaitSpec) -> WaitOutcome {
    t.lifecycle
        .for_session(caller)
        .wait(spec, &CancellationToken::new())
        .await
        .unwrap()
}

fn spawn_wait(
    t: &WaitTeam,
    caller: SessionId,
    spec: WaitSpec,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<Result<WaitOutcome, ToolError>> {
    let lc = t.lifecycle.for_session(caller);
    tokio::spawn(async move { lc.wait(spec, &cancel).await })
}

fn hold_lead(t: &WaitTeam) -> TurnLease {
    t.engine.try_begin_turn(t.root).unwrap()
}

fn provider_wait_output(result: &Value) -> String {
    let msg = hya_proto::Message::Assistant {
        id: MessageId::new(),
        agent: "hya-scout".into(),
        model: "fake".into(),
        parts: vec![hya_proto::Part::Tool {
            id: PartId::new(),
            call_id: ToolCallId::new(),
            name: "wait".into(),
            state: ToolPartState::Completed {
                input: json!({}),
                output: result.clone(),
                time_ms: 0,
            },
            provider_data: None,
        }],
        finish: None,
        tokens: None,
        last_round: None,
        last_step_start: Some(0),
    };
    let items = hya_provider::encode_input_items(&[msg], false).unwrap();
    assert_eq!(items[1]["type"], "function_call_output");
    let text = items[1]["output"].as_str().unwrap();
    assert!(text.chars().count() <= 4_500);
    text.to_owned()
}

#[tokio::test]
async fn wait_returns_on_member_report_any_mode_and_budget_kill() {
    for wake_on_mail in [false, true] {
        let team = wait_team().await;
        let _lead = hold_lead(&team);
        let (_, handle) = spawn_gated(&team, team.root, true).await;
        let waiter = spawn_wait(
            &team,
            team.root,
            spec_ms(vec![handle.clone()], WaitMode::All, 5_000, wake_on_mail),
            CancellationToken::new(),
        );
        team.sup
            .submit_report(
                team.root,
                &handle,
                ReportOutcome::Done,
                "UNIT_SHIPPED".into(),
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert!(!waiter.is_finished());
        team.provider.release_all();
        let out = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(out.woke_by, WaitWake::Members);
        assert!(out.running.is_empty() && out.mail.is_empty());
        assert_eq!(
            (
                out.finished[0].handle.as_str(),
                out.finished[0].state,
                out.finished[0].outcome.as_deref(),
                out.finished[0].report.as_deref()
            ),
            (
                handle.as_str(),
                WaitMemberState::Reported,
                Some("done"),
                Some("UNIT_SHIPPED")
            )
        );
        assert_eq!(
            out.finished[0].channel,
            Some(dm_for(&team.engine, team.root, &handle).await)
        );
    }

    // `any` mode returns on first reporting member while second keeps working.
    let any = wait_team().await;
    let _lead = hold_lead(&any);
    let (a_s, a) = spawn_gated(&any, any.root, true).await;
    let (_, b) = spawn_gated(&any, any.root, true).await;
    let waiter = spawn_wait(
        &any,
        any.root,
        spec_ms(vec![], WaitMode::Any, 5_000, false),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(!waiter.is_finished());
    any.sup
        .submit_report(any.root, &a, ReportOutcome::Done, "A_DONE".into())
        .await
        .unwrap();
    any.provider.release(a_s);
    let out = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            out.woke_by,
            out.finished[0].handle.as_str(),
            out.running[0].handle.as_str(),
            out.running[0].state
        ),
        (
            WaitWake::Members,
            a.as_str(),
            b.as_str(),
            WaitMemberState::Working
        )
    );
    any.provider.release_all();

    // Budget kill preserves failed outcome & reason in wait output.
    let bt = wait_team_with(
        SubagentLimits {
            per_team_message_budget: 1,
            ..Default::default()
        },
        support::builtin_test_runtime(),
        "hya-main",
    )
    .await;
    let _bl = hold_lead(&bt);
    let (child, handle) = spawn_gated(&bt, bt.root, true).await;
    send_mail(&bt.engine, bt.root, &handle, "one").await;
    send_mail(&bt.engine, bt.root, &handle, "two").await;
    wait_archived(&bt.engine, bt.root, &handle).await;
    let out = wait_now(
        &bt,
        bt.root,
        spec_ms(vec![handle], WaitMode::All, 25, false),
    )
    .await;
    let m = out
        .finished
        .iter()
        .chain(&out.already_finished)
        .find(|m| m.session == child)
        .unwrap();
    assert_eq!(
        (m.outcome.as_deref(), m.report.as_deref()),
        (Some("failed"), Some("per-team message budget exceeded"))
    );
}

#[tokio::test]
async fn wait_finishes_on_final_answer_and_defers_until_live_subagent_reports() {
    let team = wait_team().await;
    let _lead = hold_lead(&team);
    let (child, handle) = spawn_gated(&team, team.root, true).await;
    let waiter = spawn_wait(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 5_000, true),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    team.provider.release(child);
    let out = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            out.woke_by,
            out.finished[0].handle.as_str(),
            out.finished[0].state,
            out.finished[0].outcome.as_deref(),
            out.finished[0].report.as_deref()
        ),
        (
            WaitWake::Members,
            handle.as_str(),
            WaitMemberState::Reported,
            Some("done"),
            Some("done with the unit")
        )
    );
    assert!(out.mail.is_empty());

    let (member, mh) = spawn_gated(&team, team.root, true).await;
    let (gc, _) = spawn_gated(&team, member, true).await;
    team.provider.gate_members.store(false, Ordering::SeqCst);
    team.provider.release(member);
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !team.engine.turn_active(member)
        })
        .await
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    let now = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 0, true)).await;
    assert!(
        now.finished.is_empty()
            && now.running.len() == 1
            && now.running[0].handle == mh
            && now.running[0].state == WaitMemberState::Working
    );

    team.provider.release(gc);
    assert!(support::poll_until(Duration::from_secs(5), || async {
        team.engine.replay(team.root).await.unwrap().iter().any(|e| matches!(ExtensionEvent::from_event(&e.event), Some(ExtensionEvent::SubagentReported { child, report, .. }) if child == member && report == "done with the unit"))
    }).await);
}

#[tokio::test]
async fn wait_distinguishes_stalled_user_stopped_nothing_to_wait_for_and_unknown_target() {
    let empty = wait_team().await;
    assert_eq!(
        wait_now(
            &empty,
            empty.root,
            spec_ms(vec![], WaitMode::All, 5_000, false)
        )
        .await
        .woke_by,
        WaitWake::NothingToWaitFor
    );
    let err = empty
        .lifecycle
        .for_session(empty.root)
        .wait(
            spec_ms(vec!["ghost-1".into()], WaitMode::All, 5_000, false),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Input(m) if m.contains("ghost-1")));

    // Empty answer -> Stalled once, then subsequent wait blocks until 25ms timeout.
    let team = wait_team().await;
    team.provider.answer.lock().unwrap().clear();
    let _lead = hold_lead(&team);
    let (child, handle) = spawn_gated(&team, team.root, true).await;
    let waiter = spawn_wait(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 5_000, true),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    team.provider.release(child);
    let stalled = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stalled.woke_by,
            stalled.running[0].handle.as_str(),
            stalled.running[0].state
        ),
        (WaitWake::Stalled, handle.as_str(), WaitMemberState::Idle)
    );
    assert!(stalled.finished.is_empty() && stalled.running[0].report.is_none());

    let started = Instant::now();
    let again = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 25, true)).await;
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert_eq!(
        (again.woke_by, again.running[0].state),
        (WaitWake::Timeout, WaitMemberState::Idle)
    );

    // User-stopped member -> NothingToWaitFor immediately with StoppedByUser & durable idle.
    let ut = wait_team().await;
    let _ul = hold_lead(&ut);
    let (uc, uh) = spawn_gated(&ut, ut.root, true).await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            ut.engine.turn_active(uc)
        })
        .await
    );
    assert!(ut.engine.cancel_user_turn(uc));
    let stopped = wait_now(&ut, ut.root, spec_ms(vec![], WaitMode::All, 5_000, true)).await;
    assert_eq!(
        (
            stopped.woke_by,
            stopped.running[0].handle.as_str(),
            stopped.running[0].state
        ),
        (
            WaitWake::NothingToWaitFor,
            uh.as_str(),
            WaitMemberState::StoppedByUser
        )
    );
    assert!(
        stopped
            .to_tool_result()
            .to_string()
            .contains("[stopped by the user:")
    );
    assert!(
        support::poll_until(Duration::from_secs(2), || async {
            let p = ut.engine.read_projection(ut.root).await.unwrap();
            let team = team_projection(&p);
            team.roster
                .get(&uh)
                .is_some_and(|e| e.status == RosterStatus::Idle)
                && !team.archived.contains_key(&uh)
        })
        .await
    );
}

#[tokio::test]
async fn wait_tracks_woken_after_report_and_already_finished_targets() {
    let team = wait_team().await;
    let (a_s, a) = spawn_gated(&team, team.root, true).await;
    let (_, b) = spawn_gated(&team, team.root, true).await;
    team.sup
        .submit_report(team.root, &a, ReportOutcome::Done, "A_ONCE".into())
        .await
        .unwrap();
    team.provider.release(a_s);
    assert_eq!(
        wait_now(
            &team,
            team.root,
            spec_ms(vec![a.clone()], WaitMode::All, 5_000, true)
        )
        .await
        .finished[0]
            .report
            .as_deref(),
        Some("A_ONCE")
    );

    for _ in 0..2 {
        let again = wait_now(
            &team,
            team.root,
            spec_ms(vec![a.clone()], WaitMode::All, 5_000, true),
        )
        .await;
        assert_eq!(again.woke_by, WaitWake::NothingToWaitFor);
        assert!(again.finished.is_empty() && again.mail.is_empty());
        assert_eq!(again.already_finished[0].report.as_deref(), Some("A_ONCE"));
        assert!(
            again.to_tool_result()["output"]
                .as_str()
                .unwrap()
                .contains("already finished")
        );
    }
    let started = Instant::now();
    let mixed = wait_now(
        &team,
        team.root,
        spec_ms(vec![a.clone(), b.clone()], WaitMode::Any, 25, true),
    )
    .await;
    assert!(started.elapsed() >= Duration::from_millis(20));
    assert_eq!(
        (
            mixed.woke_by,
            mixed.already_finished[0].handle.as_str(),
            mixed.running[0].handle.as_str()
        ),
        (WaitWake::Timeout, a.as_str(), b.as_str())
    );

    let defaults = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 0, true)).await;
    assert!(
        defaults.already_finished.is_empty()
            && defaults.running.len() == 1
            && defaults.running[0].handle == b
    );
    team.provider.release_all();

    // Woken member counts as working until its NEXT report.
    let wt = wait_team().await;
    let _wl = hold_lead(&wt);
    let (child, handle) = spawn_gated(&wt, wt.root, true).await;
    wt.sup
        .submit_report(wt.root, &handle, ReportOutcome::Done, "FIRST_REPORT".into())
        .await
        .unwrap();
    wt.provider.release(child);
    assert_eq!(
        wait_now(
            &wt,
            wt.root,
            spec_ms(vec![handle.clone()], WaitMode::All, 5_000, false)
        )
        .await
        .finished[0]
            .report
            .as_deref(),
        Some("FIRST_REPORT")
    );

    wt.provider.gate_members.store(true, Ordering::SeqCst);
    send_mail(&wt.engine, wt.root, &handle, "follow-up").await;
    let waiter = spawn_wait(
        &wt,
        wt.root,
        spec_ms(vec![handle.clone()], WaitMode::All, 5_000, true),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!waiter.is_finished());
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            wt.sup
                .submit_report(
                    wt.root,
                    &handle,
                    ReportOutcome::Done,
                    "SECOND_REPORT".into(),
                )
                .await
                .is_ok()
        })
        .await
    );
    wt.provider.release(child);
    let r2 = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (r2.woke_by, r2.finished[0].report.as_deref()),
        (WaitWake::Members, Some("SECOND_REPORT"))
    );
    assert!(r2.already_finished.is_empty() && r2.mail.is_empty());
}

#[tokio::test]
async fn wait_mail_wake_channels_off_harness_mail_and_snapshot_timeout_zero() {
    let team = wait_team().await;
    let _lead = hold_lead(&team);
    let (child, handle) = spawn_gated(&team, team.root, true).await;
    send_mail(&team.engine, child, "^parent", "SNAPSHOT_MAIL").await;
    let snap = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 0, true)).await;
    assert_eq!(
        (snap.woke_by, snap.running[0].handle.as_str()),
        (WaitWake::Mail, handle.as_str())
    );
    assert!(snap.mail[0].preview.contains("SNAPSHOT_MAIL"));
    let empty = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 0, true)).await;
    assert!(empty.woke_by == WaitWake::Timeout && empty.mail.is_empty());

    let w_on = spawn_wait(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 5_000, true),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    send_mail(&team.engine, child, "^parent", "PROGRESS: halfway").await;
    let woke = tokio::time::timeout(Duration::from_secs(5), w_on)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(woke.woke_by, WaitWake::Mail);
    assert!(woke.mail[0].preview.contains("PROGRESS: halfway") && woke.running.len() == 1);

    let w_off = spawn_wait(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 30, false),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    send_mail(&team.engine, child, "^parent", "IGNORED_BY_NO_CHANNELS").await;
    let ignored = w_off.await.unwrap().unwrap();
    assert!(ignored.woke_by == WaitWake::Timeout && ignored.mail.is_empty());
    team.provider.release_all();

    // Harness mail wakes a member wait and truncates cleanly at 600 chars.
    let ht = wait_team().await;
    ht.provider.answer.lock().unwrap().clear();
    let (member, mh) = spawn_gated(&ht, ht.root, false).await;
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            !ht.engine.turn_active(member)
        })
        .await
    );
    let mw = spawn_wait(
        &ht,
        member,
        spec_ms(vec![], WaitMode::All, 5_000, true),
        CancellationToken::new(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    let notice = format!("LEADER FAILED: {}", "界".repeat(5000));
    let expected = format!("{}…", notice.chars().take(600).collect::<String>());
    let env = ht
        .engine
        .store()
        .append_harness_mail(ht.root, &mh, notice)
        .await
        .unwrap();
    ht.engine.bus().publish(env);
    let hout = tokio::time::timeout(Duration::from_secs(5), mw)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            hout.woke_by,
            hout.mail[0].from.as_str(),
            hout.mail[0].preview.as_str()
        ),
        (WaitWake::Mail, hya_proto::HARNESS_HANDLE, expected.as_str())
    );
    assert!(provider_wait_output(&hout.to_tool_result()).contains(&expected));
}

#[tokio::test]
async fn wait_mail_dedup_cursor_and_direct_mail_survives_twenty_reports() {
    let team = wait_team().await;
    let lead_turn = hold_lead(&team);
    let (child, handle) = spawn_gated(&team, team.root, true).await;
    let dm = dm_for(&team.engine, team.root, &handle).await;
    team.engine
        .mail_send(
            child,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "RESEND_FULL_REPORT".into(),
        )
        .await
        .unwrap();
    let first = wait_now(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 5_000, true),
    )
    .await;
    assert!(
        first.woke_by == WaitWake::Mail
            && first.mail[0].preview.contains("RESEND_FULL_REPORT")
            && first.mail[0].channel.as_deref() == Some(dm.as_str())
    );
    let p = team.engine.read_projection(team.root).await.unwrap();
    let team_p = team_projection(&p);
    assert_eq!(
        team_p.roster["main"].resident_cursor,
        team_p.inboxes["main"].len() as u64
    );
    assert!(
        support::steer_notice(&team.engine, team.root)
            .await
            .is_none_or(|n| !n.contains("RESEND_FULL_REPORT"))
    );
    let started = Instant::now();
    let second = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 25, true)).await;
    assert!(
        started.elapsed() >= Duration::from_millis(20)
            && second.woke_by == WaitWake::Timeout
            && second.mail.is_empty()
    );

    let fresh_status = format!("FRESH_STATUS{}", "界".repeat(588));
    team.engine
        .mail_send(
            child,
            MailEndpoint::Channel(dm),
            MailKind::Message,
            fresh_status.clone(),
        )
        .await
        .unwrap();
    let steered = support::steer_notice(&team.engine, team.root)
        .await
        .unwrap();
    assert!(steered.contains(&fresh_status) && !steered.contains("RESEND_FULL_REPORT"));
    assert!(
        wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 0, true))
            .await
            .mail[0]
            .preview
            .contains("FRESH_STATUS")
    );

    drop(lead_turn);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!transcript_has(&team.engine, team.root, "RESEND_FULL_REPORT").await);
    assert!(!transcript_has(&team.engine, team.root, "FRESH_STATUS").await);
    team.provider.release_all();

    // Direct harness mail survives 20 large finished reports within encoding budget.
    let bt = wait_team().await;
    let _bl = hold_lead(&bt);
    let (parent, ph) = spawn_gated(&bt, bt.root, true).await;
    let mut children = Vec::with_capacity(20);
    for _ in 0..20 {
        let (c, path) = spawn_gated(&bt, parent, true).await;
        bt.sup
            .submit_report(bt.root, &path, ReportOutcome::Done, "r".repeat(3000))
            .await
            .unwrap();
        children.push((c, path));
    }
    for (c, _) in &children {
        bt.provider.release(*c);
    }
    let targets: Vec<String> = children.into_iter().map(|(_, p)| p).collect();
    assert!(
        support::poll_until(Duration::from_secs(5), || async {
            let p = bt.engine.read_projection(bt.root).await.unwrap();
            let team = team_projection(&p);
            targets.iter().all(|t| team.archived.contains_key(t))
        })
        .await
    );
    let notice = "LEADER FAILED: wrap up and report your work.";
    bt.engine
        .store()
        .append_harness_mail(bt.root, &ph, notice.into())
        .await
        .unwrap();
    let out = wait_now(&bt, parent, spec_ms(targets, WaitMode::All, 0, true)).await;
    assert_eq!((out.already_finished.len(), out.more_mail), (20, false));
    assert!(provider_wait_output(&out.to_tool_result()).contains(notice));
    bt.provider.release(parent);
}

struct ReadPagesTool;
#[async_trait]
impl Tool for ReadPagesTool {
    fn name(&self) -> &str {
        "read_pages"
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.name().into(),
            description: "Read pages".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let first = ctx
            .nested
            .call("receive", json!({"timeout_secs": 0}))
            .await?;
        if input["fail"] == true {
            return Err(ToolError::Other("discard first page".into()));
        }
        let second = ctx
            .nested
            .call("receive", json!({"timeout_secs": 0}))
            .await?;
        let pages = [first, second].map(|p| {
            p["output"]
                .as_str()
                .unwrap()
                .lines()
                .filter_map(|l| {
                    l.strip_prefix("notice ")?
                        .split_once(' ')
                        .map(|(id, _)| id.to_owned())
                })
                .collect::<Vec<_>>()
        });
        Ok(json!({"pages": pages}))
    }
}

async fn mail_reader_team() -> WaitTeam {
    let fixture = support::test_catalog(&[("mail-reader", AgentRole::Main, &[])]);
    let mut bundle = (*fixture.bundles().bundles()[0]).clone();
    bundle.agents[0].resource_view.allow =
        vec!["harness:tool/wait".into(), "harness:tool/read_pages".into()];
    bundle.agents[0]
        .resource_view
        .aliases
        .insert("receive".into(), "harness:tool/wait".into());
    let catalog = Arc::new(
        AgentCatalog::new(Arc::new(BundleCatalog::from_prepared(&[bundle]).unwrap())).unwrap(),
    );
    let tools = hya_tool_loader::builtins();
    tools.register(Arc::new(ReadPagesTool)).unwrap();
    wait_team_with(
        Default::default(),
        support::test_runtime_with_catalog(Arc::new(tools), catalog).into(),
        "mail-reader",
    )
    .await
}

async fn run_wait_turn(
    t: &WaitTeam,
    eng: &SessionEngine,
    batch: Vec<FakeStep>,
) -> Result<FinishReason, CoreError> {
    let lease = hold_lead(t);
    let prompt = eng
        .admit_user_prompt(t.root, "check for mail".into())
        .await
        .unwrap();
    mail_main_from_peer(eng, t.root, "direct notice").await;
    t.provider
        .tool_batches
        .lock()
        .unwrap()
        .insert(t.root, batch);
    eng.run_admitted_turn(
        lease,
        &t.lead,
        CancellationToken::new(),
        &[],
        None,
        None,
        prompt,
    )
    .await
}

#[tokio::test]
async fn wait_transactional_commit_mask_retry_nested_pages_and_paging_steering() {
    // 1) Masked first result leaves mail unread for second wait (canonical + alias).
    for name in ["wait", "receive"] {
        let t = if name == "wait" {
            wait_team().await
        } else {
            mail_reader_team().await
        };
        let call = || FakeStep::ToolCall {
            name: name.into(),
            input: json!({"timeout_secs": 0}),
        };
        let eng = (*t.engine)
            .clone()
            .with_hooks(Arc::new(support::MaskFirstResult::default()));
        run_wait_turn(&t, &eng, vec![call(), call()]).await.unwrap();
        let deliveries: Vec<_> = eng
            .replay(t.root)
            .await
            .unwrap()
            .iter()
            .filter_map(|e| match &e.event {
                Event::ToolResult { output, .. } if output == &json!({"masked": true}) => {
                    Some("masked")
                }
                Event::ToolResult { output, .. } => {
                    assert!(provider_wait_output(output).contains("direct notice"));
                    Some("delivered")
                }
                ev if matches!(
                    ExtensionEvent::from_event(ev),
                    Some(ExtensionEvent::MailConsumed { .. })
                ) =>
                {
                    Some("ack")
                }
                _ => None,
            })
            .collect();
        assert_eq!(deliveries, ["masked", "delivered", "ack"]);
    }

    // 2) Wait commits ToolResult + MailConsumed atomically (rollback on either trigger).
    for name in ["wait", "receive"] {
        for reject in [Some("tool_result"), Some("mail_consumed"), None] {
            let t = if name == "wait" {
                wait_team().await
            } else {
                mail_reader_team().await
            };
            if let Some(tag) = reject {
                let mut tx = t.engine.store().begin_transaction().await.unwrap();
                sqlx::query(&format!(
                    "CREATE TRIGGER reject_delivery BEFORE INSERT ON event_log \
                     WHEN json_extract(NEW.payload, '$.type') = '{tag}' OR json_extract(NEW.payload, '$.kind') = '{tag}' BEGIN SELECT RAISE(ABORT, 'injected wait write failure'); END"
                ))
                .execute(&mut *tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
            }
            let res = run_wait_turn(
                &t,
                &t.engine,
                vec![FakeStep::ToolCall {
                    name: name.into(),
                    input: json!({"timeout_secs": 0}),
                }],
            )
            .await;
            let delivered: Vec<_> = t
                .engine
                .replay(t.root)
                .await
                .unwrap()
                .iter()
                .filter_map(|e| match &e.event {
                    Event::ToolResult { output, .. } => {
                        assert!(provider_wait_output(output).contains("direct notice"));
                        Some("result")
                    }
                    ev => match ExtensionEvent::from_event(ev) {
                        Some(ExtensionEvent::MailConsumed { through, .. }) => {
                            assert_eq!(through, 1);
                            Some("ack")
                        }
                        _ => None,
                    },
                })
                .collect();
            if reject.is_some() {
                assert!(
                    res.unwrap_err()
                        .to_string()
                        .contains("injected wait write failure")
                        && delivered.is_empty()
                );
            } else {
                res.unwrap();
                assert_eq!(delivered, ["result", "ack"]);
            }
        }
    }

    // 3) Nested wait pages commit only with the surviving outer tool result.
    let nt = mail_reader_team().await;
    let notices: Vec<_> = (0..10)
        .map(|i| format!("notice {i} {}", "x".repeat(590)))
        .collect();
    let _lease = hold_lead(&nt);
    let (reader, rh) = spawn_gated_as(&nt, nt.root, true, "mail-reader").await;
    for n in &notices {
        nt.engine
            .store()
            .append_harness_mail(nt.root, &rh, n.clone())
            .await
            .unwrap();
    }
    let cursor =
        team_projection(&nt.engine.read_projection(nt.root).await.unwrap()).inboxes[&rh].len();
    let mut bus = nt.engine.bus().subscribe();
    nt.provider.tool_batches.lock().unwrap().insert(
        reader,
        vec![
            FakeStep::ToolCall {
                name: "read_pages".into(),
                input: json!({"fail": true}),
            },
            FakeStep::ToolCall {
                name: "read_pages".into(),
                input: json!({"fail": false}),
            },
        ],
    );
    nt.provider.gate_members.store(false, Ordering::SeqCst);
    nt.provider.release(reader);
    let events = tokio::time::timeout(Duration::from_secs(5), async {
        let mut out = Vec::new();
        loop {
            let e = bus.recv().await.unwrap();
            let done = matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::MailConsumed { handle: ref r, .. }) if r == &rh
            );
            if done || e.event.session() == Some(reader) {
                out.push(e);
            }
            if done {
                return out;
            }
        }
    })
    .await
    .unwrap();
    let dels: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolError { message_text, .. } => {
                assert!(message_text.contains("discard first page"));
                Some("error")
            }
            Event::ToolResult { output, .. } => {
                let ids: Vec<_> = output["pages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|p| {
                        p.as_array()
                            .unwrap()
                            .iter()
                            .map(|i| i.as_str().unwrap().parse::<usize>().unwrap())
                    })
                    .collect();
                assert_eq!(ids, (0..notices.len()).collect::<Vec<_>>());
                Some("result")
            }
            ev => match ExtensionEvent::from_event(ev) {
                Some(ExtensionEvent::MailConsumed { through, .. }) => {
                    assert_eq!(usize::try_from(through).unwrap(), cursor);
                    Some("ack")
                }
                _ => None,
            },
        })
        .collect();
    assert_eq!(dels, ["error", "result", "ack"]);

    // 4) Direct mail paging across two waits skips steering onto unrelated tools.
    let pt = wait_team().await;
    let _pl = hold_lead(&pt);
    let (member, mh) = spawn_gated(&pt, pt.root, true).await;
    let p_notices: Vec<_> = (0..10)
        .map(|i| format!("notice {i} {}!", "界".repeat(590)))
        .collect();
    for n in &p_notices {
        send_mail(&pt.engine, member, "^parent", "progress").await;
        let env = pt
            .engine
            .store()
            .append_harness_mail(pt.root, &mh, n.clone())
            .await
            .unwrap();
        pt.engine.bus().publish(env);
    }
    let mut pbus = pt.engine.bus().subscribe();
    let wc = || FakeStep::ToolCall {
        name: "wait".into(),
        input: json!({"timeout_secs": 0}),
    };
    pt.provider.tool_batches.lock().unwrap().insert(
        member,
        vec![
            wc(),
            FakeStep::ToolCall {
                name: "list_agents".into(),
                input: json!({}),
            },
            wc(),
        ],
    );
    pt.provider.gate_members.store(false, Ordering::SeqCst);
    pt.provider.release(member);
    let mut delivered = 0;
    for call in 0..3 {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match pbus.recv().await.unwrap().event {
                    Event::ToolResult {
                        session, output, ..
                    } if session == member => break output,
                    Event::ToolError {
                        session,
                        message_text,
                        ..
                    } if session == member => panic!("{message_text}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        let text = provider_wait_output(&result);
        if call == 1 {
            assert!(
                result["agents"].is_array()
                    && !text.contains("NEW MAIL")
                    && !text.contains("notice ")
            );
            continue;
        }
        let count = result["metadata"]["mail"].as_array().unwrap().len();
        assert!(count > 0);
        for (i, n) in p_notices.iter().enumerate() {
            assert_eq!(
                text.contains(n),
                (delivered..delivered + count).contains(&i)
            );
        }
        delivered += count;
        assert_eq!(
            result["metadata"].get("more_mail"),
            (call == 0).then_some(&Value::Bool(true))
        );
        assert_eq!(text.contains("call `wait` again"), call == 0);
    }
    assert_eq!(delivered, p_notices.len());
    let cursors: Vec<_> = pt
        .engine
        .replay(pt.root)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match ExtensionEvent::from_event(&e.event) {
            Some(ExtensionEvent::MailConsumed {
                handle: r, through, ..
            }) if r == mh => Some(through),
            _ => None,
        })
        .collect();
    assert!(cursors.len() == 2 && cursors[0] < cursors[1]);
}

#[tokio::test]
async fn wait_times_out_and_aborts_promptly_on_cancellation() {
    let team = wait_team().await;
    let (_, handle) = spawn_gated(&team, team.root, true).await;

    let started = Instant::now();
    let out = wait_now(&team, team.root, spec_ms(vec![], WaitMode::All, 25, false)).await;
    assert!(
        started.elapsed() >= Duration::from_millis(20)
            && started.elapsed() < Duration::from_secs(1)
    );
    assert_eq!(
        (
            out.woke_by,
            out.running[0].handle.as_str(),
            out.running[0].state
        ),
        (WaitWake::Timeout, handle.as_str(), WaitMemberState::Working)
    );
    assert!(out.finished.is_empty());

    let cancel = CancellationToken::new();
    let waiter = spawn_wait(
        &team,
        team.root,
        spec_ms(vec![], WaitMode::All, 30_000, true),
        cancel.clone(),
    );
    tokio::time::sleep(Duration::from_millis(15)).await;
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap(),
        Err(ToolError::Cancelled)
    ));
    team.provider.release_all();
}

// ---- Section D: Channel policy defaults, restrictions, trust isolation, snapshots ----

fn restriction_catalog(identity: &str, channels: &str) -> BundleCatalog {
    let prepared = prepare_package(BundleSource::new(
        "restriction",
        vec![SourceFile::new(
            "bundle.yaml",
            format!("schema_version: 2\nkind: Bundle\nidentity: {{ id: {identity}, version: 1.0.0, publisher: acme }}\nagents: [{{id: reviewer, role: subagent}}]\nchannels:\n{channels}\n"),
        )],
    ))
    .unwrap();
    BundleCatalog::from_prepared(prepared.bundles()).unwrap()
}

#[test]
fn channel_policy_defaults_bundle_restrictions_and_trust_isolation() {
    let defaults =
        ChannelPolicy::from_bundle_catalog(&BundleCatalog::from_prepared(&[]).unwrap()).unwrap();
    assert!(defaults.allows_unit(
        ChannelCapability::Send,
        "anything",
        ChannelParticipantRole::UnitLeader
    ));
    assert!(defaults.allows_parent_dm(
        ChannelCapability::Report,
        "anything",
        ChannelParticipantRole::Child
    ));

    let restricted = ChannelPolicy::from_bundle_catalog(&restriction_catalog(
        "acme/restrict",
        "  - { id: unit, kind: unit, participants: [{kind: agent, agent: reviewer}], capabilities: [send], scope: unit, retention: team_session }\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .unwrap();
    assert!(restricted.allows_unit(
        ChannelCapability::Send,
        "reviewer",
        ChannelParticipantRole::DirectReports
    ));
    assert!(!restricted.allows_unit(
        ChannelCapability::Steer,
        "reviewer",
        ChannelParticipantRole::DirectReports
    ));
    assert!(restricted.allows_unit(
        ChannelCapability::Steer,
        "other",
        ChannelParticipantRole::DirectReports
    ));
    assert!(!restricted.allows_parent_dm(
        ChannelCapability::Report,
        "other",
        ChannelParticipantRole::Child
    ));
    assert!(restricted.allows_parent_dm(
        ChannelCapability::Report,
        "other",
        ChannelParticipantRole::Parent
    ));

    // Reusing preset identity (`hya/agent-channels`) cannot elevate trust.
    let spoofed = ChannelPolicy::from_bundle_catalog(&restriction_catalog(
        "hya/agent-channels",
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .unwrap();
    assert!(!spoofed.allows_parent_dm(
        ChannelCapability::Report,
        "reviewer",
        ChannelParticipantRole::Child
    ));
}

#[test]
fn channel_policy_captured_snapshot_stays_pinned_across_catalog_refresh() {
    let admitted = ChannelPolicy::from_bundle_catalog(&BundleCatalog::from_prepared(&[]).unwrap())
        .unwrap()
        .snapshot_for("reviewer");
    let refreshed = ChannelPolicy::from_bundle_catalog(&restriction_catalog(
        "acme/restrict",
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
    ))
    .unwrap()
    .snapshot_for("reviewer");
    assert_ne!(admitted.dm_child & 0b1_1111, 0);
    assert_eq!(refreshed.dm_child, 0);
    assert_ne!(admitted.dm_child & 0b1_1111, 0);
}
