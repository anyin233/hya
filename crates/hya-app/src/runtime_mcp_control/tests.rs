use std::sync::Arc;

use hya_engine::RuntimeRegistry;
use hya_mcp::McpServerConfig;

use super::RuntimeMcpControl;
use crate::runtime_reconcile::RuntimeReconciler;
use crate::test_env::StableEnvGuard;

#[tokio::test]
async fn mcp_add_keeps_failed_servers_and_never_connects_disabled() -> anyhow::Result<()> {
    let _env = StableEnvGuard::new();
    use hya_mcp::McpStatus;
    use hya_server::McpControl as _;

    let registry = Arc::new(RuntimeRegistry::new(
        hya_tool_loader::builtins(),
        crate::runtime::builtin_agent_catalog()?,
    ));
    let control = RuntimeMcpControl::new(Arc::new(RuntimeReconciler::new(registry)));
    let broken = McpServerConfig {
        command: vec!["/nonexistent/hya-mcp-missing-binary".to_string()],
        ..McpServerConfig::default()
    };
    control
        .upsert("broken".to_string(), broken.clone())
        .await
        .map_err(anyhow::Error::msg)?;
    let status = control.status().await;
    assert!(
        matches!(status.get("broken"), Some(McpStatus::Failed { error }) if !error.is_empty()),
        "{status:?}"
    );

    control
        .upsert(
            "off".to_string(),
            McpServerConfig {
                enabled: Some(false),
                ..broken
            },
        )
        .await
        .map_err(anyhow::Error::msg)?;
    let status = control.status().await;
    assert_eq!(status.get("off"), Some(&McpStatus::Disabled), "{status:?}");

    // Enabling the disabled server tries to connect; its failure is a
    // FAILED status, not an error.
    assert!(
        control
            .set_enabled("off".to_string(), true)
            .await
            .map_err(anyhow::Error::msg)?
    );
    let status = control.status().await;
    assert!(
        matches!(status.get("off"), Some(McpStatus::Failed { .. })),
        "{status:?}"
    );
    Ok(())
}
