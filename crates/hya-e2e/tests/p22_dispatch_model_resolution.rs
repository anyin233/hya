//! T2.17 — dispatch-time model resolution for subagent spawns: exact id
//! override, substring fallback (bare vendor ids excluded), and deferral to
//! the user's configured chain.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::{Value, json};

fn spawn(marker: &str, model: &str) -> ScriptStep {
    let mut task = json!({
        "description": "dispatch demo",
        "prompt": format!("report {marker}"),
        "subagent_type": "general",
        "inline_agent": {
            "description": "",
            "category": "",
            "model": "",
            "name": "",
            "prompt": "",
            "resident": false
        }
    });
    if !model.is_empty() {
        task["model"] = json!(model);
    }
    tool_step("task", task)
}

async fn child_model(env: &hya_e2e::E2eEnv, marker: &str) -> String {
    let session = env.create_session().await.expect("session");
    env.prompt(session, format!("run {marker}"))
        .await
        .expect("prompt");
    let requests = env.fake_requests().expect("requests");
    let child = requests
        .iter()
        .find(|request| {
            request["messages"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains(&format!("report {marker}")))
                })
            })
        })
        .unwrap_or_else(|| panic!("child round for {marker} not found"));
    child["model"].as_str().expect("model").to_owned()
}

#[tokio::test]
async fn t2_17_dispatch_model_resolution_branches() {
    let env = E2eEnvBuilder::new()
        .additional_models(["pref-target", "override-target"])
        .scripts(vec![
            spawn("BR1", "fake/pref-target"),
            text_step("C1"),
            text_step("P1"),
            spawn("BR2", "target"),
            text_step("C2"),
            text_step("P2"),
            spawn("VENDOR", "fake"),
            text_step("C3"),
            text_step("P3"),
            spawn("NONE", ""),
            text_step("C4"),
            text_step("P4"),
        ])
        .build()
        .await
        .expect("e2e env");

    // User configuration for the fallback tier: remembered preference.
    env.put_json(
        "/v1/agent-models/general",
        &json!({"preference": {"providerId": "fake", "modelId": "pref-target"}}),
    )
    .await
    .expect("set preference");

    // Branch 1: exact valid id dispatches directly.
    assert_eq!(
        child_model(&env, "BR1").await,
        "pref-target",
        "exact valid id must override"
    );

    // Branch 2: invalid id substring-dispatches the first stable match
    // ("fake/override-target" sorts before "fake/pref-target").
    assert_eq!(
        child_model(&env, "BR2").await,
        "override-target",
        "substring must dispatch the first catalog match"
    );

    // Branch 2 disabled for bare vendor ids: "fake" defers to the user's
    // remembered preference.
    assert_eq!(
        child_model(&env, "VENDOR").await,
        "pref-target",
        "bare vendor id must defer to the user configuration"
    );

    // Branch 3: no model requested -> user configuration chain.
    assert_eq!(
        child_model(&env, "NONE").await,
        "pref-target",
        "unspecified model must use the user configuration"
    );

    let _ = Value::Null;
}
