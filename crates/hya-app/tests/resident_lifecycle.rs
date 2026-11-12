//! M4 lifecycle invariants exercised through production app/tool entry points.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use hya_proto::{ArchiveReason, Event, ToolPartState};
use serde_json::json;
use std::sync::Arc;
use support::lifecycle::{Gate, Harness, Round, output, request_for};

fn report(text: &str) -> Round {
    Round::tool("report", json!({"result": text}))
}

#[tokio::test]
async fn i1_stale_session_start_cannot_remove_revived_episode() {
    let plugin = support::lifecycle::BlockingPlugin::new().await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    plugin.arm();
    let first = h
        .provider
        .script(vec![Round::text("old initializer must not run")]);
    let spawning = h.start_tool("task", json!({"description": "gated startup", "prompt": first, "subagent_type": "hya-task", "model": "fake"})).await;
    plugin.gate.arrived().await;
    let projection = h.engine.read_projection(h.root).await.unwrap();
    let (handle, child) = projection
        .team
        .roster
        .iter()
        .find(|(path, _)| path.as_str() != "main")
        .map(|(path, entry)| (path.clone(), entry.session))
        .unwrap();
    assert!(h.engine.cancel_user_turn(h.root));
    tokio::time::timeout(support::lifecycle::BOUND, spawning)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let epoch = h.claim_epoch(child).await;
    output(&h.tool("archive", json!({"target": handle})).await);
    assert_eq!(
        h.claim_epoch(child).await,
        epoch,
        "a live initializer must end under its own claim, without Takeover"
    );
    let revived = h.provider.script(vec![Round::text("revived episode")]);
    h.mail(&handle, revived).await.unwrap();
    plugin.gate.release();
    h.until(child, |p| p.session.messages.iter().any(|m| m.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "revived episode")))).await;
    h.idle(child, &handle).await;
    let child_events = h.engine.replay(child).await.unwrap();
    assert!(!child_events.iter().any(|e| matches!(&e.event, Event::TextReplace { text, .. } if text == "old initializer must not run")));
    assert_eq!(
        child_events
            .iter()
            .filter(
                |e| matches!(&e.event, Event::TextReplace { text, .. } if text == "revived episode")
            )
            .count(),
        1
    );
    assert!(
        h.engine
            .read_projection(h.root)
            .await
            .unwrap()
            .team
            .roster
            .contains_key(&handle)
    );
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
async fn i8_parent_archive_flushes_cooperative_stream_before_terminal_write() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let mut live = h.engine.bus().subscribe();
    let (child, handle) = h
        .spawn(vec![
            Round::text("STREAMED_BEFORE_ARCHIVE").stream_gated(gate.clone()),
        ])
        .await;
    gate.arrived().await;
    output(&h.tool("archive", json!({"target": handle})).await);
    let mut streamed = String::new();
    while let Ok(envelope) = live.try_recv() {
        if let Event::TextDelta { session, delta, .. } = envelope.event
            && session == child
            && delta == "STREAMED_BEFORE_ARCHIVE"
        {
            streamed.push_str(&delta);
        }
    }
    let events = h.engine.replay(child).await.unwrap();
    let replayed: String = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::TextReplace { text, .. } if text == "STREAMED_BEFORE_ARCHIVE" => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "STREAMED_BEFORE_ARCHIVE");
    assert_eq!(replayed, streamed);
    assert!(events.iter().any(|e| matches!(
        &e.event,
        Event::MessageFinished {
            role: hya_proto::Role::Assistant,
            finish: hya_proto::FinishReason::Cancelled,
            cause: Some(hya_proto::FinishCause::Archived),
            ..
        }
    )));
    h.shutdown().await;
}

async fn observe_sidecar_loss() -> (
    hya_proto::Projection,
    Vec<hya_proto::Envelope>,
    ToolPartState,
    String,
) {
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
    h.until(child, |p| p.session.messages.iter().any(|m| m.finish == Some(hya_proto::FinishReason::Cancelled) && m.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "SIDECAR_LOSS_TEXT")))).await;
    let waited = h
        .tool("wait", json!({"targets":[handle], "timeout_secs":1}))
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
    (root, replay, waited, streamed)
}

#[tokio::test]
async fn i8_sidecar_loss_flushes_cooperative_stream_before_failure() {
    let test = "i8_sidecar_loss_flushes_cooperative_stream_before_failure";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    let (_, replay, _, live) = observe_sidecar_loss().await;
    assert_eq!(live, "SIDECAR_LOSS_TEXT");
    assert_eq!(
        replay
            .iter()
            .filter_map(|e| match &e.event {
                Event::TextReplace { text, .. } if text == "SIDECAR_LOSS_TEXT" =>
                    Some(text.as_str()),
                _ => None,
            })
            .collect::<String>(),
        live
    );
}

#[tokio::test]
async fn i6_sidecar_loss_reports_failure_to_parent() {
    let test = "i6_sidecar_loss_reports_failure_to_parent";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    let (root, _, waited, _) = observe_sidecar_loss().await;
    assert_eq!(
        root.team.archived.len(),
        1,
        "sidecar loss recovered a live episode instead of reporting failure: {:?}",
        root.session.members
    );
    assert!(
        root.session
            .members
            .iter()
            .any(|m| m.status == hya_proto::MemberRunStatus::Failed)
    );
    assert!(output(&waited).to_string().contains("sidecar"));
}

async fn blocked_sidecar() -> (
    Harness,
    support::lifecycle::BlockingPlugin,
    support::lifecycle::SidecarControl,
    hya_proto::SessionId,
    String,
) {
    support::lifecycle::install_sidecar_bundle().await;
    let mut control = support::lifecycle::SidecarControl::new(true).await;
    let plugin = support::lifecycle::BlockingPlugin::for_hook("chat.params").await;
    let h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let initial = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_agent(
            "m4-sidecar-worker",
            vec![Round::text("initial activation").gated(initial.clone())],
        )
        .await;
    control.started().await;
    initial.arrived().await;
    // Keep setup quiescence from spending a team turn before the blocked wake.
    let lead = h.engine.try_begin_turn(h.root).unwrap();
    initial.release();
    h.idle(child, &handle).await;
    plugin.arm_session(child);
    let marker = h
        .provider
        .script(vec![Round::text("cancel-ignoring activation hook")]);
    h.mail(&handle, marker).await.unwrap();
    plugin.gate.arrived().await;
    drop(lead);
    (h, plugin, control, child, handle)
}

#[tokio::test]
async fn i2_archive_bounds_stuck_turn_and_sidecar_termination() {
    let test = "i2_archive_bounds_stuck_turn_and_sidecar_termination";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    let (mut h, plugin, mut control, child, handle) = blocked_sidecar().await;
    let epoch = h.claim_epoch(child).await;
    let began = std::time::Instant::now();
    let mut events = h.engine.bus().subscribe();
    let mut ending = h.start_tool("archive", json!({"target":handle})).await;
    let (completion, closed_after) = tokio::join!(
        tokio::time::timeout(support::lifecycle::BOUND, &mut ending),
        tokio::time::timeout(support::lifecycle::BOUND, async {
            loop {
                let event = events.recv().await.unwrap();
                if matches!(event.event, Event::MessageFinished {
                    session, role: hya_proto::Role::Assistant,
                    finish: hya_proto::FinishReason::Cancelled,
                    cause: Some(hya_proto::FinishCause::Archived), ..
                } if session == child)
                {
                    break began.elapsed();
                }
            }
        })
    );
    let bounded = completion.is_ok();
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert!(
        closed_after.unwrap() >= std::time::Duration::from_secs(5),
        "straggler was dropped before its cancellation deadline"
    );
    assert_eq!(
        h.claim_epoch(child).await,
        epoch,
        "straggler close must retain the episode claim"
    );
    assert!(!h.engine.turn_active(child));
    let close = h.engine.replay(child).await.unwrap();
    assert!(close.iter().any(|e| matches!(
        &e.event,
        Event::MessageFinished {
            role: hya_proto::Role::Assistant,
            finish: hya_proto::FinishReason::Cancelled,
            cause: Some(hya_proto::FinishCause::Archived),
            ..
        }
    )));
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    plugin.gate.release();
    control.shutdown().await;
    drop(control);
    if !bounded {
        tokio::time::timeout(support::lifecycle::BOUND, ending)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert!(bounded, "archive exceeded drain + sidecar-stop deadlines");
    assert_eq!(
        root.team.archived[&handle].reason,
        ArchiveReason::ArchivedByParent
    );
    assert!(
        root.session
            .members
            .iter()
            .any(|m| m.child == Some(child) && m.summary.contains("sidecar did not stop")),
        "ending omitted unresponsive sidecar diagnostic"
    );
}

#[tokio::test]
async fn i18_shutdown_bounds_stuck_turn_and_sidecar_and_leaves_no_claim() {
    let test = "i18_shutdown_bounds_stuck_turn_and_sidecar_and_leaves_no_claim";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    let (mut h, plugin, mut control, child, handle) = blocked_sidecar().await;
    let bounded = tokio::time::timeout(support::lifecycle::BOUND, h.built.shutdown())
        .await
        .is_ok();
    let root = h.engine.read_projection(h.root).await.unwrap();
    let active = h.engine.store().active_actor_ids().await.unwrap();
    plugin.gate.release();
    control.shutdown().await;
    drop(control);
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert!(bounded, "shutdown exceeded drain + sidecar-stop deadlines");
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Shutdown);
    assert!(
        !active.contains(&child),
        "shutdown left an active claim for restart recovery"
    );
}

#[tokio::test]
async fn i5_post_kill_task_uses_fresh_team_without_waiting_for_other_endings() {
    let test = "i5_post_kill_task_uses_fresh_team_without_waiting_for_other_endings";
    if support::lifecycle::isolated_sidecar_config_with(test, &[("HYA_SUBAGENT_TURN_BUDGET", "3")])
    {
        return;
    }
    let (mut h, plugin, mut control, _, stuck) = blocked_sidecar().await;
    let killed = h.built.resident_supervisor().team_cancel(h.root).unwrap();
    let (other, handle) = h.spawn(vec![Round::text("another real activation")]).await;
    h.until(other, |p| {
        p.session
            .messages
            .iter()
            .any(|m| m.role == hya_proto::Role::Assistant && m.finish.is_some())
    })
    .await;
    if !killed.is_cancelled() {
        h.mail(
            &handle,
            "one more real activation trips the turn budget".into(),
        )
        .await
        .unwrap();
    }
    tokio::time::timeout(support::lifecycle::BOUND, killed.cancelled())
        .await
        .unwrap();
    let mut requests = h.provider.requests.subscribe();
    let directive = h
        .provider
        .script(vec![Round::text("fresh member finished")]);
    let marker = h.provider.script(vec![Round::tool("task", json!({"description":"FRESH_LEAD_TASK", "prompt":directive, "subagent_type":"hya-task", "model":"fake"}))]);
    let prompt = h.engine.admit_user_prompt(h.root, marker).await.unwrap();
    let mut task = tokio::spawn({
        let engine = h.engine.clone();
        let root = h.root;
        let agent = h.agent.clone();
        async move {
            engine
                .run_turn_with_external_dirs_and_guidance(
                    root,
                    &agent,
                    tokio_util::sync::CancellationToken::new(),
                    &[],
                    Some(Arc::from("NEW_TEAM_GUIDANCE_M4")),
                    None,
                    Some(prompt),
                )
                .await
        }
    });
    let fast = tokio::time::timeout(std::time::Duration::from_secs(1), &mut task)
        .await
        .is_ok();
    let fresh = h.engine.read_projection(h.root).await.unwrap();
    let guidance = if fast {
        tokio::time::timeout(support::lifecycle::BOUND, async {
            loop {
                let request = requests.recv().await.unwrap();
                if request.session == h.root
                    && request.completion.messages.last().is_some_and(|m| {
                        matches!(m, hya_proto::Message::User { .. })
                            && serde_json::to_string(m).unwrap().contains("TEAM QUIESCED")
                    })
                {
                    break request
                        .completion
                        .system
                        .is_some_and(|system| system.contains("NEW_TEAM_GUIDANCE_M4"));
                }
            }
        })
        .await
        .unwrap_or(false)
    } else {
        false
    };
    // New work was checked while the old member was still ending. Revival
    // targets its archived identity, after that member's own bounded ending.
    h.archived(&stuck).await;
    assert_eq!(
        h.engine
            .read_projection(h.root)
            .await
            .unwrap()
            .team
            .archived[&stuck]
            .reason,
        ArchiveReason::Killed
    );
    let revive_marker = h.provider.script(vec![report("revived after kill")]);
    let mut revive = tokio::spawn({
        let engine = h.engine.clone();
        let root = h.root;
        let stuck = stuck.clone();
        async move {
            engine
                .mail_send(
                    root,
                    hya_proto::MailEndpoint::parse(&stuck),
                    hya_proto::MailKind::Message,
                    revive_marker,
                )
                .await
        }
    });
    let revived_within_bound = tokio::time::timeout(support::lifecycle::BOUND, &mut revive)
        .await
        .is_ok();
    plugin.gate.release();
    control.shutdown().await;
    drop(control);
    if !fast {
        tokio::time::timeout(support::lifecycle::BOUND, task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    if !revived_within_bound {
        tokio::time::timeout(support::lifecycle::BOUND, revive)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert!(
        fast,
        "new task waited for an unrelated killed episode's cleanup"
    );
    assert!(
        fresh
            .session
            .members
            .iter()
            .any(|m| m.description == "FRESH_LEAD_TASK"
                && m.status == hya_proto::MemberRunStatus::Running),
        "post-kill task reused old capacity counters"
    );
    assert!(
        guidance,
        "new main reused the killed team's activation context"
    );
    assert!(
        revived_within_bound,
        "same-member revive exceeded its ending bound"
    );
}

#[tokio::test]
async fn i6_sidecar_start_failure_reports_failure_to_parent() {
    let test = "i6_sidecar_start_failure_reports_failure_to_parent";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let path = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
        .join("m4-sidecar-control.json");
    std::fs::write(path, json!({"refuse_start":true}).to_string()).unwrap();
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::text("must not execute")])
        .await;
    h.archived(&handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Reported);
    assert!(events.iter().any(|e| matches!(&e.event, Event::SubagentReported { child:reported, outcome:hya_proto::ReportOutcome::Failed, .. } if *reported == child)), "sidecar startup failed without a parent report");
}

#[tokio::test]
async fn i23_user_hold_refuses_mail_until_explicit_steer() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![Round::text("in progress").stream_gated(gate.clone())])
        .await;
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(child));
    h.until(child, |p| {
        p.session
            .messages
            .last()
            .is_some_and(|m| m.finish == Some(hya_proto::FinishReason::Cancelled))
    })
    .await;
    assert!(
        h.mail(&handle, "parent may not resume user-stopped work".into())
            .await
            .is_err()
    );
    let next = h.provider.script(vec![report("steered to completion")]);
    h.engine.steer_subagent(child, &next).await.unwrap();
    h.archived(&handle).await;
    h.shutdown().await;
}

#[tokio::test]
async fn i16_cancelling_archive_caller_does_not_cancel_the_ending() {
    let plugin = support::lifecycle::BlockingPlugin::for_hook("chat.params").await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let (child, handle) = h.spawn(vec![Round::text("first activation")]).await;
    h.idle(child, &handle).await;
    plugin.arm_session(child);
    let marker = h.provider.script(vec![Round::text("blocked in plugin")]);
    h.mail(&handle, marker).await.unwrap();
    plugin.gate.arrived().await;
    let archiving = h.start_tool("archive", json!({"target": handle})).await;
    // This durable close proves archive has requested the child's ending,
    // rather than cancelling the lead before its tool was dispatched.
    h.until(child, |p| {
        p.session
            .messages
            .last()
            .is_some_and(|m| m.cause == Some(hya_proto::FinishCause::Archived))
    })
    .await;
    assert!(h.engine.cancel_user_turn(h.root));
    tokio::time::timeout(support::lifecycle::BOUND, archiving)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    plugin.gate.release();
    h.archived(&handle).await;
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
async fn i15_one_owed_wake_admits_one_coalesced_owned_prompt() {
    let mut h = Harness::new().await;
    let first = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![Round::text("initial activation").gated(first.clone())])
        .await;
    first.arrived().await;
    let next = Arc::new(Gate::default());
    let marker = h.provider.script(vec![
        Round::text("coalesced activation").gated(next.clone()),
    ]);
    h.mail(&handle, "COALESCE_FIRST".into()).await.unwrap();
    h.mail(&handle, format!("COALESCE_SECOND {marker}"))
        .await
        .unwrap();
    let mut requests = h.provider.requests.subscribe();
    first.release();
    next.arrived().await;
    let request = request_for(&mut requests, child).await;
    let mail_prompts = request
        .messages
        .iter()
        .filter(|m| {
            matches!(m, hya_proto::Message::User { .. })
                && serde_json::to_string(m).unwrap().contains("COALESCE_")
        })
        .collect::<Vec<_>>();
    next.release();
    h.until(h.root, |p| {
        p.team
            .roster
            .get(&handle)
            .is_some_and(|entry| entry.resident_cursor == 2)
    })
    .await;
    let requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|request| {
            request.session == child
                && serde_json::to_string(&request.completion.messages)
                    .unwrap()
                    .contains("COALESCE_")
        })
        .count();
    h.shutdown().await;
    assert_eq!(requests, 1, "two queued mails must produce one turn");
    assert_eq!(
        mail_prompts.len(),
        1,
        "one wake admitted multiple prompts: {mail_prompts:?}"
    );
    let text = serde_json::to_string(mail_prompts[0]).unwrap();
    assert!(text.contains("COALESCE_FIRST") && text.contains("COALESCE_SECOND"));
}

#[tokio::test]
async fn i15_failed_prompt_admission_rolls_back_work_started() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("initial activation")]).await;
    h.idle(child, &handle).await;
    let count = |events: &[hya_proto::Envelope]| {
        events.iter().filter(|e| matches!(&e.event, Event::ResidentWorkStarted { actor_session, .. } if *actor_session == child)).count()
    };
    let before = count(&h.engine.replay(h.root).await.unwrap());
    let before_child = h.engine.replay(child).await.unwrap();
    h.reject_wake_prompt(child).await;
    h.mail(&handle, "rejected wake".into()).await.unwrap();
    h.archived(&handle).await;
    let root_events = h.engine.replay(h.root).await.unwrap();
    let after = count(&root_events);
    let mut before_archive = hya_proto::Projection::default();
    for event in &root_events {
        if matches!(&event.event, Event::AgentArchived { handle: target, .. } if target == &handle)
        {
            break;
        }
        before_archive.apply(event);
    }
    assert_eq!(
        before_archive.team.roster[&handle].resident_cursor, 0,
        "failed admission must leave mail unread"
    );
    let after_child = h.engine.replay(child).await.unwrap();
    assert_eq!(
        after_child
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::MessageStarted {
                    role: hya_proto::Role::User,
                    ..
                }
            ))
            .count(),
        before_child
            .iter()
            .filter(|e| matches!(
                e.event,
                Event::MessageStarted {
                    role: hya_proto::Role::User,
                    ..
                }
            ))
            .count(),
        "failed admission must leave no orphan prompt"
    );
    h.heal().await;
    h.shutdown().await;
    assert_eq!(
        after, before,
        "work-start committed without its admitted prompt"
    );
}

#[tokio::test]
async fn i15_cancelled_coalesced_hook_leaves_mail_unread() {
    let plugin = support::lifecycle::BlockingPlugin::for_hook("message.user.before").await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let initial = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![
            Round::text("initial activation").gated(initial.clone()),
        ])
        .await;
    initial.arrived().await;
    h.mail(&handle, "HOOK_FIRST".into()).await.unwrap();
    h.mail(&handle, "HOOK_SECOND".into()).await.unwrap();
    plugin.arm_session(child);
    initial.release();
    plugin.gate.arrived().await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.roster[&handle].resident_cursor, 0);
    assert!(
        root.team.roster[&handle].resident_work.is_none(),
        "hooks must precede the work boundary"
    );
    assert!(
        !h.engine
            .replay(child)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(&e.event, Event::TextDelta { delta, .. } if delta.contains("HOOK_"))),
        "blocked hooks must hold no admitted prompt"
    );
    let archived = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        h.tool("archive", json!({"target": handle})),
    )
    .await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let actor = h.engine.read_projection(child).await.unwrap();
    plugin.gate.release();
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    output(&archived.expect("archive must complete before releasing the coalesced hook"));
    assert!(root.team.archived.contains_key(&handle));
    assert_eq!(root.team.inboxes[&handle].len(), 2);
    assert!(!actor.session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("HOOK_")))));
}

#[tokio::test]
async fn harness_blocking_tool_writes_only_when_released() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let path = h.dir.join("tool-completed");
    let tool = Arc::new(support::lifecycle::BlockingWrite {
        gate: gate.clone(),
        path: path.clone(),
    });
    let source = hya_core::RuntimeSource::new(
        hya_core::RuntimeSourceId::plugin("m4-test-tool"),
        [43; 32],
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
    let running = h.start_tool("m4__blocking_write", json!({})).await;
    gate.arrived().await;
    assert!(!path.exists());
    gate.release();
    tokio::time::timeout(support::lifecycle::BOUND, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "completed");
    h.shutdown().await;
}

#[tokio::test]
async fn i11_revive_skips_prior_mail_and_handoff_retains_unread_mail() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("initial work")]).await;
    h.idle(child, &handle).await;
    let busy = Arc::new(Gate::default());
    let old = h
        .provider
        .script(vec![Round::text("consumed old mail").gated(busy.clone())]);
    h.mail(&handle, format!("OLD_CONSUMED {old}"))
        .await
        .unwrap();
    busy.arrived().await;
    h.mail(&handle, "UNREAD_AT_ARCHIVE".into()).await.unwrap();
    output(&h.tool("archive", json!({"target": handle})).await);
    let previous = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .map(|m| m.id)
        .collect::<std::collections::BTreeSet<_>>();
    let next = h.provider.script(vec![Round::text("next episode")]);
    let mut requests = h.provider.requests.subscribe();
    h.mail(&handle, next).await.unwrap();
    let request = request_for(&mut requests, child).await;
    let user_text = request
        .messages
        .iter()
        .filter(|m| matches!(m, hya_proto::Message::User { id, .. } if !previous.contains(id)))
        .map(|m| serde_json::to_string(m).unwrap())
        .collect::<String>();
    let handoff = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .handoff
        .unwrap()
        .doc;
    h.shutdown().await;
    assert!(
        !user_text.contains("OLD_CONSUMED") && !user_text.contains("UNREAD_AT_ARCHIVE"),
        "prior mail was re-delivered: {user_text}"
    );
    assert!(
        handoff.contains("UNREAD_AT_ARCHIVE"),
        "unread mail missing from handoff: {handoff}"
    );
}

#[tokio::test]
async fn i12_revive_preserves_executable_bundle_sidecar_tools() {
    let test = "i12_revive_preserves_executable_bundle_sidecar_tools";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![report("episode one")])
        .await;
    h.archived(&handle).await;
    let initial = h.engine.read_projection(h.root).await.unwrap();
    let first = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.session == child)
        .map(|r| r.completion.tools.clone());
    let Some(first) = first else {
        h.shutdown().await;
        panic!(
            "sidecar spawn never reached the provider: {:?}",
            initial.session.members
        );
    };
    assert!(
        first.iter().any(|tool| tool.name.as_str() == "echo"),
        "spawn did not expose its sidecar tool: {first:?}"
    );
    let marker = h.provider.script(vec![report("episode two")]);
    h.mail(&handle, marker).await.unwrap();
    h.archived(&handle).await;
    let requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.session == child)
        .cloned()
        .collect::<Vec<_>>();
    let root = h.engine.read_projection(h.root).await.unwrap();
    h.shutdown().await;
    assert!(
        requests.len() >= 2,
        "revive could not start a provider round with its sidecar capabilities: {:?}",
        root.session.members
    );
    assert_eq!(
        requests[1].completion.tools, first,
        "revive lost spawn capabilities"
    );
}

#[tokio::test]
async fn i6_report_wait_and_search_use_real_tools() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![report("completed work")]).await;
    h.archived(&handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Reported);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let waited = h
        .tool("wait", json!({"targets": [handle], "timeout_secs": 0}))
        .await;
    assert!(
        output(&waited).to_string().contains("completed work"),
        "{waited:?}"
    );
    let found = h.tool("search_agent", json!({"query": ""})).await;
    assert!(output(&found).to_string().contains(&handle), "{found:?}");
    let child_events = h.engine.replay(child).await.unwrap();
    assert!(child_events.iter().any(
        |e| matches!(&e.event, Event::ToolCallRequested { name, .. } if name.as_str() == "report")
    ));
    h.shutdown().await;
}

#[tokio::test]
async fn i6_parent_archive_has_no_report_mail() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("working")]).await;
    h.idle(child, &handle).await;
    let before = h.engine.replay(h.root).await.unwrap().len();
    let archived = h
        .tool(
            "archive",
            json!({"target": handle, "reason": "parent cancelled"}),
        )
        .await;
    output(&archived);
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(
        root.team.archived[&handle].reason,
        ArchiveReason::ArchivedByParent
    );
    assert!(
        !h.engine.replay(h.root).await.unwrap()[before..]
            .iter()
            .any(|e| matches!(&e.event, Event::SubagentReported { .. }))
    );
    h.shutdown().await;
}

#[tokio::test]
async fn i6_turn_error_reports_failure_to_parent() {
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn(vec![Round::failure("M4 provider turn failure")])
        .await;
    h.archived(&handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let events = h.engine.replay(h.root).await.unwrap();
    let waited = h
        .tool("wait", json!({"targets": [handle], "timeout_secs": 0}))
        .await;
    h.shutdown().await;
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Reported);
    assert!(events.iter().any(|e| matches!(&e.event, Event::SubagentReported { child: reported, outcome: hya_proto::ReportOutcome::Failed, .. } if *reported == child)));
    assert!(
        output(&waited)
            .to_string()
            .contains("M4 provider turn failure")
    );
}

#[tokio::test]
async fn i6_user_archive_records_parent_archive_reason() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    h.engine.archive_subagent(child).await.unwrap();
    let root = h.engine.read_projection(h.root).await.unwrap();
    h.shutdown().await;
    assert_eq!(
        root.team.archived[&handle].reason,
        ArchiveReason::ArchivedByParent
    );
    assert!(
        root.session
            .members
            .iter()
            .any(|m| m.child == Some(child) && m.status == hya_proto::MemberRunStatus::Cancelled)
    );
}

#[tokio::test]
async fn i6_turn_budget_kill_records_failure_reason() {
    let test = "i6_turn_budget_kill_records_failure_reason";
    if support::lifecycle::isolated_config(test, &[("HYA_SUBAGENT_TURN_BUDGET", "1")]) {
        return;
    }
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("one allowed turn")]).await;
    h.archived(&handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let waited = h
        .tool("wait", json!({"targets": [handle], "timeout_secs": 0}))
        .await;
    h.shutdown().await;
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Killed);
    assert!(
        root.session
            .members
            .iter()
            .any(|m| m.child == Some(child) && m.status == hya_proto::MemberRunStatus::Failed)
    );
    assert!(
        output(&waited)
            .to_string()
            .contains("per-team turn budget exceeded")
    );
}

#[tokio::test]
async fn i6_workflow_cancel_records_parent_archive_reason() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let marker = h.provider.script(vec![
        Round::text("Workflow in progress").stream_gated(gate.clone()),
    ]);
    let directory = h.dir.join(".hya/workflows");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("cancel.hya.md"), format!("---\nkind: Workflow\nname: cancel\ndescription: Cancellation reason\nnodes:\n  work:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {marker}\n---\nflowchart TD\n  work\n")).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let running = tokio::spawn({
        let control = h.built.workflow_control();
        let root = h.root;
        let cancel = cancel.clone();
        async move {
            control
                .execute(
                    root,
                    hya_app::WorkflowInvocation::default(),
                    hya_proto::WorkflowCommand::Run {
                        name: Some("cancel".into()),
                        expected_revision: None,
                        inputs: Default::default(),
                        run: None,
                    },
                    cancel,
                )
                .await
        }
    });
    gate.arrived().await;
    let child = h
        .engine
        .read_projection(h.root)
        .await
        .unwrap()
        .session
        .members[0]
        .child
        .unwrap();
    cancel.cancel();
    tokio::time::timeout(support::lifecycle::BOUND, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let root = h.engine.read_projection(h.root).await.unwrap();
    let archived = root
        .team
        .archived
        .values()
        .find(|row| row.session == child)
        .unwrap();
    h.shutdown().await;
    assert_eq!(archived.reason, ArchiveReason::ArchivedByParent);
}

#[tokio::test]
async fn i3_terminal_write_error_releases_in_memory_episode() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("working")]).await;
    h.idle(child, &handle).await;
    h.trigger("agent_archived").await;
    let result = h.tool("archive", json!({"target": handle})).await;
    assert!(
        matches!(&result, ToolPartState::Error { message, .. } if message.contains("M4 injected")),
        "{result:?}"
    );
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    let direct = h
        .engine
        .admit_user_prompt(child, "must not revive an orphan".into())
        .await
        .unwrap_err();
    assert!(
        direct.to_string().contains("steer this subagent with mail"),
        "{direct}"
    );
    let waited = h
        .tool("wait", json!({"targets": [handle], "timeout_secs": 0}))
        .await;
    let state = output(&waited).to_string();
    let marker = h
        .provider
        .script(vec![Round::text("must not execute after failed ending")]);
    let mut requests = h.provider.requests.subscribe();
    h.mail(&handle, marker).await.unwrap();
    let resumed = tokio::time::timeout(support::lifecycle::BOUND, async {
        loop {
            if requests.recv().await.unwrap().session == child {
                break;
            }
        }
    })
    .await
    .is_ok();
    h.heal().await;
    h.shutdown().await;
    assert!(
        !state.contains("working"),
        "failed terminal write left episode working: {state}"
    );
    assert!(
        !resumed,
        "failed terminal write retained an episode that executed new mail"
    );
}

#[tokio::test]
async fn i4_failed_terminal_write_can_be_reconciled() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("working")]).await;
    h.idle(child, &handle).await;
    let epoch = h.claim_epoch(child).await;
    let before = h.engine.replay(child).await.unwrap();
    h.trigger("agent_archived").await;
    let failed = h.tool("archive", json!({"target": handle})).await;
    assert!(matches!(failed, ToolPartState::Error { .. }));
    assert_eq!(
        h.engine.replay(child).await.unwrap(),
        before,
        "failed archive must roll back the handoff and initial-prompt cancellation together"
    );
    assert!(
        h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    h.heal().await;
    let retried = h.tool("archive", json!({"target": handle})).await;
    output(&retried);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    h.archived(&handle).await;
    let reconciled_epoch = h.claim_epoch(child).await;
    h.shutdown().await;
    assert_eq!(
        reconciled_epoch,
        epoch + 1,
        "ending an orphan must take over the failed episode's claim"
    );
}

#[tokio::test]
async fn i7_refused_report_delivers_a_harness_notice() {
    let mut h = Harness::new().await;
    let summary = Arc::new(Gate::default());
    h.provider
        .handoffs
        .lock()
        .unwrap()
        .push_back(Round::text(support::lifecycle::HANDOFF).gated(summary.clone()));
    let mut requests = h.provider.requests.subscribe();
    let (child, handle) = h.spawn(vec![report("premature report")]).await;
    summary.arrived().await;
    let gate = Arc::new(Gate::default());
    let mail = h
        .provider
        .script(vec![Round::text("read new mail").gated(gate.clone())]);
    h.mail(&handle, format!("new information {mail}"))
        .await
        .unwrap();
    // Drain the initial request before releasing the summarizer.
    request_for(&mut requests, child).await;
    summary.release();
    gate.arrived().await;
    let next = request_for(&mut requests, child).await;
    let encoded = serde_json::to_string(&next.messages).unwrap();
    let prematurely_archived = h
        .engine
        .read_projection(h.root)
        .await
        .unwrap()
        .team
        .archived
        .contains_key(&handle);
    gate.release();
    h.idle(child, &handle).await;
    let later = h.provider.script(vec![report("final report")]);
    h.mail(&handle, later).await.unwrap();
    h.archived(&handle).await;
    let reports = h
        .engine
        .replay(h.root)
        .await
        .unwrap()
        .iter()
        .filter(|e| matches!(&e.event, Event::SubagentReported { .. }))
        .count();
    let child_requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.session == child && !request.completion.tools.is_empty())
        .count();
    assert_eq!(
        child_requests, 3,
        "empty wakes must never start another child turn"
    );
    h.shutdown().await;
    assert!(!prematurely_archived, "the refused report was archived");
    assert_eq!(
        reports, 1,
        "refused report was re-fired or final report lost"
    );
    assert!(
        encoded.contains("Your report was not accepted"),
        "no durable refusal notice in next request: {encoded}"
    );
}

#[tokio::test]
async fn i9_third_episode_context_contains_only_latest_handoff() {
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn(vec![Round::tool(
            "report",
            json!({"result": "EPISODE_ONE_SECRET"}),
        )])
        .await;
    h.archived(&handle).await;
    let second = h.provider.script(vec![report("EPISODE_TWO_SECRET")]);
    h.mail(&handle, second).await.unwrap();
    h.archived(&handle).await;
    let gate = Arc::new(Gate::default());
    let third = h
        .provider
        .script(vec![Round::text("third episode").gated(gate.clone())]);
    let mut requests = h.provider.requests.subscribe();
    h.mail(&handle, third).await.unwrap();
    gate.arrived().await;
    let request = request_for(&mut requests, child).await;
    let encoded = serde_json::to_string(&request.messages).unwrap();
    gate.release();
    h.shutdown().await;
    assert_eq!(encoded.matches("PRIOR HANDOFF").count(), 1, "{encoded}");
    assert!(
        !encoded.contains("EPISODE_ONE_SECRET") && !encoded.contains("EPISODE_TWO_SECRET"),
        "prior episode leaked: {encoded}"
    );
}

#[tokio::test]
async fn i10_rejected_restart_rolls_back_the_whole_revive() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![report("ready")]).await;
    h.archived(&handle).await;
    let before = h.engine.replay(child).await.unwrap();
    h.trigger("agent_restarted").await;
    let result = h.mail(&handle, "next task".into()).await;
    let after = h.engine.replay(child).await.unwrap();
    let root = h.engine.read_projection(h.root).await.unwrap();
    let active = h.engine.store().active_actor_ids().await.unwrap();
    h.heal().await;
    if result.is_err()
        && after == before
        && root.team.archived.contains_key(&handle)
        && !active.contains(&child)
    {
        let retry = h
            .provider
            .script(vec![report("revived once after healing")]);
        h.mail(&handle, retry).await.unwrap();
        h.archived(&handle).await;
        assert_eq!(h.engine.replay(h.root).await.unwrap().iter().filter(|e| matches!(&e.event, Event::AgentRestarted { child: restarted, .. } if *restarted == child)).count(), 1);
    }
    h.shutdown().await;
    assert!(result.is_err(), "fault did not reject revive: {result:?}");
    assert_eq!(
        after, before,
        "revive partially committed before AgentRestarted"
    );
    assert!(root.team.archived.contains_key(&handle));
    assert!(!active.contains(&child));
}

#[tokio::test]
#[ignore = "M4-11: I21"]
async fn i21_handle_send_persists_only_dm_channel_targets() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    output(
        &h.tool("send", json!({"channel": handle, "body": "alias mail"}))
            .await,
    );
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert!(events.iter().any(|e| matches!(&e.event, Event::MailSent { to: hya_proto::MailEndpoint::Channel(_), body, .. } if body == "alias mail")), "handle alias persisted without DM resolution");
}

#[tokio::test]
#[ignore = "M4-12: I19"]
async fn i19_shutdown_drop_releases_engine_ownership() {
    let mut h = Harness::new().await;
    let weak = Arc::downgrade(&h.engine);
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    h.shutdown().await;
    drop(h);
    assert!(
        weak.upgrade().is_none(),
        "runtime shutdown left a strong engine owner"
    );
}

#[tokio::test]
#[ignore = "M4-7: I13"]
async fn i13_capacity_is_a_live_member_lease_across_root_turns() {
    if support::lifecycle::isolated_config(
        "i13_capacity_is_a_live_member_lease_across_root_turns",
        &[("HYA_SUBAGENT_BUDGET", "2")],
    ) {
        return;
    }
    let mut h = Harness::new().await;
    let (a, first) = h.spawn(vec![Round::text("A")]).await;
    h.idle(a, &first).await;
    let (b, second) = h.spawn(vec![Round::text("B")]).await;
    h.idle(b, &second).await;
    let third = h.tool("task", json!({"description": "C", "prompt": "C", "subagent_type": "hya-task", "model": "fake"})).await;
    let overloaded = serde_json::to_string(&third)
        .unwrap()
        .to_lowercase()
        .contains("overload");
    if overloaded {
        output(&h.tool("archive", json!({"target": first})).await);
        let (c, third) = h.spawn(vec![Round::text("C")]).await;
        h.idle(c, &third).await;
        let revive = h.mail(&first, "must consume capacity".into()).await;
        assert!(revive.is_err(), "revive exceeded the two live-member lease");
    }
    h.shutdown().await;
    assert!(overloaded, "third task exceeded live capacity: {third:?}");
}

#[tokio::test]
#[ignore = "M4-7: I14"]
async fn i14_live_capacity_survives_real_process_crash() {
    let test = "i14_live_capacity_survives_real_process_crash";
    support::lifecycle::crash_seed_if_requested(test, false).await;
    if support::lifecycle::isolated_config(test, &[("HYA_SUBAGENT_BUDGET", "2")]) {
        return;
    }
    let mut h = support::lifecycle::crashed_team(test).await;
    assert_eq!(h.engine.store().active_actor_ids().await.unwrap().len(), 2);
    let result = h.tool("task", json!({"description": "over capacity after restart", "prompt": "C", "subagent_type": "hya-task", "model": "fake"})).await;
    h.shutdown().await;
    assert!(
        serde_json::to_string(&result)
            .unwrap()
            .to_lowercase()
            .contains("overload"),
        "restart lost live leases: {result:?}"
    );
}

#[tokio::test]
async fn i22_restore_prompt_precedes_queued_recovered_main_work() {
    let test = "i22_restore_prompt_precedes_queued_recovered_main_work";
    support::lifecycle::crash_seed_if_requested(test, true).await;
    let mut h = support::lifecycle::crashed_team(test).await;
    let state = hya_server::AppState::new(h.engine.clone(), Arc::new(h.agent.clone()))
        .with_auto_title(false)
        .with_pure_guidance(true);
    let mut observed = h.provider.requests.subscribe();
    state.resume_interrupted_turns().await;
    let already_called = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.session == h.root && !r.completion.tools.is_empty());
    if !already_called {
        request_for(&mut observed, h.root).await;
    }
    h.until(h.root, |p| {
        p.session
            .messages
            .last()
            .is_some_and(|m| m.role == hya_proto::Role::Assistant && m.finish.is_some())
    })
    .await;
    state
        .stop_for_restart(std::time::Duration::from_secs(5))
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
    assert!(
        !requests.is_empty(),
        "restore never called the root provider"
    );
    let first = serde_json::to_string(&requests[0].completion.messages).unwrap();
    assert!(
        first.contains("Your previous turn was interrupted by a restart."),
        "recovered main work ran before restore admission: {first}"
    );
    let projection = h.engine.read_projection(h.root).await.unwrap();
    let messages = &projection.session.messages;
    let restores = messages.iter().enumerate().filter(|(_, m)| {
        m.role == hya_proto::Role::User && m.parts.iter().any(|part| matches!(part,
            hya_proto::PartProjection::Text { text, .. } if text.contains("Your previous turn was interrupted by a restart.")))
    }).collect::<Vec<_>>();
    assert_eq!(restores.len(), 1, "restore prompt was admitted twice");
    let (index, restore) = restores[0];
    let answers = messages[index + 1..]
        .iter()
        .take_while(|m| m.role != hya_proto::Role::User)
        .filter(|m| m.role == hya_proto::Role::Assistant)
        .collect::<Vec<_>>();
    assert_eq!(
        answers.len(),
        1,
        "restore prompt did not own exactly one answer"
    );
    assert_eq!(answers[0].finish, Some(hya_proto::FinishReason::Stop));
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
        1,
        "restore prompt was executed twice"
    );
}

#[tokio::test]
#[ignore = "M4-10: I20"]
async fn i20_workflow_stage_loop_worker_and_verifier_are_residents() {
    let mut h = Harness::new().await;
    let stage = h
        .provider
        .script(vec![Round::text("stage settled without report")]);
    let worker = h
        .provider
        .script(vec![Round::text("loop worker completed")]);
    let result = h.workflow(&format!("  stage:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {stage}\n  loop:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {worker}\n    mode: loop\n    verify:\n      agent: hya-reviewer\n      model: {{ id: fake }}\n      until: Work is complete.\n      max_iterations: 1\n"), "  stage --> loop").await;
    let events = h.engine.replay(h.root).await.unwrap();
    let registrations = events
        .iter()
        .filter_map(|e| match &e.event {
            Event::AgentRegistered {
                agent_session,
                mode,
                ..
            } if *agent_session != h.root => Some((*agent_session, *mode)),
            _ => None,
        })
        .collect::<Vec<_>>();
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

#[tokio::test]
#[ignore = "M4-14: I20"]
async fn i20_ready_successor_does_not_wait_for_independent_stage() {
    let mut h = Harness::new().await;
    let slow = Arc::new(Gate::default());
    let successor = Arc::new(Gate::default());
    let predecessor = Arc::new(Gate::default());
    let a = h.provider.script(vec![
        Round::text("A finished").stream_gated(predecessor.clone()),
    ]);
    let a_marker = a.clone();
    let b = h
        .provider
        .script(vec![Round::text("B finished").gated(slow.clone())]);
    let c = h
        .provider
        .script(vec![Round::text("C finished").gated(successor.clone())]);
    let directory = h.dir.join(".hya/workflows");
    std::fs::create_dir_all(&directory).unwrap();
    let nodes = [("a", a), ("b", b), ("c", c)].into_iter().map(|(name, marker)| format!("  {name}:\n    agent: hya-task\n    model: {{ id: fake }}\n    directive: {marker}\n")).collect::<String>();
    std::fs::write(directory.join("frontier.hya.md"), format!("---\nkind: Workflow\nname: frontier\ndescription: Ready successors\nnodes:\n{nodes}---\nflowchart TD\n  a --> c\n  b\n")).unwrap();
    let running = tokio::spawn({
        let control = h.built.workflow_control();
        let root = h.root;
        async move {
            control
                .execute(
                    root,
                    hya_app::WorkflowInvocation::default(),
                    hya_proto::WorkflowCommand::Run {
                        name: Some("frontier".into()),
                        expected_revision: None,
                        inputs: Default::default(),
                        run: None,
                    },
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
        }
    });
    slow.arrived().await;
    predecessor.arrived().await;
    let a_session = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .find(|request| {
            serde_json::to_string(&request.completion.messages)
                .unwrap()
                .contains(&a_marker)
        })
        .unwrap()
        .session;
    predecessor.release();
    // StageFinished is itself delayed by the old level barrier. Observe A's
    // durable completed model answer so that barrier cannot hide the defect.
    h.until(a_session, |p| p.session.messages.iter().any(|m| m.finish.is_some() && m.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "A finished")))).await;
    let ready_before_b = tokio::time::timeout(support::lifecycle::BOUND, successor.reached())
        .await
        .is_ok();
    slow.release();
    successor.release();
    tokio::time::timeout(support::lifecycle::BOUND, running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    h.shutdown().await;
    assert!(
        ready_before_b,
        "C waited for independent B after its only predecessor A finished"
    );
}

#[tokio::test]
async fn i6_shutdown_records_shutdown_reason_and_releases_claim() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    h.shutdown().await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Shutdown);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(
        !h.engine
            .replay(h.root)
            .await
            .unwrap()
            .iter()
            .any(|e| matches!(&e.event, Event::SubagentReported { .. }))
    );
}

#[tokio::test]
async fn i17_root_delete_does_not_send_failure_mail_or_wake_members() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let grandchild_gate = Arc::new(Gate::default());
    let grandchild_prompt = h.provider.script(vec![
        Round::text("busy grandchild").stream_gated(grandchild_gate.clone()),
    ]);
    let (child, _) = h
        .spawn(vec![
            Round::tool("task", json!({"description": "grandchild", "prompt": grandchild_prompt, "subagent_type": "hya-task", "model": "fake"})),
            Round::text("busy child").stream_gated(gate.clone()),
        ])
        .await;
    gate.arrived().await;
    grandchild_gate.arrived().await;
    let grandchild = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .members[0]
        .child
        .unwrap();
    let mut events = h.engine.bus().subscribe();
    assert!(
        tokio::time::timeout(support::lifecycle::BOUND, h.engine.delete_session(h.root))
            .await
            .expect("root delete exceeded ending bound")
            .unwrap()
    );
    let active_after_delete = h.engine.store().active_actor_ids().await.unwrap();
    h.shutdown().await;
    let mut teardown = Vec::new();
    while let Ok(envelope) = events.try_recv() {
        teardown.push(envelope.event);
    }
    assert!(
        !active_after_delete.contains(&child) && !active_after_delete.contains(&grandchild),
        "root delete left live descendants: {active_after_delete:?}"
    );
    for member in [child, grandchild] {
        assert!(teardown.iter().any(|event| matches!(event, Event::AgentArchived { child, reason: ArchiveReason::RootTeardown, .. } if *child == member)), "delete bypassed terminal lifecycle effects for {member}: {teardown:?}");
    }
    assert!(
        !teardown
            .iter()
            .any(|event| matches!(event, Event::MailSent { .. })),
        "root teardown sent mail: {teardown:?}"
    );
    assert!(
        !teardown.iter().any(
            |event| matches!(event, Event::MessageStarted { session, .. } if *session == child || *session == grandchild)
        ),
        "root teardown woke a member"
    );
}

#[tokio::test]
async fn e7_live_member_turns_are_refused_before_prompt_admission() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle member")]).await;
    h.idle(child, &handle).await;
    let before = h.engine.replay(child).await.unwrap();
    let error = h
        .engine
        .admit_user_prompt(child, "direct client prompt".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .run_turn(child, &h.agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .run_shell(
            child,
            &h.agent,
            "echo must-not-run".into(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .admit_command_prompt(child, "test".into(), String::new(), "must not run".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    assert_eq!(h.engine.replay(child).await.unwrap().len(), before.len());
    let next = h.provider.script(vec![report("mail still works")]);
    h.mail(&handle, next).await.unwrap();
    h.archived(&handle).await;
    h.shutdown().await;
}

#[tokio::test]
async fn e7_archived_member_turns_are_refused_before_prompt_admission() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![report("finished")]).await;
    h.archived(&handle).await;
    let before = h.engine.replay(child).await.unwrap();
    let error = h
        .engine
        .admit_user_prompt(child, "direct client prompt".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .admit_command_prompt(child, "test".into(), String::new(), "must not run".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .run_turn(child, &h.agent, tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    let error = h
        .engine
        .run_shell(
            child,
            &h.agent,
            "echo must-not-run".into(),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("steer this subagent with mail"),
        "{error}"
    );
    assert_eq!(h.engine.replay(child).await.unwrap().len(), before.len());
    h.shutdown().await;
}

#[tokio::test]
async fn e7_member_model_switch_is_refused() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    for archived in [false, true] {
        if archived {
            output(&h.tool("archive", json!({"target": handle})).await);
        }
        let before = h.engine.replay(child).await.unwrap().len();
        let error = h
            .engine
            .switch_model(child, hya_proto::ModelRef::new("other"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("steer this subagent with mail"),
            "{error}"
        );
        assert_eq!(h.engine.replay(child).await.unwrap().len(), before);
    }
    h.shutdown().await;
}

#[tokio::test]
async fn e7_member_agent_switch_is_refused() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    for archived in [false, true] {
        if archived {
            output(&h.tool("archive", json!({"target": handle})).await);
        }
        let before = h.engine.replay(child).await.unwrap().len();
        let error = h
            .engine
            .switch_agent(child, hya_proto::AgentName::new("other"))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("steer this subagent with mail"),
            "{error}"
        );
        assert_eq!(h.engine.replay(child).await.unwrap().len(), before);
    }
    h.shutdown().await;
}

#[tokio::test]
async fn e7_member_revert_and_unrevert_are_refused() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle")]).await;
    h.idle(child, &handle).await;
    for archived in [false, true] {
        if archived {
            output(&h.tool("archive", json!({"target": handle})).await);
        }
        let before = h.engine.replay(child).await.unwrap().len();
        let error = h
            .engine
            .revert_session(child, hya_core::RevertTarget::LastUserMessage)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("steer this subagent with mail"),
            "{error}"
        );
        let error = h.engine.unrevert_session(child).await.unwrap_err();
        assert!(
            error.to_string().contains("steer this subagent with mail"),
            "{error}"
        );
        assert_eq!(h.engine.replay(child).await.unwrap().len(), before);
    }
    h.shutdown().await;
}

#[tokio::test]
async fn e7_roots_and_never_registered_children_keep_client_mutations() {
    let mut h = Harness::new().await;
    let child = h
        .engine
        .create(hya_core::CreateSession {
            parent: Some(h.root),
            agent: h.agent.name.clone(),
            model: h.agent.model.clone(),
            workdir: h.dir.to_string_lossy().into_owned(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await
        .unwrap();
    for session in [h.root, child] {
        h.engine
            .admit_user_prompt(session, "ordinary prompt".into())
            .await
            .unwrap();
        h.engine
            .switch_model(session, h.agent.model.clone())
            .await
            .unwrap();
        h.engine
            .switch_agent(session, h.agent.name.clone())
            .await
            .unwrap();
        h.engine
            .revert_session(session, hya_core::RevertTarget::LastUserMessage)
            .await
            .unwrap();
        h.engine.unrevert_session(session).await.unwrap();
    }
    h.shutdown().await;
}

#[tokio::test]
async fn e1_archive_racing_registration_never_takes_over_a_live_episode() {
    let plugin = support::lifecycle::BlockingPlugin::new().await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    plugin.arm();
    let root = h.root;
    let engine = h.engine.clone();
    // Read the committed roster directly, before initialization publishes its
    // envelopes. The archive must serialize with claim commit + installation.
    let archiving = tokio::spawn(async move {
        tokio::time::timeout(support::lifecycle::BOUND, async {
            loop {
                let p = engine.read_projection(root).await.unwrap();
                if let Some((handle, entry)) = p
                    .team
                    .roster
                    .iter()
                    .find(|(handle, _)| handle.as_str() != "main")
                {
                    let child = entry.session;
                    let handle = handle.clone();
                    let receipt = engine.archive_subagent(child).await.unwrap();
                    return (child, handle, receipt);
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    });
    let marker = h
        .provider
        .script(vec![Round::text("must be cancelled during init")]);
    let spawning = h.start_tool("task", json!({"description":"registration race", "prompt":marker, "subagent_type":"hya-task", "model":"fake"})).await;
    let (child, handle, receipt) = tokio::time::timeout(support::lifecycle::BOUND, archiving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.session, child);
    assert_eq!(
        h.claim_epoch(child).await,
        1,
        "Takeover crossed a live registration"
    );
    // The child ending must release initialization on its own; cancelling
    // the root here would race the unrelated task-admission finalization.
    tokio::time::timeout(support::lifecycle::BOUND, spawning)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    plugin.gate.release();
    let revived = h.provider.script(vec![report("revived once")]);
    h.mail(&handle, revived).await.unwrap();
    h.archived(&handle).await;
    let child_requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.session == child && !r.completion.tools.is_empty())
        .count();
    assert_eq!(child_requests, 1);
    assert_eq!(h.claim_epoch(child).await, 2);
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
async fn i3_report_write_failure_releases_episode_and_next_archive_reconciles() {
    let mut h = Harness::new().await;
    h.trigger("agent_archived").await;
    let (child, handle) = h.spawn(vec![report("report cannot commit")]).await;
    tokio::time::timeout(support::lifecycle::BOUND, async {
        while h.built.resident_supervisor().episode_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed report write retained the episode");
    assert!(
        h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    let waited = h
        .tool("wait", json!({"targets":[handle],"timeout_secs":0}))
        .await;
    assert!(!output(&waited).to_string().contains("working"));
    h.heal().await;
    output(&h.tool("archive", json!({"target":handle})).await);
    assert_eq!(h.claim_epoch(child).await, 2);
    h.shutdown().await;
}

#[tokio::test]
async fn i3_sidecar_start_and_terminal_write_failure_leaves_an_orphan() {
    let test = "i3_sidecar_start_and_terminal_write_failure_leaves_an_orphan";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let path = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
        .join("m4-sidecar-control.json");
    std::fs::write(path, json!({"refuse_start":true}).to_string()).unwrap();
    let mut h = Harness::new().await;
    h.trigger("agent_archived").await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::text("must not run")])
        .await;
    tokio::time::timeout(support::lifecycle::BOUND, async {
        while h.built.resident_supervisor().episode_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed startup ending retained the episode");
    assert!(
        h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert!(
        h.provider
            .captured
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.session != child)
    );
    h.heal().await;
    output(&h.tool("archive", json!({"target":handle})).await);
    assert_eq!(h.claim_epoch(child).await, 2);
    h.shutdown().await;
}

#[tokio::test]
async fn e1_orphan_archives_and_revival_share_the_registration_lock() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("first episode")]).await;
    h.idle(child, &handle).await;
    h.trigger("agent_archived").await;
    assert!(matches!(
        h.tool("archive", json!({"target":handle})).await,
        ToolPartState::Error { .. }
    ));
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.heal().await;
    let next = h.provider.script(vec![Round::text("revived episode")]);
    let engine = h.engine.clone();
    let root = h.root;
    let address = handle.clone();
    let reviving = tokio::spawn(async move {
        let mut events = engine.bus().subscribe();
        loop {
            if engine
                .read_projection(root)
                .await
                .unwrap()
                .team
                .archived
                .contains_key(&address)
            {
                break;
            }
            events.recv().await.unwrap();
        }
        engine
            .mail_send(
                root,
                hya_proto::MailEndpoint::parse(&address),
                hya_proto::MailKind::Message,
                next,
            )
            .await
            .unwrap();
    });
    let (first, second) = tokio::time::timeout(support::lifecycle::BOUND, async {
        tokio::join!(
            h.engine.archive_subagent(child),
            h.engine.archive_subagent(child)
        )
    })
    .await
    .unwrap();
    for result in [first, second] {
        if let Err(error) = result {
            assert!(error.to_string().contains("already archived"), "{error}");
        }
    }
    tokio::time::timeout(support::lifecycle::BOUND, reviving)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        h.claim_epoch(child).await,
        3,
        "an archive took over the revived live episode"
    );
    let p = h.engine.read_projection(h.root).await.unwrap();
    if p.team.roster.contains_key(&handle) {
        h.idle(child, &handle).await;
        output(&h.tool("archive", json!({"target":handle})).await);
    }
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn a1_cancel_wakes_an_idle_episode() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("idle before cancel")]).await;
    h.idle(child, &handle).await;
    h.built
        .resident_supervisor()
        .team_cancel(h.root)
        .unwrap()
        .cancel();
    h.archived(&handle).await;
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    assert_eq!(
        h.engine
            .read_projection(h.root)
            .await
            .unwrap()
            .team
            .archived[&handle]
            .reason,
        ArchiveReason::Shutdown
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a1_cancel_during_report_handoff_ends_without_spinning() {
    let test = "a1_cancel_during_report_handoff_ends_without_spinning";
    if support::lifecycle::isolated_config(test, &[]) {
        return;
    }
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    h.provider
        .handoffs
        .lock()
        .unwrap()
        .push_back(Round::text(support::lifecycle::HANDOFF).gated(gate.clone()));
    let (_, handle) = h.spawn(vec![report("report interrupted by cancel")]).await;
    gate.arrived().await;
    h.built
        .resident_supervisor()
        .team_cancel(h.root)
        .unwrap()
        .cancel();
    h.archived(&handle).await;
    gate.release();
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn a4_delete_racing_report_resolves_every_ending_waiter() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    h.provider
        .handoffs
        .lock()
        .unwrap()
        .push_back(Round::text(support::lifecycle::HANDOFF).gated(gate.clone()));
    let (child, handle) = h.spawn(vec![report("report racing teardown")]).await;
    gate.arrived().await;
    let mut events = h.engine.bus().subscribe();
    let supervisor = h.built.resident_supervisor();
    let root = h.root;
    let archive = supervisor.end_member(root, &handle);
    let second = supervisor.end_member(root, &handle);
    tokio::pin!(archive, second);
    assert!(futures::poll!(&mut archive).is_pending());
    assert!(futures::poll!(&mut second).is_pending());
    assert!(
        tokio::time::timeout(support::lifecycle::BOUND, h.engine.delete_session(h.root))
            .await
            .unwrap()
            .unwrap()
    );
    tokio::time::timeout(support::lifecycle::BOUND, archive)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(support::lifecycle::BOUND, second)
        .await
        .unwrap()
        .unwrap();
    gate.release();
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event.event, Event::MailSent { .. }),
            "teardown mailed a parent"
        );
    }
    h.shutdown().await;
}

#[tokio::test]
async fn a4_delete_racing_init_resolves_every_ending_waiter() {
    let plugin = support::lifecycle::BlockingPlugin::new().await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    plugin.arm();
    let spawning = h.start_tool("task", json!({"description":"init racing delete", "prompt":"must not run", "subagent_type":"hya-task", "model":"fake"})).await;
    plugin.gate.arrived().await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let (handle, child) = root
        .team
        .roster
        .iter()
        .find(|(path, _)| path.as_str() != "main")
        .map(|(path, entry)| (path.clone(), entry.session))
        .unwrap();
    let supervisor = h.built.resident_supervisor();
    let root = h.root;
    let archive = supervisor.end_member(root, &handle);
    let second = supervisor.end_member(root, &handle);
    tokio::pin!(archive, second);
    assert!(futures::poll!(&mut archive).is_pending());
    assert!(futures::poll!(&mut second).is_pending());
    assert!(
        tokio::time::timeout(support::lifecycle::BOUND, h.engine.delete_session(h.root))
            .await
            .unwrap()
            .unwrap()
    );
    tokio::time::timeout(support::lifecycle::BOUND, archive)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(support::lifecycle::BOUND, second)
        .await
        .unwrap()
        .unwrap();
    let _ = tokio::time::timeout(support::lifecycle::BOUND, spawning)
        .await
        .unwrap()
        .unwrap();
    plugin.gate.release();
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
async fn a6_reports_record_done_and_failed_run_status() {
    let mut h = Harness::new().await;
    for (outcome, expected) in [
        ("done", hya_proto::MemberRunStatus::Done),
        ("failed", hya_proto::MemberRunStatus::Failed),
    ] {
        let (child, handle) = h
            .spawn(vec![Round::tool(
                "report",
                json!({"result":"reported outcome", "outcome":outcome}),
            )])
            .await;
        h.archived(&handle).await;
        let root = h.engine.read_projection(h.root).await.unwrap();
        assert_eq!(
            root.session
                .members
                .iter()
                .find(|row| row.child == Some(child))
                .unwrap()
                .status,
            expected
        );
    }
    h.shutdown().await;
}

#[tokio::test]
async fn a3_kill_terminate_error_removes_episode_within_bound() {
    let test = "a3_kill_terminate_error_removes_episode_within_bound";
    if support::lifecycle::isolated_config(test, &[("HYA_SUBAGENT_MESSAGE_BUDGET", "1")]) {
        return;
    }
    let mut h = Harness::new().await;
    let stopping = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![Round::text("idle sidecar")],
            Arc::new(support::lifecycle::EndingSidecar {
                stopping: stopping.clone(),
                fail: true,
            }),
        )
        .await
        .unwrap();
    h.idle(child, &handle).await;
    let mut events = h.engine.bus().subscribe();
    h.mail(&handle, "first message".into()).await.unwrap();
    h.mail(&handle, "second message kills team".into())
        .await
        .unwrap();
    stopping.arrived().await;
    stopping.release();
    h.archived(&handle).await;
    tokio::time::timeout(support::lifecycle::BOUND, async {
        loop {
            if h.built.resident_supervisor().episode_count() == 0 {
                break;
            }
            events.recv().await.unwrap();
        }
    })
    .await
    .expect("terminate failure retained an episode beyond its ending bound");
    assert!(
        h.engine
            .read_projection(h.root)
            .await
            .unwrap()
            .session
            .members
            .iter()
            .any(|m| m.child == Some(child) && m.summary.contains("sidecar did not stop"))
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a6_archive_bus_observation_implies_episode_gone() {
    let mut h = Harness::new().await;
    let stopping = Arc::new(Gate::default());
    let mut bus = h.engine.bus().subscribe();
    let (child, handle) = h
        .spawn_with_sidecar(
            vec![report("sidecar report")],
            Arc::new(support::lifecycle::EndingSidecar {
                stopping: stopping.clone(),
                fail: false,
            }),
        )
        .await
        .unwrap();
    stopping.arrived().await;
    let events = std::iter::from_fn(|| bus.try_recv().ok()).collect::<Vec<_>>();
    let premature = events.iter().any(
        |event| matches!(event.event, Event::AgentArchived { child: actor, .. } if actor == child),
    );
    stopping.release();
    h.archived(&handle).await;
    if !premature {
        tokio::time::timeout(support::lifecycle::BOUND, async {
            loop {
                let event = bus.recv().await.unwrap();
                if matches!(event.event, Event::AgentArchived { child: actor, .. } if actor == child) { break; }
            }
        }).await.unwrap();
    }
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.shutdown().await;
    assert!(
        !premature,
        "archive published while sidecar termination still owned the episode"
    );
}

#[tokio::test]
async fn a5_kill_during_init_publishes_registration_first_and_spawn_fails() {
    let test = "a5_kill_during_init_publishes_registration_first_and_spawn_fails";
    if support::lifecycle::isolated_config(test, &[("HYA_SUBAGENT_MESSAGE_BUDGET", "1")]) {
        return;
    }
    let plugin = support::lifecycle::BlockingPlugin::new().await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let (sibling, sibling_handle) = h.spawn(vec![Round::text("sibling")]).await;
    h.idle(sibling, &sibling_handle).await;
    plugin.arm();
    let mut bus = h.engine.bus().subscribe();
    // The public spawn call exposes its result independently of the lead turn
    // that the budget kill also cancels.
    let result = {
        let spawning = h.spawn_with_sidecar(
            vec![Round::text("must not run")],
            Arc::new(support::lifecycle::EndingSidecar {
                stopping: Arc::new(Gate::default()),
                fail: false,
            }),
        );
        tokio::pin!(spawning);
        tokio::time::timeout(support::lifecycle::BOUND, async {
            tokio::select! {
                result = &mut spawning => panic!("initializer was not gated: {result:?}"),
                () = plugin.gate.arrived() => {},
            }
            h.mail(&sibling_handle, "first budget message".into())
                .await
                .unwrap();
            h.mail(&sibling_handle, "second budget message".into())
                .await
                .unwrap();
            spawning.await
        })
        .await
        .unwrap()
    };
    plugin.gate.release();
    let root = h.engine.read_projection(h.root).await.unwrap();
    let child = root.session.members.last().unwrap().child.unwrap();
    let mut events = Vec::new();
    tokio::time::timeout(support::lifecycle::BOUND, async {
        loop {
            let event = bus.recv().await.unwrap();
            let ended =
                matches!(event.event, Event::AgentArchived { child: actor, .. } if actor == child);
            events.push(event.event);
            if ended {
                break;
            }
        }
    })
    .await
    .unwrap();
    let registered = events.iter().position(|event| matches!(event, Event::AgentRegistered { agent_session, .. } if *agent_session == child)).unwrap();
    let archived = events
        .iter()
        .position(
            |event| matches!(event, Event::AgentArchived { child: actor, .. } if *actor == child),
        )
        .unwrap();
    assert!(
        registered < archived,
        "archive preceded registration: {events:?}"
    );
    assert!(
        matches!(result, Err(hya_core::CoreError::Cancelled)),
        "cancelled init reported spawn success: {result:?}"
    );
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
async fn i18_shutdown_reconciles_an_already_orphaned_member() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("orphan before shutdown")]).await;
    h.idle(child, &handle).await;
    let epoch = h.claim_epoch(child).await;
    h.trigger("agent_archived").await;
    assert!(matches!(
        h.tool("archive", json!({"target":handle})).await,
        ToolPartState::Error { .. }
    ));
    assert_eq!(h.built.resident_supervisor().episode_count(), 0);
    h.heal().await;
    h.shutdown().await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.archived[&handle].reason, ArchiveReason::Shutdown);
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(h.claim_epoch(child).await, epoch + 1);
}

#[tokio::test]
async fn i6_idle_sidecar_loss_drops_handle_and_next_wake_starts_fresh() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut h = Harness::new().await;
    let probe = Arc::new(support::lifecycle::LossSidecar {
        loss: tokio_util::sync::CancellationToken::new(),
        starts: Arc::new(AtomicUsize::new(0)),
        drops: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(tokio::sync::Notify::new()),
    });
    let (child, handle) = h
        .spawn_with_sidecar(vec![Round::text("first activation")], probe.clone())
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
    assert!(!events.iter().any(|e| matches!(&e.event, Event::SubagentReported { child: reported, .. } if *reported == child)));
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
        .script(vec![Round::text("fresh sidecar activation")]);
    h.mail(&handle, marker).await.unwrap();
    h.until(child, |p| p.session.messages.iter().any(|m| m.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "fresh sidecar activation")))).await;
    h.idle(child, &handle).await;
    assert_eq!(probe.starts.load(Ordering::SeqCst), 2);
    assert_eq!(h.claim_epoch(child).await, epoch);
    h.shutdown().await;
    assert_eq!(probe.drops.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn i8_running_sidecar_loss_removes_episode_before_archive_publication() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut h = Harness::new().await;
    let probe = Arc::new(support::lifecycle::LossSidecar {
        loss: tokio_util::sync::CancellationToken::new(),
        starts: Arc::new(AtomicUsize::new(0)),
        drops: Arc::new(AtomicUsize::new(0)),
        dropped: Arc::new(tokio::sync::Notify::new()),
    });
    let gate = Arc::new(Gate::default());
    let mut bus = h.engine.bus().subscribe();
    let (child, handle) = h
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
            if matches!(event.event, Event::AgentArchived { child: archived, .. } if archived == child) {
                assert_eq!(h.built.resident_supervisor().episode_count(), 0);
                assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
                break;
            }
        }
    }).await.unwrap();
    assert!(
        !h.engine
            .store()
            .active_actor_ids()
            .await
            .unwrap()
            .contains(&child)
    );
    assert_eq!(
        h.claim_epoch(child).await,
        epoch,
        "sidecar loss must not recover the claim"
    );
    let root = h.engine.replay(h.root).await.unwrap();
    assert_eq!(root.iter().filter(|e| matches!(&e.event, Event::SubagentReported { child: reported, outcome: hya_proto::ReportOutcome::Failed, report, .. } if *reported == child && report == "activation sidecar lost")).count(), 1);
    assert_eq!(root.iter().filter(|e| matches!(&e.event, Event::MailSent { from, body, .. } if from == &handle && body.contains("activation sidecar lost"))).count(), 1);
    let events = h.engine.replay(child).await.unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.event,
        Event::MessageFinished {
            role: hya_proto::Role::Assistant,
            finish: hya_proto::FinishReason::Cancelled,
            ..
        }
    )));
    assert!(events.iter().any(
        |e| matches!(&e.event, Event::TextReplace { text, .. } if text == "flush before failure")
    ));
    h.shutdown().await;
}

#[tokio::test]
async fn m45_r1_user_cancel_retains_mail_and_resumes_its_existing_prompt_once() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("initial work")]).await;
    h.idle(child, &handle).await;
    let gate = Arc::new(Gate::default());
    let marker = h.provider.script(vec![
        Round::text("cancelled work").stream_gated(gate.clone()),
        Round::text("resumed work"),
    ]);
    h.mail(&handle, format!("CANCELLED_WAKE {marker}"))
        .await
        .unwrap();
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(h.root));
    h.until(h.root, |projection| {
        projection
            .team
            .roster
            .get(&handle)
            .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
    })
    .await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let before = h.engine.read_projection(child).await.unwrap();
    let prompt = before.session.messages.iter().find(|message| message.role == hya_proto::Role::User && message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("CANCELLED_WAKE")))).unwrap().id;
    if root.team.roster[&handle].resident_cursor != 0 {
        h.shutdown().await;
        panic!(
            "cancelled wake was consumed: {:?}",
            root.team.roster[&handle]
        );
    }
    assert!(root.team.roster[&handle].resident_work.is_some());
    h.engine.resume_after_user_stop(h.root);
    h.until(child, |projection| projection.session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "resumed work")))).await;
    h.idle(child, &handle).await;
    let actor = h.engine.read_projection(child).await.unwrap();
    let root = h.engine.read_projection(h.root).await.unwrap();
    h.shutdown().await;
    let prompts = actor.session.messages.iter().filter(|message| message.role == hya_proto::Role::User && message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains("CANCELLED_WAKE")))).collect::<Vec<_>>();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].id, prompt);
    assert_eq!(root.team.roster[&handle].resident_cursor, 1);
    assert!(root.team.roster[&handle].resident_work.is_none());
    let requests = h.provider.captured.lock().unwrap();
    let resumed = requests
        .iter()
        .rev()
        .find(|r| r.session == child && !r.completion.tools.is_empty())
        .unwrap();
    assert_resumed_request(&resumed.completion, "cancelled work", None);
}

fn assert_resumed_request(
    request: &hya_provider::CompletionRequest,
    cancelled: &str,
    mail: Option<&str>,
) {
    use hya_proto::Message;
    let messages = &request.messages;
    assert!(
        matches!(messages.last(), Some(Message::User { .. })),
        "resume needs a user prompt at the tail: {messages:?}"
    );
    let roles = messages
        .iter()
        .filter_map(|m| match m {
            Message::User { .. } => Some("user"),
            Message::Assistant { .. } => Some("assistant"),
            Message::System { .. } => None,
        })
        .collect::<Vec<_>>();
    for pair in roles.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "provider roles must alternate: {messages:?}"
        );
    }
    let answer = messages
        .iter()
        .position(|m| {
            matches!(m, Message::Assistant { .. })
                && serde_json::to_string(m).unwrap().contains(cancelled)
        })
        .unwrap();
    assert!(answer < messages.len() - 1);
    if let Some(mail) = mail {
        let deliveries = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                matches!(m, Message::User { .. })
                    && serde_json::to_string(m).unwrap().contains(mail)
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        assert_eq!(
            deliveries,
            [messages.len() - 1],
            "new mail belongs after the cancelled output"
        );
    }
}

#[tokio::test]
async fn m46_s1_cancelled_initial_work_without_mail_is_owed_on_resume() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![
            Round::text("partial initial work").stream_gated(gate.clone()),
            Round::text("initial work resumed"),
        ])
        .await;
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(h.root));
    h.idle(child, &handle).await;
    let mut requests = h.provider.requests.subscribe();
    h.engine.resume_after_user_stop(h.root);
    let request = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        request_for(&mut requests, child),
    )
    .await;
    if request.is_ok() {
        h.idle(child, &handle).await;
    }
    h.shutdown().await;
    assert_resumed_request(
        &request.expect("unfinished initial work was not re-armed"),
        "partial initial work",
        None,
    );
}

#[tokio::test]
async fn m46_s1_consumed_history_does_not_start_an_extra_turn_after_cancel() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![Round::text("initial work").gated(gate.clone())])
        .await;
    let lead = h.engine.try_begin_turn(h.root).unwrap();
    gate.release();
    h.idle(child, &handle).await;
    let marker = h
        .provider
        .script(vec![Round::text("consumed historical mail")]);
    h.mail(&handle, marker).await.unwrap();
    h.until(h.root, |p| p.team.roster[&handle].resident_cursor == 1)
        .await;
    h.idle(child, &handle).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.roster[&handle].resident_cursor, 1);
    assert!(root.team.roster[&handle].resident_work.is_none());
    assert!(h.engine.cancel_user_turn(h.root));
    h.engine.resume_after_user_stop(h.root);
    drop(lead);
    let next = h.provider.script(vec![Round::text("fresh mail")]);
    h.mail(&handle, next).await.unwrap();
    h.until(child, |p| p.session.messages.iter().any(|m| m.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "fresh mail")))).await;
    h.idle(child, &handle).await;
    h.shutdown().await;
    assert_eq!(
        h.provider
            .captured
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.session == child && !r.completion.tools.is_empty())
            .count(),
        3
    );
}

#[tokio::test]
async fn m46_s2_new_mail_after_cancelled_initial_work_is_admitted_at_tail() {
    cancelled_answer_then_new_mail(false).await;
}

#[tokio::test]
async fn m46_s3_new_mail_after_cancelled_mail_work_is_admitted_at_tail() {
    cancelled_answer_then_new_mail(true).await;
}

async fn cancelled_answer_then_new_mail(mail_wake: bool) {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let rounds = vec![Round::text("cancelled answer").stream_gated(gate.clone())];
    let (child, handle) = if mail_wake {
        let (child, handle) = h.spawn(vec![Round::text("initial settled")]).await;
        h.idle(child, &handle).await;
        let marker = h.provider.script(rounds);
        h.mail(&handle, format!("S3_OLD_MAIL {marker}"))
            .await
            .unwrap();
        (child, handle)
    } else {
        h.spawn(rounds).await
    };
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(child));
    h.idle(child, &handle).await;
    let before = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == hya_proto::Role::User)
        .unwrap()
        .clone();
    let marker = h.provider.script(vec![Round::text("new mail answered")]);
    let mut requests = h.provider.requests.subscribe();
    h.engine
        .steer_subagent(child, &format!("S2_NEW_MAIL {marker}"))
        .await
        .unwrap();
    let request = request_for(&mut requests, child).await;
    h.idle(child, &handle).await;
    let actor = h.engine.read_projection(child).await.unwrap();
    let root = h.engine.read_projection(h.root).await.unwrap();
    h.shutdown().await;
    assert_resumed_request(&request, "cancelled answer", Some("S2_NEW_MAIL"));
    assert_eq!(
        *actor
            .session
            .messages
            .iter()
            .find(|m| m.id == before.id)
            .unwrap(),
        before,
        "answered prompt was edited"
    );
    assert_eq!(
        root.team.roster[&handle].resident_cursor,
        if mail_wake { 2 } else { 1 }
    );
    assert!(root.team.roster[&handle].resident_work.is_none());
}

#[tokio::test]
async fn m46_s3_unanswered_admitted_prompt_is_reused_once() {
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![Round::text("not emitted").gated(gate.clone())])
        .await;
    gate.arrived().await;
    assert!(h.engine.cancel_user_turn(child));
    h.idle(child, &handle).await;
    let prompt = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .messages
        .iter()
        .find(|m| m.role == hya_proto::Role::User)
        .unwrap()
        .id;
    let marker = h.provider.script(vec![Round::text("answered once")]);
    let mut requests = h.provider.requests.subscribe();
    h.engine.steer_subagent(child, &marker).await.unwrap();
    let request = request_for(&mut requests, child).await;
    h.idle(child, &handle).await;
    let actor = h.engine.read_projection(child).await.unwrap();
    h.shutdown().await;
    let prompts = actor
        .session
        .messages
        .iter()
        .filter(|m| m.role == hya_proto::Role::User)
        .collect::<Vec<_>>();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].id, prompt);
    assert_eq!(
        request.messages.len(),
        1,
        "empty cancelled assistant output is omitted"
    );
    assert!(matches!(&request.messages[0], hya_proto::Message::User { id, .. } if *id == prompt));
}

#[tokio::test]
async fn m46_s7_work_finish_write_failure_ends_the_episode() {
    use sqlx::Connection;
    let mut h = Harness::new().await;
    let gate = Arc::new(Gate::default());
    let (child, handle) = h
        .spawn(vec![Round::text("completed answer").gated(gate.clone())])
        .await;
    gate.arrived().await;
    let mut db =
        sqlx::SqliteConnection::connect(&format!("sqlite://{}", h.dir.join("events.db").display()))
            .await
            .unwrap();
    sqlx::query("CREATE TRIGGER reject_work_finish BEFORE INSERT ON event_log WHEN json_extract(NEW.payload, '$.type') = 'resident_work_finished' BEGIN SELECT RAISE(ABORT, 'injected work finish failure'); END").execute(&mut db).await.unwrap();
    drop(db);
    gate.release();
    let archived =
        tokio::time::timeout(std::time::Duration::from_secs(2), h.archived(&handle)).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    h.shutdown().await;
    assert!(
        archived.is_ok(),
        "failed completion did not enter its ending sequence: {:?}",
        root.team.roster.get(&handle)
    );
    assert!(
        root.team.archived.contains_key(&handle),
        "failed completion parked the live episode: {:?}",
        root.team.roster.get(&handle)
    );
    assert!(
        root.session
            .members
            .iter()
            .any(|m| m.child == Some(child) && m.status == hya_proto::MemberRunStatus::Failed)
    );
    assert!(h.engine.replay(h.root).await.unwrap().iter().any(|e| matches!(&e.event, Event::SubagentReported { child: actor, outcome: hya_proto::ReportOutcome::Failed, report, .. } if *actor == child && report.contains("injected work finish failure"))));
}

#[tokio::test]
async fn m46_s2_restart_requeues_cancelled_zero_mail_work() {
    let test = "m46_s2_restart_requeues_cancelled_zero_mail_work";
    if std::env::var("HYA_M4_CRASH_SEED").as_deref() == Ok(test) {
        use tokio::io::AsyncWriteExt;
        let h = Harness::open(
            std::path::PathBuf::from(std::env::var_os("HYA_M4_CRASH_DIR").unwrap()),
            None,
            Vec::new(),
        )
        .await;
        let gate = Arc::new(Gate::default());
        let (child, handle) = h
            .spawn(vec![
                Round::text("initial answer before process crash").stream_gated(gate.clone()),
            ])
            .await;
        gate.arrived().await;
        assert!(h.engine.cancel_user_turn(h.root));
        h.idle(child, &handle).await;
        let mut ready =
            tokio::net::TcpStream::connect(std::env::var("HYA_M4_CRASH_READY").unwrap())
                .await
                .unwrap();
        ready
            .write_all(format!("{}\n", h.root).as_bytes())
            .await
            .unwrap();
        std::future::pending::<()>().await;
    }
    let mut h = support::lifecycle::crashed_team(test).await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let (handle, child) = root
        .team
        .roster
        .iter()
        .find(|(path, _)| path.as_str() != "main")
        .map(|(path, entry)| (path.clone(), entry.session))
        .unwrap();
    let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        h.until(h.root, |p| p.team.roster[&handle].resident_work.is_none())
            .await;
    })
    .await;
    let captured = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .find(|r| r.session == child && !r.completion.tools.is_empty())
        .cloned();
    h.shutdown().await;
    request.expect("startup parked zero-mail unfinished work");
    assert_resumed_request(
        &captured.unwrap().completion,
        "initial answer before process crash",
        None,
    );
}

#[tokio::test]
async fn m45_r1_user_cancel_before_begin_work_rearms_mail_for_resume() {
    let plugin = support::lifecycle::BlockingPlugin::for_hook("message.user.before").await;
    let mut h = Harness::with_plugins(vec![plugin.spec.clone()]).await;
    let (child, handle) = h.spawn(vec![Round::text("initial work")]).await;
    h.idle(child, &handle).await;
    plugin.arm_session(child);
    let marker = h
        .provider
        .script(vec![Round::text("resumed pre-admission wake")]);
    h.mail(&handle, marker.clone()).await.unwrap();
    plugin.gate.arrived().await;
    assert!(h.engine.cancel_user_turn(h.root));
    let idle = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        h.until(h.root, |projection| {
            projection
                .team
                .roster
                .get(&handle)
                .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
        }),
    )
    .await;
    plugin.gate.release();
    if idle.is_err() {
        h.shutdown().await;
        drop(h);
        plugin.shutdown().await;
        panic!("user cancel did not release the blocked pre-admission hook");
    }
    let root = h.engine.read_projection(h.root).await.unwrap();
    assert_eq!(root.team.roster[&handle].resident_cursor, 0);
    assert!(root.team.roster[&handle].resident_work.is_none());
    h.engine.resume_after_user_stop(h.root);
    h.until(child, |projection| projection.session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "resumed pre-admission wake")))).await;
    h.idle(child, &handle).await;
    let actor = h.engine.read_projection(child).await.unwrap();
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
    assert_eq!(actor.session.messages.iter().filter(|message| message.role == hya_proto::Role::User && message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text.contains(&marker)))).count(), 1);
}

#[derive(Clone)]
struct LateLossSidecar {
    fail_stop: bool,
    dead: Arc<std::sync::atomic::AtomicBool>,
    stopped: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl hya_core::BoundSidecarFactory for LateLossSidecar {
    async fn start(
        &self,
        _: hya_core::SidecarStart,
    ) -> Result<Box<dyn hya_core::SidecarHandle>, hya_core::CoreError> {
        Ok(Box::new(self.clone()))
    }
}
#[async_trait::async_trait]
impl hya_core::SidecarHandle for LateLossSidecar {
    async fn ready(&mut self) -> Result<(), hya_core::CoreError> {
        Ok(())
    }
    async fn shutdown(&mut self) -> Result<(), hya_core::CoreError> {
        self.stopped.notify_one();
        if self.fail_stop {
            return Err(hya_core::CoreError::Invalid("late loss stop failed".into()));
        }
        Ok(())
    }
    fn is_healthy(&self) -> bool {
        !self.dead.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[tokio::test]
async fn m45_r3_sidecar_death_after_user_cancel_keeps_the_episode_held() {
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
    sidecar
        .dead
        .store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::timeout(support::lifecycle::BOUND, sidecar.stopped.notified())
        .await
        .unwrap();
    h.until(h.root, |projection| {
        projection.team.archived.contains_key(&handle)
            || projection
                .team
                .roster
                .get(&handle)
                .is_some_and(|entry| entry.status == hya_proto::RosterStatus::Idle)
    })
    .await;
    let root = h.engine.read_projection(h.root).await.unwrap();
    let held = h.engine.stopped_by_user(child);
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert!(
        !root.team.archived.contains_key(&handle),
        "sidecar death converted user stop into failure"
    );
    assert!(held);
    assert!(!events.iter().any(|envelope| matches!(&envelope.event, Event::SubagentReported { child: reported, .. } if *reported == child)));
}

#[tokio::test]
async fn m45_r3_forced_ending_keeps_a_late_dead_sidecars_stop_diagnostic() {
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
    sidecar
        .dead
        .store(true, std::sync::atomic::Ordering::SeqCst);
    output(&h.tool("archive", json!({"target": handle})).await);
    let events = h.engine.replay(h.root).await.unwrap();
    h.shutdown().await;
    assert!(events.iter().any(|envelope| matches!(&envelope.event, Event::MemberFinished { child: Some(member), summary, .. } if *member == child && summary.contains("sidecar did not stop") && summary.contains("late loss stop failed"))), "forced ending discarded its sidecar stop diagnostic: {events:?}");
}

#[tokio::test]
async fn m46_sidecar_failed_episode_revives_with_only_new_mail_and_working_tools() {
    let test = "m46_sidecar_failed_episode_revives_with_only_new_mail_and_working_tools";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    support::lifecycle::install_sidecar_bundle().await;
    let mut control = support::lifecycle::SidecarControl::new(false).await;
    let mut h = Harness::new().await;
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::text("initial")])
        .await;
    control.started().await;
    h.idle(child, &handle).await;
    let completed = h.provider.script(vec![Round::text("completed mail work")]);
    h.mail(&handle, format!("COMPLETED_BEFORE_LOSS {completed}"))
        .await
        .unwrap();
    h.until(child, |projection| projection.session.messages.iter().any(|message| message.parts.iter().any(|part| matches!(part, hya_proto::PartProjection::Text { text, .. } if text == "completed mail work")))).await;
    h.idle(child, &handle).await;
    assert_eq!(
        h.engine.read_projection(h.root).await.unwrap().team.roster[&handle].resident_cursor,
        1
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
    let doc = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .handoff
        .unwrap()
        .doc;
    assert!(doc.contains("UNREAD_BEFORE_LOSS"));
    assert!(doc.contains("INTERRUPTED_BEFORE_LOSS"));
    assert!(!doc.contains("COMPLETED_BEFORE_LOSS"));
    let next = h.provider.script(vec![
        Round::tool("echo", json!({})),
        Round::text("new episode finished"),
    ]);
    let mut requests = h.provider.requests.subscribe();
    h.mail(&handle, next.clone()).await.unwrap();
    control.started().await;
    let request = request_for(&mut requests, child).await;
    h.idle(child, &handle).await;
    let users = request
        .messages
        .iter()
        .filter(|message| matches!(message, hya_proto::Message::User { .. }))
        .map(|message| serde_json::to_string(message).unwrap())
        .collect::<String>();
    let systems = request
        .messages
        .iter()
        .filter(|message| matches!(message, hya_proto::Message::System { .. }))
        .map(|message| serde_json::to_string(message).unwrap())
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
    assert!(events.iter().any(|envelope| matches!(&envelope.event, Event::ToolResult { output, .. } if output.to_string().contains("sidecar echo"))), "revived sidecar did not execute its tool");
    h.shutdown().await;
    control.shutdown().await;
}

#[tokio::test]
async fn m46_forced_handoff_describes_the_ended_episode_instead_of_copying_its_predecessor() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![report("old episode")]).await;
    h.archived(&handle).await;
    let next = h
        .provider
        .script(vec![Round::text("CURRENT_EPISODE_STATE")]);
    h.mail(&handle, format!("CURRENT_EPISODE_GOAL {next}"))
        .await
        .unwrap();
    h.idle(child, &handle).await;
    output(&h.tool("archive", json!({"target": handle})).await);
    let handoff = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .handoff
        .unwrap();
    h.shutdown().await;
    assert_eq!(handoff.generation, 2);
    assert!(handoff.degraded);
    assert!(
        handoff
            .doc
            .contains("1. Goal - [mail from main] CURRENT_EPISODE_GOAL"),
        "{}",
        handoff.doc
    );
    assert!(
        handoff
            .doc
            .contains("2. Current state - CURRENT_EPISODE_STATE"),
        "{}",
        handoff.doc
    );
    assert!(handoff.doc.contains("(from generation 1)"));
}

#[tokio::test]
async fn m46_handoff_summarizer_receives_only_the_latest_episode() {
    let mut h = Harness::new().await;
    let (_child, handle) = h.spawn(vec![report("OLD_EPISODE_EVIDENCE")]).await;
    h.archived(&handle).await;
    let next = h.provider.script(vec![report("CURRENT_EPISODE_EVIDENCE")]);
    h.mail(&handle, next).await.unwrap();
    h.archived(&handle).await;
    let requests = h
        .provider
        .captured
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.completion.tools.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    h.shutdown().await;
    assert_eq!(requests.len(), 2);
    let latest = serde_json::to_string(&requests.last().unwrap().completion.messages).unwrap();
    assert!(latest.contains("CURRENT_EPISODE_EVIDENCE"), "{latest}");
    assert!(!latest.contains("OLD_EPISODE_EVIDENCE"), "{latest}");
}

#[tokio::test]
async fn m46_in_episode_compaction_does_not_replace_the_handoff_goal() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("initial state")]).await;
    h.idle(child, &handle).await;
    let actor = h.engine.read_projection(child).await.unwrap();
    let goal = actor
        .session
        .messages
        .iter()
        .find(|message| message.role == hya_proto::Role::User)
        .unwrap()
        .parts
        .iter()
        .filter_map(|part| match part {
            hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    h.engine.summarize_session(child).await.unwrap();
    let later = h.provider.script(vec![Round::text("latest episode state")]);
    h.mail(&handle, later).await.unwrap();
    h.idle(child, &handle).await;
    output(&h.tool("archive", json!({"target": handle})).await);
    let handoff = h
        .engine
        .read_projection(child)
        .await
        .unwrap()
        .session
        .handoff
        .unwrap();
    h.shutdown().await;
    assert!(
        handoff.doc.starts_with(&format!("1. Goal - {goal}\n")),
        "{}",
        handoff.doc
    );
    assert!(
        handoff
            .doc
            .contains("2. Current state - latest episode state")
    );
}

#[tokio::test]
async fn m46_review_s6_promptless_catalog_agent_keeps_host_base_after_revive() {
    const BASE: &str = "HOST_BASE_PROMPT_MUST_SURVIVE_REVIVE";
    let mut h = Harness::with_base_prompt(BASE).await;
    let package = hya_bundle::prepare_package(hya_bundle::BundleSource::new(
        "review-promptless",
        vec![hya_bundle::SourceFile::new("bundle.yaml", "kind: Bundle\nidentity: { id: test/review-promptless, version: 1.0.0, publisher: test }\nagents:\n  - id: review-worker\n    role: subagent\n")],
    )).unwrap();
    let catalog = hya_core::AgentCatalog::new(Arc::new(
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
        .spawn_agent("review-worker", vec![Round::text("first episode")])
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
    let second = h.provider.script(vec![Round::text("revived episode")]);
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
