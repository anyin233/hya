use std::sync::Arc;

use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct DataState {
    inner: Arc<RwLock<hya_sdk::MessageStore>>,
}

impl DataState {
    #[must_use]
    pub fn new(data: hya_sdk::MessageStore) -> Self {
        Self {
            inner: Arc::new(RwLock::new(data)),
        }
    }

    #[must_use]
    pub fn into_inner(self) -> Arc<RwLock<hya_sdk::MessageStore>> {
        self.inner
    }
}

impl Default for DataState {
    fn default() -> Self {
        Self::new(hya_sdk::MessageStore::default())
    }
}
