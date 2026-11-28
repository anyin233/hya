//! Provider kinds the runtime routes: the core OpenAI-compatible kind plus
//! the kinds of every enabled provider bundle.

use std::sync::Arc;

use hya_bundle::BuiltinBundleSelection;
use hya_provider::{
    AnthropicMessagesProtocol, GoogleProtocol, GrokBuildProtocol, OpenAiCodexProtocol,
    OpenAiResponsesProtocol, ProviderKindEntry, ProviderKinds,
};

use crate::oauth::{CodexAuthFlow, GrokAuthFlow};

/// The core kinds plus the kinds of every provider bundle `selection`
/// enables. Until the providers move into their bundles, `hya-provider`
/// still compiles their protocols in; each is registered under the bundle
/// id that will provide it, so the selection already decides.
///
/// # Errors
/// Two kinds share a label, alias, or OAuth type.
pub fn load(selection: &BuiltinBundleSelection) -> anyhow::Result<Arc<ProviderKinds>> {
    let mut kinds = ProviderKinds::core();
    let entries: [(&str, ProviderKindEntry); 5] = [
        (
            "hya/provider-anthropic",
            ProviderKindEntry {
                protocol: Arc::new(AnthropicMessagesProtocol),
                auth_flow: None,
            },
        ),
        (
            "hya/provider-google",
            ProviderKindEntry {
                protocol: Arc::new(GoogleProtocol),
                auth_flow: None,
            },
        ),
        (
            "hya/provider-openai",
            ProviderKindEntry {
                protocol: Arc::new(OpenAiResponsesProtocol),
                auth_flow: None,
            },
        ),
        (
            "hya/provider-openai",
            ProviderKindEntry {
                protocol: Arc::new(OpenAiCodexProtocol),
                auth_flow: Some(Arc::new(CodexAuthFlow)),
            },
        ),
        (
            "hya/provider-grok",
            ProviderKindEntry {
                protocol: Arc::new(GrokBuildProtocol),
                auth_flow: Some(Arc::new(GrokAuthFlow)),
            },
        ),
    ];
    for (bundle, entry) in entries {
        if selection.is_enabled(bundle) {
            kinds.register(bundle, entry)?;
        }
    }
    Ok(Arc::new(kinds))
}

/// [`load`] for the active `config.yaml`'s bundle selection.
///
/// # Errors
/// Same as [`load`].
pub fn configured() -> anyhow::Result<Arc<ProviderKinds>> {
    load(&crate::config::configured_builtin_bundles())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn labels(selection: &BuiltinBundleSelection) -> Vec<String> {
        load(selection)
            .unwrap()
            .labels()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn bare_selection_resolves_only_core_kinds() {
        let core = ["openai", "openai-compatible", "openai-completion"];
        assert_eq!(labels(&BuiltinBundleSelection::bare()), core);

        let mut anthropic = BuiltinBundleSelection::bare();
        anthropic.enable.push("hya/provider-anthropic".into());
        assert_eq!(
            labels(&anthropic),
            [
                "anthropic",
                "openai",
                "openai-compatible",
                "openai-completion"
            ]
        );

        let full = load(&BuiltinBundleSelection::full()).unwrap();
        for label in [
            "anthropic",
            "google",
            "openai-response",
            "openai-codex",
            "grok-build",
        ] {
            assert!(full.protocol(label).is_some(), "{label}");
        }
        assert!(full.supports_oauth("openai-codex") && full.supports_oauth("grok-build"));
        assert!(!full.supports_oauth("openai-response"));
        for oauth_type in ["openai-codex", "grok-build"] {
            assert_eq!(full.auth_flow(oauth_type).unwrap().oauth_type(), oauth_type);
        }
        assert!(full.auth_flow("codex").is_none() && full.auth_flow("grok").is_none());

        let mut no_grok = BuiltinBundleSelection::full();
        no_grok.disable.push("hya/provider-grok".into());
        let no_grok = load(&no_grok).unwrap();
        assert!(
            no_grok.protocol("grok-build").is_none() && no_grok.auth_flow("grok-build").is_none()
        );
    }
}
