//! MCP server configuration, preparation and connection status.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hya_tool::Tool;
use serde::{Deserialize, Serialize};

use crate::bridge::McpTool;
use crate::client::{ChildGuard, DEFAULT_CALL_TIMEOUT, McpClient, McpError};
use crate::protocol::ToolsListResult;
use crate::resource::{ResourceMap, load as load_resources};

/// User/config entry for one named MCP server (from `mcp:` in hya config).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Argv to spawn (`command[0]` is the program); empty is invalid at connect.
    #[serde(default)]
    pub command: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// HTTP(S) endpoint for remote servers. When set, `command` is ignored and
    /// the transport is Streamable HTTP unless `transport` says otherwise.
    #[serde(default)]
    pub url: Option<String>,
    /// Transport selection for `url` servers: `"http"` (Streamable HTTP,
    /// default), `"sse"` (classic HTTP+SSE), or `"stdio"` (default with `command`).
    #[serde(default)]
    pub transport: Option<String>,
    /// When `Some(false)`, the server is marked disabled and never spawned.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Per-call timeout in milliseconds; defaults to [`crate::client::DEFAULT_CALL_TIMEOUT`].
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// Connection state for one configured server name (HTTP/TUI status surface).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum McpStatus {
    /// Handshake not finished yet (listen-before-connect path).
    Connecting,
    /// Initialize + tools/list succeeded; tools are registered.
    Connected,
    /// Config has `enabled: false`; no process started.
    Disabled,
    /// Spawn or handshake failed.
    Failed {
        /// Error text for operators/UI.
        error: String,
    },
    /// Reserved for OAuth-style servers that need user auth (not used by stdio prepare today).
    NeedsAuth,
    /// Reserved for registration-required servers.
    NeedsClientRegistration {
        /// Error text explaining the registration failure.
        error: String,
    },
}

/// One connected MCP server: live client (stdio child or HTTP), namespaced
/// tools, and best-effort resources.
///
/// Holds the `ChildGuard` (stdio only) so the process stays up while tools are
/// in use.
pub struct PreparedMcpServer {
    name: String,
    _client: McpClient,
    _guard: Option<ChildGuard>,
    tools: Vec<Arc<dyn Tool>>,
    resources: ResourceMap,
}

impl Drop for PreparedMcpServer {
    fn drop(&mut self) {
        self._client.stop_now();
    }
}

impl PreparedMcpServer {
    /// Stop connection I/O and reap its stdio process before returning.
    pub async fn shutdown(&self) {
        if let Some(guard) = &self._guard {
            guard.shutdown().await;
        } else {
            self._client.shutdown().await;
        }
    }

    /// Config key / display name for this server.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Namespaced tools registered from this server's `tools/list`.
    #[must_use]
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.clone()
    }

    /// Best-effort `resources/list` map keyed `sanitized_server:sanitized_name`.
    #[must_use]
    pub fn resources(&self) -> ResourceMap {
        self.resources.clone()
    }
}

/// Spawn, initialize, list tools/resources, and return a ready [`PreparedMcpServer`].
///
/// `name` is the config id used in tool namespaces.
pub async fn prepare(name: String, config: McpServerConfig) -> Result<PreparedMcpServer, McpError> {
    connect_server(name, config, None).await
}

/// Prepare a packaged MCP server in its private materialized root.
pub async fn prepare_bundle(
    name: String,
    mut config: McpServerConfig,
    root: PathBuf,
) -> Result<PreparedMcpServer, McpError> {
    let mut env = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }
    if let Some(explicit) = config.env.take() {
        env.extend(explicit);
    }
    env.insert(
        "HYA_BUNDLE_ROOT".to_string(),
        root.to_string_lossy().into_owned(),
    );
    config.env = Some(env);
    connect_server(name, config, Some(root)).await
}

async fn connect_server(
    name: String,
    config: McpServerConfig,
    bundle_root: Option<PathBuf>,
) -> Result<PreparedMcpServer, McpError> {
    let timeout = config
        .timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CALL_TIMEOUT);
    let (client, guard) = match config.url.clone() {
        Some(url) => {
            let client = match config.transport.as_deref() {
                Some("sse") => McpClient::connect_classic_sse(&url).await?,
                Some("http") | Some("streamable_http") | None => {
                    McpClient::connect_streamable_http(url)?
                }
                Some(other) => {
                    return Err(McpError::Io(format!(
                        "unknown mcp transport: {other} (expected http, sse, or stdio)"
                    )));
                }
            };
            (client, None)
        }
        None => {
            let (client, guard) = match bundle_root.as_deref() {
                Some(root) => McpClient::spawn_bundle(&config.command, root, config.env.as_ref())?,
                None => McpClient::spawn(&config.command, config.env.as_ref())?,
            };
            (client, Some(guard))
        }
    };
    prepare_connected(name, client, guard, timeout).await
}

/// Discover MCP exports on a client-owned, already connected stdio stream.
pub async fn prepare_peer(name: String, client: McpClient) -> Result<PreparedMcpServer, McpError> {
    prepare_connected(name, client, None, DEFAULT_CALL_TIMEOUT).await
}

async fn prepare_connected(
    name: String,
    client: McpClient,
    guard: Option<ChildGuard>,
    timeout: Duration,
) -> Result<PreparedMcpServer, McpError> {
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
    Ok(PreparedMcpServer {
        name,
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
            nested: hya_tool::NestedToolPlane::disconnected(),
            permission,
            interaction,
            spawner,
            workflows: hya_tool::WorkflowPlane::disconnected(),
            operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
            mailbox: hya_tool::MailboxPlane::disconnected(),
            lifecycle: hya_tool::LifecyclePlane::disconnected(),
            project_activity: hya_tool::ProjectActivityPlane::disconnected(),
            session: None,
            parent_session: None,
            skills: hya_tool::SkillPlane::default(),
            artifacts: hya_tool::handle::ArtifactPlane::default(),
            websearch: hya_tool::WebSearchPlane::default(),
            lsp: hya_tool::LspPlane::default(),
            formatter: hya_tool::FormatterPlane::default(),
            agents: Default::default(),
            workdir: std::path::PathBuf::from("."),
            roots: vec![std::path::PathBuf::from(".")],
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn shutdown_closes_servers_even_with_retained_clients() {
        let server = prepare("owned".into(), server_config(server_command()))
            .await
            .unwrap();
        let client = server._client.clone();
        tokio::time::timeout(std::time::Duration::from_secs(5), server.shutdown())
            .await
            .unwrap();
        assert!(matches!(
            client
                .call("tools/list", json!({}), Duration::from_secs(1))
                .await,
            Err(McpError::Closed)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), server.shutdown())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn bundle_server_uses_private_cwd_and_minimal_environment() {
        let root =
            std::env::temp_dir().join(format!("hya-mcp-bundle-{}", hya_proto::SessionId::new()));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("sibling.txt"), "PACKAGED_SIBLING").unwrap();
        std::fs::write(
            root.join("server.py"),
            r#"
import json, os, sys
description = "|".join([
    open("sibling.txt").read(),
    "home=" + str("HOME" in os.environ),
    "explicit=" + os.environ.get("BUNDLE_EXPLICIT", ""),
    "rootmatch=" + str(os.path.samefile(os.getcwd(), os.environ.get("HYA_BUNDLE_ROOT"))),
])
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name": "probe", "description": description, "inputSchema": {"type": "object"}}]}
    else:
        result = {"resources": []}
    print(json.dumps({"jsonrpc":"2.0", "id":req["id"], "result":result}), flush=True)
"#,
        )
        .unwrap();
        let config = McpServerConfig {
            command: vec!["python3".to_string(), "server.py".to_string()],
            env: Some(BTreeMap::from([(
                "BUNDLE_EXPLICIT".to_string(),
                "kept".to_string(),
            )])),
            timeout_ms: Some(1000),
            ..McpServerConfig::default()
        };

        let server = prepare_bundle("fixture".to_string(), config, root.clone())
            .await
            .unwrap();
        let description = &server.tools()[0].schema().description;
        assert_eq!(
            description,
            "PACKAGED_SIBLING|home=False|explicit=kept|rootmatch=True"
        );
        drop(server);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn prepare_exposes_callable_namespaced_tool() {
        let server = prepare("good".into(), server_config(server_command()))
            .await
            .unwrap();
        let tool = server.tools().into_iter().next().unwrap();
        assert_eq!(tool.name(), "mcp__good__ping");
        let out = tool
            .execute(&ctx_allowing_mcp("mcp__good__ping"), json!({}))
            .await
            .unwrap();
        assert!(!out.is_null());
    }

    #[tokio::test]
    async fn prepare_returns_initialize_error() {
        let result = prepare(
            "bad".into(),
            server_config(server_command_initialize_error()),
        )
        .await;
        assert!(matches!(result, Err(error) if error.to_string().contains("init failed")));
    }

    #[tokio::test]
    async fn prepare_returns_tools_list_error() {
        let result = prepare(
            "bad".into(),
            server_config(server_command_tools_list_error()),
        )
        .await;
        assert!(matches!(result, Err(error) if error.to_string().contains("tools failed")));
    }

    #[tokio::test]
    async fn prepare_keeps_tools_when_resources_list_errors() {
        let server = prepare(
            "good".into(),
            server_config(server_command_resources_error()),
        )
        .await
        .unwrap();
        assert_eq!(server.tools()[0].name(), "mcp__good__ping");
        assert!(server.resources().is_empty());
    }
}
