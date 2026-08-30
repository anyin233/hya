//! JSON conformance between the dependency-light SDK mirrors and hya-proto.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use hya_proto as proto;
use hya_sdk::{
    WorkflowAvailability, WorkflowCommand, WorkflowProjection, WorkflowRouteFailureClass,
    WorkflowStagePlan, WorkflowStageRouteOutcome,
};
use serde_json::json;

#[test]
fn run_command_matches_proto_wire_shape() {
    let sdk = WorkflowCommand::Run {
        name: Some("demo".to_owned()),
        expected_revision: Some("ab".repeat(32)),
        inputs: BTreeMap::from([(String::from("topic"), String::from("engines"))]),
        run: None,
    };
    let shared = proto::WorkflowCommand::Run {
        name: Some("demo".to_owned()),
        expected_revision: Some(proto::WorkflowRevision::from_bytes([0xab; 32])),
        inputs: BTreeMap::from([(String::from("topic"), String::from("engines"))]),
        run: None,
    };
    assert_eq!(
        serde_json::to_value(sdk).expect("encode SDK command"),
        serde_json::to_value(shared).expect("encode proto command")
    );
}

#[test]
fn workflow_state_matches_proto_wire_shape() {
    let raw = json!({
        "selection": {
            "source": "bundle:demo",
            "name": "demo",
            "revision": "ab".repeat(32)
        },
        "run": {
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "workflow": {
                "source": "bundle:demo",
                "name": "demo",
                "revision": "ab".repeat(32)
            },
            "request_hash": "hash",
            "owner": "550e8400-e29b-41d4-a716-446655440001",
            "status": "running",
            "stages": [{
                "id": "prepare",
                "title": "Prepare",
                "agent": "build",
                "mode": "once",
                "level": 0,
                "status": "running",
                "members": [{"member": "550e8400-e29b-41d4-a716-446655440002", "role": "worker", "iteration": 0}]
            }]
        },
        "availability": "available"
    });
    let sdk: WorkflowProjection = serde_json::from_value(raw.clone()).expect("decode SDK state");
    assert_eq!(sdk.availability, Some(WorkflowAvailability::Available));
    let shared: proto::WorkflowProjection =
        serde_json::from_value(raw).expect("decode proto state");
    assert_eq!(
        serde_json::to_value(sdk).expect("encode SDK state"),
        serde_json::to_value(shared).expect("encode proto state")
    );
}

/// SDK mirrors preserve authored optional and admitted required route fields.
#[test]
fn workflow_stage_route_mirrors_match_proto_wire_shape() {
    let raw = json!({
        "id": "route",
        "title": "Route",
        "agent": "worker",
        "mode": "once",
        "level": 0,
        "worker_model": {
            "id": "primary",
            "fallback": [{"id": "fallback", "reasoning": "high"}]
        },
        "selected_worker_model": {"index": 1, "id": "fallback", "reasoning": "high"},
        "verifier_model": {"id": "verifier", "reasoning": "low", "fallback": []},
        "selected_verifier_model": {"index": 0, "id": "verifier", "reasoning": "none"}
    });
    let sdk: WorkflowStagePlan =
        serde_json::from_value(raw.clone()).expect("decode SDK route plan");
    assert_eq!(
        sdk.worker_model.as_ref().map(|route| route.id.as_str()),
        Some("primary")
    );
    assert_eq!(
        sdk.selected_worker_model
            .as_ref()
            .map(|candidate| (candidate.index, candidate.reasoning.as_str())),
        Some((1, "high"))
    );
    assert_eq!(
        sdk.selected_verifier_model
            .as_ref()
            .map(|candidate| candidate.reasoning.as_str()),
        Some("none")
    );
    let shared: proto::WorkflowStagePlan =
        serde_json::from_value(raw).expect("decode proto route plan");
    assert_eq!(
        serde_json::to_value(sdk).expect("encode SDK route plan"),
        serde_json::to_value(shared).expect("encode proto route plan")
    );
}

/// SDK route outcomes preserve the proto's role, index, model, effort, and
/// bounded failure-class wire values.
#[test]
fn workflow_route_outcome_mirror_matches_proto_wire_shape() {
    let raw = json!({
        "session": "550e8400-e29b-41d4-a716-446655440000",
        "run": "550e8400-e29b-41d4-a716-446655440001",
        "stage": "route",
        "member": "550e8400-e29b-41d4-a716-446655440002",
        "role": "verifier",
        "iteration": 2,
        "step": 3,
        "candidate_index": 1,
        "model": "provider/model",
        "reasoning": "none",
        "failure_class": "none"
    });
    let sdk: WorkflowStageRouteOutcome =
        serde_json::from_value(raw.clone()).expect("decode SDK route outcome");
    assert_eq!(sdk.reasoning, "none");
    assert_eq!(sdk.failure_class, WorkflowRouteFailureClass::None);
    let shared: proto::WorkflowStageRouteOutcome =
        serde_json::from_value(raw).expect("decode proto route outcome");
    assert_eq!(
        serde_json::to_value(sdk).expect("encode SDK route outcome"),
        serde_json::to_value(shared).expect("encode proto route outcome")
    );
}
