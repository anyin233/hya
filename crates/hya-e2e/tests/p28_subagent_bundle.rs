//! T2.23 — the shipped subagent bundle executes through a real backend.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const TRANSIENT: &str = "Complete the bounded task supplied by the parent agent";
const RESIDENT: &str = "Remain available for related follow-up work from the parent agent";

#[tokio::test]
async fn t2_23_default_subagent_bundle_runs_transient_report_and_resident_mail() {
    let env = E2eEnvBuilder::new()
        .route(
            TRANSIENT,
            vec![
                tool_step(
                    "report",
                    json!({"result": "TRANSIENT_REPORT_OK", "outcome": "done"}),
                ),
                text_step("TRANSIENT_MODEL_FOLLOWUP_OK"),
            ],
        )
        .route(
            RESIDENT,
            vec![
                tool_step("send", json!({"body": "RESIDENT_MAIL_OK"})),
                text_step("RESIDENT_MODEL_FOLLOWUP_OK"),
            ],
        )
        .route(
            ROOT,
            vec![
                tool_step(
                    "task",
                    json!({"members": [
                        {
                            "description": "one-shot installed worker",
                            "prompt": "report the transient result",
                            "subagent_type": "hya-transient-worker"
                        },
                        {
                            "description": "resident installed worker",
                            "prompt": "mail the parent and remain available",
                            "subagent_type": "hya-resident-worker"
                        }
                    ]}),
                ),
                text_step("ROOT_SPAWNED_BUNDLE_WORKERS"),
                text_step("ROOT_RECEIVED_FIRST_RESULT"),
                text_step("ROOT_RECEIVED_SECOND_RESULT"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("root session");
    env.prompt(session, "run both installed worker lifecycles")
        .await
        .expect("spawn workers");

    let timeout = Duration::from_secs(20);
    env.wait_route_contains(ROOT, "TRANSIENT_REPORT_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("transient report missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(ROOT, "RESIDENT_MAIL_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("resident mail missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(TRANSIENT, "Report accepted", timeout)
        .await
        .unwrap_or_else(|error| panic!("transient follow-up missing: {error}"));
    for marker in [TRANSIENT, RESIDENT] {
        let requests = env
            .fake
            .route_requests(marker)
            .expect("route requests")
            .unwrap_or_default();
        assert!(requests.len() >= 2, "missing model follow-up for {marker}");
        assert!(
            requests.iter().all(|request| request["model"] == "model"),
            "bundle worker did not inherit the runtime model: {requests:?}"
        );
    }
}
