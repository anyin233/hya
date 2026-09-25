//! T1.7 — non-yolo permission ask / once / reject against real backend, and
//! switching a session to the `yolo` permission mode mid-session.
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
                "bash",
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
                "bash",
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

/// A turn blocked on a permission ask continues once the session switches to
/// `yolo` (the pending ask is allowed once), and the next turn never asks.
#[tokio::test]
async fn t1_7_switching_to_yolo_unblocks_the_pending_ask_and_skips_later_asks() {
    let env = E2eEnvBuilder::new()
        .yolo(false)
        .permission_model("default")
        .scripts(vec![
            tool_step(
                "bash",
                json!({ "command": "printf first > e2e-yolo-first.txt" }),
            ),
            text_step("YOLO_FIRST_DONE"),
            tool_step(
                "bash",
                json!({ "command": "printf second > e2e-yolo-second.txt" }),
            ),
            text_step("YOLO_SECOND_DONE"),
        ])
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let info = env
        .get_json(&format!("/v1/sessions/{session}"))
        .await
        .expect("session info");
    assert_eq!(info["permissionMode"], json!("manual"));

    let switch = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let pending = env.list_permissions().await.expect("list permissions");
            if pending.as_array().is_some_and(|rows| !rows.is_empty()) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the first shell call never asked; diagnostics={}",
                env.diagnostics()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        env.patch_json(
            &format!("/v1/sessions/{session}"),
            &json!({ "permissionMode": "yolo" }),
        )
        .await
        .expect("switch to yolo")
    };
    let (first, updated) = tokio::join!(env.prompt(session, "run the first shell"), switch);
    first.expect("first turn finishes after the switch");
    assert_eq!(updated["permissionMode"], json!("yolo"));
    assert_eq!(
        env.read_project_file("e2e-yolo-first.txt")
            .expect("first side effect"),
        "first"
    );

    env.prompt(session, "run the second shell")
        .await
        .expect("second turn runs without asking");
    assert_eq!(
        env.read_project_file("e2e-yolo-second.txt")
            .expect("second side effect"),
        "second",
        "diagnostics={}",
        env.diagnostics()
    );
    assert!(
        env.list_permissions()
            .await
            .expect("list permissions")
            .as_array()
            .is_none_or(Vec::is_empty),
        "nothing is left pending"
    );
}
