//! `wait` exists in two tool families. The channel-tools one also wakes on
//! mail and replaces the extended-tools one whenever both are loaded — by an
//! explicit `overrides: hya/extended-tools` in its exposure policy, never by
//! load or lexicographic order (`hya/channel-tools` sorts first).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::SessionId;
use hya_tool::{LifecyclePlane, LifecycleRequest, ToolRegistry, WaitOutcome, WaitWake};
use serde_json::json;

const WITHOUT_CHANNELS: [&str; 4] = [
    "hya/base-tools",
    "hya/extended-tools",
    "hya/network-tools",
    "hya/todo-tools",
];

fn ctx(lifecycle: LifecyclePlane) -> hya_tool::ToolCtx {
    hya_tool::ToolCtx {
        permission: hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default()).0,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        workflows: hya_tool::WorkflowPlane::disconnected(),
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle,
        session: None,
        parent_session: None,
        todo: hya_tool::TodoPlane::default(),
        skills: hya_tool::SkillPlane::default(),
        artifacts: hya_tool::handle::ArtifactPlane::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: std::path::PathBuf::from("."),
        cancel: tokio_util::sync::CancellationToken::new(),
    }
}

/// Run the registry's `wait` once and return whether its request asked to
/// wake on mail.
async fn requested_mail_wake(registry: &ToolRegistry) -> bool {
    let tool = registry.get("wait").expect("wait is registered");
    let (plane, mut rx) = LifecyclePlane::new();
    let ctx = ctx(plane.for_session(SessionId::new()));
    let call = tokio::spawn(async move { tool.execute(&ctx, json!({"timeout_secs": 5})).await });
    let Some(LifecycleRequest::Wait { spec, reply, .. }) = rx.recv().await else {
        panic!("wait must send a lifecycle wait request");
    };
    reply
        .send(Ok(WaitOutcome {
            woke_by: WaitWake::NothingToWaitFor,
            finished: Vec::new(),
            running: Vec::new(),
            mail: Vec::new(),
            waited_ms: 0,
        }))
        .unwrap();
    let result = call.await.unwrap().unwrap();
    assert_eq!(result["metadata"]["woke_by"], "nothing_to_wait_for");
    spec.wake_on_mail
}

#[tokio::test]
async fn with_channel_tools_loaded_wait_is_the_mail_aware_override() {
    let registry = ToolRegistry::builtins();
    assert_eq!(
        registry.builtin_bundle_origin("wait"),
        Some("hya/channel-tools")
    );
    let resolved = registry.resolve("wait").unwrap();
    assert_eq!(resolved.permission, hya_tool::ToolPermission::ReadOnly);
    assert!(resolved.tool.schema().description.contains("mail arrives"));
    assert!(requested_mail_wake(&registry).await);
    // One advertised `wait`, never two.
    let count = registry
        .schemas()
        .iter()
        .filter(|schema| schema.name.as_str() == "wait")
        .count();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn without_channel_tools_wait_is_the_extended_member_only_tool() {
    let registry = ToolRegistry::from_tool_families(&WITHOUT_CHANNELS);
    assert!(registry.get("send").is_none(), "channel tools are absent");
    assert_eq!(
        registry.builtin_bundle_origin("wait"),
        Some("hya/extended-tools")
    );
    assert!(
        !registry
            .resolve("wait")
            .unwrap()
            .tool
            .schema()
            .description
            .contains("mail arrives")
    );
    assert!(!requested_mail_wake(&registry).await);
}

#[test]
fn channel_tools_alone_still_provide_the_mail_aware_wait() {
    let registry = ToolRegistry::from_tool_families(&["hya/channel-tools"]);
    assert_eq!(
        registry.builtin_bundle_origin("wait"),
        Some("hya/channel-tools")
    );
}
