use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location::{LocationRef, LocationResponse};
use super::model_ref::model_ref_parts;

mod types;

use types::{
    LegacyConfigProviders, LegacyProviderList, ModelInfo, ProviderAuthMethod, ProviderInfo,
    model_info, provider_info,
};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route(
            "/config",
            get(legacy_config_get).patch(legacy_config_update),
        )
        .route("/config/providers", get(legacy_config_providers))
        .route("/provider", get(legacy_provider_list))
        .route("/provider/auth", get(legacy_provider_auth))
        .route(
            "/provider/:provider_id/oauth/authorize",
            post(legacy_provider_oauth_authorize),
        )
        .route(
            "/provider/:provider_id/oauth/callback",
            post(legacy_provider_oauth_callback),
        )
        .route("/api/provider", get(provider_list))
        .route("/api/provider/:provider_id", get(provider_get))
        .route("/api/model", get(model_list))
}

type LocationQuery = Query<BTreeMap<String, String>>;

async fn legacy_config_get() -> Json<Value> {
    Json(json!({}))
}

async fn legacy_config_update(Json(payload): Json<Value>) -> Result<Json<Value>, ApiError> {
    let Some(map) = payload.as_object() else {
        return Err(ApiError::bad_request("config payload must be an object"));
    };
    if let Some(username) = map.get("username")
        && !username.is_string()
    {
        return Err(ApiError::bad_request("username must be a string"));
    }
    Ok(Json(payload))
}

async fn legacy_config_providers(State(st): State<ServerState>) -> Json<LegacyConfigProviders> {
    let active = model_ref_parts(&st.agent.model);
    Json(LegacyConfigProviders {
        providers: vec![provider_info(&active)],
        default: BTreeMap::from([(active.provider_id, active.model_id)]),
    })
}

async fn legacy_provider_list(State(st): State<ServerState>) -> Json<LegacyProviderList> {
    let active = model_ref_parts(&st.agent.model);
    Json(LegacyProviderList {
        all: vec![provider_info(&active)],
        default: BTreeMap::from([(active.provider_id.clone(), active.model_id.clone())]),
        connected: vec![active.provider_id],
    })
}

async fn legacy_provider_auth(
    State(st): State<ServerState>,
) -> Json<BTreeMap<String, Vec<ProviderAuthMethod>>> {
    let active = model_ref_parts(&st.agent.model);
    Json(BTreeMap::from([(
        active.provider_id,
        vec![ProviderAuthMethod {
            kind: "api",
            label: "API key",
        }],
    )]))
}

async fn legacy_provider_oauth_authorize(
    Path(_provider_id): Path<String>,
    Json(_payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    Err(ApiError::bad_request("unsupported provider oauth method"))
}

async fn legacy_provider_oauth_callback(
    Path(_provider_id): Path<String>,
    Json(_payload): Json<Value>,
) -> Result<Json<bool>, ApiError> {
    Err(ApiError::bad_request("unsupported provider oauth method"))
}

async fn provider_list(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<ProviderInfo>>> {
    let active = model_ref_parts(&st.agent.model);
    Json(location_response(
        &st,
        &query,
        &headers,
        vec![provider_info(&active)],
    ))
}

async fn provider_get(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let active = model_ref_parts(&st.agent.model);
    if provider_id != active.provider_id {
        let message = format!("Provider not found: {provider_id}");
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({
                "_tag": "ProviderNotFoundError",
                "providerID": provider_id,
                "message": message,
            })),
        )
            .into_response());
    }
    Ok(Json(location_response(
        &st,
        &query,
        &headers,
        provider_info(&active),
    ))
    .into_response())
}

async fn model_list(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<ModelInfo>>> {
    let active = model_ref_parts(&st.agent.model);
    Json(location_response(
        &st,
        &query,
        &headers,
        vec![model_info(&active)],
    ))
}

fn location_response<T>(
    st: &ServerState,
    query: &BTreeMap<String, String>,
    headers: &HeaderMap,
    data: T,
) -> LocationResponse<T> {
    let location = LocationRef::from_request(query, headers);
    super::location::response_at(st, &location, data)
}
