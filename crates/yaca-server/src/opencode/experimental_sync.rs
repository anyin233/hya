use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

pub(super) async fn history(
    State(st): State<ServerState>,
    Json(known): Json<BTreeMap<String, u64>>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let mut out = Vec::new();
    for info in st
        .engine
        .store()
        .list_sessions()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        let aggregate = info.session.to_string();
        let after = known.get(&aggregate).copied().unwrap_or_default();
        for env in st
            .engine
            .store()
            .replay(info.session)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
        {
            if env.seq.0 <= after {
                continue;
            }
            out.push(history_event(&aggregate, &env)?);
        }
    }
    out.sort_by_key(|event| event["seq"].as_u64().unwrap_or_default());
    Ok(Json(out))
}

fn history_event(aggregate: &str, env: &yaca_proto::Envelope) -> Result<Value, ApiError> {
    let mut data =
        serde_json::to_value(&env.event).map_err(|e| ApiError::internal(e.to_string()))?;
    let event_type = data
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    if let Value::Object(fields) = &mut data {
        fields.remove("type");
    }
    Ok(json!({
        "id": format!("evt_{}_{:016x}", aggregate.trim_start_matches("ses_"), env.seq.0),
        "aggregate_id": aggregate,
        "seq": env.seq.0,
        "type": event_type,
        "data": data,
    }))
}
