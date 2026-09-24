//! T2.31 — `wait` and `archive` through a real backend: the lead's `wait`
//! is woken inside its own turn by a worker's report (delivered once: a
//! repeated wait lists it as already finished and returns at once, and the
//! report mail is not steered again); a worker that ends its turn without a
//! report wakes the wait as `stalled`, never as finished; `archive` stops it
//! and mail to its handle wakes it again.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const WORKER: &str = "You are a resident worker spawned by a parent agent";

fn spawn_worker() -> hya_e2e::ScriptStep {
    tool_step(
        "task",
        json!({
            "description": "worker",
            "prompt": "do the unit",
            "subagent_type": "hya-worker"
        }),
    )
}

#[tokio::test]
async fn t2_31_wait_returns_when_the_worker_reports_inside_the_lead_turn() {
    let env = E2eEnvBuilder::new()
        .route(
            WORKER,
            vec![
                tool_step(
                    "report",
                    json!({"result": "WAITED_REPORT_OK", "outcome": "done"}),
                ),
                text_step("WORKER_FOLLOWUP"),
            ],
        )
        .route(
            ROOT,
            vec![
                spawn_worker(),
                tool_step("wait", json!({"timeout_secs": 60})),
                tool_step(
                    "wait",
                    json!({"targets": ["main/hya-worker-1"], "timeout_secs": 60}),
                ),
                text_step("ROOT_AFTER_WAIT"),
                text_step("ROOT_EXTRA_1"),
                text_step("ROOT_EXTRA_2"),
            ],
        )
        .build()
        .await
        .expect("e2e env");
    let session = env.create_session().await.expect("root session");
    env.prompt(session, "delegate and wait")
        .await
        .expect("prompt");
    let timeout = Duration::from_secs(30);
    env.wait_route_contains(ROOT, "Subagents finished.", timeout)
        .await
        .unwrap_or_else(|error| panic!("wait result missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(ROOT, "WAITED_REPORT_OK", timeout)
        .await
        .unwrap_or_else(|error| panic!("report missing: {error}; {}", env.diagnostics()));
    // The repeated wait returns at once (well inside its 60 s timeout).
    env.wait_route_contains(ROOT, "every target already finished", timeout)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "repeated wait did not return: {error}; {}",
                env.diagnostics()
            )
        });
    env.wait_route_contains(ROOT, "ROOT_AFTER_WAIT", timeout)
        .await
        .unwrap_or_else(|error| panic!("lead did not continue: {error}; {}", env.diagnostics()));
    let dump = env.route_dump(ROOT).expect("root route");
    assert!(
        !dump.contains("[NEW MAIL · answer"),
        "the report delivered by wait must not be steered again: {dump}"
    );
}

#[tokio::test]
async fn t2_31_archive_stops_a_worker_and_mail_wakes_it() {
    let env = E2eEnvBuilder::new()
        .route(
            WORKER,
            vec![
                text_step("WORKER_FIRST_EPISODE"),
                text_step("WORKER_WOKEN_AGAIN"),
                text_step("WORKER_EXTRA"),
            ],
        )
        .route(
            ROOT,
            vec![
                spawn_worker(),
                tool_step("wait", json!({"timeout_secs": 60})),
                tool_step(
                    "archive",
                    json!({"target": "main/hya-worker-1", "reason": "pause"}),
                ),
                tool_step(
                    "send",
                    json!({"channel": "main/hya-worker-1", "body": "WAKE_UP_PLEASE"}),
                ),
                text_step("ROOT_DONE"),
                text_step("ROOT_EXTRA_1"),
                text_step("ROOT_EXTRA_2"),
            ],
        )
        .build()
        .await
        .expect("e2e env");
    let session = env.create_session().await.expect("root session");
    env.prompt(session, "delegate, archive, wake")
        .await
        .expect("prompt");
    let timeout = Duration::from_secs(30);
    env.wait_route_contains(ROOT, "stopped without reporting", timeout)
        .await
        .unwrap_or_else(|error| panic!("stall wake missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(ROOT, "Archived `main/hya-worker-1`", timeout)
        .await
        .unwrap_or_else(|error| panic!("archive result missing: {error}; {}", env.diagnostics()));
    env.wait_route_contains(WORKER, "WAKE_UP_PLEASE", timeout)
        .await
        .unwrap_or_else(|error| {
            panic!("archived worker not woken: {error}; {}", env.diagnostics())
        });
}
