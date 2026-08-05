//! T1.7 — non-yolo permission ask / once / reject against real backend.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

#[tokio::test]
async fn t1_7_permission_once_allows_shell_side_effect() {
    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step(
                "shell",
                json!({ "command": "printf once-ok > e2e-perm-once.txt" }),
            ),
            text_step("PERM_ONCE_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt_with_permission_reply(
            session,
            "run shell with permission",
            "once",
            Duration::from_secs(30),
        )
        .await
        .expect("prompt+permission");

    assert_eq!(
        env.read_project_file("e2e-perm-once.txt")
            .expect("side effect"),
        "once-ok",
        "diagnostics={}",
        env.diagnostics()
    );
}

#[tokio::test]
async fn t1_7_permission_reject_blocks_shell_side_effect() {
    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step(
                "shell",
                json!({ "command": "printf reject-leak > e2e-perm-reject.txt" }),
            ),
            text_step("PERM_REJECT_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt_with_permission_reply(
            session,
            "run shell that must be rejected",
            "reject",
            Duration::from_secs(30),
        )
        .await
        .expect("prompt+reject completes turn");

    assert!(
        !env.project_path("e2e-perm-reject.txt").exists(),
        "rejected shell must not write file; diagnostics={}",
        env.diagnostics()
    );
}
