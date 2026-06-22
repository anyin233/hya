use serde::Deserialize;
use yaca_proto::ModelRef;

#[derive(Clone)]
pub(super) struct OpenCodeModelRefParts {
    pub(super) provider_id: String,
    pub(super) model_id: String,
}

#[derive(Deserialize)]
pub(super) struct OpenCodeModelRefRequest {
    id: String,
    #[serde(rename = "providerID")]
    provider_id: String,
    #[serde(rename = "variant")]
    _variant: Option<String>,
}

impl OpenCodeModelRefRequest {
    pub(super) fn into_model_ref(self) -> ModelRef {
        ModelRef::new(format!("{}/{}", self.provider_id, self.id))
    }
}

pub(super) fn model_ref_parts(model: &ModelRef) -> OpenCodeModelRefParts {
    if let Some((provider_id, model_id)) = model.as_str().split_once('/') {
        return OpenCodeModelRefParts {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
        };
    }
    OpenCodeModelRefParts {
        provider_id: "yaca".to_string(),
        model_id: model.to_string(),
    }
}
