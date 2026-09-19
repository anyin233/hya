//! T2.1 — single-member `task` subagent spawn via FakeLlm (ADR-0015 flow).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{
    E2eEnvBuilder, text_step, tool_step, tree_children, tree_session_ids, tree_subagent_types,
};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t2_1_task_tool_spawns_general_subagent() {
    // Non-blocking flow (ADR-0015): the root's `task` call returns the child's
    // handle immediately, the root finishes its turn, and the child resident
    // runs its first episode in its own session.
    let env = E2eEnvBuilder::new()
        .route(
            "You are hya",
            vec![
                tool_step(
                    "task",
                    json!({
                        "description": "e2e child",
                        "prompt": "do the child work",
                        "subagent_type": "general",
                        "inline_agent": {
                            "description": "",
                            "category": "",
                            "model": "",
                            "name": "",
                            "prompt": "MARKER_CHILD_PROMPT do the child work",
                            "resident": false
                        }
                    }),
                ),
                text_step("PARENT_AFTER_TASK"),
            ],
        )
        .route("MARKER_CHILD_PROMPT", vec![text_step("CHILD_TASK_OK")])
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

    // The root's own turn completes after the non-blocking spawn. The
    // quiesce steer can delay the continuation round, so poll for the
    // terminal text instead of reading the log once.
    let mut parent_ok = false;
    let mut text = String::new();
    for _ in 0..600 {
        let events = env.events(session, None).await.expect("events");
        text = String::new();
        for env_evt in events {
            match env_evt.event {
                Event::TextDelta { delta, .. } => text.push_str(&delta),
                Event::TextReplace { text: t, .. } => text.push_str(&t),
                _ => {}
            }
        }
        if text.contains("PARENT_AFTER_TASK") {
            parent_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        parent_ok,
        "parent completes its turn after the non-blocking spawn; text={text:?}; {}",
        env.diagnostics()
    );

    // …and the child resident runs its first episode in its own session.
    let mut child_ok = false;
    for _ in 0..600 {
        let mut child_text = String::new();
        for id in &child_ids {
            if let Ok(events) = env.events(id.parse().unwrap(), None).await {
                for env_evt in events {
                    match env_evt.event {
                        Event::TextDelta { delta, .. } => child_text.push_str(&delta),
                        Event::TextReplace { text: t, .. } => child_text.push_str(&t),
                        _ => {}
                    }
                }
            }
        }
        if child_text.contains("CHILD_TASK_OK") {
            child_ok = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        child_ok,
        "child resident must run its first episode; ids={child_ids:?}; {}",
        env.diagnostics()
    );
}
