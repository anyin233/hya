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
        self.resolve_route(model).map(|route| route.provider)
    }

    fn resolve_route(&self, model: &ModelRef) -> Option<ProviderRoute> {
        if let Some((provider_id, model_id)) = split_provider_model(model) {
            let routed = ModelRef::new(model_id);
            if let Some(provider) = self
                .providers
                .iter()
                .find(|p| p.id() == provider_id && p.capabilities(&routed).is_some())
                .cloned()
            {
                return Some(ProviderRoute {
                    provider,
                    model: routed,
                });
            }
        }
        self.providers
            .iter()
            .find(|p| p.capabilities(model).is_some())
            .cloned()
            .map(|provider| ProviderRoute {
                provider,
                model: model.clone(),
            })
    }

    pub async fn stream(
        &self,
        mut req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let route = self
            .resolve_route(&req.model)
            .ok_or_else(|| ProviderError::UnknownModel(req.model.to_string()))?;
        if let Some(caps) = route.provider.capabilities(&route.model) {
            crate::preflight(&caps, &req)?;
            if !caps.reasoning_request {
                req.reasoning = None;
            }
        }
        req.model = route.model;
        route.provider.stream(req, session, message).await
    }
}

struct ProviderRoute {
    provider: Arc<dyn Provider>,
    model: ModelRef,
}

fn split_provider_model(model: &ModelRef) -> Option<(&str, &str)> {
    let (provider, model) = model.as_str().split_once('/')?;
    (!provider.trim().is_empty() && !model.trim().is_empty()).then_some((provider, model))
}
