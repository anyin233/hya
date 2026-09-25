//! Integration tests for `hya-tool`: `list_channel`'s team section.
//!
//! The team section surfaces the caller's direct children with their live
//! status and harness-heartbeat freshness (ADR-0002 liveness), so the parent
//! can tell a busy child that is progressing from one that has stalled. The
//! plane is answered by a canned service loop — the engine-side rows are
//! covered by `hya-core`'s report tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use hya_proto::{ChannelKind, SessionId};
use hya_tool::{
    ChannelRow, InteractionPlane, LspPlane, MailboxPlane, MailboxRequest, MemberStatusRow,
    PermissionPlane, PermissionRules, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry,
    WebSearchPlane, handle::ArtifactPlane,
};
use tokio_util::sync::CancellationToken;

fn ctx_with(mailbox: MailboxPlane, session: SessionId) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![]));
    let (spawner, _srx) = SpawnerPlane::new();
    let (interaction, _irx) = InteractionPlane::new();
    ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
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
        cancel: CancellationToken::new(),
    }
}

/// Answer plane requests with canned rows for every variant the tool queries.
fn canned_service(
    channels: Vec<ChannelRow>,
    team: Vec<MemberStatusRow>,
) -> (MailboxPlane, tokio::task::JoinHandle<()>) {
    let (plane, mut rx) = MailboxPlane::new();
    let service = tokio::spawn(async move {
        while let Some(request) = rx.recv().await {
            match request {
                MailboxRequest::ListChannels { reply, .. } => {
                    let _ = reply.send(Ok(channels.clone()));
                }
                MailboxRequest::TeamStatus { reply, .. } => {
                    let _ = reply.send(Ok(team.clone()));
                }
                // Echo the requested id back so reads can assert routing.
                MailboxRequest::ReadChannel { channel, reply, .. } => {
                    let _ = reply.send(Ok((
                        channel.clone(),
                        vec![("main/scout-a".to_string(), "HELLO".to_string())],
                        0,
                        Some(format!("served `{channel}`")),
                    )));
                }
                // The tool only queries the two above; anything else drops its
                // reply, which the tool surfaces as an error.
                _ => {}
            }
        }
    });
    (plane, service)
}

#[tokio::test]
async fn list_channel_appends_the_team_section_with_heartbeat_freshness() {
    // Given: one DM channel plus a busy direct child last seen 42s ago.
    let session = SessionId::new();
    let (plane, service) = canned_service(
        vec![ChannelRow {
            id: "DM-aB12Cd34".to_string(),
            kind: ChannelKind::Dm,
            can_speak: true,
            peer: Some("main/worker-1".to_string()),
            unread: 2,
        }],
        vec![MemberStatusRow {
            handle: "main/worker-1".to_string(),
            status: "busy".to_string(),
            last_active_seconds: Some(42),
        }],
    );
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("list_channel").unwrap();

    // When
    let out = tool.execute(&ctx, serde_json::json!({})).await.unwrap();
    service.abort();

    // Then: the section renders the leaf, status, and freshness.
    let output = out["output"].as_str().unwrap();
    assert!(
        output.contains("  worker-1 · busy · last active 42s ago (heartbeat)"),
        "team section must render the leaf row: {output:?}"
    );
    assert!(
        output.contains("#DM-aB12Cd34 · dm · peer main/worker-1 · 2 unread"),
        "channel rows must still render: {output:?}"
    );
    assert_eq!(
        out["team"],
        serde_json::json!([{
            "handle": "main/worker-1",
            "status": "busy",
            "lastActiveSeconds": 42,
        }]),
        "the team array carries the full handle plus freshness"
    );
}

#[tokio::test]
async fn list_channel_omits_the_team_section_without_children() {
    // Given: channels but no direct children.
    let session = SessionId::new();
    let (plane, service) = canned_service(
        vec![ChannelRow {
            id: "announce-aB12Cd34".to_string(),
            kind: ChannelKind::Group,
            can_speak: true,
            peer: None,
            unread: 0,
        }],
        Vec::new(),
    );
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("list_channel").unwrap();

    // When
    let out = tool.execute(&ctx, serde_json::json!({})).await.unwrap();
    service.abort();

    // Then: no team section in text or JSON.
    let output = out["output"].as_str().unwrap();
    assert!(!output.contains("heartbeat"), "no team rows: {output:?}");
    assert!(
        out.get("team").is_none(),
        "the team key must be omitted entirely: {out}"
    );
}

#[tokio::test]
async fn list_channel_renders_a_child_without_any_heartbeat_yet() {
    // Given: a child that never emitted a heartbeat (never observed busy).
    let session = SessionId::new();
    let (plane, service) = canned_service(
        Vec::new(),
        vec![MemberStatusRow {
            handle: "main/worker-2".to_string(),
            status: "idle".to_string(),
            last_active_seconds: None,
        }],
    );
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("list_channel").unwrap();

    // When
    let out = tool.execute(&ctx, serde_json::json!({})).await.unwrap();
    service.abort();

    // Then: the row still renders, with an explicit absence marker.
    let output = out["output"].as_str().unwrap();
    assert!(
        output.contains("  worker-2 · idle · no heartbeat yet"),
        "absent freshness must be legible: {output:?}"
    );
    assert_eq!(out["team"][0]["lastActiveSeconds"], serde_json::json!(null));
}

/// Run 6: the lead typed `read #main/scout-fartooth` (a member handle, no
/// `channel://`). A `#…` path that names no file is served as a channel read,
/// where the engine resolves a member handle to the caller's DM with it.
#[tokio::test]
async fn a_hash_path_that_names_no_file_is_read_as_a_channel() {
    let session = SessionId::new();
    let (plane, service) = canned_service(Vec::new(), Vec::new());
    let ctx = ctx_with(plane, session);
    let tool = ToolRegistry::builtins().get("read").unwrap();

    let out = tool
        .execute(&ctx, serde_json::json!({"filePath": "#main/scout-a"}))
        .await
        .unwrap();
    let output = out["output"].as_str().unwrap();
    assert!(output.contains("served `#main/scout-a`"), "{output}");
    assert!(output.contains("[main/scout-a] HELLO"), "{output}");
    service.abort();
}
