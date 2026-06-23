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

struct CatalogModel {
    provider_id: String,
    model_id: String,
    tools: bool,
    context: u32,
}

async fn legacy_config_get(State(st): State<ServerState>) -> Json<Value> {
    Json(st.global.config().await)
}

async fn legacy_config_update(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(map) = payload.as_object() else {
        return Err(ApiError::bad_request("config payload must be an object"));
    };
    if let Some(username) = map.get("username")
        && !username.is_string()
    {
        return Err(ApiError::bad_request("username must be a string"));
    }
    st.global.update_config(payload.clone()).await;
    Ok(Json(payload))
}

async fn legacy_config_providers(State(st): State<ServerState>) -> Json<LegacyConfigProviders> {
    let models = catalog_models(&st);
    Json(LegacyConfigProviders {
        providers: provider_infos(&models),
        default: default_models(&models),
    })
}

async fn legacy_provider_list(State(st): State<ServerState>) -> Json<LegacyProviderList> {
    let models = catalog_models(&st);
    Json(LegacyProviderList {
        all: provider_infos(&models),
        default: default_models(&models),
        connected: provider_ids(&models),
    })
}

async fn legacy_provider_auth(
    State(st): State<ServerState>,
) -> Json<BTreeMap<String, Vec<ProviderAuthMethod>>> {
    Json(
        provider_ids(&catalog_models(&st))
            .into_iter()
            .map(|provider_id| {
                (
                    provider_id,
                    vec![ProviderAuthMethod {
                        kind: "api",
                        label: "API key",
                    }],
                )
            })
            .collect(),
    )
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
    let data = provider_infos(&catalog_models(&st));
    Json(location_response(&st, &query, &headers, data))
}

async fn provider_get(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !provider_ids(&catalog_models(&st)).contains(&provider_id) {
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
        provider_info(&provider_id),
    ))
    .into_response())
}

async fn model_list(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    headers: HeaderMap,
) -> Json<LocationResponse<Vec<ModelInfo>>> {
    let data = catalog_models(&st)
        .into_iter()
        .map(|model| {
            model_info(
                &model.provider_id,
                &model.model_id,
                model.tools,
                model.context,
            )
        })
        .collect();
    Json(location_response(&st, &query, &headers, data))
}

fn catalog_models(st: &ServerState) -> Vec<CatalogModel> {
    let models: Vec<_> = st
        .engine
        .provider_catalog()
        .into_iter()
        .map(|model| CatalogModel {
            provider_id: model.provider_id,
            model_id: model.model_id,
            tools: model.capabilities.streaming_tool_calls,
            context: model.capabilities.max_context,
        })
        .collect();
    if !models.is_empty() {
        return models;
    }
    let active = model_ref_parts(&st.agent.model);
    vec![CatalogModel {
        provider_id: active.provider_id,
        model_id: active.model_id,
        tools: false,
        context: 0,
    }]
}

fn provider_ids(models: &[CatalogModel]) -> Vec<String> {
    models
        .iter()
        .map(|model| model.provider_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn provider_infos(models: &[CatalogModel]) -> Vec<ProviderInfo> {
    provider_ids(models)
        .into_iter()
        .map(|provider_id| provider_info(&provider_id))
        .collect()
}

fn default_models(models: &[CatalogModel]) -> BTreeMap<String, String> {
    let mut defaults = BTreeMap::new();
    for model in models {
        defaults
            .entry(model.provider_id.clone())
            .or_insert_with(|| model.model_id.clone());
    }
    defaults
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
