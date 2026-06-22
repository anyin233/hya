#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use yaca_core::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    CreateSession, EventBus, HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome,
    SessionEngine, ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome,
};
use yaca_proto::{AgentName, Envelope, Event, ModelRef, PartProjection, Role};
use yaca_provider::{DevProvider, ProviderRouter};
use yaca_store::SessionStore;
use yaca_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolRegistry};

struct CommandMutatingHost;

#[async_trait::async_trait]
impl HookDispatcher for CommandMutatingHost {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        assert_eq!(input.command, "review");
        assert_eq!(input.arguments, "commit");
        CommandExecuteBeforeOutcome::Continue {
            text: format!("{} with hook context", input.text),
        }
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
async fn command_execute_before_mutates_text_before_user_message_is_admitted() {
    // Given: a session engine with a command hook that appends command context.
    let router = Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new())));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Read,
        "*",
        Mode::Allow,
    )]));
    let engine = SessionEngine::new(
        SessionStore::connect_memory().await.unwrap(),
        router,
        tools,
        permission,
        EventBus::default(),
    )
    .with_hooks(Arc::new(CommandMutatingHost));
    let session = engine
        .create(CreateSession {
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("offline"),
            workdir: ".".to_string(),
        })
        .await
        .unwrap();

    // When: a named command prompt is admitted.
    engine
        .admit_command_prompt(
            session,
            "review".to_string(),
            "commit".to_string(),
            "Review the diff".to_string(),
        )
        .await
        .unwrap();

    // Then: the stored user message contains the command hook mutation.
    let projection = engine.read_projection(session).await.unwrap();
    let user = projection
        .session
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .expect("user message");
    assert!(user.parts.iter().any(|part| {
        matches!(
            part,
            PartProjection::Text { text, .. } if text == "Review the diff with hook context"
        )
    }));
    let envelopes = engine.replay(session).await.unwrap();
    assert!(envelopes.iter().any(|envelope| {
        matches!(
            &envelope.event,
            Event::CommandExecuted {
                session: event_session,
                command,
                arguments,
                message
            } if *event_session == session
                && command == "review"
                && arguments == "commit"
                && *message == user.id
        )
    }));
}
