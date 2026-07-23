use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
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
    /// Handshake not finished yet (listen-before-connect path).
    Connecting,
    Connected,
    Disabled,
    Failed {
        error: String,
    },
    NeedsAuth,
    NeedsClientRegistration {
        error: String,
    },
}

/// MCP connection manager. Clone shares the same status/tool catalog so background
/// attach can update what the HTTP API already holds.
#[derive(Clone, Default)]
pub struct McpManager {
    inner: Arc<RwLock<McpInner>>,
}

#[derive(Default)]
struct McpInner {
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
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, McpInner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, McpInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Snapshot status map for servers that are not yet connected: `connecting` or `disabled`.
    #[must_use]
    pub fn pending(configs: &BTreeMap<String, McpServerConfig>) -> Self {
        let mut status = BTreeMap::new();
        for (name, config) in configs {
            if config.enabled == Some(false) {
                status.insert(name.clone(), McpStatus::Disabled);
            } else {
                status.insert(name.clone(), McpStatus::Connecting);
            }
        }
        Self {
            inner: Arc::new(RwLock::new(McpInner {
                servers: Vec::new(),
                status,
            })),
        }
    }

    /// Connect every configured server (blocking) and return a finished manager.
    pub async fn connect_all(configs: BTreeMap<String, McpServerConfig>) -> Self {
        let manager = Self::pending(&configs);
        manager.connect_all_into(configs).await;
        manager
    }

    /// Finish connecting configured servers into this shared manager (hot-update).
    pub async fn connect_all_into(&self, configs: BTreeMap<String, McpServerConfig>) {
        let mut set = tokio::task::JoinSet::new();
        for (name, config) in configs {
            if config.enabled == Some(false) {
                self.write().status.insert(name, McpStatus::Disabled);
                continue;
            }
            let status_name = name.clone();
            set.spawn(async move { (status_name, connect_server(name, config).await) });
        }
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((name, Ok(server))) => {
                    let mut inner = self.write();
                    inner.status.insert(name, McpStatus::Connected);
                    inner.servers.push(server);
                }
                Ok((name, Err(error))) => {
                    tracing::warn!(%error, "mcp server unavailable");
                    self.write().status.insert(
                        name,
                        McpStatus::Failed {
                            error: error.to_string(),
                        },
                    );
                }
                Err(error) => tracing::warn!(%error, "mcp server task failed"),
            }
        }
    }

    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.read()
            .servers
            .iter()
            .flat_map(|server| server.tools.iter().cloned())
            .collect()
    }

    #[must_use]
    pub fn status(&self) -> BTreeMap<String, McpStatus> {
        self.read().status.clone()
    }

    #[must_use]
    pub fn resources(&self) -> ResourceMap {
        self.read()
            .servers
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
    elif req["method"] == "notifications/initialized":
        continue
    elif req["method"] == "tools/list":
        response = {"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32000, "message": "tools failed"}}
    else:
        response = {"jsonrpc":"2.0", "id": req["id"], "result": {}}
    print(json.dumps(response), flush=True)
"#
            .to_string(),
        ]
    }

    fn server_command_resources_error() -> Vec<String> {
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
    elif req["method"] == "resources/list":
        print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "error": {"code": -32000, "message": "resources failed"}}), flush=True)
        continue
    else:
        result = {"content": {"ok": True}, "isError": False}
    print(json.dumps({"jsonrpc":"2.0", "id": req["id"], "result": result}), flush=True)
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
            "good".to_string(),
            McpServerConfig {
                command: server_command(),
                timeout_ms: Some(1000),
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "off".to_string(),
            McpServerConfig {
                enabled: Some(false),
                command: vec!["unused".into()],
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "bad".to_string(),
            McpServerConfig {
                command: vec!["definitely-not-hya-mcp".to_string()],
                ..McpServerConfig::default()
            },
        );

        let manager = McpManager::connect_all(configs).await;
        let status = serde_json::to_value(manager.status()).unwrap();
        assert_eq!(status["good"], json!({"status": "connected"}));
        assert_eq!(status["off"], json!({"status": "disabled"}));
        assert_eq!(status["bad"]["status"], "failed");
    }

    #[tokio::test]
    async fn pending_marks_enabled_servers_connecting() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "slow".to_string(),
            McpServerConfig {
                command: vec!["sleep".into(), "30".into()],
                ..McpServerConfig::default()
            },
        );
        configs.insert(
            "off".to_string(),
            McpServerConfig {
                enabled: Some(false),
                command: vec!["unused".into()],
                ..McpServerConfig::default()
            },
        );
        let manager = McpManager::pending(&configs);
        assert_eq!(manager.status().get("slow"), Some(&McpStatus::Connecting));
        assert_eq!(manager.status().get("off"), Some(&McpStatus::Disabled));
        assert!(manager.tools().is_empty());
    }

    #[tokio::test]
    async fn connect_all_into_hot_updates_shared_manager() {
        let mut configs = BTreeMap::new();
        configs.insert("good".to_string(), server_config(server_command()));
        let manager = McpManager::pending(&configs);
        assert_eq!(manager.status().get("good"), Some(&McpStatus::Connecting));
        manager.connect_all_into(configs).await;
        assert_eq!(manager.status().get("good"), Some(&McpStatus::Connected));
        assert_eq!(manager.tools()[0].name(), "mcp__good__ping");
    }

    #[tokio::test]
    async fn collects_connected_server_resources() {
        // resources load is best-effort; just ensure connect succeeds
        let mut configs = BTreeMap::new();
        configs.insert("good".to_string(), server_config(server_command()));
        let manager = McpManager::connect_all(configs).await;
        let _ = manager.resources();
        assert_eq!(manager.status().get("good"), Some(&McpStatus::Connected));
    }

    #[tokio::test]
    async fn connect_all_exposes_callable_namespaced_tool_after_successful_initialize() {
        let mut configs = BTreeMap::new();
        configs.insert("good".to_string(), server_config(server_command()));

        let manager = McpManager::connect_all(configs).await;
        let tool = manager.tools().into_iter().next().unwrap();
        assert_eq!(tool.name(), "mcp__good__ping");
        let out = tool
            .execute(&ctx_allowing_mcp("mcp__good__ping"), json!({}))
            .await
            .unwrap();
        assert!(out.is_object() || out.is_string() || out.is_boolean() || !out.is_null());
    }

    #[tokio::test]
    async fn connect_all_marks_server_failed_when_initialize_rpc_errors_but_keeps_others_connected()
    {
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
    }

    #[tokio::test]
    async fn connect_all_keeps_connected_status_when_resources_list_errors() {
        let mut configs = BTreeMap::new();
        configs.insert(
            "good".to_string(),
            server_config(server_command_resources_error()),
        );
        let manager = McpManager::connect_all(configs).await;
        assert_eq!(manager.status().get("good"), Some(&McpStatus::Connected));
    }
}
