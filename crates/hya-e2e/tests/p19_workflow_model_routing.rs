//! P19 — Workflow stage model routing through the production HTTP seam.
//!
//! A user-authored Workflow exercises an explicit worker fallback, a loop worker
//! and verifier sharing one base model at different efforts, and a later worker
//! with another model. The durable state is read before and after a production
//! backend close/reopen while every provider request comes from local FakeLlm.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, http_error_step, text_step, wait_until};
use serde_json::{Value, json};

const WORKFLOW_SOURCE: &str = r#"---
kind: Workflow
name: model-routing
description: Explicit worker and verifier model routes.
on_failure: fail_fast
inputs:
  target: Work to route.
nodes:
  prepare:
    agent: general
    directive: PREPARE {{input.target}}
    model:
      id: fake/primary
      reasoning: high
      fallback:
        - id: fake/primary-fallback
          reasoning: medium
  loop:
    agent: general
    directive: LOOP WORK
    mode: loop
    model:
      id: fake/loop
      reasoning: low
    verify:
      agent: general
      until: LOOP_OK
      max_iterations: 1
      model:
        id: fake/loop
        reasoning: high
  finish:
    agent: general
    directive: FINISH
    model:
      id: fake/final
      reasoning: medium
---
flowchart TD
  prepare --> loop
  loop --> finish
"#;

/// Return one named Stage object from a Workflow response.
fn stage<'a>(value: &'a Value, stage_id: &str) -> &'a Value {
    value
        .pointer("/state/run/stages")
        .and_then(Value::as_array)
        .and_then(|stages| stages.iter().find(|stage| stage["id"] == stage_id))
        .unwrap_or_else(|| panic!("missing Workflow Stage {stage_id}: {value}"))
}

/// Return the route outcome rows attached to one named Stage.
fn outcomes<'a>(value: &'a Value, stage_id: &str) -> &'a [Value] {
    stage(value, stage_id)["route_outcomes"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| panic!("Stage {stage_id} has no route outcomes: {value}"))
}

/// Assert one request's wire model and separately encoded reasoning effort.
fn assert_request(request: &Value, model: &str, reasoning: &str) {
    assert_eq!(request["model"], model, "request model: {request}");
    assert_eq!(
        request["reasoning_effort"], reasoning,
        "request reasoning: {request}"
    );
}

/// Prove model routing, bounded outcomes, and durable replay through HTTP.
#[tokio::test]
async fn p19_workflow_model_routes_requests_outcomes_and_replay() {
    let mut env = E2eEnvBuilder::new()
        .additional_models(["primary", "primary-fallback", "loop", "final"])
        .project_file(
            ".hya/workflows/model-routing.hya.md",
            WORKFLOW_SOURCE.as_bytes().to_vec(),
        )
        .scripts(vec![
            // HttpProvider retries each 503 three times before Workflow route
            // selection advances to the declared fallback candidate.
            http_error_step(503),
            http_error_step(503),
            http_error_step(503),
            text_step("PREPARED_OK"),
            text_step("LOOP_WORK_OK"),
            text_step(r#"{"met":true,"reason":"LOOP_OK"}"#),
            text_step("FINISHED_OK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let workflow_path = format!("/sessions/{session}/workflow");

    let info = env
        .post_json(
            &workflow_path,
            &json!({"command":"info","name":"model-routing"}),
        )
        .await
        .expect("Workflow info");
    let info_stages = info
        .pointer("/workflow/stages")
        .and_then(Value::as_array)
        .expect("Workflow info stages");
    let info_prepare = info_stages
        .iter()
        .find(|entry| entry["id"] == "prepare")
        .expect("prepare info");
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
    let info_loop = info_stages
        .iter()
        .find(|entry| entry["id"] == "loop")
        .expect("loop info");
    assert_eq!(info_loop["worker_model"]["id"], "fake/loop");
    assert_eq!(info_loop["worker_model"]["reasoning"], "low");
    assert_eq!(info_loop["verifier_model"]["id"], "fake/loop");
    assert_eq!(info_loop["verifier_model"]["reasoning"], "high");
    assert!(
        info_stages
            .iter()
            .all(|entry| !entry.to_string().contains("#variant")),
        "Workflow assignment ids must be base-only: {info}"
    );

    let started = env
        .post_json(
            &workflow_path,
            &json!({
                "command": "run",
                "name": "model-routing",
                "inputs": {"target": "the parser"}
            }),
        )
        .await
        .expect("Workflow run admission");
    assert_eq!(
        started["kind"], "run",
        "public Workflow run response: {started}"
    );
    assert_eq!(started["result"]["replayed"], false);

    wait_until(
        "Workflow run completes",
        Duration::from_secs(30),
        || async {
            let state = env.get_json(&workflow_path).await?;
            Ok(state.pointer("/state/run/status").and_then(Value::as_str) == Some("completed"))
        },
    )
    .await
    .expect("Workflow completion");
    let state_before_reopen = env.get_json(&workflow_path).await.expect("Workflow state");
    assert_eq!(
        state_before_reopen.pointer("/state/run/status"),
        Some(&Value::String("completed".into()))
    );

    let prepare_stage = stage(&state_before_reopen, "prepare");
    assert_eq!(prepare_stage["selected_worker_model"]["index"], 0);
    assert_eq!(prepare_stage["selected_worker_model"]["id"], "fake/primary");
    assert_eq!(prepare_stage["selected_worker_model"]["reasoning"], "high");

    let prepare_outcomes = outcomes(&state_before_reopen, "prepare");
    assert_eq!(prepare_outcomes.len(), 1, "one fallback stream group");

    assert_eq!(prepare_outcomes[0]["role"], "worker");
    assert_eq!(prepare_outcomes[0]["iteration"], 0);
    assert_eq!(prepare_outcomes[0]["step"], 0);
    assert_eq!(prepare_outcomes[0]["candidate_index"], 1);
    assert_eq!(prepare_outcomes[0]["model"], "fake/primary-fallback");
    assert_eq!(prepare_outcomes[0]["reasoning"], "medium");
    assert_eq!(prepare_outcomes[0]["failure_class"], "server");

    let loop_stage = stage(&state_before_reopen, "loop");
    assert_eq!(loop_stage["selected_worker_model"]["index"], 0);
    assert_eq!(loop_stage["selected_worker_model"]["id"], "fake/loop");
    assert_eq!(loop_stage["selected_worker_model"]["reasoning"], "low");
    assert_eq!(loop_stage["selected_verifier_model"]["index"], 0);
    assert_eq!(loop_stage["selected_verifier_model"]["id"], "fake/loop");
    assert_eq!(loop_stage["selected_verifier_model"]["reasoning"], "high");
    let loop_outcomes = outcomes(&state_before_reopen, "loop");
    assert_eq!(loop_outcomes.len(), 2, "worker and verifier stream groups");
    assert!(
        loop_outcomes
            .iter()
            .any(|outcome| outcome["role"] == "worker"
                && outcome["model"] == "fake/loop"
                && outcome["reasoning"] == "low"
                && outcome["failure_class"] == "none")
    );
    assert!(
        loop_outcomes
            .iter()
            .any(|outcome| outcome["role"] == "verifier"
                && outcome["model"] == "fake/loop"
                && outcome["reasoning"] == "high"
                && outcome["failure_class"] == "none")
    );

    let finish_stage = stage(&state_before_reopen, "finish");
    assert_eq!(finish_stage["selected_worker_model"]["id"], "fake/final");
    assert_eq!(finish_stage["selected_worker_model"]["reasoning"], "medium");
    let finish_outcomes = outcomes(&state_before_reopen, "finish");
    assert_eq!(finish_outcomes.len(), 1);
    assert_eq!(finish_outcomes[0]["role"], "worker");
    assert_eq!(finish_outcomes[0]["model"], "fake/final");
    assert_eq!(finish_outcomes[0]["reasoning"], "medium");
    assert_eq!(finish_outcomes[0]["failure_class"], "none");

    let requests = env.fake_requests().expect("FakeLlm requests");
    assert_eq!(requests.len(), 7, "three retries plus four stream groups");
    for request in &requests[..3] {
        assert_request(request, "primary", "high");
    }
    assert_request(&requests[3], "primary-fallback", "medium");
    assert_request(&requests[4], "loop", "low");
    assert_request(&requests[5], "loop", "high");
    assert_request(&requests[6], "final", "medium");

    let request_count = requests.len();
    env.reopen().expect("reopen production backend");
    let state_after_reopen = env
        .get_json(&workflow_path)
        .await
        .expect("replayed Workflow state");
    assert_eq!(state_after_reopen, state_before_reopen);
    assert_eq!(
        env.fake_requests()
            .expect("FakeLlm requests after replay")
            .len(),
        request_count,
        "state replay must not call the provider"
    );
}
