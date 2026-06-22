use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Serialize)]
pub(super) struct ProviderInfo {
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
pub(super) struct LegacyProviderList {
    pub(super) all: Vec<ProviderInfo>,
    pub(super) default: BTreeMap<String, String>,
    pub(super) connected: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct LegacyConfigProviders {
    pub(super) providers: Vec<ProviderInfo>,
    pub(super) default: BTreeMap<String, String>,
}

#[derive(Serialize)]
pub(super) struct ProviderAuthMethod {
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) label: &'static str,
}

#[derive(Serialize)]
pub(super) struct ModelInfo {
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

pub(super) fn provider_info(provider_id: &str) -> ProviderInfo {
    ProviderInfo {
        id: provider_id.to_string(),
        name: provider_id.to_string(),
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

pub(super) fn model_info(
    provider_id: &str,
    model_id: &str,
    tools: bool,
    context: u32,
) -> ModelInfo {
    ModelInfo {
        id: model_id.to_string(),
        provider_id: provider_id.to_string(),
        name: model_id.to_string(),
        api: ModelApi {
            id: model_id.to_string(),
            kind: "native",
            settings: json!({}),
        },
        capabilities: ModelCapabilities {
            tools,
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
        limit: ModelLimit { context, output: 0 },
    }
}
