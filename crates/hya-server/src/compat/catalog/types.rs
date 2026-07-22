use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use super::CatalogModel;

#[derive(Clone, Serialize)]
pub(super) struct ProviderInfo {
    id: String,
    name: String,
    api: NativeProviderApi,
    request: RequestInfo,
    models: BTreeMap<String, ModelInfo>,
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

#[derive(Clone, Serialize)]
pub(super) struct ModelInfo {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    name: String,
    api: ModelApi,
    capabilities: ModelCapabilities,
    request: ModelRequest,
    /// Insertion-ordered so config/catalog variant order is preserved on the wire
    /// (TUI cycle and picker use object key order).
    variants: serde_json::Map<String, Value>,
    time: ModelTime,
    cost: Vec<ModelCost>,
    status: &'static str,
    enabled: bool,
    limit: ModelLimit,
}

#[derive(Clone, Serialize)]
struct ModelApi {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    settings: Value,
}

#[derive(Clone, Serialize)]
struct ModelCapabilities {
    tools: bool,
    input: Vec<String>,
    output: Vec<String>,
}

#[derive(Clone, Serialize)]
struct ModelRequest {
    headers: BTreeMap<String, String>,
    body: Value,
    generation: Value,
    options: Value,
}

#[derive(Clone, Serialize)]
struct ModelTime {
    released: u64,
}

#[derive(Clone, Serialize)]
struct ModelCost {
    input: f64,
    output: f64,
    cache: ModelCacheCost,
}

#[derive(Clone, Serialize)]
struct ModelCacheCost {
    read: f64,
    write: f64,
}

#[derive(Clone, Serialize)]
struct ModelLimit {
    context: u32,
    output: u32,
}

pub(super) fn provider_info(provider_id: &str, catalog: &[CatalogModel]) -> ProviderInfo {
    let models = catalog
        .iter()
        .filter(|model| model.provider_id == provider_id)
        .map(|model| {
            (
                model.model_id.clone(),
                model_info(
                    &model.provider_id,
                    &model.model_id,
                    model.tools,
                    model.context,
                    &model.variants,
                ),
            )
        })
        .collect();
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
        models,
    }
}

pub(super) fn model_info(
    provider_id: &str,
    model_id: &str,
    tools: bool,
    context: u32,
    variants: &[String],
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
        variants: variants
            .iter()
            .map(|name| (name.clone(), json!({})))
            .collect(),
        time: ModelTime { released: 0 },
        cost: Vec::new(),
        status: "active",
        enabled: true,
        limit: ModelLimit { context, output: 0 },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn model_info_serializes_variants_as_keyed_object() {
        let info = model_info(
            "anthropic",
            "claude-opus-4-8",
            true,
            200_000,
            &["low".to_string(), "high".to_string()],
        );
        let value = serde_json::to_value(&info).expect("serialize model info");
        let variants = value
            .get("variants")
            .and_then(Value::as_object)
            .expect("variants must serialize as a JSON object");
        let keys: Vec<&String> = variants.keys().collect();
        assert_eq!(keys, ["low", "high"]);
        assert!(variants["low"].is_object());
    }

    #[test]
    fn model_info_preserves_configured_variant_order() {
        // Config order, not effort rank and not alphabetical (high < low < medium).
        let info = model_info(
            "gateway",
            "gpt-5.6-sol",
            true,
            200_000,
            &[
                "max".to_string(),
                "high".to_string(),
                "low".to_string(),
                "medium".to_string(),
            ],
        );
        let value = serde_json::to_value(&info).expect("serialize model info");
        let keys: Vec<&String> = value["variants"]
            .as_object()
            .expect("variants object")
            .keys()
            .collect();
        assert_eq!(keys, ["max", "high", "low", "medium"]);
    }
}
