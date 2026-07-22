use std::sync::Arc;

use hya_proto::{Message, MessageId, ModelRef, SessionId};

use crate::{
    CompactedWindow, CompletionRequest, EventStream, Provider, ProviderError, ProviderModel,
};

#[derive(Default, Clone)]
pub struct ProviderRouter {
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderRouter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, provider: Arc<dyn Provider>) -> Self {
        self.providers.push(provider);
        self
    }

    #[must_use]
    pub fn resolve(&self, model: &ModelRef) -> Option<Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| p.capabilities(model).is_some())
            .cloned()
    }

    #[must_use]
    pub fn catalog(&self) -> Vec<ProviderModel> {
        let mut models: Vec<_> = self.providers.iter().flat_map(|p| p.catalog()).collect();
        models.sort_by(|a, b| {
            a.provider_id
                .cmp(&b.provider_id)
                .then(a.model_id.cmp(&b.model_id))
        });
        models.dedup_by(|a, b| a.provider_id == b.provider_id && a.model_id == b.model_id);
        models
    }

    pub async fn stream(
        &self,
        mut req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let provider = self
            .resolve(&req.model)
            .ok_or_else(|| ProviderError::UnknownModel(req.model.to_string()))?;
        if let Some(caps) = provider.capabilities(&req.model) {
            crate::preflight(&caps, &req)?;
            if !caps.reasoning_request {
                req.reasoning = None;
            }
        }
        provider.stream(req, session, message).await
    }

    /// Compact via the resolved provider's `/responses/compact` when available.
    ///
    /// Returns `Ok(None)` when the route has no compact support (caller falls back).
    pub async fn compact_if_supported(
        &self,
        model: &ModelRef,
        messages: &[Message],
        system: Option<&str>,
    ) -> Result<Option<CompactedWindow>, ProviderError> {
        let provider = self
            .resolve(model)
            .ok_or_else(|| ProviderError::UnknownModel(model.to_string()))?;
        provider.compact_responses(model, messages, system).await
    }
}
