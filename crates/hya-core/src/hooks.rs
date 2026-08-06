//! Engine-facing hook dispatch: the trait the turn loop calls (implemented
//! out-of-process by `hya-plugin`) plus the native payload/outcome types.

use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;

use hya_proto::{Envelope, MessageId, PartId, SessionId, ToolCallId};
use hya_provider::CompletionRequest;
use serde_json::Value;

/// Host-implemented hooks the turn loop awaits around chat and tools.
///
/// **Contract for implementors:**
/// - Methods are invoked on the turn path; they must not block the runtime
///   indefinitely without respecting cancellation upstream.
/// - `*_before` outcomes may rewrite payloads (`Continue { … }`) or, for tools,
///   veto with a reason. The engine applies the returned payload and does not
///   re-read the original after a continue.
/// - `tool_execute_after` may rewrite success/error outcomes **except** the engine
///   preserves permission failures from being masked (callers must not rely on
///   rewriting denials).
/// - `dispatch_event` is fire-and-forget for live envelopes; failures should not
///   panic the host.
/// - `is_healthy` defaults to true; returning false cancels the turn when checked
///   after activation hooks.
#[async_trait]
pub trait HookDispatcher: Send + Sync {
    /// Observe a live envelope after it is published on the bus.
    fn dispatch_event(&self, envelope: &Envelope);
    /// Whether the host is still healthy enough to continue the turn.
    fn is_healthy(&self) -> bool {
        true
    }
    /// Rewrite shell/command text before execution.
    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome;
    /// Rewrite assistant text after a completed text part.
    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome;
    /// Rewrite user text before it is admitted as a message.
    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome;
    /// Adjust completion request parameters before the provider call.
    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome;
    /// Rewrite or veto tool arguments before execution.
    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome;
    /// Rewrite tool results or error messages after execution.
    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome;
}

#[derive(Clone)]
struct ActivationHookContext {
    session: SessionId,
    hooks: Arc<dyn HookDispatcher>,
}

tokio::task_local! {
    static ACTIVATION_HOOK_CONTEXT: ActivationHookContext;
}

pub(crate) async fn scope_activation_hooks<F, T>(
    session: SessionId,
    hooks: Arc<dyn HookDispatcher>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    ACTIVATION_HOOK_CONTEXT
        .scope(ActivationHookContext { session, hooks }, future)
        .await
}

pub(crate) fn activation_hook_for(session: SessionId) -> Option<Arc<dyn HookDispatcher>> {
    ACTIVATION_HOOK_CONTEXT
        .try_with(|context| (context.session == session).then(|| Arc::clone(&context.hooks)))
        .ok()
        .flatten()
}

pub(crate) fn dispatch_activation_event(envelope: &Envelope) {
    if let Some(session) = envelope.event.session()
        && let Some(hooks) = activation_hook_for(session)
    {
        hooks.dispatch_event(envelope);
    }
}

/// Input to `command_execute_before`.
pub struct CommandExecuteBeforeInput {
    /// Session executing the command.
    pub session: SessionId,
    /// Command name / binary.
    pub command: String,
    /// Argument string.
    pub arguments: String,
    /// Full command text the engine will run unless rewritten.
    pub text: String,
}

/// Outcome of `command_execute_before`.
pub enum CommandExecuteBeforeOutcome {
    /// Proceed with (possibly rewritten) `text`.
    Continue {
        /// Command text after hooks.
        text: String,
    },
}

/// Input to `text_complete`.
pub struct TextCompleteInput {
    /// Session owning the message.
    pub session: SessionId,
    /// Message id.
    pub message: MessageId,
    /// Text part id.
    pub part: PartId,
    /// Completed text.
    pub text: String,
}

/// Outcome of `text_complete`.
pub enum TextCompleteOutcome {
    /// Proceed with (possibly rewritten) text.
    Continue {
        /// Final text to store/project.
        text: String,
    },
}

/// Input to `message_user_before`.
pub struct MessageUserBeforeInput {
    /// Session receiving the user message.
    pub session: SessionId,
    /// Raw user text.
    pub text: String,
}

/// Outcome of `message_user_before`.
pub enum MessageUserBeforeOutcome {
    /// Proceed with (possibly rewritten) user text.
    Continue {
        /// Text to admit.
        text: String,
    },
}

/// Input to `chat_params`.
pub struct ChatParamsInput {
    /// Session for the completion.
    pub session: SessionId,
    /// Assistant message being built.
    pub message: MessageId,
    /// Provider request about to be sent.
    pub request: CompletionRequest,
}

/// Outcome of `chat_params`.
pub enum ChatParamsOutcome {
    /// Proceed with (possibly rewritten) request.
    Continue {
        /// Completion request after hooks.
        request: CompletionRequest,
    },
}

/// Input to `tool_execute_before`.
pub struct ToolExecuteBeforeInput {
    /// Session executing the tool.
    pub session: SessionId,
    /// Assistant message containing the call.
    pub message: MessageId,
    /// Tool-call id.
    pub call: ToolCallId,
    /// Canonical tool name.
    pub tool: String,
    /// Tool arguments JSON.
    pub input: Value,
}

/// Outcome of `tool_execute_before`.
pub enum ToolExecuteBeforeOutcome {
    /// Execute with (possibly rewritten) input.
    Continue {
        /// Arguments after hooks.
        input: Value,
    },
    /// Block execution; engine records a blocked/error outcome with `reason`.
    Veto {
        /// Human/model-visible veto reason.
        reason: String,
    },
}

/// Native tool result shape passed through after-hooks.
pub enum ToolOutcomeNative {
    /// Successful tool JSON and elapsed milliseconds.
    Ok {
        /// Tool output value.
        output: Value,
        /// Execution time in milliseconds.
        time_ms: u64,
    },
    /// Failed tool with a display message.
    Err {
        /// Error message string.
        message: String,
    },
}

/// Input to `tool_execute_after`.
pub struct ToolExecuteAfterInput {
    /// Session that ran the tool.
    pub session: SessionId,
    /// Assistant message id.
    pub message: MessageId,
    /// Tool-call id.
    pub call: ToolCallId,
    /// Tool name.
    pub tool: String,
    /// Arguments that were executed.
    pub input: Value,
    /// Result before after-hooks.
    pub result: ToolOutcomeNative,
}

/// Outcome of `tool_execute_after`.
pub enum ToolExecuteAfterOutcome {
    /// Proceed with (possibly rewritten) result.
    Continue {
        /// Final native outcome.
        result: ToolOutcomeNative,
    },
}

/// Hook host that leaves all payloads unchanged.
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
