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
use hya_tool::{ChannelPolicySnapshot, PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

use hya_core::CoreError;
use hya_core::bus::EventBus;
use hya_core::engine::{AgentSpec, CreateSession, SessionEngine};
use hya_core::resident::ResidentSupervisor;
use hya_core::run_lifecycle_service;

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

async fn engine_with_channel_restriction(channels: &str) -> Arc<SessionEngine> {
    let source = hya_bundle::BundleSource::new(
        "channel-restriction",
        vec![hya_bundle::SourceFile::new(
            "bundle.yaml",
            format!(
                "kind: AgentSetBundle\nidentity: {{ id: acme/channel-restriction, version: 1.0.0, publisher: acme }}\nchannels:\n{channels}\n"
            ),
        )],
    );
    let prepared = hya_bundle::prepare_package(source).unwrap();
    let bundles = hya_bundle::BundleCatalog::from_prepared(prepared.bundles()).unwrap();
    let agents = hya_core::AgentCatalog::new(Arc::new(bundles)).unwrap();
    let runtime = Arc::new(hya_core::RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(agents),
    ));
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(StubProvider)));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    Arc::new(SessionEngine::new(
        store,
        router,
        runtime,
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

/// Members park inside their first round until `release`; the root never
/// parks. Mail sent meanwhile stays unread (no wake, no tool result to steer).
#[derive(Default)]
struct ParkedMembers {
    root: std::sync::Mutex<Option<SessionId>>,
    release: tokio::sync::Notify,
}

#[async_trait]
impl Provider for ParkedMembers {
    fn id(&self) -> &str {
        "fake"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        StubProvider.capabilities(model)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        if *self.root.lock().unwrap() != Some(session) {
            self.release.notified().await;
        }
        StubProvider.stream(req, session, message).await
    }
}

#[tokio::test]
async fn report_gate_rejects_unread_mail() {
    let provider = Arc::new(ParkedMembers::default());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(provider.clone())),
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::default(),
    ));
    let root = root_team(&engine).await;
    *provider.root.lock().unwrap() = Some(root);
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let agent = agent_spec();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (_child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            "explore the tree".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    for _ in 0..300 {
        let projection = engine.read_projection(root).await.unwrap();
        if projection.team.roster[&handle].status == RosterStatus::Busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "one question first".to_string(),
        )
        .await
        .unwrap();

    let dm = engine
        .read_projection(root)
        .await
        .unwrap()
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Dm && channel.members.contains(&handle)
        })
        .map(|(id, _)| id.clone())
        .unwrap();
    let result = supervisor.report_gate(root, &handle).await;
    match result {
        Err(CoreError::Invalid(message)) => {
            assert!(
                message.contains("unread mail"),
                "gate error should mention unread mail: {message}"
            );
            // Actionable: the channel holding the mail, the exact read call,
            // and the reply / wait paths.
            for needle in [
                format!("#{dm}"),
                format!("`read channel://{dm}`"),
                "`list_channel`".to_string(),
                "`send`".to_string(),
                "`wait`".to_string(),
            ] {
                assert!(message.contains(&needle), "missing {needle}: {message}");
            }
        }
        other => panic!("expected unread-mail rejection, got {other:?}"),
    }
    // Nothing archived: the roster row is still live.
    let projection = engine.read_projection(root).await.unwrap();
    assert!(projection.team.roster.contains_key(&handle));
    provider.release.notify_waiters();
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
async fn archive_stops_an_idle_member_and_mail_wakes_it_under_the_same_session() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;

    // Addressed by session id, as the model saw it in the task result.
    let receipt = supervisor
        .archive_member(root, &child.to_string(), "done for now")
        .await
        .unwrap();
    assert_eq!(receipt.handle, handle);
    assert!(
        !receipt.cancelled_turn,
        "an idle member has no turn to cancel"
    );

    let projection = engine.read_projection(root).await.unwrap();
    assert!(!projection.team.roster.contains_key(&handle), "archived");
    let archived = projection.team.archived.get(&handle).unwrap();
    assert_eq!(archived.reason, ArchiveReason::ArchivedByParent);
    assert_eq!(archived.session, child);
    // The parent chose to archive: no synthesized report mail comes back to it.
    assert!(
        projection
            .team
            .inboxes
            .get("main")
            .is_none_or(|inbox| inbox.iter().all(|mail| !mail.body.contains("done for now"))),
        "archive must not mail the archiver"
    );
    // Archived members stay readable: transcript and degraded handoff.
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(
        child_projection
            .session
            .messages
            .iter()
            .any(|message| message.role == hya_proto::Role::Assistant),
        "the archived member's transcript stays readable"
    );
    assert!(
        child_projection
            .session
            .handoff
            .as_ref()
            .is_some_and(|handoff| handoff.degraded)
    );

    // Mail to the archived handle wakes it: same handle, same session.
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resume: also check the docs".to_string(),
        )
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection
            .team
            .roster
            .get(&handle)
            .map(|entry| entry.session),
        Some(child),
        "the woken member keeps its session"
    );
    wait_idle(&engine, root, &handle, child).await;
    let child_projection = engine.read_projection(child).await.unwrap();
    assert!(child_projection.session.messages.iter().any(|message| {
        message.parts.iter().any(
            |part| matches!(part, PartProjection::Text { text, .. } if text.contains("also check the docs")),
        )
    }));
}

#[tokio::test]
async fn mail_on_the_dm_channel_wakes_an_archived_member() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;
    supervisor
        .archive_member(root, &handle, "pause")
        .await
        .unwrap();
    let dm = engine
        .read_projection(root)
        .await
        .unwrap()
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Dm && channel.members.contains(&handle)
        })
        .map(|(id, _)| id.clone())
        .expect("pair DM channel");
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(dm),
            MailKind::Message,
            "wake via the DM channel".to_string(),
        )
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection
            .team
            .roster
            .get(&handle)
            .map(|entry| entry.session),
        Some(child)
    );
    wait_idle(&engine, root, &handle, child).await;
}

#[tokio::test]
async fn archive_names_actionable_errors() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;
    let leaf = handle.rsplit('/').next().unwrap().to_string();

    let lead = supervisor
        .archive_member(root, "main", "x")
        .await
        .unwrap_err();
    assert!(
        lead.to_string().contains("team lead"),
        "the lead is never archivable: {lead}"
    );
    let lead_by_session = supervisor
        .archive_member(root, &root.to_string(), "x")
        .await
        .unwrap_err();
    assert!(lead_by_session.to_string().contains("team lead"));

    let unknown = supervisor
        .archive_member(root, "nobody-7", "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        unknown.contains("nobody-7") && unknown.contains(&handle),
        "unknown targets list the live subagents: {unknown}"
    );

    // The leaf spelling resolves relative to the caller.
    supervisor.archive_member(root, &leaf, "x").await.unwrap();
    let again = supervisor
        .archive_member(root, &handle, "x")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        again.contains("already archived") && again.contains("mail"),
        "a second archive says how to wake it: {again}"
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

    // From a peer: steer skips the agent's own posts (a channel post fans
    // out to its sender too), so the backlog must come from someone else.
    mail_main_from_peer(&engine, root, "steer payload for main").await;

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

/// Models write channel ids as `##X`, `#X`, or padded with whitespace when
/// reading mail history. `read_channel_history` must normalize all of those
/// (trim + strip every leading `#`, case untouched) and echo a warning naming
/// the correction, while a genuinely unknown id still errors.
#[tokio::test]
async fn channel_read_normalizes_channel_id_spellings_with_warning() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    engine
        .mail_announce(root, "broadcast body".to_string())
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let group = projection
        .team
        .channels
        .keys()
        .find(|key| key.starts_with("announce-"))
        .unwrap()
        .clone();
    let child: SessionId = projection.team.roster.get(&handle).unwrap().session;

    let doubled = engine
        .read_channel_history(child, &format!("##{group}"), None)
        .await
        .unwrap();
    assert_eq!(doubled.channel, group, "lookup uses the stripped id");
    assert_eq!(doubled.messages.len(), 1);
    assert_eq!(
        doubled.warning.as_deref(),
        Some(format!("normalized channel id `##{group}` → `{group}`").as_str()),
        "a `##`-prefixed read must warn with the exact correction"
    );

    let padded = engine
        .read_channel_history(child, &format!("  {group}  "), None)
        .await
        .unwrap();
    assert_eq!(padded.channel, group, "padded read still finds the channel");
    assert_eq!(
        padded.warning.as_deref(),
        Some(format!("normalized channel id `  {group}  ` → `{group}`").as_str()),
        "a whitespace-padded read must warn too"
    );

    // A clean id stays silent; an unknown id is still an error.
    let clean = engine
        .read_channel_history(child, &group, None)
        .await
        .unwrap();
    assert_eq!(clean.warning, None, "no warning when nothing was stripped");
    let unknown = engine
        .read_channel_history(child, "no-such-channel", None)
        .await;
    assert!(unknown.is_err(), "an unknown channel id must still error");
}

/// Run 6: the lead read `#main/scout-fartooth` — a member handle — to see its
/// DM with that member and got "unknown channel". A member handle (canonical,
/// `#`-prefixed, or a bare leaf) resolves to the caller's DM with that member,
/// with a warning naming the DM id; a real channel id still reads silently;
/// with no such DM the error names the caller's channels and `list_channel`.
#[tokio::test]
async fn channel_read_by_member_handle_opens_the_callers_dm_with_it() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    let projection = engine.read_projection(root).await.unwrap();
    let dm = projection
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Dm
                && channel.members.contains(&handle)
                && channel.members.contains("main")
        })
        .map(|(id, _)| id.clone())
        .expect("registration mints the parent-child DM");

    let leaf = hya_proto::scope::leaf(&handle).to_string();
    for spelling in [format!("#{handle}"), handle.clone(), leaf] {
        let read = engine
            .read_channel_history(root, &spelling, None)
            .await
            .unwrap_or_else(|error| panic!("`{spelling}`: {error}"));
        assert_eq!(read.channel, dm, "`{spelling}` reads the caller's DM");
        let warning = read.warning.unwrap_or_default();
        assert!(
            warning.contains("member handle") && warning.contains(&dm),
            "`{spelling}`: {warning}"
        );
    }
    let direct = engine.read_channel_history(root, &dm, None).await.unwrap();
    assert_eq!(direct.channel, dm);
    assert_eq!(direct.warning, None, "a real channel id reads silently");

    let error = engine
        .read_channel_history(root, "#main/nobody-here", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unknown channel `#main/nobody-here`")
            && error.contains(&dm)
            && error.contains("list_channel"),
        "{error}"
    );
}

/// Harness heartbeats (ADR-0002 liveness): a resident's turn boundaries leave
/// a durable heartbeat on its roster row, so the parent can tell a busy child
/// that is progressing from one that has stalled without polling its DMs.
#[tokio::test]
async fn resident_turn_boundaries_leave_a_heartbeat_on_the_roster() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    // The work-start boundary heartbeat fired (emission is async; poll briefly).
    let mut heartbeat_ms = 0;
    for _ in 0..500 {
        let projection = engine.read_projection(root).await.unwrap();
        heartbeat_ms = projection
            .team
            .roster
            .get(&handle)
            .map_or(0, |entry| entry.heartbeat_ms);
        if heartbeat_ms > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        heartbeat_ms > 0,
        "the work-start heartbeat must land on the roster row"
    );

    // And it is durable: the root log carries the harness event.
    assert!(
        engine
            .replay(root)
            .await
            .unwrap()
            .iter()
            .any(|envelope| matches!(&envelope.event, Event::AgentHeartbeat { .. })),
        "AgentHeartbeat must be on the root log"
    );
}

/// `team_member_status` reports exactly the caller's DIRECT children with
/// live status and heartbeat freshness, computed engine-side (the tool plane
/// has no clock).
#[tokio::test]
async fn team_member_status_reports_direct_children_with_heartbeat_freshness() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_child, handle) = spawn_idle_resident(&supervisor, &engine, root, "work").await;

    let rows = engine.team_member_status(root).await.unwrap();
    assert_eq!(rows.len(), 1, "main sees exactly its one direct child");
    assert_eq!(rows[0].handle, handle);
    assert_eq!(rows[0].status, "idle");
    assert!(
        rows[0].last_active_seconds.is_some(),
        "a heartbeat was observed, so freshness must be present"
    );

    // A resident's own view has no children at all.
    let projection = engine.read_projection(root).await.unwrap();
    let child = projection.team.roster.get(&handle).unwrap().session;
    let rows = engine.team_member_status(child).await.unwrap();
    assert!(rows.is_empty(), "a leaf resident leads nobody");
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
    // From a peer: steer skips the agent's own posts.
    mail_main_from_peer(&engine, root, &long_body).await;
    let mut steer = engine
        .steer_mailbox_snapshot_with_policy(root, Some(steer_everything()))
        .await;
    let notice = steer.drain(&engine).await.unwrap().expect("notice");
    assert!(
        notice.contains("[mail from main/peer-1]") && notice.contains("配置很长"),
        "notice carries the truncated body: {notice}"
    );
}

/// Steer notices name the channel each message arrived through, so the reader
/// can `read channel://<id>` directly instead of guessing id spellings
/// (`##DM-x`, `#dm-x`) — the exact failure observed in a real multi-agent run.
#[tokio::test]
async fn steer_notice_carries_the_channel_id_for_handle_mail() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    // The persistent DM pair minted at the child's registration.
    let projection = engine.read_projection(root).await.unwrap();
    let dm = projection
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Dm
                && channel.members.contains(&handle)
                && channel.members.contains("main")
        })
        .map(|(id, _)| id.clone())
        .expect("registration mints the parent-child DM channel");

    engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "handle-addressed status update".to_string(),
        )
        .await
        .unwrap();
    let mut steer = engine.steer_mailbox_snapshot(root).await;
    let notice = steer.drain(&engine).await.unwrap().expect("notice");
    assert!(
        notice.contains(&format!("@{dm}")),
        "notice must name the DM channel inline: {notice}"
    );
    assert!(
        notice.contains("handle-addressed status update"),
        "notice still carries the body"
    );
}

/// A busy team floods the bus (every member streams deltas): the steer
/// receiver lags and the live `MailSent` is gone from the broadcast buffer.
/// The mail must still be surfaced (and consumed) from the durable inbox —
/// otherwise the report gate stays shut on mail the agent never saw.
#[tokio::test]
async fn steer_resyncs_mail_from_the_log_after_the_bus_lagged() {
    let store = SessionStore::connect_memory().await.unwrap();
    let router = Arc::new(ProviderRouter::new().with(Arc::new(StubProvider)));
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
    let engine = Arc::new(SessionEngine::new(
        store,
        router,
        support::test_runtime(Arc::new(ToolRegistry::builtins())),
        permission,
        EventBus::new(4),
    ));
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, _) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    let mut steer = engine.steer_mailbox_snapshot(root).await;
    engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "LAGGED_MAIL_BODY".to_string(),
        )
        .await
        .unwrap();
    for _ in 0..32 {
        engine.bus().publish(hya_proto::Envelope {
            seq: hya_proto::EventSeq(0),
            ts_millis: 0,
            event: Event::AgentHeartbeat {
                session: root,
                handle: "main".to_string(),
                heartbeat_ms: 1,
            },
        });
    }
    let notice = steer
        .drain(&engine)
        .await
        .unwrap()
        .expect("mail lost to bus lag must be resynced from the log");
    assert!(notice.contains("LAGGED_MAIL_BODY"), "{notice}");
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection.team.roster["main"].resident_cursor,
        projection.team.inboxes["main"].len() as u64,
        "the resynced mail is consumed durably"
    );
}

/// Mail a resident wake already injected into its turn (`inbox_through`) is
/// read: a `report` inside that turn must not be rejected for it.
#[tokio::test]
async fn report_gate_counts_mail_the_current_wake_delivered_as_read() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "follow-up question".to_string(),
        )
        .await
        .unwrap();
    // The wake claimed the inbox through its end and is still working.
    engine
        .store()
        .append_event(
            root,
            &Event::ResidentWorkStarted {
                session: root,
                actor_session: child,
                handle: handle.clone(),
                epoch: hya_proto::ActorEpoch::INITIAL,
                inbox_through: 1,
            },
        )
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    assert!(projection.team.roster[&handle].resident_cursor < 1);
    supervisor
        .report_gate(root, &handle)
        .await
        .expect("mail injected by the current wake is not unread");
}

#[tokio::test]
async fn steer_policy_denial_keeps_real_mail_out_of_the_turn_notice() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, _) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "must stay hidden".to_string(),
        )
        .await
        .unwrap();

    let mut steer = engine
        .steer_mailbox_snapshot_with_policy(root, Some(ChannelPolicySnapshot::default()))
        .await;
    assert!(steer.drain(&engine).await.unwrap().is_none());
    let projection = engine.read_projection(root).await.unwrap();
    assert_eq!(
        projection.team.roster["main"].resident_cursor,
        projection.team.inboxes["main"].len() as u64,
        "denied delivery must be consumed so it cannot block report or replay"
    );
}

#[tokio::test]
async fn report_policy_denial_rejects_the_real_lifecycle_request() {
    let engine = engine().await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, _) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    let (plane, rx) = hya_tool::LifecyclePlane::new();
    tokio::spawn(run_lifecycle_service(engine, supervisor, rx));

    let result = plane
        .for_session(child)
        .with_channel_policy(ChannelPolicySnapshot::default())
        .report(ReportOutcome::Done, "blocked".to_string())
        .await;
    assert!(
        matches!(result, Err(hya_tool::ToolError::Input(message)) if message.contains("channel policy denies report"))
    );
}

#[tokio::test]
async fn resident_mail_policy_denial_does_not_wake_an_idle_resident() {
    let engine = engine_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [send, report, steer, follow_up], scope: vertical, retention: team_session }",
    )
    .await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    let before = engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .len();

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "do not wake".to_string(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .len(),
        before
    );
    let team = engine.read_projection(root).await.unwrap();
    assert_eq!(
        team.team.roster[&handle].resident_cursor,
        team.team.inboxes[&handle].len() as u64,
        "mail denied for idle delivery must be consumed and cannot block report"
    );
}

#[tokio::test]
async fn public_mail_send_uses_the_session_captured_channel_policy() {
    let engine = engine_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: parent}], capabilities: [], scope: vertical, retention: team_session }",
    )
    .await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (_, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;

    let result = engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle),
            MailKind::Message,
            "must be denied".to_string(),
        )
        .await;
    assert!(
        matches!(result, Err(CoreError::Invalid(message)) if message.contains("channel policy denies send"))
    );
}

#[tokio::test]
async fn follow_up_policy_denial_does_not_queue_a_second_resident_turn() {
    let engine = engine_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [send, report, steer, resident_mail], scope: vertical, retention: team_session }",
    )
    .await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let agent = agent_spec();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    let (child, handle) = supervisor
        .spawn_resident(
            root,
            agent,
            (binding, Arc::from([]), resources, None),
            "initial".to_string(),
            None,
            None,
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .roster
            .get(&handle)
            .is_some_and(|entry| entry.status == RosterStatus::Busy)
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "denied follow-up".to_string(),
        )
        .await
        .unwrap();
    wait_idle(&engine, root, &handle, child).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let assistants = engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|message| message.role == hya_proto::Role::Assistant)
        .count();
    assert_eq!(assistants, 1);
}

#[tokio::test]
async fn resident_mail_without_follow_up_is_not_delivered_after_idle_recovery() {
    let engine = engine_with_channel_restriction(
        "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [send, report, steer, resident_mail], scope: vertical, retention: team_session }",
    )
    .await;
    let root = root_team(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    ensure_main(&supervisor, &engine, root).await;
    let (child, handle) = spawn_idle_resident(&supervisor, &engine, root, "explore").await;
    let before = engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .len();

    engine
        .mail_send(
            root,
            MailEndpoint::Handle(handle.clone()),
            MailKind::Message,
            "resident-only must stay hidden".to_string(),
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert_eq!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .len(),
        before
    );
    let team = engine.read_projection(root).await.unwrap();
    assert_eq!(
        team.team.roster[&handle].resident_cursor,
        team.team.inboxes[&handle].len() as u64
    );
}

/// A durable direct mail to `main` from a peer handle (raw log append: the
/// steer backlog reads the folded inbox).
async fn mail_main_from_peer(engine: &SessionEngine, root: SessionId, body: &str) {
    engine
        .store()
        .append_event(
            root,
            &Event::MailSent {
                session: root,
                from: "main/peer-1".to_string(),
                to: MailEndpoint::Handle("main".to_string()),
                kind: MailKind::Message,
                body: body.to_string(),
            },
        )
        .await
        .unwrap();
}

fn steer_everything() -> ChannelPolicySnapshot {
    ChannelPolicySnapshot {
        unit_leader: u8::MAX,
        unit_member: u8::MAX,
        dm_parent: u8::MAX,
        dm_child: u8::MAX,
    }
}
