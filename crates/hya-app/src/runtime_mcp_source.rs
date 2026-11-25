use std::collections::BTreeMap;
use std::sync::Arc;

use hya_engine::RuntimeSourceOwner;
use hya_mcp::{McpServerConfig, PreparedMcpServer};
use hya_tool::{Tool, ToolPermission};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::runtime_reconcile::{
    PreparedExport, PreparedFailure, PreparedResult, PreparedSource, SourceId, update_bytes,
};

pub(crate) fn normalize_mcp(mut spec: McpServerConfig) -> McpServerConfig {
    spec.enabled = Some(spec.enabled != Some(false));
    spec.env = Some(spec.env.unwrap_or_default());
    spec.timeout_ms = Some(
        spec.timeout_ms
            .unwrap_or(hya_mcp::client::DEFAULT_CALL_TIMEOUT.as_millis() as u64),
    );
    spec.transport = Some(match (spec.url.is_some(), spec.transport.as_deref()) {
        (false, _) => "stdio".to_string(),
        (true, None | Some("http" | "streamable_http")) => "http".to_string(),
        (true, Some(value)) => value.to_string(),
    });
    spec
}

pub(crate) async fn prepare(id: SourceId, mut config: McpServerConfig) -> PreparedResult {
    for value in config.env.iter_mut().flat_map(|env| env.values_mut()) {
        match crate::config::resolve_secret(value) {
            Ok(resolved) => *value = resolved,
            Err(error) => {
                return PreparedFailure::new(id, format!("MCP_ENV_FAILED: {error:#}")).into();
            }
        }
    }
    match hya_mcp::prepare(id.configured_id().to_string(), config).await {
        Ok(server) => PreparedResult::Ready(prepared_mcp_source(id, server)),
        Err(error) => PreparedResult::Failed(PreparedFailure::new(
            id,
            format!("MCP_START_FAILED: {error}"),
        )),
    }
}

struct McpSourceOwner(PreparedMcpServer);
impl RuntimeSourceOwner for McpSourceOwner {
    fn shutdown(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(self.0.shutdown())
    }
}

fn prepared_mcp_source(id: SourceId, server: PreparedMcpServer) -> PreparedSource {
    let tools = server.tools();
    let resources = server.resources();
    let owner = Arc::new(McpSourceOwner(server));
    let declaration_digest = digest_mcp_declaration(&tools, &resources);
    let exports = tools
        .into_iter()
        .map(|tool| {
            let declared_id = tool.name().to_string();
            PreparedExport::tool(declared_id, Vec::new(), tool, ToolPermission::Mcp)
        })
        .collect();
    PreparedSource::new(id, declaration_digest, owner, exports).with_resources(resources)
}

fn digest_mcp_declaration(
    tools: &[Arc<dyn Tool>],
    resources: &BTreeMap<String, Value>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya:mcp-declaration:v1\0");
    for tool in tools {
        if let Ok(schema) = serde_json::to_vec(&tool.schema()) {
            update_bytes(&mut digest, &schema);
        }
    }
    if let Ok(encoded) = serde_json::to_vec(resources) {
        update_bytes(&mut digest, &encoded);
    }
    digest.finalize().into()
}
