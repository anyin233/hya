//! T2.23 — the shipped subagent bundle's resident worker executes through a
//! real backend: it mails its parent, reports, and is archived.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const WORKER: &str = "You are a resident worker spawned by a parent agent";

#[tokio::test]
async fn t2_23_default_subagent_bundle_worker_mails_reports_and_archives() {
    let env = E2eEnvBuilder::new()
        .route(
            WORKER,
            vec![
                tool_step("send", json!({"body": "WORKER_MAIL_OK"})),
                tool_step(
                    "report",
                    json!({"result": "WORKER_REPORT_OK", "outcome": "done"}),
                ),
                text_step("WORKER_MODEL_FOLLOWUP_OK"),
            ],
        )
        .route(
            ROOT,
            vec![
                tool_step(
                    "task",
                    json!({
                        "description": "installed worker",
                        "prompt": "mail the parent, then report",
                        "subagent_type": "hya-worker"
                    }),
                ),
                text_step("ROOT_SPAWNED_BUNDLE_WORKER"),
                text_step("ROOT_RECEIVED_FIRST_RESULT"),
                text_step("ROOT_RECEIVED_SECOND_RESULT"),
                text_step("ROOT_RECEIVED_THIRD_RESULT"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("root session");
    env.prompt(session, "run the installed worker")
        .await
        .expect("spawn worker");

    let timeout = Duration::from_secs(20);
    env.wait_route_contains(ROOT, "WORKER_MAIL_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("worker mail missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(ROOT, "WORKER_REPORT_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("worker report missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(WORKER, "Report accepted", timeout)
        .await
        .unwrap_or_else(|error| panic!("worker follow-up missing: {error}"));
    let requests = env
        .fake
        .route_requests(WORKER)
        .expect("route requests")
        .unwrap_or_default();
    assert!(
        requests.len() >= 2,
        "missing model follow-up for the worker"
    );
    assert!(
        requests.iter().all(|request| request["model"] == "model"),
        "bundle worker did not inherit the runtime model: {requests:?}"
    );
}
