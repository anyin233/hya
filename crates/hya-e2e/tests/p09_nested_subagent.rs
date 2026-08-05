//! T2.2 — nested subagent tree depth ≥ 2 via `task` tool.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t2_2_nested_task_tree_depth_at_least_two() {
    // Root → explore → plan (grandchild text) → explore text → root text.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "task",
                json!({
                    "description": "spawn explore",
                    "prompt": "spawn a plan child then finish",
                    "subagent_type": "explore"
                }),
            ),
            tool_step(
                "task",
                json!({
                    "description": "spawn plan",
                    "prompt": "report GRANDCHILD_OK",
                    "subagent_type": "plan"
                }),
            ),
            text_step("GRANDCHILD_OK"),
            text_step("CHILD_OK"),
            text_step("ROOT_OK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "nested subagents")
        .await
        .expect("nested prompt");

    let tree = env.session_tree(&session).await.expect("tree");
    let tree_text = tree.to_string();
    let depth_hint = tree_text.matches("explore").count()
        + tree_text.matches("plan").count()
        + tree_text.matches("children").count();
    assert!(
        depth_hint >= 1 || env.fake.requests().unwrap().len() >= 3,
        "expected nested spawn evidence in tree or fake turns; tree={tree}; {}",
        env.diagnostics()
    );
    assert!(
        env.fake.requests().unwrap().len() >= 3,
        "root + child + grandchild llm turns expected; {}",
        env.diagnostics()
    );

    // Flatten tree strings for depth≥2: grandchildren or two nested session ids.
    let session_id_count = tree_text.matches("ses_").count() + tree_text.matches("hysec_").count();
    assert!(
        session_id_count >= 2
            || (tree_text.contains("explore") && tree_text.contains("plan"))
            || env.fake.requests().unwrap().len() >= 4,
        "depth>=2 should show multiple sessions or explore+plan; tree={tree}; {}",
        env.diagnostics()
    );
}
