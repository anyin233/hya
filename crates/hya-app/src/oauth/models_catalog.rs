//! Fetch provider model catalogs after OAuth login.

use crate::auth::OAuthType;

use super::OAuthError;

/// Fetch models for an OAuth provider using the just-obtained access token.
///
/// The provider-owned adapter performs endpoint construction, authentication,
/// bounded response/pagination handling, and normalization. Results are
/// process-local preview data; callers must not persist fetched or guessed ids
/// into empty provider lists.
pub async fn fetch_oauth_models(
    oauth_type: OAuthType,
    access_token: &str,
    account_id: Option<&str>,
    base_url: &str,
) -> Result<Vec<hya_provider::DiscoveredModel>, OAuthError> {
    let endpoint = match oauth_type {
        OAuthType::OpenaiCodex => OAuthType::OpenaiCodex.default_base_url(),
        OAuthType::GrokBuild => base_url,
    };
    let kinds = crate::provider_kinds::builtin().map_err(|e| OAuthError::Config(e.to_string()))?;
    let protocol = kinds
        .protocol(oauth_type.provider_kind())
        .cloned()
        .ok_or_else(|| {
            OAuthError::Config(format!(
                "provider kind `{}` is not registered",
                oauth_type.provider_kind()
            ))
        })?;
    let outcome = hya_provider::discover_models(hya_provider::CatalogDiscoveryRequest::new(
        "oauth-preview",
        protocol,
        endpoint,
        Some(hya_provider::RouteCredential {
            token: access_token.to_owned(),
            account_id: account_id.map(str::to_owned),
        }),
    ))
    .await;
    preview_models_from_outcome(outcome)
}

fn preview_models_from_outcome(
    outcome: hya_provider::ProviderDiscoveryOutcome,
) -> Result<Vec<hya_provider::DiscoveredModel>, OAuthError> {
    match outcome {
        hya_provider::ProviderDiscoveryOutcome::Discovered { models, .. } => Ok(models),
        hya_provider::ProviderDiscoveryOutcome::Empty { .. } => Err(OAuthError::Protocol(
            "OAuth model catalog returned no models".into(),
        )),
        hya_provider::ProviderDiscoveryOutcome::AuthRequired => Err(OAuthError::Protocol(
            "OAuth model catalog requires authentication".into(),
        )),
        hya_provider::ProviderDiscoveryOutcome::AuthRejected => Err(OAuthError::Protocol(
            "OAuth model catalog rejected the credential".into(),
        )),
        hya_provider::ProviderDiscoveryOutcome::Failed { error } => {
            Err(OAuthError::Protocol(error.to_string()))
        }
        hya_provider::ProviderDiscoveryOutcome::Unsupported { kind } => Err(OAuthError::Protocol(
            format!("OAuth model catalog does not support kind `{kind}`"),
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_catalog_with_shared_reasoning_normalization() {
        let payload = serde_json::json!({
            "models": [
                {
                    "slug": " gpt-5.6-sol ",
                    "default_reasoning_level": "low",
                    "supported_reasoning_levels": [
                        {"effort": "low"},
                        {"effort": "medium"},
                        {"effort": "high"}
                    ]
                },
                {"slug": "gpt-5.3-codex-spark"}
            ]
        });
        let shared =
            hya_provider::parse_catalog_payload(&hya_provider::OpenAiCodexProtocol, &payload)
                .unwrap();
        let models =
            preview_models_from_outcome(hya_provider::ProviderDiscoveryOutcome::Discovered {
                models: shared,
                auth: hya_provider::AuthPresence::Credentialed,
            })
            .unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert_eq!(models[0].reasoning_default.as_deref(), Some("low"));
        assert_eq!(models[0].reasoning_variants, vec!["low", "medium", "high"]);
        assert_eq!(models[1].id, "gpt-5.3-codex-spark");
        assert!(models[1].reasoning_variants.is_empty());
    }

    #[test]
    fn shared_grok_normalization_deduplicates_and_skips_media() {
        let payload = serde_json::json!({
            "object": "list",
            "data": [
                {"id": " grok-4.5 ", "reasoning_effort": "high", "reasoning_efforts": [
                    {"id": "high", "value": "high"},
                    {"id": "medium", "value": "medium"}
                ]},
                {"id": "grok-4.5"},
                {"id": "grok-imagine-image"},
                {"id": "grok-build-0.1"},
                " grok-string ",
                {"model": "grok-model"},
                {"name": "grok-name"},
            ]
        });
        let shared =
            hya_provider::parse_catalog_payload(&hya_provider::GrokBuildProtocol, &payload)
                .unwrap();
        let models =
            preview_models_from_outcome(hya_provider::ProviderDiscoveryOutcome::Discovered {
                models: shared,
                auth: hya_provider::AuthPresence::Credentialed,
            })
            .unwrap();
        let ids: Vec<_> = models.iter().map(|model| model.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "grok-4.5",
                "grok-build-0.1",
                "grok-string",
                "grok-model",
                "grok-name"
            ],
        );
        assert_eq!(models[0].reasoning_default.as_deref(), Some("high"));
        assert_eq!(models[0].reasoning_variants, vec!["high", "medium"]);
    }

    #[test]
    fn oauth_preview_keeps_failure_without_a_fallback_model() {
        let result = preview_models_from_outcome(hya_provider::ProviderDiscoveryOutcome::Failed {
            error: hya_provider::CatalogFailure::BodyTooLarge,
        });
        assert!(
            matches!(result, Err(OAuthError::Protocol(message)) if message.contains("too large"))
        );
    }
}
