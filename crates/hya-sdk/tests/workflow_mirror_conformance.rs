//! JSON conformance between the dependency-light SDK mirrors and hya-proto.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use hya_proto as proto;
use hya_sdk::{WorkflowAvailability, WorkflowCommand, WorkflowProjection};
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
