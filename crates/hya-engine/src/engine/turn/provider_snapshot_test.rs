use std::sync::{Arc, Mutex, Weak};

use hya_proto::{FinishReason, MessageId, ModelRef, SessionId};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider,
    ProviderCatalogSnapshot, ProviderError, ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules};

use super::{RequestLineage, SessionEngine};
use crate::EventBus;

struct PublishingProvider {
    engine: Mutex<Option<Weak<SessionEngine>>>,
    current: FakeProvider,
    replacement: Arc<ProviderRouter>,
}

#[async_trait::async_trait]
impl Provider for PublishingProvider {
    fn id(&self) -> &str {
        "publishing"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        // The first capabilities read is the engine's request preflight.
        // Publish inside it so stream open sees any accidental second read.
        let engine = self
            .engine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(engine) = engine.and_then(|engine| engine.upgrade()) {
            let catalog = ProviderCatalogSnapshot::build(Vec::new(), Vec::new(), None);
            engine.publish_provider_catalog(Arc::clone(&self.replacement), Arc::new(catalog));
        }
        self.current.capabilities(model)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.current.stream(request, session, message).await
    }
}

#[tokio::test]
async fn publish_during_preflight_uses_checked_router() -> Result<(), Box<dyn std::error::Error>> {
    let replacement = Arc::new(FakeProvider::scripted(vec![FakeStep::Finish(
        FinishReason::Stop,
    )]));
    let original = Arc::new(PublishingProvider {
        engine: Mutex::new(None),
        current: FakeProvider::scripted(vec![FakeStep::Finish(FinishReason::Stop)]),
        replacement: Arc::new(ProviderRouter::new().with(replacement.clone())),
    });
    let engine = Arc::new(SessionEngine::new(
        SessionStore::connect_memory().await?,
        Arc::new(ProviderRouter::new().with(original.clone())),
        crate::test_support::runtime(hya_tool_loader::from_tool_families(&[])),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    ));
    *original
        .engine
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::downgrade(&engine));
    let request = CompletionRequest {
        model: ModelRef::new("fake/model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let (stream, _) = engine
        .stream_with_model_fallback(
            &engine.provider_router(),
            &request,
            SessionId::new(),
            MessageId::new(),
            RequestLineage {
                agent: "hya-main",
                root_session_cache: &mut None,
            },
        )
        .await?;
    drop(stream);
    assert_eq!(
        original.current.requests().len(),
        1,
        "preflight and open must use one router"
    );
    assert!(replacement.requests().is_empty());
    Ok(())
}

#[tokio::test]
async fn handoff_uses_provider_published_after_the_first_handoff()
-> Result<(), Box<dyn std::error::Error>> {
    let model = ModelRef::new("fake/model");
    let original = Arc::new(FakeProvider::scripted(vec![FakeStep::Text(
        "original".into(),
    )]));
    let replacement = Arc::new(FakeProvider::scripted(vec![FakeStep::Text(
        "replacement".into(),
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await?,
        Arc::new(ProviderRouter::new().with(original.clone())),
        crate::test_support::runtime(hya_tool_loader::from_tool_families(&[])),
        PermissionPlane::new(PermissionRules::default()).0,
        EventBus::default(),
    );
    let summarizer = crate::ModelSummarizer::for_engine(&engine, model.clone());
    let engine = engine.with_terminal_handoff(
        Arc::new(summarizer),
        crate::TerminalHandoffOptions::default(),
    );
    let session = engine
        .create(crate::CreateSession {
            parent: None,
            agent: "hya-main".into(),
            model,
            workdir: ".".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        })
        .await?;
    assert_eq!(engine.terminal_handoff(session).await?.doc, "original");
    let clone = engine.clone();
    let previous = clone.provider_snapshot();
    let catalog = Arc::new(ProviderCatalogSnapshot::build(Vec::new(), Vec::new(), None));
    let router = Arc::new(ProviderRouter::new().with(replacement.clone()));
    engine.publish_provider_catalog(Arc::clone(&router), Arc::clone(&catalog));
    let current = clone.provider_snapshot();
    assert_eq!(previous.generation(), 0);
    assert_eq!(current.generation(), 1);
    assert!(std::ptr::eq(current.router(), router.as_ref()));
    assert!(std::ptr::eq(current.catalog(), catalog.as_ref()));
    assert_eq!(clone.terminal_handoff(session).await?.doc, "replacement");
    assert_eq!(original.requests().len(), 1);
    assert_eq!(replacement.requests().len(), 1);
    Ok(())
}
