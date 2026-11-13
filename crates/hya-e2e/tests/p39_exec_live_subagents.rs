//! T2.38 / T2.39 / T2.49 — a headless `hya exec` run does not end while the lead
//! still has live subagents. When the lead's turn ends on its own with a
//! worker still running, the run gives it another turn with a `HEADLESS RUN`
//! notice; it ends when a lead turn ends with no live subagent, or after two
//! notice turns in a row that neither `wait` nor `archive`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::process::Output;
use std::time::{Duration, Instant};

use hya_e2e::{E2eEnv, E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const WORKER: &str = "You are hya-task";
/// Every harness notice opens with this; the hya-main prompt names `HEADLESS RUN` bare.
const NOTICE: &str = "HEADLESS RUN —";
const SEED: u64 = 39;

fn worker_handle() -> String {
    let mut rng = hya_core::handle_naming::SplitMix64::seeded(SEED);
    let leaf = hya_core::handle_naming::mint_leaf("hya-task", &BTreeSet::new(), &mut rng);
    format!("main/{leaf}")
}

fn spawn_worker() -> ScriptStep {
    tool_step(
        "task",
        json!({
            "description": "worker",
            "prompt": "do the unit",
            "subagent_type": "hya-task"
        }),
    )
}

/// A worker that stays busy for `secs` in `bash`, then reports.
fn busy_worker(secs: u32) -> Vec<ScriptStep> {
    vec![
        tool_step("bash", json!({"command": format!("sleep {secs}")})),
        tool_step(
            "report",
            json!({"result": "WORKER_RESULT_OK", "outcome": "done"}),
        ),
    ]
}

/// Run `hya exec --json` direct (its own db, so it does not route to the
/// env's `serve`) with the env's config and FakeLlm.
async fn run_exec(env: &E2eEnv, prompt: &str) -> Output {
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let run = tokio::process::Command::new(&backend.binary)
        .args(["--yolo", "--model", "fake/model", "--db"])
        .arg(root.join("exec.db"))
        .args(["exec", "--json", prompt])
        .current_dir(&backend.project)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
        .env("XDG_DATA_HOME", &backend.xdg_data_home)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("HYA_HANDLE_SEED", SEED.to_string())
        .env_remove("HYA_MODEL")
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("exec finished within 60 s")
        .expect("spawn hya exec")
}

fn lead_requests(env: &E2eEnv) -> Vec<String> {
    env.fake
        .route_requests(ROOT)
        .unwrap()
        .expect("root route")
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// An idle member without a report must not give the resident supervisor a
/// second root driver between the exec lead's turns.
#[tokio::test]
async fn t2_38_exec_alone_continues_after_a_worker_goes_idle() {
    let env = E2eEnvBuilder::new()
        .route(WORKER, vec![text_step("WORKER_IDLE"), text_step("WORKER_EXTRA")])
        .route(ROOT, vec![
            spawn_worker(),
            tool_step("wait", json!({"timeout_secs": 60})),
            text_step("ROOT_STOP"),
            tool_step("archive", json!({"target": worker_handle(), "reason": "worker stopped without reporting"})),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ])
        .build().await.expect("e2e env");
    let output = run_exec(&env, "delegate, then finish").await;
    assert!(
        output.status.success(),
        "{}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    let requests = lead_requests(&env);
    assert!(
        requests[2].contains("stalled"),
        "the worker is idle before the lead ends its turn"
    );
    assert!(
        requests
            .iter()
            .all(|request| !request.contains("TEAM QUIESCED")),
        "resident synthesis must not drive exec: {requests:#?}"
    );
    assert_eq!(
        requests.len(),
        5,
        "no turn after the final answer; {}",
        env.diagnostics()
    );
    assert_eq!(
        requests[3].matches(NOTICE).count(),
        1,
        "one continuation notice"
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(1));
    let events: Vec<hya_proto::Envelope> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("exec envelope"))
        .collect();
    assert_eq!(
        events
            .iter()
            .filter(|envelope| matches!(
                envelope.event,
                hya_proto::Event::MessageStarted {
                    role: hya_proto::Role::User,
                    ..
                }
            ))
            .count(),
        1,
        "exactly one user prompt in the root transcript"
    );
    assert_eq!(
        events
            .iter()
            .filter(|envelope| matches!(
                envelope.event,
                hya_proto::Event::MessageStarted {
                    role: hya_proto::Role::Assistant,
                    ..
                }
            ))
            .count(),
        2,
        "only the initial and continuation turns; no turn starts after the final answer"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
}

#[tokio::test]
async fn t2_38_exec_lead_collects_a_live_worker_before_the_run_ends() {
    let env = E2eEnvBuilder::new()
        .route(WORKER, busy_worker(2))
        .route(
            ROOT,
            vec![
                spawn_worker(),
                // The trace: a turn that announces its next step and stops.
                text_step("Now let me look at the docs structure to add a page:"),
                tool_step("wait", json!({"timeout_secs": 60})),
                text_step("FINAL_ANSWER"),
                text_step("ROOT_EXTRA"),
            ],
        )
        .build()
        .await
        .expect("e2e env");
    let output = run_exec(&env, "delegate the unit").await;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    // The worker was not stopped: it ran its whole script and reported.
    assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(0));
    let requests = lead_requests(&env);
    assert_eq!(
        requests.len(),
        4,
        "spawn, the early stop, the notice turn's wait, and the final answer; {}",
        env.diagnostics()
    );
    assert!(!requests[1].contains(NOTICE));
    assert!(
        requests[2].contains(NOTICE),
        "notice missing: {}",
        requests[2]
    );
    assert!(
        requests[3].contains("WORKER_RESULT_OK"),
        "the report reached the lead"
    );
    // The final answer is the run's last lead turn: no notice follows it.
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    assert!(stdout.contains("FINAL_ANSWER"));
}

#[tokio::test]
async fn t2_39_exec_ends_after_two_ignored_notices() {
    let env = E2eEnvBuilder::new()
        .route(WORKER, busy_worker(30))
        .route(
            ROOT,
            vec![
                spawn_worker(),
                text_step("ROOT_STOP_1"),
                text_step("ROOT_STOP_2"),
                text_step("ROOT_STOP_3"),
                text_step("ROOT_EXTRA"),
            ],
        )
        .build()
        .await
        .expect("e2e env");
    let started = Instant::now();
    let output = run_exec(&env, "delegate the unit").await;
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    // Ended by the bound, not by the worker finishing.
    assert!(started.elapsed() < Duration::from_secs(25));
    assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(1));
    let requests = lead_requests(&env);
    assert_eq!(
        requests.len(),
        4,
        "spawn, the early stop, then two ignored notices; {}",
        env.diagnostics()
    );
    assert!(requests[2].contains(NOTICE) && requests[3].contains(NOTICE));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// Ending a resident episode with text leaves it idle, with no report to
/// collect. The lead must explicitly archive it before completing the run.
#[tokio::test]
async fn t2_49_exec_wait_reports_an_idle_worker_as_stalled() {
    let env = E2eEnvBuilder::new()
        .route(WORKER, vec![text_step("WORKER_IDLE"), text_step("WORKER_EXTRA")])
        .route(ROOT, vec![
            spawn_worker(),
            tool_step("wait", json!({"timeout_secs": 60})),
            tool_step("archive", json!({"target": worker_handle(), "reason": "worker stopped without reporting"})),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ])
        .build().await.expect("e2e env");
    let output = run_exec(&env, "delegate, collect, and archive if stalled").await;
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    let requests = lead_requests(&env);
    assert_eq!(requests.len(), 4, "{}", env.diagnostics());
    assert!(requests[2].contains("stopped without reporting"));
    assert!(requests[2].contains("stalled"));
    assert!(requests[3].contains(&format!("Archived `{}`", worker_handle())));
    assert_eq!(env.fake.route_remaining(WORKER).unwrap(), Some(1));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
}

/// Server-driven resident wakes remain distinct from exec's system notices:
/// an idle team wakes its lead with an admitted User synthesis prompt.
#[tokio::test]
async fn t2_49_idle_team_wakes_lead_with_user_synthesis_prompt() {
    let env = E2eEnvBuilder::new()
        .route(
            WORKER,
            vec![
                tool_step("bash", json!({"command": "sleep 1"})),
                text_step("WORKER_IDLE"),
                text_step("WORKER_EXTRA"),
            ],
        )
        .route(
            ROOT,
            vec![
                spawn_worker(),
                text_step("ROOT_STOP"),
                text_step("SYNTHESIZED_ANSWER"),
                text_step("ROOT_EXTRA"),
            ],
        )
        .build()
        .await
        .expect("e2e env");
    let session = env.create_session().await.expect("root session");
    env.prompt(session, "delegate the unit")
        .await
        .expect("prompt");
    env.wait_route_contains(ROOT, "TEAM QUIESCED", Duration::from_secs(30))
        .await
        .expect("team synthesis wake");
    let requests = env.fake.route_requests(ROOT).unwrap().expect("root route");
    assert!(
        requests.iter().any(|request| request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "user"
                && message["content"].to_string().contains("TEAM QUIESCED"))),
        "synthesis must be an admitted user prompt: {}",
        env.diagnostics()
    );
}
