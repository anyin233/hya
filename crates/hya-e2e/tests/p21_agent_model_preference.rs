//! T2.16 — the durable per-Agent model preference set over `/v1/agent-models`
//! steers the spawned subagent's base model, and the listing reports the
//! remembered source.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t2_16_agent_model_preference_steers_spawned_subagent_model() {
    // Script order: root requests task → child (must run on the preferred
    // model) completes → root finishes.
    let env = E2eEnvBuilder::new()
        .additional_models(["pref-target"])
        .scripts(vec![
            tool_step(
                "task",
                json!({
                    "description": "e2e preferred child",
                    "prompt": "report PREFERRED_CHILD_OK",
                    "subagent_type": "general",
                    "inline_agent": {
                        "description": "",
                        "category": "",
                        "model": "",
                        "name": "",
                        "prompt": "",
                        "resident": false
                    }
                }),
            ),
            text_step("PREFERRED_CHILD_OK"),
            text_step("PARENT_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    // Before the preference: the listing shows the process default tier.
    let listed = env
        .get_json("/v1/agent-models")
        .await
        .expect("agent-model listing");
    let general_row = listed["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .find(|row| row["agentId"] == "general")
        .expect("general row");
    assert_eq!(
        general_row["source"],
        json!("AGENT_MODEL_SOURCE_DEFAULT"),
        "no preference set yet: {general_row}"
    );

    // Set the durable preference: general -> fake/pref-target (PUT route).
    env.put_json(
        "/v1/agent-models/general",
        &json!({"preference": {"providerId": "fake", "modelId": "pref-target"}}),
    )
    .await
    .expect("set preference");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "spawn a general subagent on the preferred model")
        .await
        .expect("task prompt");

    let requests = env.fake_requests().expect("fake requests");
    assert!(
        requests.len() >= 3,
        "root task round + child + root finish expected: {requests:?}"
    );
    // The child round (the second request) must run on the preferred model.
    assert_eq!(
        requests[1]["model"],
        json!("pref-target"),
        "spawned general must run on the remembered preference: {requests:?}"
    );

    // The listing now reports the remembered tier for general.
    let listed = env
        .get_json("/v1/agent-models")
        .await
        .expect("agent-model listing after set");
    let general_row = listed["agents"]
        .as_array()
        .expect("agents array")
        .iter()
        .find(|row| row["agentId"] == "general")
        .expect("general row after set");
    assert_eq!(
        general_row["source"],
        json!("AGENT_MODEL_SOURCE_REMEMBERED"),
        "preference is durable and catalog-matching: {general_row}"
    );
    assert_eq!(
        general_row["preference"]["modelId"],
        json!("pref-target"),
        "{general_row}"
    );
}
