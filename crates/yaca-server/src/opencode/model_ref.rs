use serde::{Deserialize, Serialize};
use yaca_proto::ModelRef;

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
        provider_id: "yaca".to_string(),
        model_id: raw.to_string(),
        variant,
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
