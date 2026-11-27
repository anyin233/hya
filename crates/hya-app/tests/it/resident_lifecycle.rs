//! Resident composition, sidecar lifecycle, crash recovery, and capacity-lease
//! invariants owned by `hya-app`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::support;

use async_trait::async_trait;
use hya_engine::{BoundSidecarFactory, CoreError, SessionStoreExt as _, SidecarHandle, SidecarStart};
use hya_proto::{Event, FinishReason, Role, ToolPartState};
use hya_tool::extension::{ExtensionEvent, members_projection, team_projection};
use hya_tool::{ArchiveReason, ReportOutcome};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use support::lifecycle::{Gate, Harness, Round, output, request_for};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const COOPERATIVE: Duration = Duration::from_secs(3);

#[tokio::test]
async fn sidecar_loss_flushes_stream_and_reports_failure_to_parent() {
    let test = "sidecar_loss_flushes_stream_and_reports_failure_to_parent";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let mut control = support::lifecycle::SidecarControl::new(false).await;
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let mut live = h.engine.bus().subscribe();
    let (child, handle) = h
        .spawn_agent(
            "m4-sidecar-worker",
            vec![Round::text("SIDECAR_LOSS_TEXT").stream_gated(gate.clone())],
        )
        .await;
    control.started().await;
    gate.arrived().await;
    control.die().await;
    h.until(child, |p| {
        p.session.messages.iter().any(|m| {
            m.finish == Some(FinishReason::Cancelled)
                && m.parts.iter().any(|part| {
                    matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "SIDECAR_LOSS_TEXT")
                })
        })
    })
    .await;
    let waited = h
        .tool("wait", json!({"targets": [handle], "timeout_secs": 1}))
        .await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let replay = h.engine.replay(child).await.unwrap();
    let mut streamed = String::new();
    while let Ok(event) = live.try_recv() {
        if let Event::TextDelta { session, delta, .. } = event.event
            && session == child
            && delta == "SIDECAR_LOSS_TEXT"
        {
            streamed.push_str(&delta);
        }
    }
    control.shutdown().await;
    drop(control);
    h.shutdown().await;

    assert_eq!(streamed, "SIDECAR_LOSS_TEXT");
    assert_eq!(
        replay
            .iter()
            .filter_map(|e| match &e.event {
                Event::TextReplace { text, .. } if text == "SIDECAR_LOSS_TEXT" =>
                    Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        streamed
    );
    let root_team = team_projection(&root);
    let root_members = members_projection(&root);
    assert_eq!(root_team.archived.len(), 1, "{root_members:?}");
    assert!(
        root_members
            .iter()
            .any(|m| m.status == hya_proto::MemberRunStatus::Failed)
    );
    assert!(output(&waited).to_string().contains("sidecar"));
}

#[tokio::test]
async fn sidecar_start_failure_reports_failure_to_parent() {
    let test = "sidecar_start_failure_reports_failure_to_parent";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let path = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
        .join("m4-sidecar-control.json");
    std::fs::write(path, json!({"refuse_start": true}).to_string()).unwrap();
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::text("must not execute")])
        .await;
    h.archived(&handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert_eq!(team_projection(&root).archived[&handle].reason, ArchiveReason::Reported);
    assert!(
        events.iter().any(|e| matches!(
            ExtensionEvent::from_event(&e.event),
            Some(ExtensionEvent::SubagentReported {
                child: reported,
                outcome: ReportOutcome::Failed,
                ..
            }) if reported == child
        )),
        "sidecar startup failed without a parent report"
    );
}

#[tokio::test]
async fn sidecar_failed_episode_revives_with_new_mail_and_preserved_tools() {
    let test = "sidecar_failed_episode_revives_with_new_mail_and_preserved_tools";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let mut control = support::lifecycle::SidecarControl::new(false).await;
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::unfinished("initial")])
        .await;
    control.started().await;
    h.idle(child, &handle).await;
    let first_tools = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.session == child)
        .map(|r| r.completion.tools.clone())
        .expect("sidecar spawn never reached the provider");
    assert!(
        first_tools.iter().any(|tool| tool.name.as_str() == "echo"),
        "spawn did not expose its sidecar tool: {first_tools:?}"
    );

    let completed = h
        .provider
        .script(vec![Round::unfinished("completed mail work")]);
    h.mail(&handle, format!("COMPLETED_BEFORE_LOSS {completed}"))
        .await
        .unwrap();
    h.until(child, |p| {
        p.session.messages.iter().any(|m| {
            m.parts.iter().any(|part| {
                matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "completed mail work")
            })
        })
    })
    .await;
    h.idle(child, &handle).await;
    assert_eq!(
        team_projection(&h.engine.read_projection(h.root).await.unwrap()).roster[&handle].resident_cursor,
        2
    );

    let gate = Arc::new(Gate::default());
    let handled = h.provider.script(vec![
        Round::text("interrupted state").stream_gated(gate.clone()),
    ]);
    h.mail(&handle, format!("INTERRUPTED_BEFORE_LOSS {handled}"))
        .await
        .unwrap();
    gate.arrived().await;
    h.mail(&handle, "UNREAD_BEFORE_LOSS".into()).await.unwrap();
    control.die().await;
    h.archived(&handle).await;

    let doc = hya_tool::extension::handoff_projection(
        &h.engine.read_projection(child).await.unwrap(),
    )
    .unwrap()
    .doc;
    assert!(doc.contains("UNREAD_BEFORE_LOSS"));
    assert!(doc.contains("INTERRUPTED_BEFORE_LOSS"));
    assert!(!doc.contains("COMPLETED_BEFORE_LOSS"));

    let next = h.provider.script(vec![
        Round::tool("echo", json!({})),
        Round::unfinished("new episode finished"),
    ]);
    let mut requests = h.provider.requests.subscribe();
    h.mail(&handle, next.clone()).await.unwrap();
    control.started().await;
    let request = request_for(&mut requests, child).await;
    h.idle(child, &handle).await;

    assert_eq!(
        request.tools, first_tools,
        "revive lost executable bundle sidecar capabilities"
    );
    let users = request
        .messages
        .iter()
        .filter(|m| matches!(m, hya_proto::Message::User { .. }))
        .map(|m| serde_json::to_string(m).unwrap())
        .collect::<String>();
    let systems = request
        .messages
        .iter()
        .filter(|m| matches!(m, hya_proto::Message::System { .. }))
        .map(|m| serde_json::to_string(m).unwrap())
        .collect::<String>();
    assert!(
        !users.contains("COMPLETED_BEFORE_LOSS")
            && !users.contains("INTERRUPTED_BEFORE_LOSS")
            && !users.contains("UNREAD_BEFORE_LOSS"),
        "{users}"
    );
    assert_eq!(users.matches(&next).count(), 1);
    assert_eq!(systems.matches("UNREAD_BEFORE_LOSS").count(), 1);
    let events = h.engine.replay(child).await.unwrap();
    assert!(
        events.iter().any(|e| matches!(
            &e.event,
            Event::ToolResult { output, .. } if output.to_string().contains("sidecar echo")
        )),
        "revived sidecar did not execute its tool"
    );
    h.shutdown().await;
    control.shutdown().await;
}

#[tokio::test]
async fn promptless_catalog_agent_keeps_host_base_after_revive() {
    const BASE: &str = "HOST_BASE_PROMPT_MUST_SURVIVE_REVIVE";
    let mut h = Harness::with_base_prompt(BASE).await;
    let package = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "review-promptless",
        vec![hya_bundle::SourceFile::new(
            "bundle.yaml",
            "kind: Bundle\nschema_version: 2\nidentity: { id: test/review-promptless, version: 1.0.0, publisher: test }\nagents:\n  - id: review-worker\n    role: subagent\n",
        )],
    ))
    .unwrap();
    let catalog = hya_engine::AgentCatalog::new(Arc::new(
        hya_bundle::BundleCatalog::from_verified_catalogs(&[&package]).unwrap(),
    ))
    .unwrap();
    h.engine
        .runtime_registry()
        .publish_catalog(Arc::new(catalog))
        .unwrap();
    assert!(
        h.engine
            .bind_runtime(&h.dir)
            .unwrap()
            .resolve_agent("review-worker")
            .unwrap()
            .prompt
            .is_none()
    );
    let (child, handle) = h
        .spawn_agent("review-worker", vec![Round::unfinished("first episode")])
        .await;
    h.idle(child, &handle).await;
    let first = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.session == child)
        .unwrap()
        .completion
        .system
        .clone()
        .unwrap_or_default();
    output(&h.tool("archive", json!({"target": handle})).await);
    let second = h
        .provider
        .script(vec![Round::unfinished("revived episode")]);
    h.mail(&handle, second).await.unwrap();
    h.idle(child, &handle).await;
    let revived = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r.session == child)
        .unwrap()
        .completion
        .system
        .clone()
        .unwrap_or_default();
    h.shutdown().await;
    assert!(first.contains(BASE), "spawn lost host base: {first}");
    assert!(revived.contains(BASE), "revive lost host base: {revived}");
}

#[tokio::test]
async fn idle_sidecar_loss_drops_handle_and_next_wake_starts_fresh() {
    let mut h = Harness::new().await;
    let probe = Arc::new(support::lifecycle::LossSidecar {
        loss: CancellationToken::new(),
        starts: Arc::new(AtomicUsize::new(0)),
        drops: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(Notify::new()),
    });
    let (child, handle) = h
        .spawn_with_sidecar(vec![Round::unfinished("first activation")], probe.clone())
        .await
        .unwrap();
    h.idle(child, &handle).await;
    let epoch = h.claim_epoch(child).await;
    probe.loss.cancel();
    tokio::time::timeout(support::lifecycle::BOUND, probe.dropped.notified())
        .await
        .unwrap();
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    let events = h.engine.replay(h.root).await.unwrap();
    assert!(!events.iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::SubagentReported { child: reported, .. }) if reported == child
    )));
    assert!(
        h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let marker = h
        .provider
        .script(vec![Round::unfinished("fresh sidecar activation")]);
    h.mail(&handle, marker).await.unwrap();
    h.until(child, |p| {
        p.session.messages.iter().any(|m| {
            m.parts.iter().any(|part| {
                matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "fresh sidecar activation")
            })
        })
    })
    .await;
    h.idle(child, &handle).await;
    assert_eq!(probe.starts.load(Ordering::SeqCst), 2);
    assert_eq!(h.claim_epoch(child).await, epoch);
    h.shutdown().await;
    assert_eq!(probe.drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn running_sidecar_loss_removes_episode_before_archive_publication() {
    let mut h = Harness::new().await;
    let probe = Arc::new(support::lifecycle::LossSidecar {
        loss: CancellationToken::new(),
        starts: Arc::new(AtomicUsize::new(0)),
        drops: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(Notify::new()),
    });
    let gate = Arc::new(Gate::default());
    let mut bus = h.engine.bus().subscribe();
    let (child, _handle) = h
        .spawn_with_sidecar(
            vec![Round::text("flush before failure").stream_gated(gate.clone())],
            probe.clone(),
        )
        .await
        .unwrap();
    gate.arrived().await;
    let epoch = h.claim_epoch(child).await;
    probe.loss.cancel();
    tokio::time::timeout(support::lifecycle::BOUND, async {
        loop {
            let event = bus.recv().await.unwrap();
            if matches!(
                ExtensionEvent::from_event(&event.event),
                Some(ExtensionEvent::AgentArchived { child: archived, .. }) if archived == child
            ) {
                assert_eq!(h.built.resident_supervisor().episode_count(), 0);
                assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(h.claim_epoch(child).await, epoch);
    let root = h.engine.replay(h.root).await.unwrap();
    assert_eq!(
        root.iter()
            .filter(|e| matches!(
                ExtensionEvent::from_event(&e.event),
                Some(ExtensionEvent::SubagentReported {
                    child: reported,
                    outcome: ReportOutcome::Failed,
                    report,
                    ..
                }) if reported == child && report == "activation sidecar lost"
            ))
            .count(),
        1
    );
    let events = h.engine.replay(child).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.event,
        Event::TextReplace { text, .. } if text == "flush before failure"
    )));
    h.shutdown().await;
}

#[derive(Clone)]
struct LateLossSidecar {
    fail_stop: bool,
    dead: Arc<AtomicBool>,
    stopped: Arc<Notify>,
}

#[async_trait]
impl BoundSidecarFactory for LateLossSidecar {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for LateLossSidecar {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.stopped.notify_one();
        if self.fail_stop {
            return Err(CoreError::Invalid("late loss stop failed".into()));
        }
        Ok(())
    }
    fn is_healthy(&self) -> bool {
        !self.dead.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn sidecar_death_after_user_cancel_keeps_episode_held_and_forced_end_keeps_diagnostic() {
    // 1. User cancel followed by sidecar death keeps the episode held without failing it.
    let mut h = Harness::new().await;
    let sidecar = LateLossSidecar {
        fail_stop: false,
        dead: Arc::default(),
        stopped: Arc::default(),
    };
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![Round::text("user-cancelled stream").stream_gated(gate.clone())],
            Arc::new(sidecar.clone()),
        )
        .await
        .unwrap();
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(h.root));
    sidecar.dead.store(true, Ordering::SeqCst);
    tokio::time::timeout(support::lifecycle::BOUND, sidecar.stopped.notified())
        .await
        .unwrap();
    h.until(h.root, |p| {
        let team = team_projection(p);
        team.archived.contains_key(&handle)
            || team
                .roster
                .get(&handle)
                .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
    })
    .await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let held = h.engine.stopped_by_user(child);
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert!(!team_projection(&root).archived.contains_key(&handle));
    assert!(held);
    assert!(!events.iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::SubagentReported { child: reported, .. }) if reported == child
    )));

    // 2. Forced ending preserves a dead sidecar's shutdown diagnostic.
    let mut h = Harness::new().await;
    let sidecar = LateLossSidecar {
        fail_stop: true,
        dead: Arc::default(),
        stopped: Arc::default(),
    };
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![Round::text("interrupted by archive").stream_gated(gate.clone())],
            Arc::new(sidecar.clone()),
        )
        .await
        .unwrap();
    gate.arrived().await;
    sidecar.dead.store(true, Ordering::SeqCst);
    output(&h.tool("archive", json!({"target": handle})).await);
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert!(events.iter().any(|e| matches!(
        ExtensionEvent::from_event(&e.event),
        Some(ExtensionEvent::MemberFinished { child: Some(member), summary, .. })
            if member == child
                && summary.contains("sidecar did not stop")
                && summary.contains("late loss stop failed")
    )));
}

#[derive(Clone)]
struct CompletedLoss {
    loss: CancellationToken,
    stopped: Arc<Notify>,
    checks: Arc<AtomicUsize>,
    lose_at: usize,
}

#[async_trait]
impl BoundSidecarFactory for CompletedLoss {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for CompletedLoss {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.stopped.notify_one();
        Ok(())
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }
    fn is_healthy(&self) -> bool {
        if self.checks.fetch_add(1, Ordering::SeqCst) + 1 < self.lose_at {
            return true;
        }
        self.loss.cancel();
        false
    }
}

#[tokio::test]
async fn completed_turn_loss_preserves_done_report_and_live_episode() {
    for report in [true, false] {
        let mut h = Harness::new().await;
        let gate = Arc::new(Gate::default());
        let stopped = Arc::new(Notify::new());
        let round = if report {
            Round::tool("report", json!({"result": "F1_ACCEPTED_DONE"}))
        } else {
            Round::text("")
        };
        let (child, handle) = h
            .spawn_with_sidecar(
                vec![round.gated(gate.clone())],
                Arc::new(CompletedLoss {
                    loss: CancellationToken::new(),
                    stopped: stopped.clone(),
                    checks: Arc::new(AtomicUsize::new(0)),
                    lose_at: 1,
                }),
            )
            .await
            .unwrap();
        gate.arrived().await;
        gate.release();
        tokio::time::timeout(COOPERATIVE, stopped.notified())
            .await
            .unwrap();
        if report {
            h.archived(&handle).await;
        } else {
            h.until(h.root, |p| {
                let team = team_projection(p);
                team.archived.contains_key(&handle)
                    || team
                        .roster
                        .get(&handle)
                        .is_some_and(|e| e.status == hya_proto::RosterStatus::Idle)
            })
            .await;
        }
        let projection = h.engine.read_projection(h.root).await.unwrap();
        let replay = h.engine.replay(child).await.unwrap();
        h.shutdown().await;
        assert!(replay.iter().any(|e| matches!(
            e.event,
            Event::MessageFinished {
                role: Role::Assistant,
                finish: FinishReason::Stop,
                ..
            }
        )));
        if report {
            assert!(
                members_projection(&projection)
                    .iter()
                    .any(|m| m.child == Some(child) && m.status == hya_proto::MemberRunStatus::Done)
            );
        } else {
            assert!(team_projection(&projection).roster.contains_key(&handle));
        }
    }
}

#[derive(Clone)]
struct StoppingLoss {
    loss: CancellationToken,
    stopping: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl BoundSidecarFactory for StoppingLoss {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for StoppingLoss {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.stopping.notify_one();
        self.release.notified().await;
        Ok(())
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }
}

#[tokio::test]
async fn root_teardown_during_loss_sidecar_stop_wins_without_report_mail() {
    let mut h = Harness::new().await;
    let sidecar = Arc::new(StoppingLoss {
        loss: CancellationToken::new(),
        stopping: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let turn = Arc::new(Gate::default());
    let (child, _) = h
        .spawn_with_sidecar(
            vec![Round::text("loss drain").stream_gated(turn.clone())],
            sidecar.clone(),
        )
        .await
        .unwrap();
    turn.arrived().await;
    sidecar.loss.cancel();
    tokio::time::timeout(COOPERATIVE, sidecar.stopping.notified())
        .await
        .unwrap();
    let mut events = h.engine.bus().subscribe();
    let delete = tokio::spawn({
        let engine = h.engine.clone();
        let root = h.root;
        async move { engine.delete_session(root).await }
    });
    tokio::time::timeout(COOPERATIVE, async {
        while h.built.resident_supervisor().team_cancel(h.root).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    sidecar.release.notify_one();
    assert!(
        tokio::time::timeout(COOPERATIVE, delete)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    );
    let mut tail = Vec::new();
    while let Ok(e) = events.try_recv() {
        tail.push(e.event);
    }
    h.shutdown().await;
    assert!(tail.iter().any(|e| matches!(
        ExtensionEvent::from_event(e),
        Some(ExtensionEvent::AgentArchived {
            child: actor,
            reason: ArchiveReason::RootTeardown,
            ..
        }) if actor == child
    )));
    assert!(
        !tail
            .iter()
            .any(|e| matches!(
                ExtensionEvent::from_event(e),
                Some(ExtensionEvent::MailSent { .. } | ExtensionEvent::SubagentReported { .. })
            ))
    );
}

#[derive(Clone)]
struct DefaultHealthProbe {
    loss: CancellationToken,
    observed: Arc<AtomicBool>,
}

#[async_trait]
impl BoundSidecarFactory for DefaultHealthProbe {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(self.clone()))
    }
}

#[async_trait]
impl SidecarHandle for DefaultHealthProbe {
    async fn ready(&mut self) -> Result<(), CoreError> {
        self.observed.store(self.is_healthy(), Ordering::SeqCst);
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        Some(self.loss.clone())
    }
}

struct CachedLossFactory {
    engine: std::sync::Weak<hya_engine::SessionEngine>,
    actor: Arc<std::sync::Mutex<Option<hya_proto::SessionId>>>,
    starts: Arc<AtomicUsize>,
    idle: Arc<Notify>,
}

struct CachedLossHandle {
    engine: std::sync::Weak<hya_engine::SessionEngine>,
    actor: Arc<std::sync::Mutex<Option<hya_proto::SessionId>>>,
    first: bool,
    armed: AtomicBool,
    loss: CancellationToken,
    idle: Arc<Notify>,
}

#[async_trait]
impl BoundSidecarFactory for CachedLossFactory {
    async fn start(&self, _: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        Ok(Box::new(CachedLossHandle {
            engine: self.engine.clone(),
            actor: self.actor.clone(),
            first: self.starts.fetch_add(1, Ordering::SeqCst) == 0,
            armed: AtomicBool::new(false),
            loss: CancellationToken::new(),
            idle: self.idle.clone(),
        }))
    }
}

#[async_trait]
impl SidecarHandle for CachedLossHandle {
    async fn ready(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), CoreError> {
        Ok(())
    }
    fn loss_token(&self) -> Option<CancellationToken> {
        if self.first
            && let Some(actor) = *self.actor.lock().unwrap()
        {
            if !self.engine.upgrade().unwrap().turn_active(actor) {
                self.armed.store(true, Ordering::SeqCst);
                self.idle.notify_one();
            } else if self.armed.load(Ordering::SeqCst) {
                self.loss.cancel();
            }
        }
        Some(self.loss.clone())
    }
}

#[tokio::test]
async fn sidecar_default_health_and_cached_loss_replacement() {
    // 1. Default health check rejects an already-cancelled loss token.
    let mut h = Harness::new().await;
    let observed = Arc::new(AtomicBool::new(true));
    let loss = CancellationToken::new();
    loss.cancel();
    let (_, handle) = h
        .spawn_with_sidecar(
            vec![Round::text("never run")],
            Arc::new(DefaultHealthProbe {
                loss,
                observed: observed.clone(),
            }),
        )
        .await
        .unwrap();
    h.archived(&handle).await;
    h.shutdown().await;
    assert!(!observed.load(Ordering::SeqCst));

    // 2. A cached handle that dies before the next turn's health check is replaced cleanly.
    let mut h = Harness::new().await;
    let actor = Arc::new(std::sync::Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let idle = Arc::new(Notify::new());
    let initial = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![Round::unfinished("first activation").gated(initial.clone())],
            Arc::new(CachedLossFactory {
                engine: Arc::downgrade(&h.engine),
                actor: actor.clone(),
                starts: starts.clone(),
                idle: idle.clone(),
            }),
        )
        .await
        .unwrap();
    initial.arrived().await;
    *actor.lock().unwrap() = Some(child);
    initial.release();
    h.idle(child, &handle).await;
    tokio::time::timeout(COOPERATIVE, idle.notified())
        .await
        .unwrap();
    let epoch = h.claim_epoch(child).await;
    let fresh = Arc::new(Gate::default());
    let marker = h.provider.script(vec![
        Round::text("fresh cached activation").gated(fresh.clone()),
    ]);
    h.mail(&handle, marker).await.unwrap();
    let entered = tokio::time::timeout(COOPERATIVE, fresh.reached()).await;
    fresh.release();
    let start_count = starts.load(Ordering::SeqCst);
    let same_epoch = h.claim_epoch(child).await == epoch;
    h.shutdown().await;
    assert!(entered.is_ok());
    assert_eq!(start_count, 2);
    assert!(same_epoch);
}

#[tokio::test]
async fn shutdown_reconciles_orphan_and_drop_releases_engine_ownership() {
    let mut h = Harness::new().await;
    let weak = Arc::downgrade(&h.engine);
    let (child, handle) = h
        .spawn(vec![Round::unfinished("orphan before shutdown")])
        .await;
    h.idle(child, &handle).await;
    let epoch = h.claim_epoch(child).await;
    h.trigger("agent_archived").await;
    assert!(matches!(
        h.tool("archive", json!({"target": handle})).await,
        ToolPartState::Error { .. }
    ));
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.heal().await;
    h.shutdown().await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(
        team_projection(&root).archived[&handle].reason,
        ArchiveReason::Shutdown
    );
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(h.claim_epoch(child).await, epoch + 1);
    drop(h);
    assert!(
        weak.upgrade().is_none(),
        "runtime shutdown left a strong engine owner"
    );
}

#[tokio::test]
async fn live_capacity_survives_process_crash_and_enforces_revive() {
    let test = "live_capacity_survives_process_crash_and_enforces_revive";
    support::lifecycle::crash_seed_if_requested(test, false).await;
    if support::lifecycle::isolated_config(test, &[("HYA_SUBAGENT_BUDGET", "2")]) {
        return;
    }
    let mut h = support::lifecycle::crashed_team(test).await;
    assert_eq!(h.engine.store().active_actor_ids().await.unwrap().len(), 2);

    // Budget (2) is full after crash recovery -> spawning a third member overloads.
    let overloaded = h
        .tool(
            "task",
            json!({"description": "C", "prompt": "C", "subagent_type": "hya-task", "model": "fake"}),
        )
        .await;
    assert!(
        serde_json::to_string(&overloaded)
            .unwrap()
            .to_lowercase()
            .contains("overload"),
        "restart lost live leases: {overloaded:?}"
    );

    // Archive one member, take its slot with a fresh spawn, then verify reviving it overloads.
    let first = team_projection(&h.engine.read_projection(h.root).await.unwrap())
        .roster
        .keys()
        .find(|k| k.as_str() != "main")
        .unwrap()
        .clone();
    output(&h.tool("archive", json!({"target": first})).await);
    let (c, third) = h.spawn(vec![Round::unfinished("C")]).await;
    h.idle(c, &third).await;
    assert!(matches!(
        h.mail(&first, "must consume capacity".into()).await,
        Err(CoreError::CapacityExceeded { .. })
    ));
    let rejected_send = h
        .tool(
            "send",
            json!({"channel": first, "body": "must consume capacity"}),
        )
        .await;
    assert!(
        matches!(&rejected_send, ToolPartState::Error { value: Some(value), .. } if value["error"]["type"] == "overloaded"),
        "{rejected_send:?}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn restore_prompt_precedes_queued_recovered_main_work() {
    let test = "restore_prompt_precedes_queued_recovered_main_work";
    support::lifecycle::crash_seed_if_requested(test, true).await;
    let mut h = support::lifecycle::crashed_team(test).await;
    let state = hya_server::AppState::new(h.engine.clone(), Arc::new(h.agent.clone()))
        .with_auto_title(false)
        .with_pure_guidance(true);
    let mut observed = h.provider.requests.subscribe();
    let restore_gate = Arc::new(Gate::default());
    h.provider
        .unscripted
        .lock()
        .unwrap()
        .push_back(Round::text("RESTORE_ANSWER").gated(restore_gate.clone()));

    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.built.resume_interrupted(&state).await.unwrap();
    restore_gate.arrived().await;
    assert!(h.engine.turn_active(h.root));
    assert_eq!(h.built.resident_supervisor().episode_count(), 2);
    assert_eq!(
        h.provider
            .captured
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.session == h.root && !r.completion.tools.is_empty())
            .count(),
        1
    );
    restore_gate.release();

    let mail_request = tokio::time::timeout(support::lifecycle::BOUND, async {
        loop {
            let request = request_for(&mut observed, h.root).await;
            let latest = request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m, hya_proto::Message::User { .. }));
            if latest.is_some_and(|m| {
                serde_json::to_string(m)
                    .unwrap()
                    .contains("QUEUED_BEFORE_RESTORE")
            }) {
                break request;
            }
        }
    })
    .await
    .expect("persisted root mail was never driven after restore");
    assert!(
        !serde_json::to_string(
            mail_request
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m, hya_proto::Message::User { .. }))
                .unwrap()
        )
        .unwrap()
        .contains("Your previous turn was interrupted by a restart.")
    );
    h.until(h.root, |p| {
        p.session
            .messages
            .last()
            .is_some_and(|m| m.role == Role::Assistant && m.finish.is_some())
    })
    .await;
    state
        .stop_for_restart(Duration::from_secs(5))
        .await
        .unwrap();
    let requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.session == h.root && !r.completion.tools.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    h.shutdown().await;
    assert!(!requests.is_empty());
    assert!(
        serde_json::to_string(&requests[0].completion.messages)
            .unwrap()
            .contains("Your previous turn was interrupted by a restart.")
    );
    let projection = h.engine.read_projection(h.root).await.unwrap();
    let restores: Vec<_> = projection
        .session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.role == Role::User
                && m.parts.iter().any(|part| {
                    matches!(
                        part,
                        hya_proto::PartProjection::Text { text, .. }
                            if text.contains("Your previous turn was interrupted by a restart.")
                    )
                })
        })
        .collect();
    assert_eq!(restores.len(), 1);
    let (index, restore) = restores[0];
    let answers: Vec<_> = projection.session.messages[index + 1..]
        .iter()
        .take_while(|m| m.role != Role::User)
        .filter(|m| m.role == Role::Assistant)
        .collect();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].finish, Some(FinishReason::Stop));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r
                .completion
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m, hya_proto::Message::User { .. }))
                .is_some_and(|m| m.id() == restore.id))
            .count(),
        1
    );
}

#[tokio::test]
async fn start_recovered_preserves_actor_ended_during_restore() {
    let test = "start_recovered_preserves_actor_ended_during_restore";
    support::lifecycle::crash_seed_if_requested(test, false).await;
    let mut h = support::lifecycle::crashed_team(test).await;
    let supervisor = h.built.resident_supervisor();
    let root = h.engine.read_projection(h.root).await.unwrap();
    let (handle, ended) = team_projection(&root)
        .roster
        .iter()
        .find(|(handle, _)| handle.as_str() != "main")
        .map(|(handle, entry)| (handle.clone(), entry.session))
        .unwrap();
    supervisor.end_member(h.root, &handle).await.unwrap();
    let started = supervisor.start_recovered().await;
    let episodes = supervisor.episode_count();
    let projection = h.engine.read_projection(h.root).await.unwrap();
    let active = h.engine.store().active_actor_ids().await.unwrap();
    h.shutdown().await;
    assert_eq!(started.unwrap(), 1);
    assert_eq!(episodes, 1);
    assert!(team_projection(&projection).archived.contains_key(&handle));
    assert!(!active.contains(&ended));
}

#[tokio::test]
async fn with_built_engine_starts_recovery_before_body() {
    let test = "with_built_engine_starts_recovery_before_body";
    support::lifecycle::crash_seed_if_requested(test, false).await;
    let mut h = support::lifecycle::crashed_team(test).await;
    let replacement = hya_app::build_session_engine_pure(
        hya_store::SessionStore::connect_memory().await.unwrap(),
        hya_provider::ProviderRouter::new().with(h.provider.clone()),
        &h.agent,
        Default::default(),
        Vec::new(),
        Default::default(),
    )
    .await
    .unwrap();
    let built = std::mem::replace(&mut h.built, replacement);
    let count = hya_app::with_built_session_engine(built, |built| {
        std::future::ready(Ok::<_, CoreError>(
            built.resident_supervisor().episode_count(),
        ))
    })
    .await
    .unwrap();
    h.shutdown().await;
    assert_eq!(count, 2, "the embedding body received staged actors");
}

#[tokio::test]
async fn workflow_stage_loop_worker_and_verifier_are_residents() {
    let mut h = Harness::new().await;
    let stage = h
        .provider
        .script(vec![Round::text("stage settled without report")]);
    let worker = h
        .provider
        .script(vec![Round::text("loop worker completed")]);
    let result = h
        .workflow(
            &format!(
                "  stage:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {stage}\n  loop:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {worker}\n    mode: loop\n    verify:\n      agent: hya-reviewer\n      model: {{ id: fake }}\n      until: Work is complete.\n      max_iterations: 1\n"
            ),
            "  stage --> loop",
        )
        .await;
    let events = h.engine.replay(h.root).await.unwrap();
    let registrations: Vec<_> = events
        .iter()
        .filter_map(|e| match ExtensionEvent::from_event(&e.event) {
            Some(ExtensionEvent::AgentRegistered {
                agent_session,
                mode,
                ..
            }) if agent_session != h.root => Some((agent_session, mode)),
            _ => None,
        })
        .collect();
    h.shutdown().await;
    assert!(
        registrations.len() >= 3,
        "stage, worker, verifier must execute: {result:?}; {registrations:?}"
    );
    assert!(
        registrations.iter().all(|(_, mode)| mode.is_resident()),
        "Workflow still uses transient execution: {registrations:?}"
    );
}
