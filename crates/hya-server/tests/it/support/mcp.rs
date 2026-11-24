use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hya_mcp::{McpManager, McpServerConfig, McpStatus};
use hya_server::McpControl;
use serde_json::Value;
use tokio::sync::RwLock;

type ControlFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Default)]
pub struct TestMcpControl {
    configs: RwLock<BTreeMap<String, McpServerConfig>>,
    managers: RwLock<BTreeMap<String, Arc<McpManager>>>,
    status: RwLock<BTreeMap<String, McpStatus>>,
}

impl TestMcpControl {
    pub async fn new(configs: BTreeMap<String, McpServerConfig>) -> Arc<Self> {
        let control = Arc::new(Self::default());
        for (name, config) in configs {
            assert!(control.upsert(name, config).await.is_ok());
        }
        control
    }

    async fn apply(&self, name: String, config: McpServerConfig) {
        self.configs
            .write()
            .await
            .insert(name.clone(), config.clone());
        if config.enabled == Some(false) {
            self.managers.write().await.remove(&name);
            self.status.write().await.insert(name, McpStatus::Disabled);
            return;
        }
        let manager =
            Arc::new(McpManager::connect_all(BTreeMap::from([(name.clone(), config)])).await);
        let status = manager
            .status()
            .get(&name)
            .cloned()
            .unwrap_or_else(|| McpStatus::Failed {
                error: "test MCP manager returned no status".to_string(),
            });
        self.managers.write().await.insert(name.clone(), manager);
        self.status.write().await.insert(name, status);
    }
}

impl McpControl for TestMcpControl {
    fn status(&self) -> ControlFuture<'_, BTreeMap<String, McpStatus>> {
        Box::pin(async move { self.status.read().await.clone() })
    }

    fn upsert(
        &self,
        name: String,
        config: McpServerConfig,
    ) -> ControlFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.apply(name, config).await;
            Ok(())
        })
    }

    fn set_enabled(&self, name: String, enabled: bool) -> ControlFuture<'_, Result<bool, String>> {
        Box::pin(async move {
            let Some(mut config) = self.configs.read().await.get(&name).cloned() else {
                return Ok(false);
            };
            config.enabled = Some(enabled);
            self.apply(name, config).await;
            Ok(true)
        })
    }

    fn resources(&self) -> ControlFuture<'_, BTreeMap<String, Value>> {
        Box::pin(async move {
            self.managers
                .read()
                .await
                .values()
                .flat_map(|manager| manager.resources())
                .collect()
        })
    }
}
