#![allow(clippy::unwrap_used)]

use hya_tool::ToolRegistry;
use serde_json::json;

#[tokio::test]
async fn invalid_tool_reports_argument_error_in_open_code_shape() {
    // Given
    let tool = ToolRegistry::builtins().get("invalid").unwrap();

    // When
    let out = tool
        .execute(
            &hya_tool::ToolCtx {
                permission: hya_tool::PermissionPlane::new(hya_tool::PermissionRules::default()).0,
                interaction: hya_tool::InteractionPlane::new().0,
                spawner: hya_tool::SpawnerPlane::new().0,
                mailbox: hya_tool::MailboxPlane::disconnected(),
                session: None,
                parent_session: None,
                todo: hya_tool::TodoPlane::default(),
                skills: hya_tool::SkillPlane::default(),
                websearch: hya_tool::WebSearchPlane::default(),
                lsp: hya_tool::LspPlane::default(),
                formatter: hya_tool::FormatterPlane::default(),
                agents: hya_tool::AgentCatalogPlane::default(),
                workdir: std::path::PathBuf::from("."),
                cancel: tokio_util::sync::CancellationToken::new(),
            },
            json!({ "tool": "read", "error": "missing path" }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "Invalid Tool");
    assert_eq!(
        out["output"],
        "The arguments provided to the tool are invalid: missing path"
    );
    assert_eq!(out["metadata"], json!({}));
}
