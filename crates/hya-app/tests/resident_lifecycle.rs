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
    output(&h.tool("archive", json!({"target": handle})).await);
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
async fn i8_sidecar_loss_flushes_cooperative_stream_before_recovery() {
    let test = "i8_sidecar_loss_flushes_cooperative_stream_before_recovery";
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
#[ignore = "M4-4: I6"]
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
    let (child, handle) = h
        .spawn_agent("m4-sidecar-worker", vec![Round::text("initial activation")])
        .await;
    control.started().await;
    h.idle(child, &handle).await;
    plugin.arm_session(child);
    let marker = h
        .provider
        .script(vec![Round::text("cancel-ignoring activation hook")]);
    h.mail(&handle, marker).await.unwrap();
    plugin.gate.arrived().await;
    (h, plugin, control, child, handle)
}

#[tokio::test]
#[ignore = "M4-4: I2"]
async fn i2_archive_bounds_stuck_turn_and_sidecar_termination() {
    let test = "i2_archive_bounds_stuck_turn_and_sidecar_termination";
    if support::lifecycle::isolated_sidecar_config(test) {
        return;
    }
    let (mut h, plugin, mut control, child, handle) = blocked_sidecar().await;
    let mut ending = h.start_tool("archive", json!({"target":handle})).await;
    let bounded = tokio::time::timeout(support::lifecycle::BOUND, &mut ending)
        .await
        .is_ok();
    let root = h.engine.read_projection(h.root).await.unwrap();
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
#[ignore = "M4-3: I5"]
async fn i5_post_kill_task_uses_fresh_team_without_waiting_for_other_endings() {
    let test = "i5_post_kill_task_uses_fresh_team_without_waiting_for_other_endings";
    if support::lifecycle::isolated_sidecar_config_with(test, &[("HYA_SUBAGENT_TURN_BUDGET", "3")])
    {
        return;
    }
    let (mut h, plugin, mut control, _, stuck) = blocked_sidecar().await;
    let (other, handle) = h.spawn(vec![Round::text("another real activation")]).await;
    h.until(other, |p| {
        p.session
            .messages
            .iter()
            .any(|m| m.role == hya_proto::Role::Assistant && m.finish.is_some())
    })
    .await;
    if !h
        .engine
        .read_projection(h.root)
        .await
        .unwrap()
        .team
        .archived
        .contains_key(&stuck)
    {
        h.mail(
            &handle,
            "one more real activation trips the turn budget".into(),
        )
        .await
        .unwrap();
    }
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
                if request.session == h.root && request.completion.messages.last().is_some_and(|m| matches!(m, hya_proto::Message::System { content, .. } if content.contains("TEAM QUIESCED"))) {
                    break request.completion.system.is_some_and(|system| system.contains("NEW_TEAM_GUIDANCE_M4"));
                }
            }
        }).await.unwrap_or(false)
    } else {
        false
    };
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
#[ignore = "M4-2: I6"]
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
    h.shutdown().await;
    drop(h);
    plugin.shutdown().await;
}

#[tokio::test]
#[ignore = "M4-5: I15"]
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
    h.shutdown().await;
    assert_eq!(
        mail_prompts.len(),
        1,
        "one wake admitted multiple prompts: {mail_prompts:?}"
    );
    let text = serde_json::to_string(mail_prompts[0]).unwrap();
    assert!(text.contains("COALESCE_FIRST") && text.contains("COALESCE_SECOND"));
}

#[tokio::test]
#[ignore = "M4-5: I15"]
async fn i15_failed_prompt_admission_rolls_back_work_started() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("initial activation")]).await;
    h.idle(child, &handle).await;
    let count = |events: &[hya_proto::Envelope]| {
        events.iter().filter(|e| matches!(&e.event, Event::ResidentWorkStarted { actor_session, .. } if *actor_session == child)).count()
    };
    let before = count(&h.engine.replay(h.root).await.unwrap());
    h.reject_wake_prompt(child).await;
    h.mail(&handle, "rejected wake".into()).await.unwrap();
    h.archived(&handle).await;
    let after = count(&h.engine.replay(h.root).await.unwrap());
    h.heal().await;
    h.shutdown().await;
    assert_eq!(
        after, before,
        "work-start committed without its admitted prompt"
    );
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
#[ignore = "M4-6: I11"]
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
#[ignore = "M4-6: I12"]
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
#[ignore = "M4-3: I6"]
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
#[ignore = "M4-2: I3"]
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
#[ignore = "M4-2: I4"]
async fn i4_failed_terminal_write_can_be_reconciled() {
    let mut h = Harness::new().await;
    let (child, handle) = h.spawn(vec![Round::text("working")]).await;
    h.idle(child, &handle).await;
    let epoch = h.claim_epoch(child).await;
    h.trigger("agent_archived").await;
    let failed = h.tool("archive", json!({"target": handle})).await;
    assert!(matches!(failed, ToolPartState::Error { .. }));
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
#[ignore = "M4-1: I7"]
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
#[ignore = "M4-6: I9"]
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
#[ignore = "M4-6: I10"]
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
#[ignore = "M4-3: I17"]
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
