//! T2.13, T2.14 — user-authored Workflow fan-out/fan-in composition, stage model
//! routing (with 503 fallback and per-role reasoning effort), and durable replay
//! across backend close/reopen.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{
    E2eEnvBuilder, http_error_step, text_step, tool_step, tree_children, tree_session_ids,
    tree_subagent_types, wait_until,
};
use hya_proto::Event;
use serde_json::{Value, json};

const COMPOSE_WORKFLOW: &str = r#"---
kind: Workflow
name: compose
description: Explore fans out to two implementations; review joins both.
on_failure: collect_all
inputs:
  target: What to explore.
nodes:
  explore:
    agent: hya-task
    directive: EXPLORE {{input.target}}
  impl_a:
    agent: hya-task
    directive: IMPL A
  impl_b:
    agent: hya-task
    directive: IMPL B
  review:
    agent: hya-task
    directive: REVIEW both
---
flowchart TD
  explore --> impl_a & impl_b
  impl_a & impl_b --> review
"#;

const ROUTING_WORKFLOW: &str = r#"---
kind: Workflow
name: model-routing
description: Explicit worker and verifier model routes.
on_failure: fail_fast
inputs:
  target: Work to route.
nodes:
  prepare:
    agent: hya-task
    directive: PREPARE {{input.target}}
    model:
      id: fake/primary
      reasoning: high
      fallback:
        - id: fake/primary-fallback
          reasoning: medium
  loop:
    agent: hya-task
    directive: LOOP WORK
    mode: loop
    model:
      id: fake/loop
      reasoning: low
    verify:
      agent: hya-task
      until: LOOP_OK
      max_iterations: 1
      model:
        id: fake/loop
        reasoning: high
  finish:
    agent: hya-task
    directive: FINISH
    model:
      id: fake/final
      reasoning: medium
---
flowchart TD
  prepare --> loop
  loop --> finish
"#;

fn raw_state(value: &Value) -> Value {
    value.clone()
}

fn find_stage<'a>(raw: &'a Value, stage_id: &str) -> &'a Value {
    raw.pointer("/run/stages")
        .and_then(Value::as_array)
        .and_then(|stages| stages.iter().find(|s| s["id"] == stage_id))
        .unwrap_or_else(|| panic!("missing Workflow Stage {stage_id}: {raw}"))
}

fn assert_request(request: &Value, model: &str, reasoning: &str) {
    assert_eq!(request["model"], model, "request model: {request}");
    assert_eq!(
        request["reasoning_effort"], reasoning,
        "request reasoning: {request}"
    );
}

#[tokio::test]
async fn workflow_fan_out_fan_in_model_routing_and_replay() {
    let mut env = E2eEnvBuilder::new()
        .yolo(true)
        .additional_models(["primary", "primary-fallback", "loop", "final"])
        .project_file(
            ".hya/workflows/compose.hya.md",
            COMPOSE_WORKFLOW.as_bytes().to_vec(),
        )
        .project_file(
            ".hya/workflows/model-routing.hya.md",
            ROUTING_WORKFLOW.as_bytes().to_vec(),
        )
        .build()
        .await
        .expect("e2e env");

    // 1. T2.13: user-authored Workflow fan-out / fan-in via the `workflow` tool.
    env.fake
        .push_scripts([
            tool_step(
                "workflow",
                json!({
                    "action": "run",
                    "name": "compose",
                    "inputs": { "target": "the parser" }
                }),
            ),
            text_step("EXPLORED_OK"),
            text_step("IMPL_OK"),
            text_step("IMPL_OK"),
            text_step("REVIEWED_OK"),
            text_step("PARENT_AFTER_WORKFLOW"),
        ])
        .unwrap();
    let s_compose = env.create_session().await.expect("compose session");
    env.prompt(s_compose, "run my composed workflow")
        .await
        .expect("workflow prompt");

    let tree = env.session_tree(&s_compose).await.expect("session tree");
    assert_eq!(tree_children(&tree).len(), 4, "4 stage sessions: {tree}");
    assert_eq!(
        tree_session_ids(&tree)
            .iter()
            .filter(|id| *id != &s_compose.to_string())
            .count(),
        4
    );
    assert!(tree_subagent_types(&tree).iter().all(|k| k == "hya-task"));

    let compose_text: String = env
        .events(s_compose, None)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            Event::TextDelta { delta, .. } => Some(delta),
            Event::TextReplace { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(compose_text.contains("PARENT_AFTER_WORKFLOW"));

    let compose_reqs = env.fake_requests().unwrap();
    let compose_dump = serde_json::to_string(&compose_reqs).unwrap();
    for needle in [
        "EXPLORE the parser",
        "IMPL A",
        "IMPL B",
        "REVIEW both",
        "<stage id=\\\"impl_a\\\"",
        "<stage id=\\\"impl_b\\\"",
    ] {
        assert!(compose_dump.contains(needle), "missing {needle}");
    }

    // 2. T2.14: Workflow stage model routing, 503 fallback, per-role reasoning, durable replay.
    let before_routing = env.fake_requests().unwrap().len();
    env.fake
        .push_scripts([
            http_error_step(503),
            http_error_step(503),
            http_error_step(503),
            text_step("PREPARED_OK"),
            text_step("LOOP_WORK_OK"),
            text_step(r#"{"met":true,"reason":"LOOP_OK"}"#),
            text_step("FINISHED_OK"),
        ])
        .unwrap();

    let s_route = env.create_session().await.expect("routing session");
    let workflow_cmd = format!("/v2/sessions/{s_route}/bundles/hya%2Fworkflow/command");
    let workflow_path = format!("/v2/sessions/{s_route}/bundles/hya%2Fworkflow/state");

    let info = env
        .post_json(&workflow_cmd, &json!({"info": {"name": "model-routing"}}))
        .await
        .expect("workflow info");
    let info_stages = info
        .pointer("/info/stages")
        .and_then(Value::as_array)
        .unwrap();
    let info_prepare = info_stages.iter().find(|s| s["id"] == "prepare").unwrap();
    assert_eq!(info_prepare["worker_model"]["id"], "fake/primary");
    assert_eq!(info_prepare["worker_model"]["reasoning"], "high");
    assert_eq!(
        info_prepare["worker_model"]["fallback"][0]["id"],
        "fake/primary-fallback"
    );
    assert_eq!(
        info_prepare["worker_model"]["fallback"][0]["reasoning"],
        "medium"
    );
    let info_loop = info_stages.iter().find(|s| s["id"] == "loop").unwrap();
    assert_eq!(info_loop["worker_model"]["id"], "fake/loop");
    assert_eq!(info_loop["worker_model"]["reasoning"], "low");
    assert_eq!(info_loop["verifier_model"]["id"], "fake/loop");
    assert_eq!(info_loop["verifier_model"]["reasoning"], "high");
    assert!(
        info_stages
            .iter()
            .all(|e| !e.to_string().contains("#variant"))
    );

    let started = env
        .post_json(
            &workflow_cmd,
            &json!({"run": {"name": "model-routing", "inputs": {"target": "the parser"}}}),
        )
        .await
        .expect("workflow run");
    assert!(started.get("started").is_some());

    wait_until("Workflow completes", Duration::from_secs(30), || async {
        let state = env.get_json(&workflow_path).await?;
        Ok(raw_state(&state)
            .pointer("/run/status")
            .and_then(Value::as_str)
            == Some("completed"))
    })
    .await
    .expect("workflow completion");

    let state_before = env.get_json(&workflow_path).await.unwrap();
    let raw_before = raw_state(&state_before);
    let prepare = find_stage(&raw_before, "prepare");
    assert_eq!(prepare["selected_worker_model"]["id"], "fake/primary");
    assert_eq!(prepare["route_outcomes"][0]["candidate_index"], 1);
    assert_eq!(
        prepare["route_outcomes"][0]["model"],
        "fake/primary-fallback"
    );
    assert_eq!(prepare["route_outcomes"][0]["reasoning"], "medium");
    assert_eq!(prepare["route_outcomes"][0]["failure_class"], "server");

    let loop_stage = find_stage(&raw_before, "loop");
    let loop_outcomes = loop_stage["route_outcomes"].as_array().unwrap();
    assert_eq!(loop_outcomes.len(), 2);
    assert!(loop_outcomes.iter().any(|o| {
        o["role"] == "worker" && o["model"] == "fake/loop" && o["reasoning"] == "low"
    }));
    assert!(loop_outcomes.iter().any(|o| {
        o["role"] == "verifier" && o["model"] == "fake/loop" && o["reasoning"] == "high"
    }));

    let finish = find_stage(&raw_before, "finish");
    assert_eq!(finish["selected_worker_model"]["id"], "fake/final");
    assert_eq!(finish["route_outcomes"][0]["model"], "fake/final");

    let all_reqs = env.fake_requests().unwrap();
    let routing_reqs = &all_reqs[before_routing..];
    assert_eq!(routing_reqs.len(), 7, "3 retries + 4 stream groups");
    for req in &routing_reqs[..3] {
        assert_request(req, "primary", "high");
    }
    assert_request(&routing_reqs[3], "primary-fallback", "medium");
    assert_request(&routing_reqs[4], "loop", "low");
    assert_request(&routing_reqs[5], "loop", "high");
    assert_request(&routing_reqs[6], "final", "medium");

    // Durable replay survives reopen without any new provider request.
    let total_reqs = all_reqs.len();
    env.reopen().expect("reopen backend");
    let state_after = env.get_json(&workflow_path).await.unwrap();
    assert_eq!(raw_state(&state_after), raw_before);
    assert_eq!(env.fake_requests().unwrap().len(), total_reqs);
}
