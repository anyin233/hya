#![allow(clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use tokio_util::sync::CancellationToken;
use yaca_core::{AgentSpec, CreateSession, EventBus, SessionEngine};
use yaca_proto::{AgentName, FinishReason, Message, MessageId, ModelRef, Part, SessionId};
use yaca_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use yaca_store::SessionStore;
use yaca_tool::{PermissionPlane, PermissionRules, ToolRegistry};

struct RecordingProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.requests.lock().expect("requests").push(req);
        let events = FakeProvider::materialize(
            &[
                FakeStep::Text("assistant response".to_string()),
                FakeStep::Finish(FinishReason::Stop),
            ],
            session,
            message,
        );
        Ok(Box::pin(stream::iter(
            events.into_iter().map(Ok::<_, ProviderError>),
        )))
    }
}

fn request_text(req: &CompletionRequest) -> String {
    let mut out = String::new();
    for message in &req.messages {
        match message {
            Message::System { content, .. } => out.push_str(content),
            Message::User { parts, .. } | Message::Assistant { parts, .. } => {
                for part in parts {
                    if let Part::Text { text, .. } = part {
                        out.push_str(text);
                    }
                }
            }
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
async fn compact_context_prunes_prior_messages_from_next_provider_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingProvider {
        requests: requests.clone(),
    };
    let router = ProviderRouter::new().with(Arc::new(provider));
    let store = SessionStore::connect_memory().await.expect("store");
    let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        store,
        Arc::new(router),
        Arc::new(ToolRegistry::builtins()),
        permission,
        EventBus::default(),
    );
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("model-a"),
        reasoning: None,
        system_prompt: "base".to_string(),
        workdir: ".".into(),
    };
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: ".".to_string(),
        })
        .await
        .expect("session");

    engine
        .admit_user_prompt(session, "old prompt".to_string())
        .await
        .expect("old prompt");
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .expect("first turn");
    engine
        .compact_context(session, "summary of old work".to_string())
        .await
        .expect("compact");
    engine
        .admit_user_prompt(session, "new prompt".to_string())
        .await
        .expect("new prompt");
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .expect("second turn");

    let requests = requests.lock().expect("requests");
    let second = requests.last().expect("second request");
    let text = request_text(second);
    assert!(text.contains("summary of old work"));
    assert!(text.contains("new prompt"));
    assert!(!text.contains("old prompt"));
}
