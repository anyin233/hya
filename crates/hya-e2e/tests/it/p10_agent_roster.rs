//! T2.3 — agent roster / role visibility via public Compat API.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use hya_proto::Event;
use serde_json::json;

#[tokio::test]
async fn t2_3_agent_roster_lists_hya_main_and_spawnable_roles() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("list_agents", json!({})),
            tool_step(
                "task",
                json!({
                    "description": "Private verifier probe",
                    "prompt": "Verify a goal",
                    "subagent_type": "hya-goal-verifier"
                }),
            ),
            text_step("roster-probed"),
        ])
        .build()
        .await
        .expect("e2e env");

    let agents = env.list_agents().await.expect("list agents");
    let text = agents.to_string();
    assert!(
        text.contains("hya-main"),
        "roster must include hya-main; agents={agents}; {}",
        env.diagnostics()
    );
    // Built-in ordinary agents used for task spawn.
    let has_spawnable =
        text.contains("hya-task") || text.contains("hya-scout") || text.contains("hya-plan");
    assert!(
        has_spawnable,
        "roster should expose spawnable agent roles; agents={agents}; {}",
        env.diagnostics()
    );

    // Optional directory-scoped route (same catalog).
    let scoped = env
        .get_json(&format!(
            "/v1/agents?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .expect("scoped agents");
    let scoped_text = scoped.to_string();
    assert!(
        scoped_text.contains("hya-main"),
        "directory-scoped /v1/agents must list hya-main; {}",
        env.diagnostics()
    );

    let session = env.create_session().await.expect("ordinary session");
    env.prompt(session, "Probe agent visibility and authorization")
        .await
        .expect("prompt");
    let events = env.events(session, None).await.expect("events");
    let results = events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::ToolResult { output, .. } => Some(output.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1, "only list_agents succeeds: {events:?}");
    assert!(
        results[0].contains("hya-task"),
        "ordinary roster: {results:?}"
    );
    for id in [
        "hya-goal-guide",
        "hya-goal-verifier",
        "hya-pir-planner",
        "hya-pir-implementer",
        "hya-pir-reviewer",
    ] {
        assert!(
            !results[0].contains(id),
            "private {id} leaked into list_agents: {results:?}"
        );
    }
    assert!(events.iter().any(|envelope| matches!(&envelope.event,
        Event::ToolError { message_text, .. } if message_text.contains("AGENT_SPAWN_NOT_ALLOWED")
    )), "private task target must be refused: {events:?}");
}
