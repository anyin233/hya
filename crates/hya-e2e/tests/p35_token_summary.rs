//! T2.30 — the packaged `hya-extra/token-summary` Plugin bundle installs, its
//! Bun process serves its session-scoped `usage` API endpoint
//! (`GET /v1/sessions/{session}/bundles/hya-extra%2Ftoken-summary/usage`;
//! per-model input/cache/output
//! token usage, thinking/visible split) over the request-scoped
//! `session.usage` capability for a session tree (root + a spawned
//! subagent), and its `token_summary` agent tool renders the same data as a
//! Markdown table that reaches the model on the follow-up request. Requires
//! `bun` on `PATH`.
//!
//! The scripted turn spawns a subagent via `task` (round 1) and then calls
//! the bundle's own `token-summary__token_summary` tool (round 2, always a
//! tool call so it is never subject to the engine's team-quiescence
//! "premature final answer" retry) before finishing with plain text. Because
//! a spawned `task` member makes the root's *own* team-completion decision
//! timing-dependent (a text-only reply given before the subagent reports is
//! accepted but does not end the turn — the engine nudges once more), the
//! script pads the tail with a second identical final-text step and every
//! usage assertion below reads the actual recorded round count back from the
//! response instead of assuming a fixed one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_bundle::{BundleSource, write_public_package};
use hya_e2e::{E2eEnv, E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::{Value, json};

/// Prompt/completion/reasoning tokens FakeLlm reports on every response from
/// the moment `set_usage` is called; FakeLlm's OpenAI-shaped usage always
/// reports `completion_tokens_details.reasoning_tokens`, so every round's
/// thinking split is known (the unknown-split path is covered by `bun test`).
const PROMPT: u64 = 100;
const COMPLETION: u64 = 20;
const REASONING: u64 = 5;

const BUNDLE_SEGMENT: &str = "hya-extra%2Ftoken-summary";

/// Absolute path to the in-tree `bundles/extra/token-summary` source directory.
fn token_summary_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../bundles/extra/token-summary")
        .canonicalize()
        .expect("token-summary bundle source directory exists")
}

/// Package the real bundle and install it into `env`'s isolated data home.
fn install_token_summary_bundle(env: &E2eEnv, root: &Path) {
    let source = BundleSource::read_directory(token_summary_dir()).expect("read source");
    let package = root.join("token-summary.hyabundle");
    std::fs::write(&package, write_public_package(&source).expect("package")).unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_file(&package).unwrap();
}

/// GET a backend path, returning the status and JSON body.
async fn get(env: &E2eEnv, path: &str) -> (u16, Value) {
    let response = env
        .http
        .get(format!("{}{path}", env.backend.url))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

fn number(value: &Value) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("not a u64: {value}"))
}

#[tokio::test]
async fn t2_30_token_summary_api_and_tool_report_session_tree_usage() {
    let root = std::env::temp_dir().join(format!("hya-token-summary-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();

    let env = E2eEnvBuilder::new()
        .route(
            "You are hya",
            vec![
                // Round 1: spawn a subagent.
                tool_step(
                    "task",
                    json!({
                        "description": "token summary child",
                        "prompt": "do the child work",
                        "subagent_type": "general",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_TOKEN_SUMMARY_CHILD do the child work",
                            "resident": false
                        }
                    }),
                ),
                // Round 2: call the bundle's own tool. A tool call is never
                // treated as a (possibly premature) final answer by the
                // engine's team-quiescence check, so this round always runs
                // right after round 1 regardless of the subagent's timing.
                tool_step(
                    "token-summary__token_summary",
                    json!({ "scope": "tree", "format": "table" }),
                ),
                // Round 3: attempt to finish. If the subagent has not yet
                // reported, the engine accepts this into history but keeps
                // the turn open and asks once more (round 4) after it does.
                text_step("PARENT_FINAL"),
                text_step("PARENT_FINAL"),
            ],
        )
        .route(
            "MARKER_TOKEN_SUMMARY_CHILD",
            vec![text_step("CHILD_TOKEN_SUMMARY_OK")],
        )
        .build()
        .await
        .expect("e2e env");
    env.fake.set_usage(PROMPT, COMPLETION, REASONING).unwrap();

    install_token_summary_bundle(&env, &root);

    let session = env.create_session().await.unwrap();
    let turn = env
        .prompt(session, "spawn a child, summarize usage, then stop")
        .await
        .unwrap();
    assert!(
        turn.error_message.is_empty(),
        "turn must finish cleanly: {}; {}",
        turn.error_message,
        env.diagnostics()
    );
    // --- (a) the `usage` session endpoint reports the whole tree -------
    let usage_path = format!("/v1/sessions/{session}/bundles/{BUNDLE_SEGMENT}/usage");
    let mut tree_body = Value::Null;
    for _ in 0..100 {
        let (status, body) = get(&env, &usage_path).await;
        assert_eq!(status, 200, "{body}; {}", env.diagnostics());
        tree_body = body;
        // Wait for both sessions to show up *and* for each to have folded at
        // least one `fake/model` round: a session can appear in the tree (via
        // the spawn edge) slightly before its own first round is folded.
        let rows = tree_body["sessions"].as_array();
        let settled = rows.is_some_and(|rows| {
            rows.len() == 2
                && rows.iter().all(|row| {
                    row["models"][0]["rounds"]
                        .as_u64()
                        .is_some_and(|rounds| rounds >= 1)
                })
        });
        if settled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The HTTP body is the bundle's own JSON (no envelope).
    let report = &tree_body;
    assert_eq!(report["session"], session.to_string());
    assert_eq!(report["scope"], "tree", "{report}");
    assert_eq!(report["generated_by"], "hya-extra/token-summary");
    let sessions = report["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2, "root and its subagent: {report}");

    // Exactly one model was ever used, so the tree total equals it. The
    // exact round count is timing-dependent (see module docs), so every
    // check below is relative to the recorded `rounds`, not a literal.
    let models = report["models"].as_array().unwrap();
    assert_eq!(models.len(), 1, "{report}");
    let model = &models[0];
    assert_eq!(model["model"], "fake/model");
    let rounds = number(&model["rounds"]);
    assert!(
        rounds >= 3,
        "expected at least task + token_summary + one final round: {report}"
    );
    assert_eq!(number(&model["input"]), rounds * PROMPT, "{report}");
    assert_eq!(number(&model["output"]), rounds * COMPLETION, "{report}");
    assert_eq!(number(&model["thinking"]), rounds * REASONING, "{report}");
    assert_eq!(
        number(&model["visible_output"]),
        rounds * (COMPLETION - REASONING),
        "{report}"
    );
    assert_eq!(number(&model["unsplit_output"]), 0, "{report}");
    assert_eq!(number(&model["cache_creation"]), 0, "{report}");
    assert_eq!(number(&model["cache_read"]), 0, "{report}");
    assert_eq!(number(&model["prompt_total"]), rounds * PROMPT, "{report}");
    // Only one model was ever used, so the grand total carries the same
    // fields as that model's row, minus `model` itself.
    let mut total_from_model = model.clone();
    total_from_model.as_object_mut().unwrap().remove("model");
    assert_eq!(report["total"], total_from_model, "{report}");

    let all_requests = env.fake.requests().unwrap();

    // Root and the subagent split those rounds between them; the subagent
    // contributed at least its one required round.
    let root_row = sessions
        .iter()
        .find(|row| row["parent"].is_null())
        .expect("root row");
    let child_row = sessions
        .iter()
        .find(|row| !row["parent"].is_null())
        .expect("child row");
    assert_eq!(child_row["parent"], session.to_string());
    assert_eq!(child_row["agent"], "general");
    let root_rounds = number(&root_row["models"][0]["rounds"]);
    let child_rounds = number(&child_row["models"][0]["rounds"]);
    assert_eq!(root_row["models"][0]["model"], "fake/model", "{report}");
    assert_eq!(child_row["models"][0]["model"], "fake/model", "{report}");
    assert_eq!(root_rounds + child_rounds, rounds, "{report}");
    assert!(child_rounds >= 1, "{report}");
    assert!(
        root_rounds >= 2,
        "root made at least the task + token_summary rounds: {report}"
    );

    // `?scope=session` narrows to the root session's own rounds only.
    let (status, single) = get(&env, &format!("{usage_path}?scope=session")).await;
    assert_eq!(status, 200, "{single}");
    let single_report = &single;
    assert_eq!(single_report["scope"], "session");
    assert_eq!(single_report["sessions"].as_array().unwrap().len(), 1);
    let single_model = &single_report["models"].as_array().unwrap()[0];
    assert_eq!(
        number(&single_model["rounds"]),
        root_rounds,
        "{single_report}"
    );

    // A bad query is the bundle's own 400 with an `{error}` body.
    let (status, refused) = get(&env, &format!("{usage_path}?scope=root")).await;
    assert_eq!(status, 400, "{refused}");
    assert!(refused["error"].is_string(), "{refused}");

    // --- (b) the `token_summary` tool's Markdown table reaches the model --
    let followup = fake_requests_from(&all_requests, 1);
    assert!(
        followup.contains("fake/model")
            && followup.contains("cache creation")
            && followup.contains("thinking")
            && followup.contains("**total**"),
        "expected the token_summary Markdown table in a follow-up request: {followup}; {}",
        env.diagnostics()
    );

    std::fs::remove_dir_all(&root).unwrap();
}
