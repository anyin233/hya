use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use serde_json::{Value, json};

use crate::{ApiError, ServerState, parse_session};

pub(super) async fn history(
    State(st): State<ServerState>,
    Json(known): Json<BTreeMap<String, u64>>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let mut out = st
        .engine
        .store()
        .sync_history(&known)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
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

pub(super) async fn replay(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(directory) = payload.get("directory").and_then(Value::as_str) else {
        return Err(ApiError::bad_request("sync replay missing directory"));
    };
    let Some(events) = payload.get("events").and_then(Value::as_array) else {
        return Err(ApiError::bad_request("sync replay requires events"));
    };
    let Some(first) = events.first() else {
        return Err(ApiError::bad_request("sync replay requires events"));
    };
    let Some(session_id) = first.get("aggregateID").and_then(Value::as_str) else {
        return Err(ApiError::bad_request(
            "sync replay event missing aggregateID",
        ));
    };
    let start = first
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| ApiError::bad_request("sync replay event missing seq"))?;
    for (index, event) in events.iter().enumerate() {
        if event.get("aggregateID").and_then(Value::as_str) != Some(session_id) {
            return Err(ApiError::bad_request(
                "sync replay events must belong to the same aggregate",
            ));
        }
        let expected = start + u64::try_from(index).unwrap_or(u64::MAX);
        if event.get("seq").and_then(Value::as_u64) != Some(expected) {
            return Err(ApiError::bad_request(format!(
                "sync replay sequence mismatch at index {index}: expected {expected}"
            )));
        }
    }
    let inserted = st
        .engine
        .store()
        .replay_sync_events(events)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    super::sync_projector::project_replay(&st, directory, &inserted).await?;
    Ok(Json(json!({ "sessionID": session_id })))
}

pub(super) async fn steal(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if !query.contains_key("workspace") {
        return Err(ApiError::bad_request("sync steal missing workspace"));
    }
    let Some(session_id) = payload.get("sessionID").and_then(Value::as_str) else {
        return Err(ApiError::bad_request("sync steal missing sessionID"));
    };
    let session = parse_session(session_id)?;
    let projection = st
        .engine
        .store()
        .read_projection(session)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if projection.session.id.is_none() {
        return Err(ApiError::bad_request(format!(
            "Session not found: {session_id}"
        )));
    }
    // ponytail: no workspace ownership column yet; add mutation when yaca stores it.
    Ok(Json(json!({ "sessionID": session_id })))
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
