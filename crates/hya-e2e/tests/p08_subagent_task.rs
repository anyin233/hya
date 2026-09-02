//! T2.1 — single-member `task` subagent spawn via FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{
    E2eEnvBuilder, text_step, tool_step, tree_children, tree_session_ids, tree_subagent_types,
};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t2_1_task_tool_spawns_general_subagent() {
    // Script order: root requests task → child completes with text → root finishes.
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step(
                "task",
                json!({
                    "description": "e2e child",
                    "prompt": "report CHILD_TASK_OK",
                    "subagent_type": "general",
                    "inline_agent": {
                        "description": "",
                        "category": "",
                        "model": "",
                        "name": "",
                        "prompt": "",
                        "resident": false
                    }
                }),
            ),
            text_step("CHILD_TASK_OK"),
            text_step("PARENT_AFTER_TASK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let root_id = session.to_string();
    let _ = env
        .prompt(session, "spawn a general subagent")
        .await
        .expect("task prompt");

    let tree = env.session_tree(&session).await.expect("session tree");
    let children = tree_children(&tree);
    assert!(
        !children.is_empty(),
        "run tree must have >=1 child after task spawn; tree={tree}; {}",
        env.diagnostics()
    );

    let kinds = tree_subagent_types(&tree);
    assert!(
        kinds.iter().any(|k| k == "general"),
        "child member.subagent_type must be general; kinds={kinds:?}; tree={tree}; {}",
        env.diagnostics()
    );

    let ids = tree_session_ids(&tree);
    let child_ids: Vec<_> = ids.iter().filter(|id| *id != &root_id).collect();
    assert!(
        !child_ids.is_empty(),
        "tree must include a distinct child session id; root={root_id}; ids={ids:?}; tree={tree}; {}",
        env.diagnostics()
    );

    let events = env.events(session, None).await.expect("events");
    let mut text = String::new();
    for env_evt in events {
        match env_evt.event {
            Event::TextDelta { delta, .. } => text.push_str(&delta),
            Event::TextReplace { text: t, .. } => text.push_str(&t),
            _ => {}
        }
    }
    assert!(
        text.contains("PARENT_AFTER_TASK"),
        "parent must resume with final text after subagent; text={text:?}; {}",
        env.diagnostics()
    );
}
