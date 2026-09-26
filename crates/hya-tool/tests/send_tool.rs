//! Integration tests for `hya-tool`: the unified `send` channel tool.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_proto::{MailEndpoint, MailKind, SessionId};
use hya_tool::{
    InteractionPlane, LspPlane, MailReceipt, MailboxPlane, MailboxRequest, PermissionPlane,
    PermissionRules, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError, ToolRegistry,
    WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// One captured request plus its reply slot, recorded by the canned service.
#[derive(Debug)]
enum Captured {
    Send { to: MailEndpoint, kind: MailKind },
    SendDefault,
}

fn ctx_with(mailbox: MailboxPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let (spawner, _srx) = SpawnerPlane::new();
    let (interaction, _irx) = InteractionPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: mailbox.for_session(session),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        workdir: PathBuf::from("."),
        roots: vec![PathBuf::from(".")],
        cancel: CancellationToken::new(),
    }
}

/// Answer every plane request with a canned receipt and record what the
/// tool asked for.
fn canned_service() -> (
    MailboxPlane,
    tokio::sync::mpsc::UnboundedReceiver<Captured>,
    tokio::task::JoinHandle<()>,
) {
    let (plane, mut rx) = MailboxPlane::new();
    let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let service = tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            match request {
                MailboxRequest::Send {
                    to, kind, reply, ..
                } => {
                    let _ = seen_tx.send(Captured::Send {
                        to: to.clone(),
                        kind,
                    });
                    let _ = reply.send(Ok(MailReceipt {
                        from: "main/lead".to_string(),
                        to,
                        recipients: 2,
                    }));
                }
                MailboxRequest::SendDefault { reply, .. } => {
                    let _ = seen_tx.send(Captured::SendDefault);
                    let _ = reply.send(Ok(MailReceipt {
                        from: "main/lead".to_string(),
                        to: MailEndpoint::Channel("announce-aB12Cd34".to_string()),
                        recipients: 2,
                    }));
                }
                _ => {}
            }
        }
    });
    (plane, seen_rx, service)
}

async fn run_send(input: serde_json::Value) -> (serde_json::Value, Captured) {
    let session = SessionId::new();
    let (plane, mut seen, _service) = canned_service();
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("send").unwrap();
    let out = tool.execute(&ctx, input).await.expect("send result");
    let captured = seen.recv().await.expect("captured request");
    (out, captured)
}

#[tokio::test]
async fn send_hashes_and_bare_channel_ids_route_to_the_channel() {
    let (out, captured) = run_send(json!({ "channel": "#ops", "body": "hello" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Channel(c), .. } if c == "ops"
        ),
        "{captured:?}"
    );
    assert_eq!(out["title"], "Sent to #ops");
    assert_eq!(out["metadata"]["recipients"], 2);

    let (_, captured) = run_send(json!({ "channel": "DM-aB12Cd34", "body": "psst" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Channel(c), .. } if c == "DM-aB12Cd34"
        ),
        "{captured:?}"
    );

    let (_, captured) =
        run_send(json!({ "channel": "announce-aB12Cd34", "body": "all hands" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Channel(c), .. } if c == "announce-aB12Cd34"
        ),
        "{captured:?}"
    );
}

#[tokio::test]
async fn send_handles_and_the_parent_sentinel_route_to_direct_mail() {
    let (_, captured) =
        run_send(json!({ "channel": "main/worker-1", "body": "do the thing" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Handle(h), kind: MailKind::Message } if h == "main/worker-1"
        ),
        "{captured:?}"
    );

    // The legacy `to` field spelling still parses.
    let (_, captured) = run_send(json!({ "to": "main/worker-2", "body": "hi" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Handle(h), .. } if h == "main/worker-2"
        ),
        "{captured:?}"
    );

    let (_, captured) = run_send(json!({ "channel": "^parent", "body": "done" })).await;
    assert!(
        matches!(
            &captured,
            Captured::Send { to: MailEndpoint::Handle(h), .. } if h == "^parent"
        ),
        "{captured:?}"
    );
}

#[tokio::test]
async fn send_without_a_channel_uses_the_role_default() {
    let (out, captured) = run_send(json!({ "body": "status update" })).await;
    assert!(matches!(captured, Captured::SendDefault), "{captured:?}");
    assert_eq!(out["title"], "Sent to #announce-aB12Cd34");
    assert_eq!(out["metadata"]["to"], "#announce-aB12Cd34");
}

#[tokio::test]
async fn send_rejects_empty_bodies() {
    let session = SessionId::new();
    let (plane, _seen, _service) = canned_service();
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("send").unwrap();
    let err = tool
        .execute(&ctx, json!({ "channel": "#ops", "body": "   " }))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ToolError::Input(m) if m.contains("empty")),
        "{err:?}"
    );
}

#[test]
fn send_replaces_dm_and_broadcast_in_the_registry() {
    let registry = ToolRegistry::builtins();
    assert!(registry.get("send").is_some());
    assert!(registry.get("dm").is_none());
    assert!(registry.get("broadcast").is_none());
    let canonical: Vec<String> = registry
        .snapshot()
        .canonical_tools()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert!(!canonical.contains(&"dm".to_string()));
    assert!(!canonical.contains(&"broadcast".to_string()));
    assert_eq!(canonical.len(), 28);
}
