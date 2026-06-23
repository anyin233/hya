use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use yaca_mcp::{McpManager, McpServerConfig, McpStatus};

#[derive(Clone)]
pub(crate) struct McpHttpState {
    configs: Arc<RwLock<BTreeMap<String, McpServerConfig>>>,
    managers: Arc<RwLock<BTreeMap<String, Arc<McpManager>>>>,
    status: Arc<RwLock<BTreeMap<String, McpStatus>>>,
}

impl McpHttpState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            configs: Arc::new(RwLock::new(BTreeMap::new())),
            managers: Arc::new(RwLock::new(BTreeMap::new())),
            status: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    pub(super) async fn status(&self, manager: &McpManager) -> BTreeMap<String, McpStatus> {
        let mut status = manager.status();
        status.extend(self.status.read().await.clone());
        status
    }

    pub(super) async fn add_config(&self, name: String, config: McpServerConfig) -> McpStatus {
        self.configs
            .write()
            .await
            .insert(name.clone(), config.clone());
        let status = connect_status(name.clone(), config, &self.managers).await;
        self.status.write().await.insert(name, status.clone());
        status
    }

    pub(super) async fn connect_config(&self, name: &str) -> Option<McpStatus> {
        let mut config = self.configs.read().await.get(name).cloned()?;
        config.enabled = Some(true);
        self.configs
            .write()
            .await
            .insert(name.to_string(), config.clone());
        let status = connect_status(name.to_string(), config, &self.managers).await;
        self.status
            .write()
            .await
            .insert(name.to_string(), status.clone());
        Some(status)
    }

    pub(super) async fn disconnect_config(&self, name: &str) -> bool {
        let Some(mut config) = self.configs.read().await.get(name).cloned() else {
            return false;
        };
        config.enabled = Some(false);
        self.configs.write().await.insert(name.to_string(), config);
        self.managers.write().await.remove(name);
        self.status
            .write()
            .await
            .insert(name.to_string(), McpStatus::Disabled);
        true
    }

    pub(super) async fn resources(&self, manager: &McpManager) -> BTreeMap<String, Value> {
        let mut resources = manager.resources();
        for manager in self.managers.read().await.values() {
            resources.extend(manager.resources());
        }
        resources
    }
}

async fn connect_status(
    name: String,
    config: McpServerConfig,
    managers: &RwLock<BTreeMap<String, Arc<McpManager>>>,
) -> McpStatus {
    if config.enabled == Some(false) {
        managers.write().await.remove(&name);
        return McpStatus::Disabled;
    }
    let mut configs = BTreeMap::new();
    configs.insert(name.clone(), config);
    let manager = Arc::new(McpManager::connect_all(configs).await);
    let status = manager
        .status()
        .get(&name)
        .cloned()
        .unwrap_or_else(|| McpStatus::Failed {
            error: "MCP server did not report status".to_string(),
        });
    managers.write().await.insert(name, manager);
    status
}
