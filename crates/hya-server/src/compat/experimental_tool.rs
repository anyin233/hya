use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Query, State};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

pub(super) async fn list(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let Some(_provider) = query
        .get("provider")
        .filter(|provider| !provider.is_empty())
    else {
        return Err(ApiError::bad_request("tool list missing provider"));
    };
    let Some(model) = query.get("model").filter(|model| !model.is_empty()) else {
        return Err(ApiError::bad_request("tool list missing model"));
    };
    let mut schemas = st.engine.tool_schemas();
    schemas.sort_by(|left, right| left.name.as_str().cmp(right.name.as_str()));
    Ok(Json(
        schemas
            .into_iter()
            .filter(|schema| include_tool(schema.name.as_str(), model))
            .map(|schema| {
                json!({
                    "id": schema.name.to_string(),
                    "description": schema.description,
                    "parameters": schema.input_schema,
                })
            })
            .collect(),
    ))
}

pub(super) async fn ids(State(st): State<ServerState>) -> Json<Vec<String>> {
    let mut ids: Vec<_> = st
        .engine
        .tool_schemas()
        .into_iter()
        .map(|schema| schema.name.to_string())
        .collect();
    ids.sort();
    Json(ids)
}

fn include_tool(id: &str, model: &str) -> bool {
    let use_patch = model.contains("gpt-") && !model.contains("oss") && !model.contains("gpt-4");
    match id {
        "apply_patch" => use_patch,
        "edit" | "write" => !use_patch,
        _ => true,
    }
}
