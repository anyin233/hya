//! Channel-plane scenarios (`dm`, `broadcast`) against the real backend
//! (ADR-0015/0016 flow).
//!
//! Every teammate is a resident (ADR-0015: all subagents are), spawned via the
//! non-blocking `task` tool. Delivery is proven the only honest way — the
//! message appears in the recipient's own next model request (`route_dump`).
//!
//! Routing note: the ROOT system prompt always contains its marker, so every
//! root step (including the initial `task` call) must live on the root's
//! route; anything left in the shared queue would be stolen by the child.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use hya_e2e::{E2eEnvBuilder, text_step, tool_step};
use serde_json::{Value, json};

/// Route markers — prefixes of each agent's system prompt (routing keys).
const SYS_ROOT: &str = "You are hya";
const SYS_CHILD: &str = "SYS_MARKER_CHILD";

const TIMEOUT: Duration = Duration::from_secs(20);

/// One `task` member spawning a resident whose system prompt starts with `marker`.
fn resident_member(marker: &str, directive: &str) -> Value {
    json!({
        "description": format!("resident {marker}"),
        "prompt": directive,
        "subagent_type": "general",
        "inline_agent": { "prompt": format!("{marker} You are a resident teammate.") }
    })
}

/// T2.5 (vertical DM): a child's send with no channel defaults to the
/// parent DM and is delivered into the parent's next model request.
#[tokio::test]
async fn t2_5_send_default_to_parent_is_delivered_into_the_parents_next_turn() {
    let env = E2eEnvBuilder::new()
        .route(
            SYS_CHILD,
            vec![
                tool_step("send", json!({ "body": "CHILD_HELLO_PARENT" })),
                text_step("CHILD_DONE"),
            ],
        )
        .route(
            SYS_ROOT,
            vec![
                tool_step(
                    "task",
                    json!({ "members": [
                        resident_member(SYS_CHILD, "dm your parent then finish")
                    ]}),
                ),
                text_step("ROOT_SPAWNED"),
                // Woken by the child's dm; the request must carry its body.
                text_step("ROOT_GOT_MAIL"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "spawn the team")
        .await
        .expect("spawn prompt");

    env.wait_route_contains(SYS_ROOT, "CHILD_HELLO_PARENT", TIMEOUT)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "parent never received the dm: {e}; root={}",
                env.route_dump(SYS_ROOT).unwrap_or_default()
            )
        });
}

/// T2.6 (broadcast): the root's send with no channel defaults to its unit
/// group channel and reaches its direct child.
#[tokio::test]
async fn t2_6_send_default_broadcast_reaches_the_direct_child() {
    let env = E2eEnvBuilder::new()
        .route(
            SYS_CHILD,
            vec![text_step("CHILD_IDLE"), text_step("CHILD_HEARD")],
        )
        .route(
            SYS_ROOT,
            vec![
                tool_step(
                    "task",
                    json!({ "members": [
                        resident_member(SYS_CHILD, "wait for a broadcast")
                    ]}),
                ),
                text_step("ROOT_SPAWNED"),
                tool_step("send", json!({ "body": "ALL_HANDS_BROADCAST" })),
                text_step("ROOT_DONE"),
            ],
        )
        .build()
        .await
        .expect("e2e env");

    let session = env.create_session().await.expect("session");
    let _ = env
        .prompt(session, "spawn the team")
        .await
        .expect("spawn prompt");

    env.wait_route_contains(SYS_CHILD, "ALL_HANDS_BROADCAST", TIMEOUT)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "child never heard the broadcast: {e}; child={}",
                env.route_dump(SYS_CHILD).unwrap_or_default()
            )
        });
}
