#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use yaca_proto::{Event, FinishReason, MessageId, ModelRef, SessionId};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError, ProviderRouter,
};

struct NamedProvider {
    id: &'static str,
    seen_models: Arc<Mutex<Vec<String>>>,
}

impl NamedProvider {
    fn new(id: &'static str, seen_models: Arc<Mutex<Vec<String>>>) -> Self {
        Self { id, seen_models }
    }
}

#[async_trait]
impl Provider for NamedProvider {
    fn id(&self) -> &str {
        self.id
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "shared").then(Capabilities::default)
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        _session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.seen_models
            .lock()
            .expect("seen model lock")
            .push(req.model.as_str().to_string());
        let session = SessionId::new();
        let event = Event::MessageFinished {
            session,
            message,
            finish: FinishReason::Stop,
        };
        Ok(Box::pin(stream::iter([Ok(event)])))
    }
}

#[test]
fn provider_qualified_model_resolves_to_named_provider() {
    let first_seen = Arc::new(Mutex::new(Vec::new()));
    let second_seen = Arc::new(Mutex::new(Vec::new()));
    let router = ProviderRouter::new()
        .with(Arc::new(NamedProvider::new("first", first_seen)))
        .with(Arc::new(NamedProvider::new("second", second_seen)));

    let provider = router
        .resolve(&ModelRef::new("second/shared"))
        .expect("provider-qualified model should resolve");

    assert_eq!(provider.id(), "second");
}

#[tokio::test]
async fn provider_qualified_stream_passes_bare_model_to_provider() {
    let first_seen = Arc::new(Mutex::new(Vec::new()));
    let second_seen = Arc::new(Mutex::new(Vec::new()));
    let router = ProviderRouter::new()
        .with(Arc::new(NamedProvider::new("first", first_seen.clone())))
        .with(Arc::new(NamedProvider::new("second", second_seen.clone())));
    let req = CompletionRequest {
        model: ModelRef::new("second/shared"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
    };

    let _stream = router
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .expect("provider-qualified model should stream");

    assert!(first_seen.lock().unwrap().is_empty());
    assert_eq!(second_seen.lock().unwrap().as_slice(), ["shared"]);
}
