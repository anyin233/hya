use std::collections::BTreeMap;

use serde_json::Value;
use sqlx::Row;

use crate::{SessionStore, StoreError};

impl SessionStore {
    pub async fn replay_sync_events(&self, events: &[Value]) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        for event in events {
            let Some(aggregate) = event.get("aggregateID").and_then(Value::as_str) else {
                continue;
            };
            let Some(seq) = event.get("seq").and_then(Value::as_u64) else {
                continue;
            };
            let payload = serde_json::to_string(&history_event(event))?;
            sqlx::query(
                "INSERT OR REPLACE INTO sync_event (aggregate_id, seq, payload) VALUES (?, ?, ?)",
            )
            .bind(aggregate)
            .bind(seq.min(i64::MAX as u64) as i64)
            .bind(payload)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn sync_history(
        &self,
        known: &BTreeMap<String, u64>,
    ) -> Result<Vec<Value>, StoreError> {
        let rows = sqlx::query("SELECT aggregate_id, seq, payload FROM sync_event ORDER BY seq")
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let aggregate: String = row.try_get("aggregate_id")?;
            let seq: i64 = row.try_get("seq")?;
            let seq = seq.max(0) as u64;
            if known.get(&aggregate).is_none_or(|after| seq > *after) {
                let payload: String = row.try_get("payload")?;
                out.push(serde_json::from_str(&payload)?);
            }
        }
        Ok(out)
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
