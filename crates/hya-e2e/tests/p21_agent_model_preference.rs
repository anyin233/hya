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
        .route(
            "You are hya",
            vec![
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
                            "prompt": "MARKER_PREFERRED_CHILD report PREFERRED_CHILD_OK",
                            "resident": false
                        }
                    }),
                ),
                text_step("PARENT_DONE"),
            ],
        )
        .route(
            "MARKER_PREFERRED_CHILD",
            vec![text_step("PREFERRED_CHILD_OK")],
        )
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

    let child_requests = env
        .fake
        .route_requests("MARKER_PREFERRED_CHILD")
        .expect("route requests")
        .unwrap_or_default();
    assert!(
        !child_requests.is_empty(),
        "the preferred child episode must run: all={}",
        env.fake_requests().expect("requests").len()
    );
    // The child round must run on the preferred model.
    assert_eq!(
        child_requests[0]["model"],
        json!("pref-target"),
        "spawned general must run on the remembered preference: {child_requests:?}"
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
