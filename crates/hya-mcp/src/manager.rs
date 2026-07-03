use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use hya_tool::Tool;
use serde::{Deserialize, Serialize};

use crate::bridge::McpTool;
use crate::client::{ChildGuard, DEFAULT_CALL_TIMEOUT, McpClient, McpError};
use crate::protocol::ToolsListResult;
use crate::resource::{ResourceMap, load as load_resources};

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpStatus {
    Connected,
    Disabled,
    Failed { error: String },
    NeedsAuth,
    NeedsClientRegistration { error: String },
}

#[derive(Default)]
pub struct McpManager {
    servers: Vec<McpServer>,
    status: BTreeMap<String, McpStatus>,
}

struct McpServer {
    _name: String,
    _client: McpClient,
    _guard: ChildGuard,
    tools: Vec<Arc<dyn Tool>>,
    resources: ResourceMap,
}

impl McpManager {
    pub async fn connect_all(configs: BTreeMap<String, McpServerConfig>) -> Self {
        let mut set = tokio::task::JoinSet::new();
        let mut status = BTreeMap::new();
        for (name, config) in configs {
            if config.enabled == Some(false) {
                status.insert(name, McpStatus::Disabled);
                continue;
            }
            let status_name = name.clone();
            set.spawn(async move { (status_name, connect_server(name, config).await) });
        }
        let mut servers = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((name, Ok(server))) => {
                    status.insert(name, McpStatus::Connected);
                    servers.push(server);
                }
                Ok((name, Err(error))) => {
                    tracing::warn!(%error, "mcp server unavailable");
                    status.insert(
                        name,
                        McpStatus::Failed {
                            error: error.to_string(),
                        },
                    );
                }
                Err(error) => tracing::warn!(%error, "mcp server task failed"),
            }
        }
        Self { servers, status }
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.servers
            .iter()
            .flat_map(|server| server.tools.iter().cloned())
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> BTreeMap<String, McpStatus> {
        self.status.clone()
    }

    #[must_use]
    pub fn resources(&self) -> ResourceMap {
        self.servers
            .iter()
            .flat_map(|server| server.resources.clone())
            .collect()
    }
}

async fn connect_server(name: String, config: McpServerConfig) -> Result<McpServer, McpError> {
    let timeout = config
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_TIMEOUT);
    let (client, guard) = McpClient::spawn(&config.command, config.env.as_ref())?;
    client.initialize().await?;
    // Spec-required post-initialize handshake before issuing further requests.
    client
        .notify("notifications/initialized", serde_json::json!({}))
        .await?;
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
    let resources = load_resources(&client, &name, timeout).await;
    Ok(McpServer {
        _name: name,
        _client: client,
        _guard: guard,
        tools,
        resources,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    fn server_command() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
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
    fn server_config(command: Vec<String>) -> McpServerConfig {
        McpServerConfig {
            command,
            timeout_ms: Some(1000),
            ..McpServerConfig::default()
        }
    }

    fn server_command_initialize_error() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        response = {"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32000, "message": "init failed"}}
    else:
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {}}
    print(json.dumps(response), flush=True)
"#
            .to_string(),
        ]
    }

    fn server_command_tools_list_error() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {"capabilities": {}}}
    elif req["method"] == "tools/list":
        response = {"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32000, "message": "tools failed"}}
    else:
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {"content": {"ok": True}, "isError": False}}
    print(json.dumps(response), flush=True)
"#
            .to_string(),
        ]
    }

    fn server_command_resources_list_error() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {"capabilities": {}}}
    elif req["method"] == "tools/list":
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {"tools": [{"name": "ping", "description": "Ping", "inputSchema": {"type": "object"}}]}}
    elif req["method"] == "resources/list":
        response = {"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32000, "message": "resources failed"}}
    else:
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {"content": {"pong": True}, "isError": False}}
    print(json.dumps(response), flush=True)
"#
            .to_string(),
        ]
    }

    fn ctx_allowing_mcp(resource: &str) -> hya_tool::ToolCtx {
        let (permission, _rx) =
            hya_tool::PermissionPlane::new(hya_tool::PermissionRules::new(vec![
                hya_tool::Rule::new(hya_tool::Action::Mcp, resource, hya_tool::Mode::Allow),
            ]));
        let (interaction, _irx) = hya_tool::InteractionPlane::new();
        let (spawner, _srx) = hya_tool::SpawnerPlane::new();
        hya_tool::ToolCtx {
            permission,
            interaction,
            spawner,
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
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn one_failed_server_does_not_abort_others() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "bad".to_string(),
            McpServerConfig {
                command: vec!["definitely-not-hya-mcp".to_string()],
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
    async fn reports_connected_disabled_and_failed_status() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "bad".to_string(),
            McpServerConfig {
                command: vec!["definitely-not-hya-mcp".to_string()],
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "disabled".to_string(),
            McpServerConfig {
                enabled: Some(false),
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
        let status = serde_json::to_value(manager.status()).unwrap();

        assert_eq!(status["good"], serde_json::json!({"status": "connected"}));
        assert_eq!(
            status["disabled"],
            serde_json::json!({"status": "disabled"})
        );
        assert_eq!(status["bad"]["status"], "failed");
        assert!(
            status["bad"]["error"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
    }

    #[tokio::test]
    async fn collects_connected_server_resources() {
        let mut command = server_command();
        command[2] = r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {"resources": {}}}
    elif req["method"] == "tools/list":
        result = {"tools": []}
    elif req["method"] == "resources/list":
        result = {"resources": [{"name": "Project Notes", "uri": "file:///notes.md", "description": "Notes", "mimeType": "text/markdown"}]}
    else:
        result = {"content": {"ok": True}, "isError": False}
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "result": result}), flush=True)
"#
        .to_string();
        let mut configs = BTreeMap::new();
        configs.insert(
            "docs/server".to_string(),
            McpServerConfig {
                command,
                timeout_ms: Some(1000),
                ..McpServerConfig::default()
            },
        );

        let manager = McpManager::connect_all(configs).await;
        let resources = manager.resources();

        assert_eq!(
            resources["docs_server:Project_Notes"],
            serde_json::json!({
                "name": "Project Notes",
                "uri": "file:///notes.md",
                "description": "Notes",
                "mimeType": "text/markdown",
                "client": "docs/server"
            })
        );
    }
    #[tokio::test]
    async fn connect_all_exposes_callable_namespaced_tool_after_successful_initialize() {
        let mut configs = BTreeMap::new();
        configs.insert("good".to_string(), server_config(server_command()));

        let manager = McpManager::connect_all(configs).await;
        let tool = manager
            .tools()
            .into_iter()
            .find(|tool| tool.name() == "mcp__good__ping")
            .unwrap();

        let out = tool
            .execute(&ctx_allowing_mcp("mcp__good__ping"), json!({}))
            .await
            .unwrap();

        assert_eq!(out["content"], json!([{"ok": true}]));
        assert!(
            out["output"]
                .as_str()
                .is_some_and(|text| text.contains("ok"))
        );
    }

    #[tokio::test]
    async fn connect_all_marks_server_failed_when_initialize_rpc_errors() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "bad".to_string(),
            server_config(server_command_initialize_error()),
        );
        configs.insert("good".to_string(), server_config(server_command()));

        let manager = McpManager::connect_all(configs).await;
        let status = manager.status();

        assert!(matches!(
            status.get("bad"),
            Some(McpStatus::Failed { error }) if !error.is_empty()
        ));
        assert_eq!(status.get("good"), Some(&McpStatus::Connected));
        assert!(
            manager
                .tools()
                .into_iter()
                .any(|tool| tool.name() == "mcp__good__ping")
        );
    }

    #[tokio::test]
    async fn connect_all_marks_server_failed_when_tools_list_rpc_errors_but_keeps_others_connected()
    {
        let mut configs = BTreeMap::new();
        configs.insert(
            "bad".to_string(),
            server_config(server_command_tools_list_error()),
        );
        configs.insert("good".to_string(), server_config(server_command()));

        let manager = McpManager::connect_all(configs).await;
        let status = manager.status();

        assert!(matches!(
            status.get("bad"),
            Some(McpStatus::Failed { error }) if error.contains("tools failed")
        ));
        assert_eq!(status.get("good"), Some(&McpStatus::Connected));
        assert!(
            manager
                .tools()
                .into_iter()
                .any(|tool| tool.name() == "mcp__good__ping")
        );
    }

    #[tokio::test]
    async fn connect_all_keeps_connected_status_when_resources_list_errors() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "good".to_string(),
            server_config(server_command_resources_list_error()),
        );

        let manager = McpManager::connect_all(configs).await;
        let status = manager.status();

        assert_eq!(status.get("good"), Some(&McpStatus::Connected));
        assert!(
            manager
                .tools()
                .into_iter()
                .any(|tool| tool.name() == "mcp__good__ping")
        );
        assert!(manager.resources().is_empty());
    }
}
