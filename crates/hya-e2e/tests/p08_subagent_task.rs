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
                        "subagent_type": "hya-task",
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
        .prompt(session, "spawn a hya-task subagent")
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
        kinds.iter().any(|k| k == "hya-task"),
        "child member.subagent_type must be hya-task; kinds={kinds:?}; tree={tree}; {}",
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

#[tokio::test]
async fn task_batch_creates_two_children_with_independent_outputs() {
    let env = E2eEnvBuilder::new()
        .route("You are hya", vec![
            tool_step("task", json!({
                "context":"Batch shared contract",
                "tasks":[
                    {"description":"First child", "prompt":"First assignment", "inline_agent":{"prompt":"BATCH_CHILD_ONE"}},
                    {"description":"Second child", "prompt":"Second assignment", "inline_agent":{"prompt":"BATCH_CHILD_TWO"}}
                ]
            })),
            text_step("BATCH_PARENT_CONTINUED"),
        ])
        .route("BATCH_CHILD_ONE", vec![text_step("BATCH_FIRST_RESULT")])
        .route("BATCH_CHILD_TWO", vec![text_step("BATCH_SECOND_RESULT")])
        .build().await.expect("environment");
    let parent = env.create_session().await.unwrap();
    env.prompt(parent, "Launch the independent batch")
        .await
        .unwrap();
    let tree = env.session_tree(&parent).await.unwrap();
    let ids: Vec<_> = tree_session_ids(&tree)
        .into_iter()
        .filter(|id| id != &parent.to_string())
        .collect();
    assert_eq!(ids.len(), 2, "two distinct child sessions: {tree}");
    assert_ne!(ids[0], ids[1]);
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let mut outputs = Vec::new();
            for id in &ids {
                let events = env.events(id.parse().unwrap(), None).await.unwrap();
                outputs.push(
                    events
                        .into_iter()
                        .filter_map(|envelope| match envelope.event {
                            Event::TextDelta { delta, .. } => Some(delta),
                            Event::TextReplace { text, .. } => Some(text),
                            _ => None,
                        })
                        .collect::<String>(),
                );
            }
            if outputs
                .iter()
                .any(|text| text.contains("BATCH_FIRST_RESULT"))
                && outputs
                    .iter()
                    .any(|text| text.contains("BATCH_SECOND_RESULT"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("both child sessions must produce their own output");
    let events = env.events(parent, None).await.unwrap();
    let results: Vec<_> = events
        .into_iter()
        .filter_map(|envelope| match envelope.event {
            Event::ToolResult { output, .. } if output["metadata"]["members"].is_array() => {
                Some(output)
            }
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 1, "one batch tool result");
    assert_eq!(
        results[0]["metadata"]["members"].as_array().unwrap().len(),
        2
    );
    assert_eq!(results[0]["metadata"]["status"], "running");
}
