//! Shared provider catalog snapshot and model-list discovery parser contracts.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::ModelRef;
use hya_provider::{
    AnthropicMessagesProtocol, Capabilities, CatalogFailure, CatalogNotice, DiscoveredModel,
    GoogleProtocol, ModelCatalogSource, OpenAiChatProtocol, OpenAiCodexProtocol,
    OpenAiResponsesProtocol, Protocol, ProviderAuthState, ProviderCatalogResult,
    ProviderCatalogSnapshot, ProviderCatalogSource, ProviderCatalogState, ProviderModel,
    parse_catalog_payload, valid_provider_id,
};
use serde_json::json;

fn model(provider_id: &str, model_id: &str, source: ModelCatalogSource) -> ProviderModel {
    ProviderModel {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        capabilities: Capabilities::default(),
        reasoning_variants: Vec::new(),
        reasoning_default: None,
        reasoning: None,
        display_name: None,
        source,
    }
}

fn state(
    provider_id: &str,
    source: ProviderCatalogSource,
    result: ProviderCatalogResult,
) -> ProviderCatalogState {
    ProviderCatalogState {
        provider_id: provider_id.to_string(),
        kind: "openai".to_string(),
        supports_oauth: false,
        min_output_tokens: 1,
        source,
        auth: ProviderAuthState::Unauthenticated,
        result,
    }
}

#[test]
fn provider_ids_and_offline_provider_canonical_contract() {
    for id in ["a", "Team_openai-2", "hya", &"a".repeat(64)] {
        assert!(valid_provider_id(id), "valid ID {id:?}");
    }
    for id in ["", "a.b", "a/b", "a b", "é", &"a".repeat(65)] {
        assert!(!valid_provider_id(id), "invalid ID {id:?}");
    }

    let provider = hya_provider::DevProvider::new();
    assert_eq!(hya_provider::Provider::id(&provider), "hya");
    assert!(
        hya_provider::Provider::capabilities(&provider, &ModelRef::new("hya/offline")).is_some()
    );
    assert!(hya_provider::Provider::capabilities(&provider, &ModelRef::new("offline")).is_none());
    assert!(
        hya_provider::Provider::capabilities(&provider, &ModelRef::new("hya/anything")).is_none()
    );
}

#[test]
fn catalog_snapshot_sorting_dedup_defaults_provenance_and_offline_isolation() {
    // 1. Sorts, deduplicates, preserves provenance & zero-row provider state.
    let live = ProviderCatalogSnapshot::build(
        vec![
            model("zeta", "same", ModelCatalogSource::Configured),
            model("alpha", "z", ModelCatalogSource::Discovered),
            model("alpha", "a", ModelCatalogSource::Discovered),
            model("zeta", "same", ModelCatalogSource::Configured),
        ],
        vec![
            state(
                "zeta",
                ProviderCatalogSource::Configured,
                ProviderCatalogResult::Models,
            ),
            state(
                "empty",
                ProviderCatalogSource::None,
                ProviderCatalogResult::Empty,
            ),
            state(
                "alpha",
                ProviderCatalogSource::Discovered,
                ProviderCatalogResult::Models,
            ),
        ],
        Some(ModelRef::new("alpha/a")),
    );
    assert_eq!(
        live.models()
            .iter()
            .map(|r| (format!("{}/{}", r.provider_id, r.model_id), r.source))
            .collect::<Vec<_>>(),
        [
            ("alpha/a".to_string(), ModelCatalogSource::Discovered),
            ("alpha/z".to_string(), ModelCatalogSource::Discovered),
            ("zeta/same".to_string(), ModelCatalogSource::Configured),
        ]
    );
    assert_eq!(
        live.providers()
            .iter()
            .map(|p| p.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "empty", "zeta"]
    );
    assert_eq!(live.default_model(), &ModelRef::new("alpha/a"));
    assert!(live.notice().is_none());

    // 2. Empty snapshot injects canonical hya/offline row & notice; replaces declared hya state.
    let empty = ProviderCatalogSnapshot::build(
        Vec::new(),
        vec![state(
            "openai",
            ProviderCatalogSource::None,
            ProviderCatalogResult::Unavailable,
        )],
        None,
    );
    assert_eq!(empty.models().len(), 1);
    assert_eq!(empty.models()[0].source, ModelCatalogSource::Offline);
    assert_eq!(empty.default_model().as_str(), "hya/offline");
    assert!(matches!(
        empty.notice(),
        Some(CatalogNotice::ConfigureProvider { .. })
    ));
    assert_eq!(
        empty
            .providers()
            .iter()
            .map(|p| p.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["hya", "openai"]
    );

    let replaced_hya = ProviderCatalogSnapshot::build(
        Vec::new(),
        vec![state(
            "hya",
            ProviderCatalogSource::None,
            ProviderCatalogResult::Unavailable,
        )],
        None,
    );
    assert_eq!(replaced_hya.providers().len(), 1);
    assert_eq!(
        replaced_hya.providers()[0].source,
        ProviderCatalogSource::Offline
    );
    assert_eq!(
        replaced_hya.providers()[0].auth,
        ProviderAuthState::NotApplicable
    );
    assert_eq!(
        replaced_hya.providers()[0].result,
        ProviderCatalogResult::Offline
    );

    // 3. Live snapshot strips offline rows/providers; remote rows cannot claim hya/offline.
    let live_stripped = ProviderCatalogSnapshot::build(
        vec![
            model("hya", "offline", ModelCatalogSource::Offline),
            model("openai", "gpt", ModelCatalogSource::Configured),
        ],
        vec![
            state(
                "hya",
                ProviderCatalogSource::Offline,
                ProviderCatalogResult::Offline,
            ),
            state(
                "openai",
                ProviderCatalogSource::Configured,
                ProviderCatalogResult::Models,
            ),
        ],
        None,
    );
    assert_eq!(live_stripped.models().len(), 1);
    assert_eq!(
        live_stripped.models()[0].source,
        ModelCatalogSource::Configured
    );
    assert!(
        !live_stripped
            .providers()
            .iter()
            .any(|p| p.provider_id == "hya")
    );

    let reserved = ProviderCatalogSnapshot::build(
        vec![model("hya", "offline", ModelCatalogSource::Discovered)],
        Vec::new(),
        Some(ModelRef::new("hya/offline")),
    );
    assert_eq!(reserved.models().len(), 1);
    assert_eq!(reserved.models()[0].source, ModelCatalogSource::Offline);

    // 4. Default falls back to snapshot member and canonicalizes unique bare names.
    for requested in ["missing/model", "gpt"] {
        let snap = ProviderCatalogSnapshot::build(
            vec![model("openai", "gpt", ModelCatalogSource::Configured)],
            Vec::new(),
            Some(ModelRef::new(requested)),
        );
        assert_eq!(
            snap.default_model(),
            &ModelRef::new("openai/gpt"),
            "requested={requested}"
        );
    }
}

#[test]
fn catalog_discovery_parses_provider_payloads_and_metadata() {
    // OpenAI data ids with trim, dedup, empty drop, and case sensitivity.
    let openai = parse_catalog_payload(
        &OpenAiChatProtocol,
        &json!({"data": [{"id": " gpt-5 "}, {"id": "gpt-5"}, {"id": ""}, {"id": "GPT-5"}]}),
    )
    .unwrap();
    assert_eq!(
        openai,
        vec![
            DiscoveredModel {
                id: "gpt-5".into(),
                ..DiscoveredModel::default()
            },
            DiscoveredModel {
                id: "GPT-5".into(),
                ..DiscoveredModel::default()
            },
        ]
    );

    // Codex keeps slug & reasoning metadata.
    let codex = parse_catalog_payload(
        &OpenAiCodexProtocol,
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
        codex,
        vec![DiscoveredModel {
            id: "codex-model".into(),
            reasoning_default: Some("high".into()),
            reasoning_variants: vec!["low".into(), "high".into()],
            ..DiscoveredModel::default()
        }]
    );

    // Google filters generateContent, strips models/ prefix, keeps displayName & limits.
    let google = parse_catalog_payload(
        &GoogleProtocol,
        &json!({
            "models": [
                {
                    "name": "models/gemini-2.5-pro",
                    "displayName": "Gemini X",
                    "inputTokenLimit": 1_048_576,
                    "outputTokenLimit": 65_536,
                    "supportedGenerationMethods": ["generateContent"]
                },
                {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]}
            ]
        }),
    )
    .unwrap();
    assert_eq!(google.len(), 1);
    assert_eq!(google[0].id, "gemini-2.5-pro");
    assert_eq!(google[0].display_name.as_deref(), Some("Gemini X"));
    assert_eq!(google[0].context_limit, Some(1_048_576));
    assert_eq!(google[0].output_limit, Some(65_536));

    // Anthropic & gateway display names and token limits.
    let anthropic = parse_catalog_payload(
        &AnthropicMessagesProtocol,
        &json!({"data": [{"id": "claude-x", "display_name": "Claude X"}]}),
    )
    .unwrap();
    assert_eq!(anthropic[0].display_name.as_deref(), Some("Claude X"));
    assert_eq!(anthropic[0].context_limit, None);

    let gateway = parse_catalog_payload(
        &OpenAiChatProtocol,
        &json!({"data": [{
            "id": "vendor/model",
            "name": "Vendor Model",
            "context_length": 131_072,
            "top_provider": {"max_completion_tokens": 8_192}
        }]}),
    )
    .unwrap();
    assert_eq!(gateway[0].display_name.as_deref(), Some("Vendor Model"));
    assert_eq!(gateway[0].context_limit, Some(131_072));
    assert_eq!(gateway[0].output_limit, Some(8_192));
}

#[test]
fn catalog_discovery_rejects_guessed_slug_and_malformed_shapes() {
    let cases: [(&dyn Protocol, _); 2] = [
        (
            &OpenAiChatProtocol,
            json!({ "data": [{ "slug": "guessed" }] }),
        ),
        (&OpenAiResponsesProtocol, json!({ "models": [] })),
    ];
    for (protocol, payload) in cases {
        assert_eq!(
            parse_catalog_payload(protocol, &payload).unwrap_err(),
            CatalogFailure::Schema,
            "{}",
            protocol.kind()
        );
    }
}
