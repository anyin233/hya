//! P17 — user-assembled workflow composition end to end.
//!
//! A user-authored file under `<workdir>/.hya/workflows` (never a preset) is
//! discovered by the live backend and launched mid-session through the
//! `workflow` tool: fan-out (explore → two parallel impls) then fan-in (review
//! joins both upstream sections), with the lead resuming on the final report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{
    E2eEnvBuilder, text_step, tool_step, tree_children, tree_session_ids, tree_subagent_types,
};
use hya_proto::Event;
use serde_json::json;

const WORKFLOW_SOURCE: &str = r#"---
kind: Workflow
name: compose
description: Explore fans out to two implementations; review joins both.
on_failure: collect_all
inputs:
  target: What to explore.
nodes:
  explore:
    agent: general
    directive: EXPLORE {{input.target}}
  impl_a:
    agent: general
    directive: IMPL A
  impl_b:
    agent: general
    directive: IMPL B
  review:
    agent: general
    directive: REVIEW both
---
flowchart TD
  explore --> impl_a & impl_b
  impl_a & impl_b --> review
"#;

#[tokio::test]
async fn p17_user_authored_workflow_runs_fan_out_fan_in_via_tool() {
    // Shared queue consumed in order; the two fan-out members run in parallel
    // but answer the SAME scripted text, so arrival order cannot skew results.
    // Level barriers guarantee no later stage can steal an earlier step.
    let env = E2eEnvBuilder::new()
        .yolo(true)
        .project_file(
            ".hya/workflows/compose.hya.md",
            WORKFLOW_SOURCE.as_bytes().to_vec(),
        )
        .scripts(vec![
            tool_step(
                "workflow",
                json!({
                    "action": "run",
                    "name": "compose",
                    "inputs": { "target": "the parser" }
                }),
            ),
            text_step("EXPLORED_OK"),
            text_step("IMPL_OK"),
            text_step("IMPL_OK"),
            text_step("REVIEWED_OK"),
            text_step("PARENT_AFTER_WORKFLOW"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let root_id = session.to_string();
    let _ = env
        .prompt(session, "run my composed workflow")
        .await
        .expect("workflow prompt");

    let tree = env.session_tree(&session).await.expect("session tree");
    let children = tree_children(&tree);
    assert_eq!(
        children.len(),
        4,
        "exactly four member sessions must spawn (fan-out then join); tree={tree}; {}",
        env.diagnostics()
    );
    let ids = tree_session_ids(&tree);
    assert!(
        ids.iter().filter(|id| *id != &root_id).count() == 4,
        "all members run in distinct child sessions; ids={ids:?}"
    );
    let kinds = tree_subagent_types(&tree);
    assert!(
        kinds.iter().all(|k| k == "general"),
        "every stage resolves its declared agent; kinds={kinds:?}"
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
        text.contains("PARENT_AFTER_WORKFLOW"),
        "lead must resume with the workflow report in context; text={text:?}; {}",
        env.diagnostics()
    );

    // Fan-out/fan-in is visible in the actual member directives recorded by
    // FakeLlm: one EXPLORE turn feeding both IMPL turns, and the REVIEW turn
    // embedding BOTH bounded upstream sections.
    let requests = env.fake_requests().expect("fake requests");
    let contains = |needle: &str| {
        requests.iter().any(|request| {
            serde_json::to_string(request)
                .unwrap_or_default()
                .contains(needle)
        })
    };
    for directive in ["\"EXPLORE the parser\"", "IMPL A", "IMPL B", "REVIEW both"] {
        assert!(
            contains(directive),
            "member directive `{directive}` must reach a member; {}",
            env.diagnostics()
        );
    }
    assert!(
        contains("<stage id=\\\"impl_a\\\"") && contains("<stage id=\\\"impl_b\\\""),
        "join directive must embed both fanned-in evidence entries; {}",
        env.diagnostics()
    );
}
