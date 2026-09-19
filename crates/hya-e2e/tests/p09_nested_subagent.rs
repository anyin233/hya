//! T2.2 — nested subagent tree depth ≥ 2 via `task` tool (ADR-0015 flow).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{
    E2eEnvBuilder, text_step, tool_step, tree_max_depth, tree_session_ids, tree_subagent_types,
};
use serde_json::json;

#[tokio::test]
async fn t2_2_nested_task_tree_depth_at_least_two() {
    // Non-blocking flow (ADR-0015): the root spawns `explore` and ends its
    // turn; the explore resident's first episode spawns `plan`; the plan
    // grandchild runs its own episode last.
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
            text_step("ROOT_OK"),
            tool_step(
                "task",
                json!({
                    "description": "spawn plan",
                    "prompt": "report GRANDCHILD_OK",
                    "subagent_type": "plan"
                }),
            ),
            text_step("CHILD_OK"),
            text_step("GRANDCHILD_OK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let root_id = session.to_string();
    let _ = env
        .prompt(session, "nested subagents")
        .await
        .expect("nested prompt");

    // Both episodes race; wait until the tree shows all three sessions.
    let mut tree_ok = false;
    let mut tree = serde_json::Value::Null;
    for _ in 0..600 {
        tree = env.session_tree(&session).await.expect("tree");
        let ids = tree_session_ids(&tree);
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        if unique.len() >= 3 {
            tree_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        tree_ok,
        "nested spawn must produce >=3 sessions (root→child→grandchild); tree={tree}; {}",
        env.diagnostics()
    );

    let depth = tree_max_depth(&tree);
    assert!(
        depth >= 2,
        "nested spawn must produce tree depth>=2 (root→child→grandchild); depth={depth}; tree={tree}; {}",
        env.diagnostics()
    );

    let kinds = tree_subagent_types(&tree);
    assert!(
        kinds.iter().any(|k| k == "explore"),
        "tree must include explore child; kinds={kinds:?}; tree={tree}; {}",
        env.diagnostics()
    );
    assert!(
        kinds.iter().any(|k| k == "plan"),
        "tree must include plan grandchild; kinds={kinds:?}; tree={tree}; {}",
        env.diagnostics()
    );

    let _ = root_id;
}
