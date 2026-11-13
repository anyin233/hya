//! A headless `hya exec` run does not end while the lead's own work is
//! unfinished: when its turn ends on its own with pending or in-progress
//! todos, or with a last message announcing a step it did not take, the run
//! gives it another turn with a `HEADLESS RUN` notice. A plain summary ends
//! the run; a notice ignored twice in a row ends it too.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::Output;
use std::time::Duration;

use hya_e2e::{E2eEnv, E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
/// Every harness notice opens with this; the hya-main prompt names `HEADLESS RUN` bare.
const NOTICE: &str = "HEADLESS RUN —";

/// Run `hya exec --json [extra..] prompt` direct (its own db, so it does not
/// route to the env's `serve`) with the env's config and FakeLlm.
async fn run_exec(env: &E2eEnv, extra: &[&str], prompt: &str) -> Output {
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");
    let run = tokio::process::Command::new(&backend.binary)
        .args(["--yolo", "--model", "fake/model", "--db"])
        .arg(root.join("exec.db"))
        .args(["exec", "--json"])
        .args(extra)
        .arg(prompt)
        .current_dir(&backend.project)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
        .env("XDG_DATA_HOME", &backend.xdg_data_home)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env_remove("HYA_MODEL")
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(90), run)
        .await
        .expect("exec finished within 90 s")
        .expect("spawn hya exec")
}

async fn run_lead(script: Vec<ScriptStep>, extra: &[&str]) -> (E2eEnv, Output, Vec<String>) {
    let env = E2eEnvBuilder::new()
        .route(ROOT, script)
        .build()
        .await
        .expect("e2e env");
    let output = run_exec(&env, extra, "do the task").await;
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    let requests = env
        .fake
        .route_requests(ROOT)
        .unwrap()
        .expect("root route")
        .iter()
        .map(ToString::to_string)
        .collect();
    (env, output, requests)
}

fn bash(command: &str) -> ScriptStep {
    tool_step("bash", json!({ "command": command }))
}

fn todo_status(updates: &[(&str, &str)]) -> ScriptStep {
    let updates: Vec<_> = updates
        .iter()
        .map(|(id, status)| json!({ "id": id, "status": status }))
        .collect();
    tool_step("todo__update_status", json!({ "updates": updates }))
}

#[tokio::test]
async fn e1_unfinished_todos_continue_the_run() {
    let add = |content: &str| json!({ "op": "add", "content": content });
    let (env, output, requests) = run_lead(
        vec![
            tool_step(
                "todo__update_content",
                json!({ "operations": [add("one"), add("two"), add("three"), add("four")] }),
            ),
            todo_status(&[("1", "completed"), ("2", "in_progress")]),
            text_step("Item one is done."),
            bash("echo working"),
            todo_status(&[("2", "completed"), ("3", "completed"), ("4", "blocked")]),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 6, "{}", env.diagnostics());
    assert!(!requests[2].contains(NOTICE));
    assert!(
        requests[3].contains(NOTICE) && requests[3].contains("#3 [pending]"),
        "notice missing: {}",
        requests[3]
    );
    assert!(
        !requests[3].contains("#1 ["),
        "completed items are not listed"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e2_an_announced_step_continues_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("Now verify the file format:"),
            bash("echo checked"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(requests[1].contains(NOTICE) && requests[1].contains("Now verify the file format:"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e3_a_plain_summary_ends_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            bash("echo done > a.txt"),
            text_step("Done. Changed files:\n- a.txt"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 2, "{}", env.diagnostics());
    assert!(requests.iter().all(|request| !request.contains(NOTICE)));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e4_two_ignored_notices_end_the_run() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("Let me check:"),
            text_step("Let me check:"),
            text_step("Let me check:"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(
        requests.len(),
        3,
        "the stop, then two ignored notices; {}",
        env.diagnostics()
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// With a 40 s budget the run starts inside the final window (90 s): the
/// opening names the budget and the first tool result carries the FINAL
/// notice, once.
#[tokio::test]
async fn e5_a_budgeted_run_gets_its_deadline_notice_in_turn() {
    let (env, _, requests) = run_lead(
        vec![
            bash("sleep 1"),
            bash("echo again"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &["--time-budget", "40"],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(
        requests[0].contains("HEADLESS RUN — time budget: 40s"),
        "opening missing: {}",
        requests[0]
    );
    assert!(
        requests[1].contains("HEADLESS RUN — FINAL"),
        "in-turn notice missing: {}",
        requests[1]
    );
    assert_eq!(
        requests[2].matches("HEADLESS RUN — FINAL").count(),
        1,
        "told once per stage"
    );
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// Within the grace (30 s for a 30 s budget) no continuation turn starts.
#[tokio::test]
async fn e7_no_continuation_past_the_grace() {
    let (env, _, requests) = run_lead(
        vec![text_step("Now let me:"), text_step("ROOT_EXTRA")],
        &["--time-budget", "30"],
    )
    .await;
    assert_eq!(requests.len(), 1, "{}", env.diagnostics());
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

/// A budgeted run shortens a long `bash` timeout to the time left minus the
/// grace (40 s budget, 30 s grace: about 10 s), and says so.
#[tokio::test]
async fn e6_bash_timeouts_are_capped_to_the_time_left() {
    // `sleep 120` returning inside run_exec's 90 s limit is the cap at work.
    let (env, output, requests) = run_lead(
        vec![
            tool_step("bash", json!({ "command": "sleep 120", "timeout": 300 })),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &["--time-budget", "40"],
    )
    .await;
    assert_eq!(requests.len(), 2, "{}", env.diagnostics());
    assert!(
        requests[1].contains("Timeout shortened to"),
        "cap not explained: {}",
        requests[1]
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(r#""deadlineCapped":true"#), "{stdout}");
    assert!(stdout.contains(r#""timedOut":true"#), "{stdout}");
}

#[tokio::test]
async fn e9_dsml_leak_triggers_continuation_and_recovers() {
    let (env, output, requests) = run_lead(
        vec![
            text_step("<｜DSML｜ invoke>bash\n<parameter name=\"command\">echo recovered > recovered.txt</parameter>\n</｜DSML｜ invoke>"),
            bash("echo recovered > recovered.txt"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    ).await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(!requests[0].contains(NOTICE));
    assert!(requests[1].contains(NOTICE));
    assert!(requests[1].contains("no tool was executed"));
    assert!(
        requests[1]
            .contains("Re-issue the intended tool call now using the tool-calling interface")
    );
    assert_eq!(
        env.read_project_file("recovered.txt").unwrap(),
        "recovered\n"
    );
    let events: Vec<hya_proto::Envelope> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("canonical exec envelope"))
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
        "continuation must not admit another user prompt"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}

#[tokio::test]
async fn e8_spurious_summary_triggers_a_user_facing_completion() {
    let (env, _, requests) = run_lead(
        vec![
            text_step("<analysis>Work remains.</analysis><summary>Check the output.</summary>"),
            bash("echo checked"),
            text_step("FINAL_ANSWER"),
            text_step("ROOT_EXTRA"),
        ],
        &[],
    )
    .await;
    assert_eq!(requests.len(), 3, "{}", env.diagnostics());
    assert!(requests[1].contains("internal <summary> block"));
    assert!(requests[1].contains("deliver your normal final answer"));
    assert_eq!(env.fake.route_remaining(ROOT).unwrap(), Some(1));
}
