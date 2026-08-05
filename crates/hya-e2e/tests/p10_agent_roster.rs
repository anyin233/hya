//! T2.3 — agent roster / role visibility via public Compat API.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, text_step};

#[tokio::test]
async fn t2_3_agent_roster_lists_build_and_spawnable_roles() {
    let env = E2eEnvBuilder::new()
        .scripts(vec![text_step("roster-noop")])
        .build()
        .await
        .expect("e2e env");

    let agents = env.list_agents().await.expect("list agents");
    let text = agents.to_string();
    assert!(
        text.contains("build"),
        "roster must include build; agents={agents}; {}",
        env.diagnostics()
    );
    // Built-in ordinary agents used for task spawn.
    let has_spawnable = text.contains("general")
        || text.contains("explore")
        || text.contains("plan");
    assert!(
        has_spawnable,
        "roster should expose spawnable agent roles; agents={agents}; {}",
        env.diagnostics()
    );

    // Optional directory-scoped route (same catalog).
    let scoped = env
        .get_json(&format!(
            "/api/agent?directory={}",
            env.backend.workdir_str()
        ))
        .await
        .expect("scoped agents");
    let scoped_text = scoped.to_string();
    assert!(
        scoped_text.contains("build"),
        "directory-scoped /api/agent must list build; {}",
        env.diagnostics()
    );
}
