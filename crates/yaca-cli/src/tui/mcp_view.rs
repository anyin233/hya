use yaca_mcp::{McpConnectionState, McpConnectionStatus};
use yaca_tui::{ConnectorState, ConnectorView};

#[must_use]
pub fn connector_views(statuses: &[McpConnectionStatus]) -> Vec<ConnectorView> {
    statuses
        .iter()
        .map(|status| ConnectorView {
            name: status.name.clone(),
            state: match status.state {
                McpConnectionState::Connected => ConnectorState::Connected,
                McpConnectionState::NeedsAuth => ConnectorState::NeedsAuth,
                McpConnectionState::Unavailable => ConnectorState::Failed(
                    status.error.clone().unwrap_or_else(|| "Failed".to_string()),
                ),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use yaca_mcp::{McpConnectionState, McpConnectionStatus};
    use yaca_tui::{ConnectorState, ConnectorView};

    use super::connector_views;

    #[test]
    fn connector_views_map_mcp_statuses_to_sidebar_states() {
        let statuses = [
            McpConnectionStatus {
                name: "codegraph".to_string(),
                state: McpConnectionState::Connected,
                error: None,
            },
            McpConnectionStatus {
                name: "linear-server".to_string(),
                state: McpConnectionState::NeedsAuth,
                error: None,
            },
            McpConnectionStatus {
                name: "broken".to_string(),
                state: McpConnectionState::Unavailable,
                error: Some("spawn failed".to_string()),
            },
        ];

        assert_eq!(
            connector_views(&statuses),
            vec![
                ConnectorView {
                    name: "codegraph".to_string(),
                    state: ConnectorState::Connected,
                },
                ConnectorView {
                    name: "linear-server".to_string(),
                    state: ConnectorState::NeedsAuth,
                },
                ConnectorView {
                    name: "broken".to_string(),
                    state: ConnectorState::Failed("spawn failed".to_string()),
                },
            ]
        );
    }
}
