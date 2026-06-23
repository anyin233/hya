use crate::config::ModelEntry;
use yaca_proto::ModelRef;
use yaca_tui::AppState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ModelIdentity {
    pub model: String,
    pub provider: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AmbiguousModel {
    pub model: String,
}

pub(super) fn normalize_model_ref(value: &str) -> ModelIdentity {
    split_provider_model(value).map_or_else(
        || ModelIdentity {
            model: value.trim().to_string(),
            provider: None,
        },
        |(provider, model)| ModelIdentity {
            model: model.to_string(),
            provider: Some(provider.to_string()),
        },
    )
}

pub(super) fn resolve_model_argument(
    models: &[ModelEntry],
    argument: &str,
) -> Result<ModelIdentity, AmbiguousModel> {
    let identity = normalize_model_ref(argument);
    if identity.provider.is_some() {
        return Ok(identity);
    }
    let mut matched_provider: Option<&str> = None;
    for entry in models.iter().filter(|entry| entry.id == identity.model) {
        let provider = entry.provider.trim();
        if provider.is_empty() {
            continue;
        }
        match matched_provider {
            None => matched_provider = Some(provider),
            Some(current) if current == provider => {}
            Some(_) => {
                return Err(AmbiguousModel {
                    model: identity.model,
                });
            }
        }
    }
    Ok(ModelIdentity {
        model: identity.model,
        provider: matched_provider.map(str::to_string),
    })
}

pub(super) fn model_ref_string(identity: &ModelIdentity) -> String {
    model_ref_from_parts(&identity.model, identity.provider.as_deref())
}

pub(super) fn app_fields(value: &str) -> (String, Option<String>) {
    let identity = normalize_model_ref(value);
    (identity.model, identity.provider)
}

pub(super) fn selected_ref(model: &str, provider: Option<&str>) -> ModelRef {
    ModelRef::new(model_ref_from_parts(model, provider))
}

pub(super) fn apply_ref(app: &mut AppState, value: &str) -> ModelRef {
    let identity = normalize_model_ref(value);
    app.set_model_identity(identity.model.clone(), identity.provider.clone());
    ModelRef::new(model_ref_string(&identity))
}

pub(super) fn model_ref_from_parts(model: &str, provider: Option<&str>) -> String {
    provider
        .filter(|provider| !provider.trim().is_empty())
        .map_or_else(
            || model.to_string(),
            |provider| format!("{provider}/{model}"),
        )
}

fn split_provider_model(value: &str) -> Option<(&str, &str)> {
    let (provider, model) = value.split_once('/')?;
    let provider = provider.trim();
    let model = model.trim();
    (!provider.is_empty() && !model.is_empty()).then_some((provider, model))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use crate::config::ModelEntry;

    use super::*;

    #[test]
    fn normalizes_provider_qualified_stored_model_ref() {
        // Given
        let stored = "openai/gpt-5";

        // When
        let identity = normalize_model_ref(stored);

        // Then
        assert_eq!(identity.model, "gpt-5");
        assert_eq!(identity.provider.as_deref(), Some("openai"));
        assert_eq!(model_ref_string(&identity), stored);
    }

    #[test]
    fn preserves_model_ids_that_contain_slashes_after_provider() {
        // Given
        let stored = "openrouter/vendor/model";

        // When
        let identity = normalize_model_ref(stored);

        // Then
        assert_eq!(identity.model, "vendor/model");
        assert_eq!(identity.provider.as_deref(), Some("openrouter"));
        assert_eq!(model_ref_string(&identity), stored);
    }

    #[test]
    fn resolves_unique_bare_model_to_catalog_provider() {
        // Given
        let models = vec![ModelEntry {
            id: "claude-sonnet".to_string(),
            provider: "anthropic".to_string(),
        }];

        // When
        let identity =
            resolve_model_argument(&models, "claude-sonnet").expect("unique bare model resolves");

        // Then
        assert_eq!(identity.model, "claude-sonnet");
        assert_eq!(identity.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn applies_provider_qualified_ref_to_app_state_and_runtime_model() {
        // Given
        let mut app = AppState::default();

        // When
        let model = apply_ref(&mut app, "openai/gpt-5");

        // Then
        assert_eq!(app.model, "gpt-5");
        assert_eq!(app.model_provider_label.as_deref(), Some("openai"));
        assert_eq!(model.as_str(), "openai/gpt-5");
    }
}
