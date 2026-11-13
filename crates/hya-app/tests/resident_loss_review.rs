//! M4-4 review regressions through the app's public activation/turn/mail paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;

use async_trait::async_trait;
use hya_core::{BoundSidecarFactory, CoreError, SidecarHandle, SidecarStart};
use hya_proto::{
    ArchiveReason, Event, FinishCause, FinishReason, MailEndpoint, MailKind, ReportOutcome, Role,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;
use support::lifecycle::{Gate, Harness, Round};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const COOPERATIVE: Duration = Duration::from_secs(3);

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

async fn completed_turn_loss(report: bool) {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let stopped = Arc::new(Notify::new());
    let round = if report {
        Round::tool("report", json!({"result":"F1_ACCEPTED_DONE"}))
    } else {
        Round::text("F1_COMPLETED_WORK")
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
            p.team.archived.contains_key(&handle)
                || p.team
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
            projection
                .session
                .members
                .iter()
                .any(|m| m.child == Some(child) && m.status == hya_proto::MemberRunStatus::Done),
            "completed report overwritten: {:?}",
            projection.session.members
        );
    } else {
        assert!(
            projection.team.roster.contains_key(&handle),
            "completed work was archived as loss: {:?}",
            projection.team.archived
        );
    }
}

#[tokio::test]
async fn f1_loss_after_completed_report_keeps_done() {
    completed_turn_loss(true).await;
}
#[tokio::test]
async fn f1_loss_after_completed_turn_keeps_episode() {
    completed_turn_loss(false).await;
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
async fn f2_root_teardown_during_loss_sidecar_stop_wins_without_report_mail() {
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
    assert!(tail.iter().any(|e| matches!(e, Event::AgentArchived {child: actor, reason: ArchiveReason::RootTeardown, ..} if *actor==child)), "teardown lost to sidecar failure: {tail:?}");
    assert!(
        !tail
            .iter()
            .any(|e| matches!(e, Event::MailSent { .. } | Event::SubagentReported { .. })),
        "teardown emitted report mail: {tail:?}"
    );
}

#[tokio::test]
async fn f3_team_cancel_closes_main_straggler_before_releasing_lease() {
    let plugin = support::lifecycle::BlockingPlugin::for_hook("chat.params").await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let child_gate = Arc::new(Gate::default());
    let (child, _) = h
        .spawn(vec![
            Round::text("busy child").stream_gated(child_gate.clone()),
        ])
        .await;
    child_gate.arrived().await;
    plugin.arm_session(h.root);
    h.engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".into()),
            MailKind::Message,
            "F3_MAIN_WAKE".into(),
        )
        .await
        .unwrap();
    plugin.gate.arrived().await;
    h.built
        .resident_supervisor()
        .team_cancel(h.root)
        .unwrap()
        .cancel();
    assert!(
        h.engine
            .wait_turn_idle(h.root, Duration::from_secs(7))
            .await
    );
    let root = h.engine.read_projection(h.root).await.unwrap();
    let answer = root.session.messages.last().unwrap().clone();
    plugin.gate.release();
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert_eq!(answer.role, Role::Assistant);
    assert_eq!(
        answer.finish,
        Some(FinishReason::Cancelled),
        "main straggler was dropped open"
    );
    assert_eq!(answer.cause, Some(FinishCause::Shutdown));
}

async fn report_after_consumed_progress(failed: bool) {
    let mut h = Harness::new().await;
    let root_gate = Arc::new(Gate::default());
    let tool = Arc::new(support::lifecycle::BlockingWrite {
        gate: root_gate.clone(),
        path: h.dir.join("f4-control"),
    });
    let source = hya_core::RuntimeSource::new(
        hya_core::RuntimeSourceId::plugin("f4-control"),
        [64; 32],
        Arc::new(()),
        vec![hya_core::RuntimeSourceExport::tool(
            "m4__blocking_write",
            "m4__blocking_write",
            Vec::new(),
            tool,
            hya_tool::ToolPermission::Tool,
        )],
    );
    h.engine
        .runtime_registry()
        .refresh(|candidate| candidate.upsert_sources(vec![source]))
        .unwrap();
    let child_gate = Arc::new(Gate::default());
    let round = if failed {
        Round::failure("F4_FAILURE_REPORT")
    } else {
        Round::tool("report", json!({"result":"F4_FINAL_REPORT"}))
    };
    let (child, handle) = h.spawn(vec![round.gated(child_gate.clone())]).await;
    child_gate.arrived().await;
    let root_turn = h.start_tool("m4__blocking_write", json!({})).await;
    root_gate.arrived().await;
    h.engine
        .mail_send(
            child,
            MailEndpoint::Handle("main".into()),
            MailKind::Message,
            "F4_PROGRESS".into(),
        )
        .await
        .unwrap();
    root_gate.release();
    tokio::time::timeout(COOPERATIVE, root_turn)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        h.engine.read_projection(h.root).await.unwrap().team.roster["main"].resident_cursor,
        2
    );
    let mut requests = h.provider.requests.subscribe();
    child_gate.release();
    h.archived(&handle).await;
    let wake = tokio::time::timeout(COOPERATIVE, async {
        loop {
            let request = requests.recv().await.unwrap();
            if request.session == h.root
                && serde_json::to_string(&request.completion.messages)
                    .unwrap()
                    .contains(if failed {
                        "F4_FAILURE_REPORT"
                    } else {
                        "F4_FINAL_REPORT"
                    })
            {
                break;
            }
        }
    })
    .await;
    h.shutdown().await;
    assert!(wake.is_ok(), "terminal mail lost its team before routing");
}
#[tokio::test]
async fn f4_report_after_consumed_progress_wakes_main() {
    report_after_consumed_progress(false).await;
}
#[tokio::test]
async fn f4_failure_after_consumed_progress_wakes_main() {
    report_after_consumed_progress(true).await;
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
#[tokio::test]
async fn f5_activation_default_health_rejects_a_cancelled_loss_token() {
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
    let healthy = observed.load(Ordering::SeqCst);
    h.shutdown().await;
    assert!(
        !healthy,
        "SidecarHandle's default health ignored an already-fired loss token"
    );
}

fn register_blocking_write(h: &Harness, gate: Arc<Gate>) {
    let source = hya_core::RuntimeSource::new(
        hya_core::RuntimeSourceId::plugin("f1-control"),
        [61; 32],
        Arc::new(()),
        vec![hya_core::RuntimeSourceExport::tool(
            "m4__blocking_write",
            "m4__blocking_write",
            Vec::new(),
            Arc::new(support::lifecycle::BlockingWrite {
                gate,
                path: h.dir.join("f1-control"),
            }),
            hya_tool::ToolPermission::Tool,
        )],
    );
    h.engine
        .runtime_registry()
        .refresh(|candidate| candidate.upsert_sources(vec![source]))
        .unwrap();
}

#[tokio::test]
async fn f1_empty_steered_wake_with_lost_cached_sidecar_stays_live() {
    let mut h = Harness::new().await;
    let tool_gate = Arc::new(Gate::default());
    register_blocking_write(&h, tool_gate.clone());
    let stopped = Arc::new(Notify::new());
    let checks = Arc::new(AtomicUsize::new(0));
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![
                Round::tool("m4__blocking_write", json!({})),
                Round::text("finished steered work"),
            ],
            Arc::new(CompletedLoss {
                loss: CancellationToken::new(),
                stopped: stopped.clone(),
                checks: checks.clone(),
                lose_at: 3,
            }),
        )
        .await
        .unwrap();
    tool_gate.arrived().await;
    let (barrier_child, barrier_handle) = h.spawn(vec![Round::text("listener barrier idle")]).await;
    h.idle(barrier_child, &barrier_handle).await;
    let barrier = Arc::new(Gate::default());
    let marker = h.provider.script(vec![
        Round::text("listener passed target mail").gated(barrier.clone()),
    ]);
    h.mail(&handle, "already steered into the tool result".into())
        .await
        .unwrap();
    h.mail(&barrier_handle, marker).await.unwrap();
    barrier.arrived().await;
    tool_gate.release();
    tokio::time::timeout(COOPERATIVE, stopped.notified())
        .await
        .unwrap();
    support::lifecycle::output(
        &h.tool("wait", json!({"targets":[handle],"timeout_secs":1}))
            .await,
    );
    h.until(h.root, |p| {
        p.team.archived.contains_key(&handle)
            || p.team
                .roster
                .get(&handle)
                .is_some_and(|r| r.status == hya_proto::RosterStatus::Idle)
    })
    .await;
    let projection = h.engine.read_projection(h.root).await.unwrap();
    let count = h
        .engine
        .replay(child)
        .await
        .unwrap()
        .iter()
        .filter(|e| {
            matches!(
                e.event,
                Event::MessageStarted {
                    role: Role::Assistant,
                    ..
                }
            )
        })
        .count();
    barrier.release();
    h.shutdown().await;
    assert!(
        checks.load(Ordering::SeqCst) >= 3,
        "the empty follow-up did not reach its post-turn health check"
    );
    assert!(
        projection.team.roster.contains_key(&handle),
        "empty wake failed the episode"
    );
    assert_eq!(projection.team.roster[&handle].resident_cursor, 2);
    assert_eq!(count, 1, "empty follow-up must start no assistant turn");
}

#[tokio::test]
async fn f1_report_in_a_turn_interrupted_by_loss_still_fails() {
    let plugin = support::lifecycle::BlockingPlugin::for_hook("tool.execute.after").await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let tool_gate = Arc::new(Gate::default());
    register_blocking_write(&h, tool_gate.clone());
    let sidecar = Arc::new(support::lifecycle::LossSidecar {
        loss: CancellationToken::new(),
        starts: Arc::new(AtomicUsize::new(0)),
        drops: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(Notify::new()),
    });
    plugin.arm();
    let round = Round {
        steps: vec![
            hya_provider::FakeStep::ToolCall {
                name: "report".into(),
                input: json!({"result":"must not commit done"}),
            },
            hya_provider::FakeStep::ToolCall {
                name: "m4__blocking_write".into(),
                input: json!({}),
            },
            hya_provider::FakeStep::Finish(FinishReason::ToolCalls),
        ],
        gate: None,
        stream_gate: None,
        error: None,
    };
    let (child, handle) = h
        .spawn_with_sidecar(vec![round], sidecar.clone())
        .await
        .unwrap();
    // The report tool body submitted Done; tool two remains blocked.
    plugin.gate.arrived().await;
    plugin.gate.release();
    tool_gate.arrived().await;
    sidecar.loss.cancel();
    h.archived(&handle).await;
    let events = h.engine.replay(h.root).await.unwrap();
    let turns = h.engine.replay(child).await.unwrap();
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert!(events.iter().any(|e| matches!(e.event,Event::SubagentReported {child:actor,outcome:ReportOutcome::Failed,..} if actor==child)));
    assert!(!events.iter().any(|e| matches!(e.event,Event::SubagentReported {child:actor,outcome:ReportOutcome::Done,..} if actor==child)));
    assert!(turns.iter().any(|e| matches!(
        e.event,
        Event::MessageFinished {
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            ..
        }
    )));
}

struct CachedLossFactory {
    engine: std::sync::Weak<hya_core::SessionEngine>,
    actor: Arc<std::sync::Mutex<Option<hya_proto::SessionId>>>,
    starts: Arc<AtomicUsize>,
    idle: Arc<Notify>,
}
struct CachedLossHandle {
    engine: std::sync::Weak<hya_core::SessionEngine>,
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
            let active = self.engine.upgrade().unwrap().turn_active(actor);
            if !active {
                self.armed.store(true, Ordering::SeqCst);
                self.idle.notify_one();
            } else if self.armed.load(Ordering::SeqCst) {
                // The transport dies after the next lease is taken, before the
                // cached handle's pre-turn health check can accept it.
                self.loss.cancel();
            }
        }
        Some(self.loss.clone())
    }
}
#[tokio::test]
async fn f5_cached_loss_is_replaced_before_the_next_turn() {
    let mut h = Harness::new().await;
    let actor = Arc::new(std::sync::Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let idle = Arc::new(Notify::new());
    let initial = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![Round::text("first activation").gated(initial.clone())],
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
    assert!(
        entered.is_ok(),
        "cached loss failed the episode instead of replacing the handle"
    );
    assert_eq!(start_count, 2);
    assert!(same_epoch, "idle loss must not replace the episode claim");
}
