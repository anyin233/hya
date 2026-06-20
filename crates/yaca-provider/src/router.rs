use std::sync::Arc;

use yaca_proto::{MessageId, ModelRef, SessionId};

use crate::{CompletionRequest, EventStream, Provider, ProviderError};

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

    pub async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let provider = self
            .resolve(&req.model)
            .ok_or_else(|| ProviderError::UnknownModel(req.model.to_string()))?;
        if let Some(caps) = provider.capabilities(&req.model) {
            crate::preflight(&caps, &req)?;
        }
        provider.stream(req, session, message).await
    }
}
