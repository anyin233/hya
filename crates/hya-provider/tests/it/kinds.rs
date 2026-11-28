//! Provider kind registration: labels, aliases, and OAuth types are unique.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use async_trait::async_trait;
use hya_provider::test_support::stub_protocol;
use hya_provider::{
    OAuthCredential, OAuthError, OAuthLoginRequest, ProviderAuthFlow, ProviderKindEntry,
    ProviderKinds,
};

struct Flow(&'static str);

#[async_trait]
impl ProviderAuthFlow for Flow {
    fn oauth_type(&self) -> &str {
        self.0
    }
    fn default_base_url(&self) -> &str {
        "https://example.test/v1"
    }
    fn default_model(&self) -> &str {
        "model"
    }
    async fn login(&self, _request: OAuthLoginRequest) -> Result<OAuthCredential, OAuthError> {
        Err(OAuthError::Timeout("not interactive".into()))
    }
    async fn refresh(&self, credential: &OAuthCredential) -> Result<OAuthCredential, OAuthError> {
        Ok(credential.clone())
    }
}

fn entry(label: &str, flow: Option<&'static str>) -> ProviderKindEntry {
    ProviderKindEntry {
        protocol: stub_protocol(label),
        auth_flow: flow.map(|oauth_type| Arc::new(Flow(oauth_type)) as Arc<dyn ProviderAuthFlow>),
    }
}

#[test]
fn provider_kinds_reject_duplicate_label_alias_and_oauth_type() {
    let mut kinds = ProviderKinds::core();
    assert_eq!(
        kinds.labels().collect::<Vec<_>>(),
        ["openai", "openai-compatible", "openai-completion"]
    );
    assert!(kinds.protocol("openai-compatible").is_some());
    assert!(kinds.protocol("anthropic").is_none());

    kinds
        .register("hya/provider-x", entry("x", Some("x-oauth")))
        .unwrap();
    assert!(kinds.supports_oauth("x"));
    assert!(!kinds.supports_oauth("openai"));
    assert_eq!(kinds.auth_flow("x-oauth").unwrap().oauth_type(), "x-oauth");
    assert!(kinds.auth_flow("y-oauth").is_none());

    let label = kinds.register("hya/other", entry("x", None)).unwrap_err();
    assert_eq!(
        label.to_string(),
        "provider kind `x` from hya/other is already registered by hya/provider-x"
    );
    let alias = kinds
        .register("hya/other", entry("openai-completion", None))
        .unwrap_err();
    assert_eq!(
        (alias.name.as_str(), alias.existing.as_str()),
        ("openai-completion", "core")
    );
    let flow = kinds
        .register("hya/other", entry("y", Some("x-oauth")))
        .unwrap_err();
    assert_eq!(
        (flow.what, flow.name.as_str(), flow.existing.as_str()),
        ("OAuth type", "x-oauth", "hya/provider-x")
    );
    // A rejected entry registers nothing.
    assert!(kinds.protocol("y").is_none());
}
