#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Provider model-list discovery parser contract tests.

use hya_provider::{CatalogFailure, DiscoveredModel, ProviderKind, parse_catalog_payload};
use serde_json::json;

#[test]
fn openai_catalog_uses_data_ids_and_exact_normalization() {
    let payload = json!({
        "data": [
            {"id": " gpt-5 "},
            {"id": "gpt-5"},
            {"id": ""},
            {"id": "GPT-5"}
        ]
    });
    let models = parse_catalog_payload(ProviderKind::OpenAiCompatible, &payload).unwrap();
    assert_eq!(
        models,
        vec![
            DiscoveredModel {
                id: "gpt-5".to_string(),
                reasoning_default: None,
                reasoning_variants: Vec::new(),
            },
            DiscoveredModel {
                id: "GPT-5".to_string(),
                reasoning_default: None,
                reasoning_variants: Vec::new(),
            },
        ],
    );
}

#[test]
fn openai_catalog_rejects_slug_as_a_guessed_id() {
    let error = parse_catalog_payload(
        ProviderKind::OpenAiCompatible,
        &json!({ "data": [{ "slug": "guessed" }] }),
    )
    .unwrap_err();
    assert_eq!(error, CatalogFailure::Schema);
}

#[test]
fn codex_catalog_keeps_reasoning_metadata_during_normalization() {
    let models = parse_catalog_payload(
        ProviderKind::OpenAiCodex,
        &json!({
            "models": [{
                "slug": " codex-model ",
                "default_reasoning_level": "high",
                "supported_reasoning_levels": [{"effort": "low"}, {"effort": "high"}]
            }]
        }),
    )
    .unwrap();
    assert_eq!(
        models,
        vec![DiscoveredModel {
            id: "codex-model".to_string(),
            reasoning_default: Some("high".to_string()),
            reasoning_variants: vec!["low".to_string(), "high".to_string()],
        }],
    );
}

#[test]
fn google_catalog_filters_generation_capability_and_strips_resource_prefix() {
    let payload = json!({
        "models": [
            {"name": "models/gemini-2.5-pro", "supportedGenerationMethods": ["generateContent"]},
            {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]}
        ]
    });
    let models = parse_catalog_payload(ProviderKind::Google, &payload).unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["gemini-2.5-pro"]
    );
}

#[test]
fn malformed_provider_shape_fails_closed() {
    let error =
        parse_catalog_payload(ProviderKind::OpenAiResponse, &json!({"models": []})).unwrap_err();
    assert_eq!(error, CatalogFailure::Schema);
}
