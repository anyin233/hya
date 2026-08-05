//! T1.9 — project skill discovery + skill tool load.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::json;

const SKILL_MD: &str = r#"---
name: e2e-skill
description: Process E2E skill fixture
---
E2E_SKILL_BODY_MARKER for agent suite.
"#;

#[tokio::test]
async fn t1_9_skill_tool_loads_project_skill_body() {
    let env = E2eEnvBuilder::new()
        .skill_file(".hya/skills/e2e-skill/SKILL.md", SKILL_MD)
        .scripts(vec![
            tool_step("skill", json!({ "name": "e2e-skill" })),
            text_step("SKILL_LOADED"),
        ])
        .build()
        .await
        .expect("e2e env");

    let skills = env
        .get_json(&format!("/skill?directory={}", env.backend.workdir_str()))
        .await
        .expect("skill list");
    let listed = skills
        .as_array()
        .cloned()
        .or_else(|| skills.get("data").and_then(|d| d.as_array()).cloned())
        .unwrap_or_default();
    assert!(
        listed
            .iter()
            .any(|s| s.get("name").and_then(|n| n.as_str()) == Some("e2e-skill")),
        "project skill must appear in /skill; body={skills}; {}",
        env.diagnostics()
    );

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "load e2e-skill")
        .await
        .expect("skill prompt");

    let requests = env.fake.requests().expect("fake requests");
    assert!(
        requests.len() >= 2,
        "skill tool turn + follow-up model turn required; {}",
        env.diagnostics()
    );
    // Marker only appears after skill tool loads SKILL.md into the tool result
    // that is sent on the *next* completion request — not in the tool-call args.
    let follow_up = fake_requests_from(&requests, 1);
    assert!(
        follow_up.contains("E2E_SKILL_BODY_MARKER"),
        "follow-up FakeLlm request must include loaded skill body; follow_up={follow_up}; {}",
        env.diagnostics()
    );
}
