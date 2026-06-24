use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SyncState {
    pub sessions: HashMap<String, hya_sdk::Session>,
    pub messages: HashMap<String, Vec<hya_sdk::Message>>,
}
