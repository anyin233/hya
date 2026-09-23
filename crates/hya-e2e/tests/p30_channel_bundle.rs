//! Installed channel policy constrains real child sends and uninstalls cleanly.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use hya_bundle::{BundleSource, SourceFile, write_public_package};
use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::json;

const ROOT: &str = "You are hya";
const SECOND_ROOT: &str = "You are hya-main";
const CHILD: &str = "CHANNEL_POLICY_CHILD";

#[tokio::test]
async fn t2_25_installed_channel_policy_denies_child_send_and_uninstall_restores_default() {
    let source = BundleSource::new(
        "channel-policy",
        vec![SourceFile::new(
            "bundle.yaml",
            "kind: AgentSetBundle\nidentity: { id: acme/channel-policy, version: 1.0.0, publisher: acme }\nchannels:\n  - id: child-dm\n    kind: parent_dm\n    participants: [{ kind: role, role: child }]\n    capabilities: [report, steer, follow_up, resident_mail]\n    scope: vertical\n    retention: team_session\n",
        )],
    );
    let package = std::env::temp_dir().join(format!(
        "hya-channel-policy-{}.hyabundle",
        hya_proto::SessionId::new()
    ));
    std::fs::write(&package, write_public_package(&source).unwrap()).unwrap();
    let spawn = |marker: &str| {
        tool_step(
            "task",
            json!({"members": [{
                "description": "channel-policy worker",
                "prompt": "send a message to the parent",
                "subagent_type": "general",
                "inline_agent": {"prompt": format!("{marker} Send the requested message.")}
            }]}),
        )
    };
    let env = E2eEnvBuilder::new()
        .route(
            CHILD,
            vec![
                tool_step("send", json!({"body": "DENIED_CHANNEL_PAYLOAD"})),
                text_step("DENIED_CHILD_FINISHED"),
            ],
        )
        .route(
            "RESTORED_CHILD",
            vec![
                tool_step("send", json!({"body": "RESTORED_CHANNEL_PAYLOAD"})),
                text_step("RESTORED_CHILD_FINISHED"),
            ],
        )
        .route(
            SECOND_ROOT,
            vec![
                spawn("RESTORED_CHILD"),
                text_step("SECOND_ROOT_DONE"),
                text_step("MAIL_RECEIVED"),
            ],
        )
        .route(
            ROOT,
            vec![
                spawn(CHILD),
                text_step("FIRST_ROOT_DONE"),
                text_step("FIRST_ROOT_SETTLED"),
            ],
        )
        .build()
        .await
        .unwrap();
    let installed = env
        .backend
        .bundle_cli(&["bundle", "install", "-y", package.to_str().unwrap()])
        .unwrap();
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    std::fs::remove_file(package).unwrap();
    let first = env.create_session().await.unwrap();
    env.prompt(first, "spawn the first worker").await.unwrap();
    env.wait_route_contains(CHILD, "channel policy", Duration::from_secs(20))
        .await
        .unwrap_or_else(|error| panic!("send was not refused: {error}; {}", env.diagnostics()));
    assert!(
        !env.route_dump(ROOT)
            .unwrap_or_default()
            .contains("DENIED_CHANNEL_PAYLOAD")
    );

    let removed = env
        .backend
        .bundle_cli(&["bundle", "uninstall", "-y", "acme/channel-policy"])
        .unwrap();
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let second = env.create_session_with_agent("hya-main").await.unwrap();
    env.prompt(second, "spawn a new worker under the default policy")
        .await
        .unwrap();
    env.wait_route_contains(
        SECOND_ROOT,
        "RESTORED_CHANNEL_PAYLOAD",
        Duration::from_secs(20),
    )
    .await
    .unwrap_or_else(|error| {
        panic!(
            "default delivery was not restored: {error}; {}",
            env.diagnostics()
        )
    });
    assert!(
        !env.route_dump(ROOT)
            .unwrap_or_default()
            .contains("DENIED_CHANNEL_PAYLOAD")
    );
}
