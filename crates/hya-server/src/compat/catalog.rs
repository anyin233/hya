use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hya_provider::{
    ModelCatalogSource, ProviderCatalogResult, ProviderCatalogSource, ProviderCatalogState,
};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location::{LocationRef, LocationResponse};

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
    variants: Vec<String>,
    source: &'static str,
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
    let states = catalog_states(&st);
    Json(LegacyConfigProviders {
        providers: provider_infos(&models, states),
        default: default_models(&models),
        default_model: catalog_default(&st),
    })
}

async fn legacy_provider_list(State(st): State<ServerState>) -> Json<LegacyProviderList> {
    let models = catalog_models(&st);
    let states = catalog_states(&st);
    Json(LegacyProviderList {
        all: provider_infos(&models, states),
        default: default_models(&models),
        default_model: catalog_default(&st),
        connected: connected_provider_ids(states),
    })
}

async fn legacy_provider_auth(
    State(st): State<ServerState>,
) -> Json<BTreeMap<String, Vec<ProviderAuthMethod>>> {
    Json(
        catalog_states(&st)
            .iter()
            .filter(|state| state.source != ProviderCatalogSource::Offline)
            .map(|state| {
                (
                    state.provider_id.clone(),
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
    let models = catalog_models(&st);
    let data = provider_infos(&models, catalog_states(&st));
    Json(location_response(&st, &query, &headers, data))
}

async fn provider_get(
    State(st): State<ServerState>,
    Query(query): LocationQuery,
    Path(provider_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let models = catalog_models(&st);
    let states = catalog_states(&st);
    if !provider_ids(states).iter().any(|id| id == &provider_id) {
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
        provider_info(
            &provider_id,
            &models,
            states.iter().find(|state| state.provider_id == provider_id),
        ),
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
                &model.variants,
                model.source,
            )
        })
        .collect();
    Json(location_response(&st, &query, &headers, data))
}

fn catalog_models(st: &ServerState) -> Vec<CatalogModel> {
    st.engine
        .provider_catalog()
        .iter()
        .map(|model| CatalogModel {
            provider_id: model.provider_id.clone(),
            model_id: model.model_id.clone(),
            tools: model.capabilities.streaming_tool_calls,
            context: model.capabilities.max_context,
            variants: model.reasoning_variants.clone(),
            source: model_source(model.source),
        })
        .collect()
}

fn catalog_states(st: &ServerState) -> &[ProviderCatalogState] {
    st.engine.provider_catalog_snapshot().providers()
}

fn provider_ids(states: &[ProviderCatalogState]) -> Vec<String> {
    states
        .iter()
        .map(|state| state.provider_id.clone())
        .collect()
}

fn provider_infos(models: &[CatalogModel], states: &[ProviderCatalogState]) -> Vec<ProviderInfo> {
    provider_ids(states)
        .into_iter()
        .map(|provider_id| {
            provider_info(
                &provider_id,
                models,
                states.iter().find(|state| state.provider_id == provider_id),
            )
        })
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

fn catalog_default(st: &ServerState) -> Value {
    let snapshot = st.engine.provider_catalog_snapshot();
    let default = snapshot.default_model().as_str();
    snapshot
        .models()
        .iter()
        .find(|model| {
            default
                .strip_prefix(model.provider_id.as_str())
                .and_then(|suffix| suffix.strip_prefix('/'))
                == Some(model.model_id.as_str())
        })
        .map_or(Value::Null, |model| {
            json!({
                "providerID": model.provider_id,
                "modelID": model.model_id,
            })
        })
}

fn connected_provider_ids(states: &[ProviderCatalogState]) -> Vec<String> {
    states
        .iter()
        .filter(|state| {
            state.source == ProviderCatalogSource::Discovered
                && state.result == ProviderCatalogResult::Models
        })
        .map(|state| state.provider_id.clone())
        .collect()
}

fn model_source(source: ModelCatalogSource) -> &'static str {
    match source {
        ModelCatalogSource::Configured => "configured",
        ModelCatalogSource::Discovered => "discovered",
        ModelCatalogSource::Offline => "offline",
    }
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

/// Provider catalog slices for the TUI single-RTT bootstrap payload.
pub(super) fn bootstrap_provider_payload(st: &ServerState) -> (Value, Value) {
    let models = catalog_models(st);
    let states = catalog_states(st);
    let providers = provider_infos(&models, states);
    let default = default_models(&models);
    let connected = connected_provider_ids(states);
    (
        json!({
            "providers": providers,
            "default": default,
            "defaultModel": catalog_default(st),
        }),
        json!({
            "all": providers,
            "default": default,
            "defaultModel": catalog_default(st),
            "connected": connected,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_excludes_explicit_and_failed_provider_states() {
        let states = vec![
            ProviderCatalogState {
                provider_id: "configured".into(),
                kind: hya_provider::ProviderKind::OpenAiCompatible,
                source: ProviderCatalogSource::Configured,
                auth: hya_provider::ProviderAuthState::Unauthenticated,
                result: ProviderCatalogResult::Models,
            },
            ProviderCatalogState {
                provider_id: "failed".into(),
                kind: hya_provider::ProviderKind::OpenAiCompatible,
                source: ProviderCatalogSource::None,
                auth: hya_provider::ProviderAuthState::AuthRequired,
                result: ProviderCatalogResult::Unavailable,
            },
            ProviderCatalogState {
                provider_id: "discovered".into(),
                kind: hya_provider::ProviderKind::OpenAiCompatible,
                source: ProviderCatalogSource::Discovered,
                auth: hya_provider::ProviderAuthState::Credentialed,
                result: ProviderCatalogResult::Models,
            },
        ];
        assert_eq!(connected_provider_ids(&states), ["discovered"]);
    }
}
