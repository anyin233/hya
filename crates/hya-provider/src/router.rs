//! Ordered list of [`Provider`] routes with first-match model resolution.

use std::sync::Arc;

use hya_proto::{Message, MessageId, ModelRef, SessionId};

use crate::{
    CompactedWindow, CompletionRequest, EventStream, Provider, ProviderError, ProviderModel,
};

/// Multiplexes multiple providers; the first route that claims a model wins.
#[derive(Default, Clone)]
pub struct ProviderRouter {
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderRouter {
    /// Empty router (no routes claim any model).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no providers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// One configured-identity blob per route, or `None` if any route fails closed.
    #[must_use]
    pub fn configured_identities_v1(&self) -> Option<Vec<Vec<u8>>> {
        let mut identities = Vec::with_capacity(self.providers.len());
        for provider in &self.providers {
            let identity = provider.configured_identity_v1()?;
            if identity.is_empty() {
                return None;
            }
            identities.push(identity);
        }
        Some(identities)
    }

    /// Append a provider route (registration order = resolve priority).
    #[must_use]
    pub fn with(mut self, provider: Arc<dyn Provider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// First provider whose `capabilities(model)` is `Some`.
    #[must_use]
    pub fn resolve(&self, model: &ModelRef) -> Option<Arc<dyn Provider>> {
        self.providers
            .iter()
            .find(|p| p.capabilities(model).is_some())
            .cloned()
    }

    /// Capabilities advertised by the route that claims `model`.
    ///
    /// Exposes `max_context` so compaction can scale its threshold to the real
    /// window instead of a flat constant.
    #[must_use]
    pub fn capabilities(&self, model: &ModelRef) -> Option<crate::Capabilities> {
        self.providers.iter().find_map(|p| p.capabilities(model))
    }

    /// Merged catalog rows from all routes, sorted and deduped by provider+model id.
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

    /// Resolve `req.model`, run [`crate::preflight`], clear unsupported reasoning, then stream.
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
