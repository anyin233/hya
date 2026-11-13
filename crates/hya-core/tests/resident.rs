//! Phase 4 resident-lifecycle tests (ADR-0002): event-driven wake, quiescence +
//! main synthesis, and the per-team message-budget kill. All drive the offline
//! `DevProvider` (no network), which emits exactly one assistant message per turn.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hya_core::{
    AgentSpec, CreateSession, EventBus, ResidentSupervisor, SessionEngine, SubagentGovernor,
    SubagentLimits,
};
use hya_proto::{AgentName, MailEndpoint, MailKind, ModelRef, Role, RosterStatus, SessionId};
use hya_provider::{DevProvider, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

async fn engine_with(limits: SubagentLimits) -> (Arc<SessionEngine>, AgentSpec) {
    let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
    let tools = Arc::new(ToolRegistry::builtins());
    let (perm, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = Arc::new(
        SessionEngine::new(
            store,
            router,
            support::test_runtime(tools),
            perm,
            EventBus::default(),
        )
        .with_governor(SubagentGovernor::new(limits)),
    );
    // Catalog-backed id: resident turns exact-resolve session AgentName; "worker"
    // is a roster handle only and is not a Bundle definition.
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: "x".to_string(),
        workdir: PathBuf::from("/tmp"),
        reasoning: None,
    };
    (engine, agent)
}

async fn make_root(engine: &SessionEngine) -> SessionId {
    engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn make_child(engine: &SessionEngine, root: SessionId) -> SessionId {
    engine
        .create(CreateSession {
            parent: Some(root),
            agent: AgentName::new("hya-task"),
            model: ModelRef::new("hya/offline"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap()
}

async fn assistant_turns(engine: &SessionEngine, session: SessionId) -> usize {
    engine
        .read_projection(session)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .count()
}

/// Roster status by **leaf** name. The roster is keyed by canonical path
/// (`main/worker-1`); all members in these tests are direct reports of main.
async fn roster_status(
    engine: &SessionEngine,
    root: SessionId,
    handle: &str,
) -> Option<RosterStatus> {
    let team = engine.read_projection(root).await.unwrap().team;
    team.roster.get(&format!("main/{handle}")).map(|e| e.status)
}

/// Poll `cond` against a fresh projection until it holds or the deadline passes.
async fn wait_until<F>(engine: &SessionEngine, root: SessionId, mut cond: F) -> bool
where
    F: FnMut(&hya_proto::Projection) -> bool,
{
    for _ in 0..200 {
        let projection = engine.read_projection(root).await.unwrap();
        if cond(&projection) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// A resident idle with no mail runs no turn; a single inbound mail wakes it for
/// EXACTLY one turn, after which it returns to idle.
#[tokio::test]
async fn resident_wakes_for_exactly_one_turn_then_idles() {
    let (engine, agent) = engine_with(SubagentLimits::default()).await;
    let root = make_root(&engine).await;
    let worker = make_child(&engine, root).await;
    let supervisor = ResidentSupervisor::start(engine.clone());

    // Born idle: no initial directive, so it parks at zero cost and runs no turn.
    supervisor
        .register_existing_resident(root, worker, "worker-1".to_string(), agent, None)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        assistant_turns(&engine, worker).await,
        0,
        "an idle resident with no mail must not run any turn"
    );

    // One inbound mail wakes it.
    engine
        .mail_send(
            root,
            MailEndpoint::Handle("worker-1".to_string()),
            MailKind::Message,
            "please look at this".to_string(),
        )
        .await
        .unwrap();

    // Wait for the mail-triggered turn to complete AND the resident to be idle
    // again (roster status returns to Idle after the single turn).
    let mut settled = false;
    for _ in 0..300 {
        if assistant_turns(&engine, worker).await == 1
            && roster_status(&engine, root, "worker-1").await == Some(RosterStatus::Idle)
        {
            settled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        settled,
        "resident should run exactly one turn for the mail and return to idle"
    );

    // Give any erroneous extra turn a chance to appear, then confirm there is none.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        assistant_turns(&engine, worker).await,
        1,
        "exactly one turn per wake — no spurious extra turns"
    );
}

/// Two residents that each run their initial turn and go idle drive the team to
/// quiescence, which wakes the main agent to synthesize WITHOUT user input.
#[tokio::test]
async fn quiescence_wakes_main_to_synthesize() {
    let (engine, agent) = engine_with(SubagentLimits::default()).await;
    let root = make_root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());

    // Main-as-actor must be registered so quiescence has someone to wake.
    // Catalog-derived root roster/resource policy (root session agent is "build").
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let root_agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let root_resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, root_agents, root_resources),
            None,
            None,
        )
        .await
        .unwrap();

    // Two residents, each with an initial directive → each runs one turn, then idles.
    let a = make_child(&engine, root).await;
    let b = make_child(&engine, root).await;
    supervisor
        .register_existing_resident(
            root,
            a,
            "worker-1".to_string(),
            agent.clone(),
            Some("do part A".to_string()),
        )
        .await
        .unwrap();
    supervisor
        .register_existing_resident(
            root,
            b,
            "worker-2".to_string(),
            agent.clone(),
            Some("do part B".to_string()),
        )
        .await
        .unwrap();

    // Quiescence fires: main is woken and its transcript carries the synthesis
    // directive in one owned User message plus an assistant turn.
    let woke = {
        let mut ok = false;
        for _ in 0..300 {
            let proj = engine.read_projection(root).await.unwrap();
            let has_directive = proj.session.messages.iter().any(|m| {
                matches!(m.role, Role::User)
                    && m.parts.iter().any(|p| matches!(
                        p,
                        hya_proto::PartProjection::Text { text, .. } if text.contains("TEAM QUIESCED")
                    ))
            });
            let main_assistant = proj
                .session
                .messages
                .iter()
                .filter(|m| matches!(m.role, Role::Assistant))
                .count();
            if has_directive && main_assistant >= 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ok
    };
    assert!(
        woke,
        "quiescence must wake main with an owned synthesis prompt"
    );

    // Both residents finished idle and are quiescent. Root teardown
    // (`force_archive_team`) is an explicit driver-owned call at the end of an
    // interactive root turn, so a parked quiescent team stays parked.
    assert_eq!(
        roster_status(&engine, root, "worker-1").await,
        Some(RosterStatus::Idle)
    );
    assert_eq!(
        roster_status(&engine, root, "worker-2").await,
        Some(RosterStatus::Idle)
    );

    // Termination: the team does not re-synthesize forever. Main's synthesis turn
    // produces no new work, so it settles at a small, bounded number of turns.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let main_turns = assistant_turns(&engine, root).await;
    assert!(
        main_turns <= 3,
        "main must settle (no infinite re-synthesis); ran {main_turns} turns"
    );
}

/// Preparing a report notice without committing its tool result leaves the
/// report unread, so the lead still owes a wake after its current turn.
#[tokio::test]
async fn preparing_a_report_notice_keeps_the_lead_wake_owed() {
    let (engine, agent) = engine_with(SubagentLimits::default()).await;
    let root = make_root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let lease = engine.try_begin_turn(root).unwrap();
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, agents, resources),
            None,
            None,
        )
        .await
        .unwrap();

    let child = make_child(&engine, root).await;
    supervisor
        .register_existing_resident(root, child, "worker-1".to_string(), agent, None)
        .await
        .unwrap();
    engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".to_string()),
            MailKind::Message,
            "worker failed; no result".to_string(),
        )
        .await
        .unwrap();
    supervisor
        .archive_member(root, "worker-1", "failed")
        .await
        .unwrap();
    let mut steer = engine
        .steer_mailbox_snapshot_with_policy(
            root,
            Some(hya_tool::ChannelPolicySnapshot {
                unit_leader: u8::MAX,
                unit_member: u8::MAX,
                dm_parent: u8::MAX,
                dm_child: u8::MAX,
            }),
        )
        .await;
    let notice = steer.drain(&engine).await.unwrap().expect("report notice");
    assert!(notice.contains("worker failed; no result"));
    drop(lease);

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        assistant_turns(&engine, root).await,
        1,
        "preparing a notice must leave the report owed to a lead wake"
    );
}

/// A mail loop that exceeds the per-team message budget kills the whole team: the
/// team cancel token fires and every member is archived.
#[tokio::test]
async fn message_budget_kill_cancels_the_team() {
    let (engine, agent) = engine_with(SubagentLimits {
        per_team_message_budget: 3,
        ..SubagentLimits::default()
    })
    .await;
    let root = make_root(&engine).await;
    let supervisor = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let root_agents = engine
        .agent_roster_for_binding(&binding, "hya-main")
        .unwrap();
    let root_resources = engine
        .agent_resource_policy_for_binding(&binding, "hya-main")
        .unwrap();
    supervisor
        .ensure_main(
            root,
            agent.clone(),
            (binding, root_agents, root_resources),
            None,
            None,
        )
        .await
        .unwrap();

    let a = make_child(&engine, root).await;
    let b = make_child(&engine, root).await;
    supervisor
        .register_existing_resident(root, a, "worker-1".to_string(), agent.clone(), None)
        .await
        .unwrap();
    supervisor
        .register_existing_resident(root, b, "worker-2".to_string(), agent.clone(), None)
        .await
        .unwrap();

    let cancel = supervisor.team_cancel(root).expect("team is tracked");
    assert!(!cancel.is_cancelled(), "team starts live");

    // Drive a vertical mail loop past the budget of 3 (ADR-0016: sibling DMs
    // no longer exist, so the loop runs between the root and worker-1).
    // Stop at the first violation: later downward mail can revive a killed member.
    for i in 0..4 {
        let (from, to) = if i % 2 == 0 {
            (root, "worker-1")
        } else {
            (a, "main")
        };
        let _ = engine
            .mail_send(
                from,
                MailEndpoint::Handle(to.to_string()),
                MailKind::Message,
                format!("ping {i}"),
            )
            .await;
    }

    // The team is killed: its cancel fires and members enter the archive.
    let killed = {
        let mut ok = false;
        for _ in 0..300 {
            if cancel.is_cancelled() {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        ok
    };
    assert!(killed, "exceeding the message budget must cancel the team");

    assert!(
        wait_until(&engine, root, |p| {
            ["main/worker-1", "main/worker-2"].iter().all(|handle| {
                !p.team.roster.contains_key(*handle) && p.team.archived.contains_key(*handle)
            })
        })
        .await,
        "killed members leave the live roster and can be revived"
    );
}

#[tokio::test]
async fn registered_initial_task_survives_claim_takeover_before_first_turn() {
    let (engine, agent) = engine_with(SubagentLimits::default()).await;
    let root = make_root(&engine).await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let child = make_child(&engine, root).await;
    let child_turn = engine.try_begin_turn(child).unwrap();
    let handle = "main/worker-1".to_string();
    let supervisor = ResidentSupervisor::start(engine.clone());
    supervisor
        .register_existing_resident(
            root,
            child,
            "worker-1".into(),
            agent.clone(),
            Some("durable initial task".into()),
        )
        .await
        .unwrap();
    let previous_cancel = supervisor.team_cancel(root).unwrap();
    assert_eq!(assistant_turns(&engine, child).await, 0);
    assert!(
        engine.read_projection(root).await.unwrap().team.roster[&handle]
            .resident_work
            .is_none()
    );
    // Fence the old episode while real turn leases keep its directive queued.
    // Cancelling first is a graceful ending, not a process-loss simulation.
    let recovered = engine
        .store()
        .recover_claim(child, hya_proto::OwnerRunId::new())
        .await
        .unwrap();
    let disposition = engine
        .recover_resident_actor(&recovered, root, &handle)
        .await
        .unwrap();
    assert!(
        matches!(disposition.work, hya_core::ResidentRecovery::Queued { .. }),
        "initial task recovery after cancellation: {:?}",
        disposition.work
    );
    previous_cancel.cancel();
    drop(supervisor);
    let supervisor = ResidentSupervisor::start(engine.clone());
    let binding = engine.bind_runtime(&agent.workdir).unwrap();
    let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
    supervisor
        .register_recovered_resident(
            root,
            handle,
            agent,
            (binding, Arc::from([]), resources, None),
            recovered,
            disposition.work,
        )
        .await
        .unwrap();
    drop(child_turn);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let projection = engine.read_projection(child).await.unwrap();
            if projection.session.messages.iter().any(|message| {
                message.role == Role::Assistant
                    && message.parts.iter().any(|part| {
                        matches!(part,
                        hya_proto::PartProjection::Text { text, .. }
                            if text.contains("durable initial task"))
                    })
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(root_turn);
    supervisor
        .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(5))
        .await;
}

#[tokio::test]
async fn spawn_directive_and_followup_are_parent_dm_mail_even_under_restrictive_policy() {
    let package = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "deny-dm-wakes",
        vec![hya_bundle::SourceFile::new("bundle.yaml", "kind: Bundle\nidentity: { id: test/deny-dm-wakes, version: 1.0.0, publisher: test }\nchannels:\n  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }\n")],
    )).unwrap();
    let catalog = hya_core::AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_verified_catalogs(&[&package]).unwrap(),
    ))
    .unwrap();
    let runtime = Arc::new(hya_core::RuntimeRegistry::new(
        ToolRegistry::builtins(),
        Arc::new(catalog),
    ));
    let (permission, _) = PermissionPlane::new(PermissionRules::default());
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
        runtime,
        permission,
        EventBus::default(),
    ));
    let root = make_root(&engine).await;
    let root_turn = engine.try_begin_turn(root).unwrap();
    let child = make_child(&engine, root).await;
    let child_turn = engine.try_begin_turn(child).unwrap();
    let supervisor = ResidentSupervisor::start(engine.clone());
    let agent = AgentSpec {
        name: AgentName::new("hya-task"),
        model: ModelRef::new("hya/offline"),
        system_prompt: String::new(),
        workdir: "/tmp".into(),
        reasoning: None,
    };
    supervisor
        .register_existing_resident(
            root,
            child,
            "worker".into(),
            agent,
            Some("inspect parser".into()),
        )
        .await
        .unwrap();
    let projection = engine.read_projection(root).await.unwrap();
    let mail = &projection.team.inboxes["main/worker"][0];
    assert_eq!(mail.from, "main");
    assert_eq!(mail.body, "inspect parser");
    let MailEndpoint::Channel(dm) = &mail.to else {
        panic!("directive must name its DM channel")
    };
    assert_eq!(
        projection.team.channels[dm].kind,
        hya_proto::ChannelKind::Dm
    );
    assert!(
        engine
            .read_projection(child)
            .await
            .unwrap()
            .session
            .messages
            .is_empty()
    );
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "also inspect tests".into(),
        )
        .await
        .unwrap();
    drop(child_turn);
    tokio::time::timeout(Duration::from_secs(5), async {
        while assistant_turns(&engine, child).await == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        wait_until(&engine, root, |p| p.team.roster["main/worker"]
            .resident_work
            .is_none())
        .await
    );
    let actor = engine.read_projection(child).await.unwrap();
    let prompts: Vec<_> = actor
        .session
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .collect();
    assert_eq!(prompts.len(), 1);
    assert!(
        matches!(&prompts[0].parts[0], hya_proto::PartProjection::Text { text, .. }
        if text == "[mail from main] inspect parser\n\n[mail from main] also inspect tests")
    );
    engine
        .mail_send(
            root,
            MailEndpoint::Channel(dm.clone()),
            MailKind::Message,
            "parent followup".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while assistant_turns(&engine, child).await < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(root_turn);
    supervisor
        .drain(hya_proto::FinishCause::Shutdown, Duration::from_secs(5))
        .await;
}
