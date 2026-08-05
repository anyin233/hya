//! T1.13 — project AGENTS.md is layered into Compat-guided turns (context management).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step};

const AGENTS_MARKER: &str = "E2E_AGENTS_CONTEXT_MARKER_use_spaces";

#[tokio::test]
async fn t1_13_compat_prompt_includes_project_agents_md_in_model_request() {
    let env = E2eEnvBuilder::new()
        .project_file(
            "AGENTS.md",
            format!("# Project agents\n\n{AGENTS_MARKER}\n").into_bytes(),
        )
        .scripts(vec![text_step("AGENTS_GUIDED_OK")])
        .build()
        .await
        .expect("e2e env");

    // Compat v2 prompt path injects discover_context_files guidance; native
    // /sessions/:id/prompt does not.
    let session = env.compat_create_session().await.expect("compat session");
    let _ = env
        .compat_prompt_and_wait(
            session,
            "follow project agents guidance",
            Duration::from_secs(30),
        )
        .await
        .expect("compat prompt");

    let requests = env.fake.requests().expect("fake requests");
    assert!(
        !requests.is_empty(),
        "FakeLlm must receive a completion; {}",
        env.diagnostics()
    );
    let dumped = serde_json::to_string(&requests).unwrap_or_default();
    assert!(
        dumped.contains(AGENTS_MARKER),
        "model request must include AGENTS.md body marker; dump={dumped}; {}",
        env.diagnostics()
    );
    assert!(
        dumped.contains("Project context") || dumped.contains("AGENTS.md"),
        "model request should label project context; dump={dumped}; {}",
        env.diagnostics()
    );
}
