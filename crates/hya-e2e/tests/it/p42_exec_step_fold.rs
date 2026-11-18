//! T2.53 — a headless `hya exec` run is one prompt and one assistant message
//! holding every step. When the steps outgrow the threshold, old steps fold
//! into the compaction summary and the run goes on: later requests carry the
//! summary, the prompt and the newest steps, not the folded ones.
//!
//! Each step is a `bash` call with a large command. Inputs, unlike outputs,
//! cannot be spilled, so only a fold can bring the run back under the window.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, ScriptStep, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
/// Opening of the fixed compaction agent's system prompt.
const SUMMARIZER: &str = "You are an anchored context summarization assistant";
const STEPS: usize = 12;
const PROMPT: &str = "do the long task";

/// A step whose command is ~60k chars (~11k tokens) opening with `STEP_<n>_BEGIN`.
fn long_step(step: usize) -> ScriptStep {
    let filler = "lorem ipsum dolor sit amet ".repeat(2_200);
    tool_step(
        "bash",
        json!({ "command": format!("true # STEP_{step}_BEGIN {filler}") }),
    )
}

#[tokio::test]
async fn t2_53_headless_single_turn_folds_old_steps_instead_of_overflowing() {
    let mut script: Vec<_> = (0..STEPS).map(long_step).collect();
    script.push(text_step("FINAL_ANSWER"));
    let env = E2eEnvBuilder::new()
        .route(ROOT, script)
        .route(
            SUMMARIZER,
            (1..=STEPS)
                .map(|n| text_step(format!("E2E_STEP_SUMMARY_{n}")))
                .collect(),
        )
        .build()
        .await
        .expect("e2e env");
    let backend = &env.backend;
    let root = backend.project.parent().expect("e2e root");

    // A 120k window (threshold 90k): twelve ~11k-token steps do not fit.
    let config_path = backend.xdg_config_home.join("hya").join("config.yaml");
    let config = std::fs::read_to_string(&config_path).unwrap();
    let config = config.replacen(
        "    - id: model\n",
        "    - id: model\n      limit:\n        context: 120000\n",
        1,
    );
    std::fs::write(&config_path, config).unwrap();

    let run = tokio::process::Command::new(&backend.binary)
        .args(["--yolo", "--model", "fake/model", "--db"])
        .arg(root.join("exec.db"))
        .args(["exec", "--json", PROMPT])
        .current_dir(&backend.project)
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", &backend.xdg_config_home)
        .env("XDG_DATA_HOME", &backend.xdg_data_home)
        .env("XDG_STATE_HOME", root.join("state"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("HYA_COMPACTION_KEEP_RECENT", "1")
        .env("HYA_COMPACTION_RESERVE_TOKENS", "0")
        .env_remove("HYA_MODEL")
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(120), run)
        .await
        .expect("exec finished within 120 s")
        .expect("spawn hya exec");
    assert!(
        output.status.success(),
        "exec failed: {}; {}",
        String::from_utf8_lossy(&output.stderr),
        env.diagnostics()
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FINAL_ANSWER"));

    let requests: Vec<String> = env
        .fake
        .route_requests(ROOT)
        .unwrap()
        .expect("root route")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(requests.len(), STEPS + 1, "{}", env.diagnostics());
    let summaries: Vec<String> = env
        .fake
        .route_requests(SUMMARIZER)
        .unwrap()
        .expect("summarizer route")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert!(!summaries.is_empty(), "old steps were folded");
    assert!(
        summaries[0].contains("STEP_0_BEGIN") && summaries[0].contains(PROMPT),
        "the summarizer sees the folded steps and the prompt"
    );

    // The final request: the latest summary, the prompt verbatim, the newest
    // step, and none of the steps folded first.
    let last = requests.last().unwrap();
    let latest = format!("E2E_STEP_SUMMARY_{}", summaries.len());
    assert!(last.contains(&latest), "summary {latest} is sent");
    assert!(last.contains(PROMPT), "the prompt stays verbatim");
    assert!(last.contains(&format!("STEP_{}_BEGIN", STEPS - 1)));
    assert!(
        !last.contains("STEP_0_BEGIN"),
        "the first step left the request"
    );
}
