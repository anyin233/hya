//! T2.50–T2.52 — `tools.code.mode` and the first-party `hya/code-mode` bundle.
//!
//! - T2.50 `only`: the model is offered `code-mode__run` alone, with the
//!   hidden tools described inside it; a program reads a project file through
//!   `tools.read` and its output reaches the follow-up request; a direct call
//!   of a hidden tool is refused.
//! - T2.51 `with_tools`: `code-mode__run` is offered next to the usual tools
//!   and a program can call them.
//! - T2.52 default: the bundle publishes nothing.
//!
//! Requires `bun` on `PATH`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnv, E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

const TOOL: &str = "code-mode__run";
const MARKER: &str = "CODE_MODE_FILE_MARKER";
const READ_PROGRAM: &str = "return await tools.read({ path: 'notes.txt' });";

/// Names of the tools the first model request offers, and the description of
/// `code-mode__run` when it is among them.
fn offered(env: &E2eEnv) -> (Vec<String>, Option<String>) {
    let requests = env.fake.requests().unwrap();
    let tools = requests[0]["tools"].as_array().cloned().unwrap_or_default();
    let names = tools
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect();
    let description = tools
        .iter()
        .find(|tool| tool["function"]["name"] == TOOL)
        .map(|tool| {
            tool["function"]["description"]
                .as_str()
                .unwrap()
                .to_string()
        });
    (names, description)
}

async fn run(mode: Option<&str>, steps: Vec<hya_e2e::ScriptStep>) -> E2eEnv {
    let mut builder = E2eEnvBuilder::new()
        .route("You are hya", steps)
        .project_file("notes.txt", format!("{MARKER}\n"));
    if let Some(mode) = mode {
        builder = builder.code_mode(mode);
    }
    let env = builder.build().await.expect("e2e env");
    let session = env.create_session().await.unwrap();
    let turn = env.prompt(session, "read the notes").await.unwrap();
    assert!(
        turn.error_message.is_empty(),
        "turn must finish cleanly: {}; {}",
        turn.error_message,
        env.diagnostics()
    );
    env
}

fn followup(env: &E2eEnv) -> String {
    fake_requests_from(&env.fake.requests().unwrap(), 1)
}

#[tokio::test]
async fn t2_50_only_offers_the_code_tool_and_runs_hidden_tools_from_programs() {
    let env = run(
        Some("only"),
        vec![
            tool_step(TOOL, json!({ "script": READ_PROGRAM })),
            tool_step("read", json!({ "path": "notes.txt" })),
            text_step("DONE"),
        ],
    )
    .await;

    let (names, description) = offered(&env);
    assert_eq!(names, [TOOL], "{}", env.diagnostics());
    let description = description.unwrap();
    assert!(description.contains("\n\n### read\n"), "{description}");
    assert!(description.contains("\n\n### bash\n"), "{description}");

    let followup = followup(&env);
    assert!(followup.contains("Script completed"), "{followup}");
    assert!(followup.contains(MARKER), "{followup}");
    assert!(followup.contains("#1 L1 tools.read"), "{followup}");
    assert!(
        followup.contains("`read` is not offered here: call it from a `code-mode__run` program"),
        "{followup}"
    );
}

#[tokio::test]
async fn t2_51_with_tools_offers_the_code_tool_next_to_the_others() {
    let env = run(
        Some("with_tools"),
        vec![
            tool_step(TOOL, json!({ "script": READ_PROGRAM })),
            tool_step("read", json!({ "path": "notes.txt" })),
            text_step("DONE"),
        ],
    )
    .await;

    let (names, description) = offered(&env);
    for name in [TOOL, "read", "bash", "edit"] {
        assert!(
            names.iter().any(|offered| offered == name),
            "{name}: {names:?}"
        );
    }
    assert!(!description.unwrap().contains("### read"));

    let followup = followup(&env);
    assert!(followup.contains("#1 L1 tools.read"), "{followup}");
    assert!(!followup.contains("is not offered here"), "{followup}");
    assert!(followup.matches(MARKER).count() >= 2, "{followup}");
}

#[tokio::test]
async fn t2_52_default_mode_publishes_no_code_tool() {
    let env = run(None, vec![text_step("DONE")]).await;
    let (names, description) = offered(&env);
    assert!(names.iter().any(|name| name == "read"), "{names:?}");
    assert_eq!(description, None::<String>, "{names:?}");
    let listed = env.backend.bundle_cli(&["bundle", "list"]).unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout);
    let row = listed
        .lines()
        .find(|line| line.contains("hya/code-mode"))
        .unwrap_or_else(|| panic!("hya/code-mode is listed: {listed}"));
    assert!(row.contains("disabled"), "{row}");
    let _: Value = json!(null);
}
