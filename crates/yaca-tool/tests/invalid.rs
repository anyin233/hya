#![allow(clippy::unwrap_used)]

use serde_json::json;
use yaca_tool::ToolRegistry;

#[tokio::test]
async fn invalid_tool_reports_argument_error_in_open_code_shape() {
    // Given
    let tool = ToolRegistry::builtins().get("invalid").unwrap();

    // When
    let out = tool
        .execute(
            &yaca_tool::ToolCtx {
                permission: yaca_tool::PermissionPlane::new(yaca_tool::PermissionRules::default())
                    .0,
                interaction: yaca_tool::InteractionPlane::new().0,
                spawner: yaca_tool::SpawnerPlane::new().0,
                session: None,
                parent_session: None,
                todo: yaca_tool::TodoPlane::default(),
                skills: yaca_tool::SkillPlane::default(),
                websearch: yaca_tool::WebSearchPlane::default(),
                lsp: yaca_tool::LspPlane::default(),
                formatter: yaca_tool::FormatterPlane::default(),
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
