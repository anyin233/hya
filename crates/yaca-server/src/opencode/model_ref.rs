use serde::Deserialize;
use yaca_proto::ModelRef;

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
