//! The runtime configuration bag shared by the `/v2` config routes and
//! process state.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::RwLock;

#[derive(Clone)]
pub(crate) struct GlobalState {
    config: Arc<RwLock<Value>>,
}

impl GlobalState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(json!({}))),
        }
    }

    pub(crate) async fn config(&self) -> Value {
        self.config.read().await.clone()
    }

    pub(crate) async fn update_config(&self, config: Value) {
        *self.config.write().await = config;
    }
}
#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::GlobalState;

    #[tokio::test]
    async fn the_bag_starts_empty_and_is_replaced() {
        let state = GlobalState::new();
        assert_eq!(state.config().await, json!({}));
        state.update_config(json!({"x": 1})).await;
        assert_eq!(state.config().await, json!({"x": 1}));
    }
}
