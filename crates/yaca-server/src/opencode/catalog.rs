use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};
use yaca_proto::ModelRef;

use crate::{ApiError, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/provider", get(provider_list))
        .route("/api/provider/:provider_id", get(provider_get))
        .route("/api/model", get(model_list))
}

#[derive(Clone)]
struct ActiveModel {
    provider_id: String,
    model_id: String,
}

#[derive(Serialize)]
struct LocationResponse<T> {
    location: LocationInfo,
    data: T,
}

#[derive(Serialize)]
struct LocationInfo {
    directory: String,
    #[serde(rename = "workspaceID", skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    project: ProjectInfo,
}

#[derive(Serialize)]
struct ProjectInfo {
    id: &'static str,
    directory: String,
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

async fn provider_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<ProviderInfo>>> {
    let active = active_model(&st.agent.model);
    Json(location_response(&st, vec![provider_info(&active)]))
}

async fn provider_get(
    State(st): State<ServerState>,
    Path(provider_id): Path<String>,
) -> Result<Json<LocationResponse<ProviderInfo>>, ApiError> {
    let active = active_model(&st.agent.model);
    if provider_id != active.provider_id {
        return Err(ApiError::not_found(format!(
            "Provider not found: {provider_id}"
        )));
    }
    Ok(Json(location_response(&st, provider_info(&active))))
}

async fn model_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<ModelInfo>>> {
    let active = active_model(&st.agent.model);
    Json(location_response(&st, vec![model_info(&active)]))
}

fn active_model(model: &ModelRef) -> ActiveModel {
    if let Some((provider_id, model_id)) = model.as_str().split_once('/') {
        return ActiveModel {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
        };
    }
    ActiveModel {
        provider_id: "yaca".to_string(),
        model_id: model.to_string(),
    }
}

fn location_response<T>(st: &ServerState, data: T) -> LocationResponse<T> {
    LocationResponse {
        location: location_info(st),
        data,
    }
}

fn location_info(st: &ServerState) -> LocationInfo {
    let directory = workdir(st).to_string_lossy().into_owned();
    LocationInfo {
        directory: directory.clone(),
        workspace_id: None,
        project: ProjectInfo {
            id: "global",
            directory,
        },
    }
}

fn workdir(st: &ServerState) -> PathBuf {
    match std::fs::canonicalize(&st.agent.workdir) {
        Ok(path) => path,
        Err(_) => st.agent.workdir.clone(),
    }
}

fn provider_info(active: &ActiveModel) -> ProviderInfo {
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

fn model_info(active: &ActiveModel) -> ModelInfo {
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
