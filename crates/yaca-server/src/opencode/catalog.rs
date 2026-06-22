use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location::LocationResponse;
use super::model_ref::{OpenCodeModelRefParts, model_ref_parts};

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

#[derive(Clone, Serialize)]
struct ProviderInfo {
    id: String,
    name: String,
    api: NativeProviderApi,
    request: RequestInfo,
}

#[derive(Clone, Serialize)]
struct NativeProviderApi {
    #[serde(rename = "type")]
    kind: &'static str,
    settings: Value,
}

#[derive(Clone, Serialize)]
struct RequestInfo {
    headers: BTreeMap<String, String>,
    body: Value,
}

#[derive(Serialize)]
struct LegacyProviderList {
    all: Vec<ProviderInfo>,
    default: BTreeMap<String, String>,
    connected: Vec<String>,
}

#[derive(Serialize)]
struct LegacyConfigProviders {
    providers: Vec<ProviderInfo>,
    default: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ModelInfo {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    name: String,
    api: ModelApi,
    capabilities: ModelCapabilities,
    request: ModelRequest,
    variants: Vec<ModelVariant>,
    time: ModelTime,
    cost: Vec<ModelCost>,
    status: &'static str,
    enabled: bool,
    limit: ModelLimit,
}

#[derive(Serialize)]
struct ModelApi {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    settings: Value,
}

#[derive(Serialize)]
struct ModelCapabilities {
    tools: bool,
    input: Vec<String>,
    output: Vec<String>,
}

#[derive(Serialize)]
struct ModelRequest {
    headers: BTreeMap<String, String>,
    body: Value,
    generation: Value,
    options: Value,
}

#[derive(Serialize)]
struct ModelVariant {
    id: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

#[derive(Serialize)]
struct ModelTime {
    released: u64,
}

#[derive(Serialize)]
struct ModelCost {
    input: f64,
    output: f64,
    cache: ModelCacheCost,
}

#[derive(Serialize)]
struct ModelCacheCost {
    read: f64,
    write: f64,
}

#[derive(Serialize)]
struct ModelLimit {
    context: u32,
    output: u32,
}

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

async fn legacy_provider_auth() -> Json<Value> {
    Json(json!({}))
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

async fn provider_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<ProviderInfo>>> {
    let active = model_ref_parts(&st.agent.model);
    Json(super::location::response(&st, vec![provider_info(&active)]))
}

async fn provider_get(
    State(st): State<ServerState>,
    Path(provider_id): Path<String>,
) -> Result<Json<LocationResponse<ProviderInfo>>, ApiError> {
    let active = model_ref_parts(&st.agent.model);
    if provider_id != active.provider_id {
        return Err(ApiError::not_found(format!(
            "Provider not found: {provider_id}"
        )));
    }
    Ok(Json(super::location::response(&st, provider_info(&active))))
}

async fn model_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<ModelInfo>>> {
    let active = model_ref_parts(&st.agent.model);
    Json(super::location::response(&st, vec![model_info(&active)]))
}

fn provider_info(active: &OpenCodeModelRefParts) -> ProviderInfo {
    ProviderInfo {
        id: active.provider_id.clone(),
        name: active.provider_id.clone(),
        api: NativeProviderApi {
            kind: "native",
            settings: json!({}),
        },
        request: RequestInfo {
            headers: BTreeMap::new(),
            body: json!({}),
        },
    }
}

fn model_info(active: &OpenCodeModelRefParts) -> ModelInfo {
    ModelInfo {
        id: active.model_id.clone(),
        provider_id: active.provider_id.clone(),
        name: active.model_id.clone(),
        api: ModelApi {
            id: active.model_id.clone(),
            kind: "native",
            settings: json!({}),
        },
        capabilities: ModelCapabilities {
            tools: false,
            input: Vec::new(),
            output: Vec::new(),
        },
        request: ModelRequest {
            headers: BTreeMap::new(),
            body: json!({}),
            generation: json!({}),
            options: json!({}),
        },
        variants: Vec::new(),
        time: ModelTime { released: 0 },
        cost: Vec::new(),
        status: "active",
        enabled: true,
        limit: ModelLimit {
            context: 0,
            output: 0,
        },
    }
}
