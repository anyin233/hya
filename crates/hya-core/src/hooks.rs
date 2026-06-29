//! Engine-facing hook dispatch: the trait the turn loop calls (implemented
//! out-of-process by `hya-plugin`) plus the native payload/outcome types.

use async_trait::async_trait;
use hya_proto::{Envelope, MessageId, PartId, SessionId, ToolCallId};
use hya_provider::CompletionRequest;
use serde_json::Value;

#[async_trait]
pub trait HookDispatcher: Send + Sync {
    fn dispatch_event(&self, envelope: &Envelope);
    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome;
    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome;
    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome;
    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome;
    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome;
    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome;
}

pub struct CommandExecuteBeforeInput {
    pub session: SessionId,
    pub command: String,
    pub arguments: String,
    pub text: String,
}

pub enum CommandExecuteBeforeOutcome {
    Continue { text: String },
}

pub struct TextCompleteInput {
    pub session: SessionId,
    pub message: MessageId,
    pub part: PartId,
    pub text: String,
}

pub enum TextCompleteOutcome {
    Continue { text: String },
}

pub struct MessageUserBeforeInput {
    pub session: SessionId,
    pub text: String,
}

pub enum MessageUserBeforeOutcome {
    Continue { text: String },
}

pub struct ChatParamsInput {
    pub session: SessionId,
    pub message: MessageId,
    pub request: CompletionRequest,
}

pub enum ChatParamsOutcome {
    Continue { request: CompletionRequest },
}

pub struct ToolExecuteBeforeInput {
    pub session: SessionId,
    pub message: MessageId,
    pub call: ToolCallId,
    pub tool: String,
    pub input: Value,
}

pub enum ToolExecuteBeforeOutcome {
    Continue { input: Value },
    Veto { reason: String },
}

pub enum ToolOutcomeNative {
    Ok { output: Value, time_ms: u64 },
    Err { message: String },
}

pub struct ToolExecuteAfterInput {
    pub session: SessionId,
    pub message: MessageId,
    pub call: ToolCallId,
    pub tool: String,
    pub input: Value,
    pub result: ToolOutcomeNative,
}

pub enum ToolExecuteAfterOutcome {
    Continue { result: ToolOutcomeNative },
}

pub struct NoopHookHost;

#[async_trait]
impl HookDispatcher for NoopHookHost {
    fn dispatch_event(&self, _envelope: &Envelope) {}

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        TextCompleteOutcome::Continue { text: input.text }
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
