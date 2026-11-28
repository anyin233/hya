//! T2.1–T2.6, T2.9, T2.16–T2.17, T2.23, T2.31, T2.49 (server quiesce) —
//! subagent roster/spawn/batch/nested tree, model preference & dispatch-time model
//! resolution, resident mailbox DM/broadcast/channels/report, wait/archive/wake, and
//! server-side `TEAM QUIESCED` synthesis.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use hya_e2e::{
    E2eEnvBuilder, ScriptStep, text_step, tool_step, tree_children, tree_max_depth,
    tree_session_ids, tree_subagent_types, wait_until,
};
use hya_proto::Event;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(30);

fn spawn_marked(marker: &str, model: &str) -> ScriptStep {
    let mut task = json!({
        "description": format!("child {marker}"),
        "prompt": format!("report {marker}"),
        "subagent_type": "hya-task",
        "inline_agent": {
            "description": "",
            "category": "",
            "model": "",
            "name": "",
            "prompt": format!("MARKER_{marker} report {marker}"),
            "resident": false
        }
    });
    if !model.is_empty() {
        task["model"] = json!(model);
    }
    tool_step("task", task)
}

/// T2.1, T2.2, T2.3, T2.16, T2.17 on one booted backend.
#[tokio::test]
async fn subagent_roster_spawn_batch_nested_and_model_routing() {
    let env = E2eEnvBuilder::new()
        .additional_models(["pref-target", "override-target"])
        .route("MARKER_SINGLE", vec![text_step("CHILD_TASK_OK")])
        .route("BATCH_CHILD_ONE", vec![text_step("BATCH_FIRST_RESULT")])
        .route("BATCH_CHILD_TWO", vec![text_step("BATCH_SECOND_RESULT")])
        .route("MARKER_BR1", vec![text_step("BR1_OK")])
        .route("MARKER_BR2", vec![text_step("BR2_OK")])
        .route("MARKER_VENDOR", vec![text_step("VENDOR_OK")])
        .route("MARKER_NONE", vec![text_step("NONE_OK")])
        .route(
            "MARKER_NESTED_CHILD",
            vec![
                tool_step(
                    "task",
                    json!({
                        "description": "spawn hya-plan",
                        "prompt": "report GRANDCHILD_OK",
                        "subagent_type": "hya-plan"
                    }),
                ),
                text_step("NESTED_CHILD_OK"),
            ],
        )
        .route("hya-plan", vec![text_step("GRANDCHILD_OK")])
        .build()
        .await
        .expect("e2e env");

    // 1. T2.3: /v2/agents roster + list_agents hides workflow-private agents + spawn refusal.
    let agents = env.list_agents().await.expect("list agents").to_string();
    assert!(agents.contains("hya-main") && agents.contains("hya-task"));
    let scoped = env
        .get_json(&format!(
            "/v2/agents?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .unwrap()
        .to_string();
    assert!(scoped.contains("hya-main"));

    let s_roster = env.create_session().await.unwrap();
    env.fake
        .route_session(
            "probe roster",
            [
                tool_step("list_agents", json!({})),
                tool_step(
                    "task",
                    json!({
                        "description": "Private verifier probe",
                        "prompt": "Verify a goal",
                        "subagent_type": "hya-goal-verifier"
                    }),
                ),
                text_step("ROSTER_PROBED"),
            ],
        )
        .unwrap();
    env.prompt(s_roster, "probe roster").await.unwrap();
    let roster_events = env.events(s_roster, None).await.unwrap();
    let roster_results: Vec<_> = roster_events
        .iter()
        .filter_map(|e| match &e.event {
            Event::ToolResult { output, .. } => Some(output.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(roster_results.len(), 1);
    assert!(roster_results[0].contains("hya-task"));
    for private in [
        "hya-goal-guide",
        "hya-goal-verifier",
        "hya-pir-planner",
        "hya-pir-implementer",
        "hya-pir-reviewer",
    ] {
        assert!(!roster_results[0].contains(private), "leaked {private}");
    }
    assert!(roster_events.iter().any(|e| matches!(
        &e.event,
        Event::ToolError { message_text, .. } if message_text.contains("AGENT_SPAWN_NOT_ALLOWED")
    )));

    // 2. T2.1 (single spawn + DM mail event). Each phase scripts its own session:
    // any session may still get a late `TEAM QUIESCED` synthesis wake.
    let s_single = env.create_session().await.unwrap();
    env.fake
        .route_session(
            "spawn a single hya-task",
            [
                tool_step(
                    "task",
                    json!({
                        "description": "e2e child",
                        "prompt": "do the child work",
                        "subagent_type": "hya-task",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_SINGLE do the child work",
                            "resident": false
                        }
                    }),
                ),
                tool_step("wait", json!({"timeout_secs": 30})),
                text_step("PARENT_AFTER_TASK"),
            ],
        )
        .unwrap();
    env.prompt(s_single, "spawn a single hya-task")
        .await
        .unwrap();
    let tree = env.session_tree(&s_single).await.unwrap();
    assert!(!tree_children(&tree).is_empty(), "{tree}");
    assert!(tree_subagent_types(&tree).iter().any(|k| k == "hya-task"));
    let child_ids: Vec<_> = tree_session_ids(&tree)
        .into_iter()
        .filter(|id| id != &s_single.to_string())
        .collect();
    assert_eq!(child_ids.len(), 1);
    assert!(
        env.route_dump("MARKER_SINGLE")
            .unwrap()
            .contains("[mail from main] do the child work")
    );
    let single_events = env.events(s_single, None).await.unwrap();
    let dm = single_events
        .iter()
        .find_map(
            |e| match hya_tool::extension::ExtensionEvent::from_event(&e.event) {
                Some(hya_tool::extension::ExtensionEvent::MailSent {
                    from,
                    to: hya_proto::MailEndpoint::Channel(ch),
                    body,
                    ..
                }) if from == "main" && body == "do the child work" => Some(ch),
                _ => None,
            },
        )
        .expect("DM MailSent event");
    assert!(single_events.iter().any(|e| matches!(
        hya_tool::extension::ExtensionEvent::from_event(&e.event),
        Some(hya_tool::extension::ExtensionEvent::ChannelCreated { ref channel, kind: hya_proto::ChannelKind::Dm, .. }) if channel == &dm
    )));

    // 3. T2.1 (batch spawn: two distinct children with independent outputs).
    let s_batch = env.create_session().await.unwrap();
    env.fake
        .route_session(
            "launch batch",
            [
                tool_step(
                    "task",
                    json!({
                        "context": "Batch shared contract",
                        "tasks": [
                            {"description": "First", "prompt": "First work", "inline_agent": {"prompt": "BATCH_CHILD_ONE"}},
                            {"description": "Second", "prompt": "Second work", "inline_agent": {"prompt": "BATCH_CHILD_TWO"}}
                        ]
                    }),
                ),
                tool_step("wait", json!({"timeout_secs": 30})),
                text_step("BATCH_DONE"),
            ],
        )
        .unwrap();
    env.prompt(s_batch, "launch batch").await.unwrap();
    let btree = env.session_tree(&s_batch).await.unwrap();
    let bids: Vec<_> = tree_session_ids(&btree)
        .into_iter()
        .filter(|id| id != &s_batch.to_string())
        .collect();
    assert_eq!(bids.len(), 2);
    assert_ne!(bids[0], bids[1]);
    let batch_results: Vec<_> = env
        .events(s_batch, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::ToolResult { output, .. } if output["metadata"]["members"].is_array() => {
                Some(output)
            }
            _ => None,
        })
        .collect();
    assert_eq!(batch_results.len(), 1);
    assert_eq!(
        batch_results[0]["metadata"]["members"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(batch_results[0]["metadata"]["status"], "running");
    assert!(
        env.route_dump("BATCH_CHILD_ONE")
            .unwrap()
            .contains("First work")
            && env
                .route_dump("BATCH_CHILD_TWO")
                .unwrap()
                .contains("Second work")
    );

    // 4. T2.16 + T2.17: /v2/agent-models preference + all four dispatch resolution branches.
    let listed = env.get_json("/v2/agent-models").await.unwrap();
    let task_row = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agentId"] == "hya-task")
        .unwrap();
    assert_eq!(task_row["source"], json!("AGENT_MODEL_SOURCE_DEFAULT"));

    // Branch 1 (exact valid id) & Branch 2 (substring match) before setting preference.
    for (marker, requested_model, expected_wire) in [
        ("BR1", "fake/pref-target", "pref-target"),
        ("BR2", "target", "override-target"),
    ] {
        let s = env.create_session().await.unwrap();
        env.fake
            .route_session(
                format!("dispatch {marker}"),
                [
                    spawn_marked(marker, requested_model),
                    tool_step("wait", json!({"timeout_secs": 30})),
                    text_step("DONE"),
                ],
            )
            .unwrap();
        env.prompt(s, format!("dispatch {marker}")).await.unwrap();
        let reqs = env
            .fake
            .route_requests(&format!("MARKER_{marker}"))
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            reqs[0]["model"],
            json!(expected_wire),
            "branch {marker} model mismatch"
        );
    }

    // Set durable preference: hya-task -> fake/pref-target.
    env.put_json(
        "/v2/agent-models/hya-task",
        &json!({"preference": {"providerId": "fake", "modelId": "pref-target"}}),
    )
    .await
    .unwrap();
    let listed = env.get_json("/v2/agent-models").await.unwrap();
    let task_row = listed["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agentId"] == "hya-task")
        .unwrap();
    assert_eq!(task_row["source"], json!("AGENT_MODEL_SOURCE_REMEMBERED"));
    assert_eq!(task_row["preference"]["modelId"], json!("pref-target"));

    // Branch 3 (bare vendor id defers) & Branch 4 (unspecified uses remembered preference).
    for (marker, requested_model) in [("VENDOR", "fake"), ("NONE", "")] {
        let s = env.create_session().await.unwrap();
        env.fake
            .route_session(
                format!("dispatch {marker}"),
                [
                    spawn_marked(marker, requested_model),
                    tool_step("wait", json!({"timeout_secs": 30})),
                    text_step("DONE"),
                ],
            )
            .unwrap();
        env.prompt(s, format!("dispatch {marker}")).await.unwrap();
        let reqs = env
            .fake
            .route_requests(&format!("MARKER_{marker}"))
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            reqs[0]["model"],
            json!("pref-target"),
            "branch {marker} must use remembered preference"
        );
    }

    // 5. T2.2: nested subagent tree depth >= 2 (root -> hya-task -> hya-plan).
    let s_nested = env.create_session().await.unwrap();
    env.fake
        .route_session(
            "nested subagents",
            [
                tool_step(
                    "task",
                    json!({
                        "description": "spawn nested child",
                        "prompt": "spawn a hya-plan child then finish",
                        "subagent_type": "hya-task",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_NESTED_CHILD spawn hya-plan",
                            "resident": false
                        }
                    }),
                ),
                text_step("ROOT_NESTED_OK"),
            ],
        )
        .unwrap();
    env.prompt(s_nested, "nested subagents").await.unwrap();

    wait_until("nested tree depth >= 2", TIMEOUT, || async {
        let tree = env.session_tree(&s_nested).await?;
        let mut unique = tree_session_ids(&tree);
        unique.sort();
        unique.dedup();
        Ok(unique.len() >= 3 && tree_max_depth(&tree) >= 2)
    })
    .await
    .unwrap_or_else(|e| panic!("nested tree incomplete: {e}; {}", env.diagnostics()));
    let nested_tree = env.session_tree(&s_nested).await.unwrap();

    let kinds = tree_subagent_types(&nested_tree);
    assert!(
        kinds.iter().any(|k| k == "hya-task") && kinds.iter().any(|k| k == "hya-plan"),
        "expected hya-task and hya-plan in {kinds:?}"
    );
}

/// T2.4, T2.5, T2.6, T2.9, T2.23, T2.49 (server quiesce) — resident DM, broadcast,
/// `channels` listing, default subagent bundle worker `report` (no extra round after
/// accepted report), and server-side `TEAM QUIESCED` User synthesis wake.
#[tokio::test]
async fn mailbox_dm_broadcast_worker_report_and_quiesce() {
    const SYS_BROADCAST: &str = "SYS_BROADCAST_CHILD";
    const SYS_WORKER: &str = "You are hya-task";

    let env = E2eEnvBuilder::new()
        .route(
            SYS_BROADCAST,
            // Empty replies keep this resident live (not finished) so it receives the
            // broadcast and then makes the team quiesce once idle.
            vec![text_step(""), text_step(""), text_step("EXTRA")],
        )
        .route(
            SYS_WORKER,
            vec![
                tool_step("send", json!({"body": "WORKER_MAIL_OK"})),
                tool_step(
                    "report",
                    json!({"result": "WORKER_REPORT_OK", "outcome": "done"}),
                ),
                text_step("WORKER_FOLLOWUP_MUST_NOT_RUN"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    // Session 1 (T2.5 + T2.23): default subagent bundle worker sends vertical DM to parent,
    // calls `report`, takes no model round after accepted report, and archives cleanly.
    const S1: &str = "run default worker";
    env.fake
        .route_session(
            S1,
            [
                tool_step(
                    "task",
                    json!({
                        "description": "default bundle worker",
                        "prompt": "mail parent then report",
                        "subagent_type": "hya-task"
                    }),
                ),
                text_step("ROOT_SPAWNED_WORKER"),
                text_step("ROOT_ACK_1"),
                text_step("ROOT_ACK_2"),
                text_step("ROOT_ACK_3"),
            ],
        )
        .unwrap();
    let s1 = env.create_session().await.unwrap();
    env.prompt(s1, S1).await.unwrap();
    env.wait_route_contains(S1, "WORKER_MAIL_OK", TIMEOUT)
        .await
        .expect("parent received worker DM");
    env.wait_route_contains(S1, "WORKER_REPORT_OK", TIMEOUT)
        .await
        .expect("parent received worker report");
    env.wait_session_idle(&s1, TIMEOUT)
        .await
        .expect("session 1 idle");

    let worker_reqs = env.fake.route_requests(SYS_WORKER).unwrap().unwrap();
    assert_eq!(
        worker_reqs.len(),
        2,
        "no model request may follow accepted report"
    );
    assert!(
        worker_reqs
            .iter()
            .all(|r| !r.to_string().contains("Report accepted") && r["model"] == "model")
    );

    // Session 2 (T2.4, T2.6, T2.9, T2.49 server quiesce): spawn live resident, inspect
    // `channels`, broadcast `ALL_HANDS_BROADCAST`, and verify User `TEAM QUIESCED` wake.
    const S2: &str = "broadcast to live team";
    env.fake
        .route_session(
            S2,
            [
                tool_step(
                    "task",
                    json!({ "members": [{
                        "description": "broadcast listener",
                        "prompt": "stay ready for broadcast",
                        "subagent_type": "hya-task",
                        "inline_agent": { "prompt": format!("{SYS_BROADCAST} resident") }
                    }]}),
                ),
                tool_step("list_channel", json!({})),
                tool_step("send", json!({ "body": "ALL_HANDS_BROADCAST" })),
                text_step("ROOT_SPAWNED_AND_SENT"),
                text_step("SYNTHESIZED_ANSWER"),
                text_step("ROOT_EXTRA"),
            ],
        )
        .unwrap();
    let s2 = env.create_session().await.unwrap();
    env.prompt(s2, S2).await.unwrap();
    env.wait_route_contains(SYS_BROADCAST, "ALL_HANDS_BROADCAST", TIMEOUT)
        .await
        .expect("child heard broadcast");
    env.wait_route_contains(S2, "TEAM QUIESCED", TIMEOUT)
        .await
        .expect("server quiesce synthesis wake");

    let root_reqs = env.fake.route_requests(S2).unwrap().unwrap();
    let root_dump = root_reqs
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        root_dump.contains("· group ·")
            && root_dump.contains("· dm ·")
            && root_dump.contains("hya-task-"),
        "list_channel tool must surface real channels and team handles: {root_dump}"
    );
    assert!(
        root_reqs.iter().any(|req| req["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["role"] == "user" && m["content"].to_string().contains("TEAM QUIESCED"))),
        "TEAM QUIESCED must be an admitted user prompt"
    );
}

/// T2.31 — `wait` woken by report (and repeated wait returns at once without re-steering),
/// plain-answer worker finishes cleanly, empty-reply worker stalls `wait`, `archive` stops
/// it, and `send` to its handle wakes it again — all on one seeded backend.
#[tokio::test]
async fn wait_report_plain_answer_stall_archive_and_wake() {
    const SEED: u64 = 36;
    let mut rng = hya_proto::handle_naming::SplitMix64::seeded(SEED);
    let handle_report = format!(
        "main/{}",
        hya_proto::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng)
    );
    let _handle_plain = format!(
        "main/{}",
        hya_proto::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng)
    );
    let handle_stall = format!(
        "main/{}",
        hya_proto::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng)
    );

    let spawn_inline = |marker: &str| {
        tool_step(
            "task",
            json!({
                "description": format!("worker {marker}"),
                "prompt": "do the unit",
                "subagent_type": "hya-task",
                "inline_agent": { "prompt": format!("{marker} You are a worker.") }
            }),
        )
    };

    let env = E2eEnvBuilder::new()
        .backend_env("HYA_HANDLE_SEED", SEED.to_string())
        .route(
            "MARKER_W_REPORT",
            vec![tool_step(
                "report",
                json!({"result": "WAITED_REPORT_OK", "outcome": "done"}),
            )],
        )
        .route("MARKER_W_PLAIN", vec![text_step("PLAIN_ANSWER_OK")])
        // Session 3 spawns the default `hya-task` agent (no inline_agent override) so waking
        // it from archive matches the persisted agent's `"You are hya-task"` system prompt.
        .route(
            "You are hya-task",
            vec![
                text_step(""),
                text_step("WORKER_WOKEN_AGAIN"),
                text_step("WORKER_EXTRA"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    // 1. Report wakes `wait`; repeated `wait` returns immediately; report not re-steered.
    const S1: &str = "delegate and wait";
    let s1 = env.create_session().await.unwrap();
    env.fake
        .route_session(
            S1,
            [
                spawn_inline("MARKER_W_REPORT"),
                tool_step("wait", json!({"timeout_secs": 60})),
                tool_step(
                    "wait",
                    json!({"targets": [handle_report], "timeout_secs": 60}),
                ),
                text_step("ROOT_AFTER_WAIT"),
            ],
        )
        .unwrap();
    env.prompt(s1, S1).await.unwrap();
    let dump1 = env.route_dump(S1).unwrap();
    assert!(
        dump1.contains("Subagents finished.")
            && dump1.contains("WAITED_REPORT_OK")
            && dump1.contains("every target already finished")
            && !dump1.contains("[NEW MAIL · answer"),
        "wait+repeat assertions failed: {dump1}"
    );
    assert!(
        env.session_context(&s1)
            .await
            .unwrap()
            .to_string()
            .contains("ROOT_AFTER_WAIT")
    );

    // 2. Plain answer without `report` finishes the worker cleanly (never stalled).
    const S2: &str = "delegate plain worker";
    let s2 = env.create_session().await.unwrap();
    env.fake
        .route_session(
            S2,
            [
                spawn_inline("MARKER_W_PLAIN"),
                tool_step("wait", json!({"timeout_secs": 60})),
                text_step("ROOT_AFTER_PLAIN"),
            ],
        )
        .unwrap();
    env.prompt(s2, S2).await.unwrap();
    let s2_dump = env.route_dump(S2).unwrap();
    assert!(
        s2_dump.contains("Subagents finished.")
            && s2_dump.contains("PLAIN_ANSWER_OK")
            && !s2_dump.contains("stopped without reporting"),
        "plain answer must finish worker: {s2_dump}"
    );

    // 3. Empty reply stalls `wait`; `archive` stops worker; `send` to handle wakes it.
    const S3: &str = "delegate, archive, wake";
    let s3 = env.create_session().await.unwrap();
    env.fake
        .route_session(
            S3,
            [
                tool_step(
                    "task",
                    json!({
                        "description": "worker stall",
                        "prompt": "do the unit",
                        "subagent_type": "hya-task"
                    }),
                ),
                tool_step("wait", json!({"timeout_secs": 60})),
                tool_step(
                    "archive",
                    json!({"target": handle_stall, "reason": "pause"}),
                ),
                tool_step(
                    "send",
                    json!({"channel": handle_stall, "body": "WAKE_UP_PLEASE"}),
                ),
                text_step("ROOT_DONE"),
                text_step("ROOT_EXTRA_1"),
                text_step("ROOT_EXTRA_2"),
            ],
        )
        .unwrap();
    env.prompt(s3, S3).await.unwrap();
    env.wait_route_contains(S3, "stopped without reporting", TIMEOUT)
        .await
        .expect("stall wake");
    env.wait_route_contains(S3, &format!("Archived `{handle_stall}`"), TIMEOUT)
        .await
        .expect("archive confirmation");
    env.wait_route_contains("You are hya-task", "WAKE_UP_PLEASE", TIMEOUT)
        .await
        .expect("archived worker woken by mail");
}

/// Verify `hya/agent-channels` native ABI v2 `UriSchemaHandler` (`channel://`)
/// resolves identically through BOTH `hya/hashline` `read` (default full mode)
/// AND standard `hya-bare` `read` (`bundles.builtin.disable: [hya/hashline]`),
/// and verify the read-only `BundleApi` endpoints (`/channels`,
/// `/channels/{channel}/messages`, `/inbox`, `/team-status`).
#[tokio::test]
async fn channel_uri_read_works_with_both_hashline_and_bare_read_and_bundle_api() {
    for (label, disable_hashline) in [("hashline", false), ("bare-read", true)] {
        const SEED: u64 = 99;
        let mut rng = hya_proto::handle_naming::SplitMix64::seeded(SEED);
        let child_handle = format!(
            "main/{}",
            hya_proto::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng)
        );
        let prompt = format!("channel uri read probe ({label})");
        let marker = format!("MARKER_URI_{}", label.replace('-', "_"));

        let mut builder = E2eEnvBuilder::new()
            .backend_env("HYA_HANDLE_SEED", SEED.to_string())
            .route(
                marker.clone(),
                vec![
                    tool_step(
                        "send",
                        json!({"channel": "^parent", "body": format!("HELLO_FROM_CHILD_{label}")}),
                    ),
                    tool_step(
                        "report",
                        json!({"result": format!("REPORT_OK_{label}"), "outcome": "done"}),
                    ),
                ],
            );
        if disable_hashline {
            builder = builder.disable_builtin_bundle("hya/hashline");
        }
        let env = builder.build().await.expect("e2e env");

        let session = env.create_session().await.unwrap();
        env.fake
            .route_session(
                &prompt,
                [
                    tool_step(
                        "task",
                        json!({
                            "description": "uri probe child",
                            "prompt": "send DM and report",
                            "subagent_type": "hya-task",
                            "inline_agent": {
                                "prompt": format!("{marker} send a message to parent then report")
                            }
                        }),
                    ),
                    tool_step("wait", json!({"timeout_secs": 30})),
                    tool_step(
                        "read",
                        json!({"path": format!("channel://{child_handle}?last=5")}),
                    ),
                    text_step(format!("DONE_{label}")),
                    text_step(format!("QUIESCE_{label}")),
                ],
            )
            .unwrap();
        env.prompt(session, &prompt).await.unwrap();

        let dump = env.route_dump(&prompt).unwrap();
        assert!(
            dump.contains(&format!("HELLO_FROM_CHILD_{label}")),
            "{label}: `read channel://{child_handle}?last=5` must resolve via HandleRouter: {dump}"
        );

        // Verify `hya/agent-channels` read-only BundleApi endpoints on the live server.
        let channels_resp = env
            .get_json(&format!(
                "/v2/sessions/{session}/bundles/hya%2Fagent-channels/channels"
            ))
            .await
            .unwrap_or_else(|e| panic!("{label}: GET /channels failed: {e}"));
        let channels = channels_resp["channels"]
            .as_array()
            .expect("channels array");
        assert!(
            !channels.is_empty(),
            "{label}: expected at least group channel: {channels_resp}"
        );
        let group_id = channels[0]["id"].as_str().expect("channel id");

        let messages_resp = env
            .get_json(&format!(
                "/v2/sessions/{session}/bundles/hya%2Fagent-channels/channels/{group_id}/messages?last=10"
            ))
            .await
            .unwrap_or_else(|e| panic!("{label}: GET /channels/{group_id}/messages failed: {e}"));
        assert_eq!(
            messages_resp["channel"], group_id,
            "{label}: {messages_resp}"
        );

        let inbox_resp = env
            .get_json(&format!(
                "/v2/sessions/{session}/bundles/hya%2Fagent-channels/inbox"
            ))
            .await
            .unwrap_or_else(|e| panic!("{label}: GET /inbox failed: {e}"));
        assert_eq!(inbox_resp["handle"], "main", "{label}: {inbox_resp}");

        let status_resp = env
            .get_json(&format!(
                "/v2/sessions/{session}/bundles/hya%2Fagent-channels/team-status"
            ))
            .await
            .unwrap_or_else(|e| panic!("{label}: GET /team-status failed: {e}"));
        assert!(
            status_resp["members"].is_array(),
            "{label}: expected members array: {status_resp}"
        );
    }
}
