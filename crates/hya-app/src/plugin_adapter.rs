//! Adapts plugin process handles to the runtime engine's interfaces.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use hya_engine::{
    BundleApiProvider, BundleApiReply, BundleApiRequest, HostSessionReads, RuntimeSourceOwner,
    UsageScope,
};
use hya_plugin::messages::ApiRequestParams;
use hya_plugin::{PluginHost, PreparedPlugin, SessionUsageReader, SessionUsageScope};
use hya_proto::{ToolCallId, ToolSchema};
use hya_tool::{NamedTool, Tool, ToolCtx, ToolError, ToolResultPolicy};
use serde_json::Value;

mod hooks;

/// Engine hook, bundle API, and source ownership adapters over an ordered plugin host.
pub struct PluginHostAdapter {
    host: Arc<PluginHost>,
    plugins: Vec<PreparedPlugin>,
}

impl PluginHostAdapter {
    /// Retain the host and its ordered plugin handles for an engine view.
    #[must_use]
    pub fn new(host: Arc<PluginHost>) -> Self {
        let plugins = host.prepared_plugins();
        Self { host, plugins }
    }

    pub(crate) fn named_tool(self: &Arc<Self>, name: String, tool: Arc<dyn Tool>) -> Arc<dyn Tool> {
        Arc::new(PluginSourceTool {
            _owner: self.clone(),
            inner: NamedTool::new(name, tool),
        })
    }
}

#[async_trait]
impl BundleApiProvider for PluginHostAdapter {
    async fn request(&self, request: BundleApiRequest) -> Result<BundleApiReply, String> {
        let result = self
            .host
            .request_api(ApiRequestParams {
                api: request.api,
                method: request.method.as_str().to_string(),
                path: request.path,
                path_params: request.path_params,
                query: request.query,
                body: request.body,
                session: request.session,
                call: ToolCallId::new(),
                host_capability: String::new(),
            })
            .await?;
        Ok(BundleApiReply {
            status: result.status,
            body: result.body,
        })
    }
}

impl RuntimeSourceOwner for PluginHostAdapter {
    fn shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Err(error) = self.host.terminate().await {
                tracing::warn!(%error, "stop plugin connection");
            }
        })
    }
}

struct PluginSourceTool {
    // A tool can outlive its source snapshot. Keep that source discoverable for shutdown.
    _owner: Arc<PluginHostAdapter>,
    inner: NamedTool,
}

#[async_trait]
impl Tool for PluginSourceTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        self.inner.execute(ctx, input).await
    }
}

/// Give a bundle process read-only usage reporting for the session on its lease.
#[must_use]
pub fn session_usage_reader(reads: Arc<dyn HostSessionReads>) -> SessionUsageReader {
    Arc::new(move |session, scope| {
        let reads = reads.clone();
        Box::pin(async move {
            let scope = match scope {
                SessionUsageScope::Session => UsageScope::Session,
                SessionUsageScope::Tree => UsageScope::Tree,
                SessionUsageScope::Root => UsageScope::Root,
            };
            let report = reads
                .session_usage(session, scope)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_value(report).map_err(|error| error.to_string())
        })
    })
}
