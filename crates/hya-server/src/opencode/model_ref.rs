use hya_proto::ModelRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider id [`model_ref_parts`] reports for a bare (prefix-less) model ref;
/// echoing it back must round-trip to a bare id so the router still resolves.
pub(super) const BARE_PROVIDER: &str = "hya";

#[derive(Clone)]
pub(super) struct OpenCodeModelRefParts {
    pub(super) provider_id: String,
    pub(super) model_id: String,
    pub(super) variant: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct OpenCodeModel {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    variant: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct OpenCodeModelRefRequest {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "variant")]
    variant: Option<String>,
}

impl OpenCodeModelRefRequest {
    pub(super) fn into_model_ref(self) -> ModelRef {
        let base = format!("{}/{}", self.provider_id, self.id);
        let Some(variant) = self.variant.filter(|variant| !variant.is_empty()) else {
            return ModelRef::new(base);
        };
        ModelRef::new(format!("{base}#{variant}"))
    }
}

pub(super) fn model_ref_parts(model: &ModelRef) -> OpenCodeModelRefParts {
    let (raw, variant) = split_variant(model.as_str());
    if let Some((provider_id, model_id)) = raw.split_once('/') {
        return OpenCodeModelRefParts {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            variant,
        };
    }
    OpenCodeModelRefParts {
        provider_id: BARE_PROVIDER.to_string(),
        model_id: raw.to_string(),
        variant,
    }
}

/// Parse the `model` field a client attaches to a prompt into a [`ModelRef`].
///
/// Accepts the OpenCode object form (`{ providerID, modelID | id, variant? }`) or
/// a bare/`provider/model` string. A `providerID` of [`BARE_PROVIDER`] is dropped
/// so the agent's default (prefix-less) model round-trips to a router-resolvable id.
pub(super) fn model_ref_from_value(value: &Value) -> Option<ModelRef> {
    match value {
        Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| ModelRef::new(trimmed))
        }
        Value::Object(map) => {
            let field = |key: &str| {
                map.get(key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            };
            let model_id = field("modelID").or_else(|| field("id"))?;
            let base = match field("providerID") {
                Some(provider) if provider != BARE_PROVIDER => format!("{provider}/{model_id}"),
                _ => model_id.to_string(),
            };
            Some(match field("variant") {
                Some(variant) => ModelRef::new(format!("{base}#{variant}")),
                None => ModelRef::new(base),
            })
        }
        _ => None,
    }
}

pub(super) fn model_info(model: &ModelRef) -> OpenCodeModel {
    let parts = model_ref_parts(model);
    OpenCodeModel {
        id: parts.model_id,
        provider_id: parts.provider_id,
        variant: parts.variant,
    }
}

fn split_variant(value: &str) -> (&str, Option<String>) {
    if let Some((base, variant)) = value.rsplit_once('#')
        && !variant.is_empty()
    {
        return (base, Some(variant.to_string()));
    }
    (value, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_provider_is_preserved() {
        assert_eq!(
            model_ref_from_value(&json!({ "providerID": "mock", "modelID": "claude-opus-4-8" })),
            Some(ModelRef::new("mock/claude-opus-4-8"))
        );
    }

    #[test]
    fn bare_provider_sentinel_round_trips_to_bare_id() {
        assert_eq!(
            model_ref_from_value(&json!({ "providerID": "hya", "modelID": "claude-sonnet-4-6" })),
            Some(ModelRef::new("claude-sonnet-4-6"))
        );
    }

    #[test]
    fn accepts_id_alias_and_variant() {
        assert_eq!(
            model_ref_from_value(&json!({ "providerID": "p", "id": "m", "variant": "high" })),
            Some(ModelRef::new("p/m#high"))
        );
    }

    #[test]
    fn string_and_missing_id_and_other_types() {
        assert_eq!(
            model_ref_from_value(&json!("mock/claude-opus-4-8")),
            Some(ModelRef::new("mock/claude-opus-4-8"))
        );
        assert_eq!(model_ref_from_value(&json!({ "providerID": "mock" })), None);
        assert_eq!(model_ref_from_value(&json!(null)), None);
    }
}
