//! Managed [`NativeService`] and [`NativeHook`] for `hya/network`.

use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{
    HookPoint, HostError, HostRuntime, NativeHook, NativeService, ServiceHealth,
    ToolVisibilityInput, ToolVisibilityOutcome,
};
use serde_json::json;

use crate::config::NetworkPlane;

/// Lifecycle service owning `hya/network`'s per-bundle `config.yml` state.
pub struct NetworkService {
    plane: Arc<NetworkPlane>,
}

impl NetworkService {
    /// Create a service backed by `plane`.
    #[must_use]
    pub fn new(plane: Arc<NetworkPlane>) -> Self {
        Self { plane }
    }
}

#[async_trait]
impl NativeService for NetworkService {
    fn id(&self) -> &str {
        "network"
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        self.plane.reload_from_host(runtime.as_ref()).await?;
        self.plane.set_running(true);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.plane.set_running(false);
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        let cfg = self.plane.config();
        let details = json!({
            "webfetch": {
                "enabled": cfg.webfetch.enabled,
            },
            "websearch": {
                "enabled": cfg.websearch.enabled,
                "provider": cfg.websearch.provider.id(),
                "has_endpoint_override": cfg.websearch.endpoint.is_some(),
                "has_key": cfg.websearch.key.as_deref().is_some_and(|k| !k.is_empty()),
            },
        });
        if self.plane.is_running() {
            ServiceHealth {
                healthy: true,
                status: "ready".to_string(),
                details: Some(details),
            }
        } else {
            ServiceHealth {
                healthy: false,
                status: "stopped".to_string(),
                details: Some(details),
            }
        }
    }
}

/// Visibility hook hiding `webfetch` and/or `websearch` when disabled in
/// `hya/network`'s `config.yml`.
pub struct NetworkVisibilityHook {
    plane: Arc<NetworkPlane>,
    runtime: Option<Arc<dyn HostRuntime>>,
}

impl NetworkVisibilityHook {
    /// Create a visibility hook backed by `plane` and optional `runtime`.
    #[must_use]
    pub fn new(plane: Arc<NetworkPlane>, runtime: Option<Arc<dyn HostRuntime>>) -> Self {
        Self { plane, runtime }
    }
}

#[async_trait]
impl NativeHook for NetworkVisibilityHook {
    fn id(&self) -> &str {
        "network-visibility"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::ToolVisibility]
    }

    async fn tool_visibility(
        &self,
        _input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        let cfg = self.plane.effective_config(self.runtime.as_deref()).await;
        let mut hidden = Vec::new();
        if !cfg.webfetch.enabled {
            hidden.push("webfetch".to_string());
        }
        if !cfg.websearch.enabled {
            hidden.push("websearch".to_string());
        }
        if hidden.is_empty() {
            Ok(ToolVisibilityOutcome::Preserve)
        } else {
            Ok(ToolVisibilityOutcome::Hide { tools: hidden })
        }
    }
}
