use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub(crate) struct SyncState {
    inner: Arc<RwLock<BTreeMap<String, BTreeMap<u64, Value>>>>,
}

impl SyncState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn replay(&self, events: &[Value]) {
        let mut state = self.inner.write().await;
        for event in events {
            let Some(aggregate) = event.get("aggregateID").and_then(Value::as_str) else {
                continue;
            };
            let Some(seq) = event.get("seq").and_then(Value::as_u64) else {
                continue;
            };
            state
                .entry(aggregate.to_string())
                .or_default()
                .insert(seq, history_event(event));
        }
    }

    pub(crate) async fn history(&self, known: &BTreeMap<String, u64>) -> Vec<Value> {
        let state = self.inner.read().await;
        state
            .iter()
            .flat_map(|(aggregate, events)| {
                events
                    .iter()
                    .filter(move |(seq, _)| known.get(aggregate).is_none_or(|after| **seq > *after))
                    .map(|(_, event)| event.clone())
            })
            .collect()
    }
}

fn history_event(event: &Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "id".to_string(),
        event.get("id").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "aggregate_id".to_string(),
        event.get("aggregateID").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "seq".to_string(),
        event.get("seq").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "type".to_string(),
        event.get("type").cloned().unwrap_or(Value::Null),
    );
    out.insert(
        "data".to_string(),
        event
            .get("data")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
    );
    Value::Object(out)
}
