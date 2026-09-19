//! T2.17 — dispatch-time model resolution for subagent spawns: exact id
//! override, substring fallback (bare vendor ids excluded), and deferral to
//! the user's configured chain.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::json;

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
            "prompt": format!("MARKER_{marker} report {marker}"),
            "resident": false
        }
    });
    if !model.is_empty() {
        task["model"] = json!(model);
    }
    tool_step("task", task)
}

/// One isolated backend per branch: the quiesce steer can issue extra
/// parent rounds, which would otherwise consume the next branch's script
/// steps out of order on a shared route queue.
async fn child_model(marker: &str, model: &str, extra: &[&str]) -> String {
    let env = E2eEnvBuilder::new()
        .additional_models(extra.iter().copied())
        .route("You are hya", vec![spawn(marker, model), text_step("P")])
        .route(format!("MARKER_{marker}"), vec![text_step("C")])
        .build()
        .await
        .expect("e2e env");
    let session = env.create_session().await.expect("session");
    env.prompt(session, format!("run {marker}"))
        .await
        .expect("prompt");
    let marker_key = format!("MARKER_{marker}");
    for _ in 0..600 {
        let child = env
            .fake
            .route_requests(&marker_key)
            .expect("route requests")
            .unwrap_or_default();
        if let Some(first) = child.first() {
            return first["model"].as_str().expect("model").to_owned();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("child round for {marker} never matched its route");
}

#[tokio::test]
async fn t2_17_dispatch_model_resolution_branches() {
    const EXTRA: &[&str] = &["pref-target", "override-target"];

    // Branch 1: exact valid id dispatches directly.
    assert_eq!(
        child_model("BR1", "fake/pref-target", EXTRA).await,
        "pref-target",
        "exact valid id must override"
    );

    // Branch 2: invalid id substring-dispatches the first stable match
    // ("fake/override-target" sorts before "fake/pref-target").
    assert_eq!(
        child_model("BR2", "target", EXTRA).await,
        "override-target",
        "substring must dispatch the first catalog match"
    );

    // Branch 3: bare vendor id defers to the user's remembered preference.
    {
        let env = E2eEnvBuilder::new()
            .additional_models(EXTRA.iter().copied())
            .route("You are hya", vec![spawn("VENDOR", "fake"), text_step("P")])
            .route("MARKER_VENDOR", vec![text_step("C")])
            .build()
            .await
            .expect("e2e env");
        env.put_json(
            "/v1/agent-models/general",
            &json!({"preference": {"providerId": "fake", "modelId": "pref-target"}}),
        )
        .await
        .expect("set preference");
        let session = env.create_session().await.expect("session");
        env.prompt(session, "run VENDOR").await.expect("prompt");
        for _ in 0..600 {
            let child = env
                .fake
                .route_requests("MARKER_VENDOR")
                .expect("route requests")
                .unwrap_or_default();
            if let Some(first) = child.first() {
                assert_eq!(
                    first["model"],
                    json!("pref-target"),
                    "bare vendor id must defer to the user configuration"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    // Branch 4: no model requested -> user configuration chain.
    {
        let env = E2eEnvBuilder::new()
            .additional_models(EXTRA.iter().copied())
            .route("You are hya", vec![spawn("NONE", ""), text_step("P")])
            .route("MARKER_NONE", vec![text_step("C")])
            .build()
            .await
            .expect("e2e env");
        env.put_json(
            "/v1/agent-models/general",
            &json!({"preference": {"providerId": "fake", "modelId": "pref-target"}}),
        )
        .await
        .expect("set preference");
        let session = env.create_session().await.expect("session");
        env.prompt(session, "run NONE").await.expect("prompt");
        for _ in 0..600 {
            let child = env
                .fake
                .route_requests("MARKER_NONE")
                .expect("route requests")
                .unwrap_or_default();
            if let Some(first) = child.first() {
                assert_eq!(
                    first["model"],
                    json!("pref-target"),
                    "unspecified model must use the user configuration"
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}
