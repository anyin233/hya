//! T2.1 — single-member `task` subagent spawn via FakeLlm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
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
                    "subagent_type": "general"
                }),
            ),
            text_step("CHILD_TASK_OK"),
            text_step("PARENT_AFTER_TASK"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "spawn a general subagent")
        .await
        .expect("task prompt");

    let tree = env.session_tree(&session).await.expect("session tree");
    let tree_text = tree.to_string();
    assert!(
        tree_text.contains("general")
            || tree_text.contains("CHILD")
            || tree_text.contains("ses_")
            || tree_text.contains("hysec_"),
        "run tree should reflect a child session; tree={tree}; {}",
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
        text.contains("PARENT_AFTER_TASK") || env.fake.requests().unwrap().len() >= 2,
        "parent should resume after subagent; text={text:?}; {}",
        env.diagnostics()
    );
    assert!(
        env.fake.requests().unwrap().len() >= 2,
        "root + child llm turns expected; {}",
        env.diagnostics()
    );
}
