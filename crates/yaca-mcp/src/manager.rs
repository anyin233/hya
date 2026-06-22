use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use yaca_tool::Tool;

use crate::bridge::McpTool;
use crate::client::{ChildGuard, DEFAULT_CALL_TIMEOUT, McpClient, McpError};
use crate::protocol::ToolsListResult;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub struct McpManager {
    servers: Vec<McpServer>,
    statuses: Vec<McpConnectionStatus>,
}

struct McpServer {
    name: String,
    _client: McpClient,
    _guard: ChildGuard,
    tools: Vec<Arc<dyn Tool>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConnectionState {
    Connected,
    NeedsAuth,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpConnectionStatus {
    pub name: String,
    pub state: McpConnectionState,
}

enum ConnectOutcome {
    Connected(McpServer),
    Failed {
        name: String,
        state: McpConnectionState,
        error: McpError,
    },
}

impl McpManager {
    pub async fn connect_all(configs: BTreeMap<String, McpServerConfig>) -> Self {
        let mut set = tokio::task::JoinSet::new();
        for (name, config) in configs {
            if config.enabled == Some(false) {
                continue;
            }
            set.spawn(async move {
                match connect_server(name.clone(), config).await {
                    Ok(server) => ConnectOutcome::Connected(server),
                    Err(error) => ConnectOutcome::Failed {
                        name,
                        state: connection_state_for_error(&error),
                        error,
                    },
                }
            });
        }
        let mut servers = Vec::new();
        let mut statuses = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(ConnectOutcome::Connected(server)) => {
                    statuses.push(McpConnectionStatus {
                        name: server.name.clone(),
                        state: McpConnectionState::Connected,
                    });
                    servers.push(server);
                }
                Ok(ConnectOutcome::Failed { name, state, error }) => {
                    statuses.push(McpConnectionStatus { name, state });
                    tracing::warn!(%error, "mcp server unavailable");
                }
                Err(error) => tracing::warn!(%error, "mcp server task failed"),
            }
        }
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        statuses.sort_by(|a, b| a.name.cmp(&b.name));
        Self { servers, statuses }
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.servers
            .iter()
            .flat_map(|server| server.tools.iter().cloned())
            .collect()
    }

    #[must_use]
    pub fn statuses(&self) -> &[McpConnectionStatus] {
        &self.statuses
    }
}

async fn connect_server(name: String, config: McpServerConfig) -> Result<McpServer, McpError> {
    let timeout = config
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_TIMEOUT);
    let (client, guard) = McpClient::spawn(&config.command, config.env.as_ref())?;
    client.initialize().await?;
    let value = client
        .call("tools/list", serde_json::json!({}), timeout)
        .await?;
    let listed: ToolsListResult =
        serde_json::from_value(value).map_err(|e| McpError::Json(e.to_string()))?;
    let tools = listed
        .tools
        .into_iter()
        .filter_map(|tool| McpTool::try_new(&name, tool, client.clone(), timeout))
        .collect();
    Ok(McpServer {
        name,
        _client: client,
        _guard: guard,
        tools,
    })
}

const fn connection_state_for_error(error: &McpError) -> McpConnectionState {
    match error {
        McpError::Rpc { .. } => McpConnectionState::NeedsAuth,
        McpError::EmptyCommand
        | McpError::MissingPipe(_)
        | McpError::Io(_)
        | McpError::Json(_)
        | McpError::Timeout { .. }
        | McpError::Closed
        | McpError::OversizedLine => McpConnectionState::Unavailable,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn server_command() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name": "ping", "description": "Ping", "inputSchema": {"type": "object"}}]}
    else:
        result = {"content": {"ok": True}, "isError": False}
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "result": result}), flush=True)
"#
            .to_string(),
        ]
    }

    fn auth_error_command() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32001, "message": "unauthorized"}}), flush=True)
"#
            .to_string(),
        ]
    }

    #[tokio::test]
    async fn one_failed_server_does_not_abort_others() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "bad".to_string(),
            McpServerConfig {
                command: vec!["definitely-not-yaca-mcp".to_string()],
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "good".to_string(),
            McpServerConfig {
                command: server_command(),
                timeout_ms: Some(1000),
                ..McpServerConfig::default()
            },
        );

        let manager = McpManager::connect_all(configs).await;
        let names: Vec<String> = manager
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_string())
            .collect();
        assert_eq!(names, vec!["mcp__good__ping".to_string()]);
    }

    #[tokio::test]
    async fn connection_statuses_include_connected_needs_auth_and_unavailable_servers() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "auth".to_string(),
            McpServerConfig {
                command: auth_error_command(),
                timeout_ms: Some(1000),
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "missing".to_string(),
            McpServerConfig {
                command: vec!["definitely-not-yaca-mcp".to_string()],
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "ok".to_string(),
            McpServerConfig {
                command: server_command(),
                timeout_ms: Some(1000),
                ..McpServerConfig::default()
            },
        );

        let manager = McpManager::connect_all(configs).await;
        let statuses = manager.statuses();

        assert_eq!(
            statuses,
            &[
                McpConnectionStatus {
                    name: "auth".to_string(),
                    state: McpConnectionState::NeedsAuth,
                },
                McpConnectionStatus {
                    name: "missing".to_string(),
                    state: McpConnectionState::Unavailable,
                },
                McpConnectionStatus {
                    name: "ok".to_string(),
                    state: McpConnectionState::Connected,
                },
            ]
        );
    }
}
