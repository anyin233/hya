#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use hya_core::{
    AgentSpec, ChatParamsInput, ChatParamsOutcome, CreateSession, EventBus, HookDispatcher,
    MessageUserBeforeInput, MessageUserBeforeOutcome, SessionEngine, TextCompleteInput,
    TextCompleteOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use hya_proto::{AgentName, Envelope, Event, FinishReason, ModelRef, PartProjection, Role};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use tokio_util::sync::CancellationToken;

struct TextCompleteHost;

#[async_trait::async_trait]
impl HookDispatcher for TextCompleteHost {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        assert_eq!(input.text, "draft");
        TextCompleteOutcome::Continue {
            text: "final".to_string(),
        }
    }

    async fn command_execute_before(
        &self,
        input: hya_core::CommandExecuteBeforeInput,
    ) -> hya_core::CommandExecuteBeforeOutcome {
        hya_core::CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }
}

#[tokio::test]
async fn text_complete_replaces_assistant_text_before_projection_finishes() {
    // Given: a provider emits a draft text block and a text-complete hook rewrites it.
    let dir = PathBuf::from(".");
    let provider = FakeProvider::scripted(vec![
        FakeStep::Text("draft".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let router = Arc::new(ProviderRouter::new().with(Arc::new(provider)));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        tools,
        permission,
        EventBus::default(),
    )
    .with_hooks(Arc::new(TextCompleteHost));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: ".".to_string(),
        })
        .await
        .unwrap();
    engine
        .admit_user_prompt(session, "say it".to_string())
        .await
        .unwrap();
    let agent = AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new("fake"),
        system_prompt: "you are build".to_string(),
        workdir: dir,
        reasoning: None,
    };

    // When: the assistant turn completes its streamed text part.
    engine
        .run_turn(session, &agent, CancellationToken::new())
        .await
        .unwrap();

    // Then: projection and replay both record the hook's replacement text.
    let projection = engine.read_projection(session).await.unwrap();
    let assistant = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::Assistant)
        .expect("assistant message");
    assert!(
        assistant
            .parts
            .iter()
            .any(|part| { matches!(part, PartProjection::Text { text, .. } if text == "final") })
    );
    assert!(
        engine
            .replay(session)
            .await
            .unwrap()
            .iter()
            .any(|envelope| matches!(envelope.event, Event::TextReplace { .. }))
    );
}
