//! Claude imports execute through the ordinary bundle runtime after source removal.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_e2e::{E2eEnvBuilder, fake_requests_from, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t2_24_claude_import_runs_packaged_hook_and_skill() {
    let root = std::env::temp_dir().join(format!("hya-claude-e2e-{}", std::process::id()));
    for dir in [".claude-plugin", "hooks", "skills/claude-help"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        r#"{"name":"claude-e2e","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("skills/claude-help/SKILL.md"),
        "---\nname: claude-help\ndescription: Imported help\n---\nCLAUDE_PACKAGED_SKILL\n",
    )
    .unwrap();
    std::fs::write(
        root.join("hooks/guard.py"),
        "import json\nprint(json.dumps({'decision':'block','reason':'CLAUDE_PACKAGED_GUARD'}))\n",
    )
    .unwrap();
    std::fs::write(root.join("hooks/hooks.json"), serde_json::to_vec(&json!({"hooks":{"PreToolUse":[{"matcher":"Read","hooks":[{"type":"command","command":"python3 ${CLAUDE_PLUGIN_ROOT}/hooks/guard.py"}]}]}})).unwrap()).unwrap();
    let env = E2eEnvBuilder::new()
        .scripts(vec![
            tool_step("read", json!({"path":"README.md"})),
            tool_step("skill", json!({"name":"claude-help"})),
            text_step("IMPORTED"),
            tool_step("skill", json!({"name":"claude-help"})),
            text_step("REMOVED"),
        ])
        .build()
        .await
        .unwrap();
    let install = env
        .backend
        .bundle_cli(&["bundle", "install", "--claude", root.to_str().unwrap()])
        .unwrap();
    assert!(
        install.status.success(),
        "{}",
        String::from_utf8_lossy(&install.stderr)
    );
    std::fs::remove_dir_all(&root).unwrap();
    let session = env.create_session().await.unwrap();
    env.prompt(session, "use imported hook and skill")
        .await
        .unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 2);
    assert!(
        followup.contains("CLAUDE_PACKAGED_GUARD"),
        "{followup}; {}",
        env.diagnostics()
    );
    assert!(
        followup.contains("CLAUDE_PACKAGED_SKILL"),
        "{followup}; {}",
        env.diagnostics()
    );
    let uninstall = env
        .backend
        .bundle_cli(&["bundle", "uninstall", "claude/claude-e2e"])
        .unwrap();
    assert!(
        uninstall.status.success(),
        "{}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    let session = env.create_session().await.unwrap();
    env.prompt(session, "use removed skill").await.unwrap();
    let requests = env.fake.requests().unwrap();
    let followup = fake_requests_from(&requests, 4);
    assert!(
        followup.contains("skill not found: claude-help"),
        "{followup}"
    );
}
