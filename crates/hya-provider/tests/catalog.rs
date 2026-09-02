//! Shared provider catalog snapshot contract tests.

use hya_proto::ModelRef;
use hya_provider::{
    Capabilities, CatalogNotice, ModelCatalogSource, ProviderAuthState, ProviderCatalogResult,
    ProviderCatalogSnapshot, ProviderCatalogSource, ProviderCatalogState, ProviderKind,
    ProviderModel,
};

fn model(provider_id: &str, model_id: &str, source: ModelCatalogSource) -> ProviderModel {
    ProviderModel {
        provider_id: provider_id.to_string(),
        model_id: model_id.to_string(),
        capabilities: Capabilities::default(),
        reasoning_variants: Vec::new(),
        reasoning_default: None,
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
        kind: ProviderKind::OpenAiCompatible,
        source,
        auth: ProviderAuthState::Unauthenticated,
        result,
    }
}

#[test]
fn snapshot_sorts_deduplicates_and_keeps_zero_row_provider_state() {
    let snapshot = ProviderCatalogSnapshot::build(
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
        snapshot
            .models()
            .iter()
            .map(|row| format!("{}/{}", row.provider_id, row.model_id))
            .collect::<Vec<_>>(),
        ["alpha/a", "alpha/z", "zeta/same"],
    );
    assert_eq!(
        snapshot
            .providers()
            .iter()
            .map(|provider| provider.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "empty", "zeta"],
    );
    assert_eq!(snapshot.default_model(), &ModelRef::new("alpha/a"));
    assert!(snapshot.notice().is_none());
}

#[test]
fn empty_snapshot_adds_only_canonical_offline_row_and_notice() {
    let snapshot = ProviderCatalogSnapshot::build(
        Vec::new(),
        vec![state(
            "openai",
            ProviderCatalogSource::None,
            ProviderCatalogResult::Unavailable,
        )],
        None,
    );

    assert_eq!(snapshot.models().len(), 1);
    let row = &snapshot.models()[0];
    assert_eq!(row.provider_id, "hya");
    assert_eq!(row.model_id, "offline");
    assert_eq!(row.source, ModelCatalogSource::Offline);
    assert_eq!(snapshot.default_model().as_str(), "hya/offline");
    assert!(matches!(
        snapshot.notice(),
        Some(CatalogNotice::ConfigureProvider { .. })
    ));
    assert_eq!(
        snapshot
            .providers()
            .iter()
            .map(|provider| provider.provider_id.as_str())
            .collect::<Vec<_>>(),
        ["hya", "openai"],
    );
}

#[test]
fn offline_snapshot_replaces_a_declared_hya_state_with_canonical_metadata() {
    let snapshot = ProviderCatalogSnapshot::build(
        Vec::new(),
        vec![state(
            "hya",
            ProviderCatalogSource::None,
            ProviderCatalogResult::Unavailable,
        )],
        None,
    );

    assert_eq!(snapshot.providers().len(), 1);
    let provider = &snapshot.providers()[0];
    assert_eq!(provider.provider_id, "hya");
    assert_eq!(provider.source, ProviderCatalogSource::Offline);
    assert_eq!(provider.auth, ProviderAuthState::NotApplicable);
    assert_eq!(provider.result, ProviderCatalogResult::Offline);
}

#[test]
fn live_snapshot_never_contains_offline_row() {
    let snapshot = ProviderCatalogSnapshot::build(
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

    assert_eq!(snapshot.models().len(), 1);
    assert_eq!(snapshot.models()[0].source, ModelCatalogSource::Configured);
    assert!(snapshot.notice().is_none());
    assert!(
        !snapshot
            .providers()
            .iter()
            .any(|provider| provider.provider_id == "hya")
    );
    assert!(
        !snapshot
            .models()
            .iter()
            .any(|row| row.model_id == "offline")
    );
}

#[test]
fn default_must_be_a_snapshot_member() {
    let snapshot = ProviderCatalogSnapshot::build(
        vec![model("openai", "gpt", ModelCatalogSource::Configured)],
        Vec::new(),
        Some(ModelRef::new("missing/model")),
    );
    assert_eq!(snapshot.default_model(), &ModelRef::new("openai/gpt"));
}

#[test]
fn unique_bare_default_is_canonicalized_to_its_snapshot_row() {
    let snapshot = ProviderCatalogSnapshot::build(
        vec![model("openai", "gpt", ModelCatalogSource::Configured)],
        Vec::new(),
        Some(ModelRef::new("gpt")),
    );
    assert_eq!(snapshot.default_model(), &ModelRef::new("openai/gpt"));
}

#[test]
fn remote_row_cannot_claim_the_reserved_offline_reference() {
    let snapshot = ProviderCatalogSnapshot::build(
        vec![model("hya", "offline", ModelCatalogSource::Discovered)],
        Vec::new(),
        Some(ModelRef::new("hya/offline")),
    );
    assert_eq!(snapshot.models().len(), 1);
    assert_eq!(snapshot.models()[0].source, ModelCatalogSource::Offline);
}

#[test]
fn model_provenance_preserves_configured_discovered_and_offline() {
    let configured = model("a", "one", ModelCatalogSource::Configured);
    let discovered = model("b", "two", ModelCatalogSource::Discovered);
    let snapshot = ProviderCatalogSnapshot::build(vec![configured, discovered], Vec::new(), None);
    assert_eq!(snapshot.models()[0].source, ModelCatalogSource::Configured);
    assert_eq!(snapshot.models()[1].source, ModelCatalogSource::Discovered);

    let offline = ProviderCatalogSnapshot::build(Vec::new(), Vec::new(), None);
    assert_eq!(offline.models()[0].source, ModelCatalogSource::Offline);
}

#[test]
fn offline_provider_claims_only_canonical_reference() {
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
