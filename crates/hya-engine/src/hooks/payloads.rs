use hya_proto::{AgentName, MessageId, ModelRef, PartId, SessionId, ToolCallId};
use hya_provider::{CompletionRequest, ProviderError};
use hya_tool::{Action, Resource};
use serde_json::Value;

/// Input to `permission_approve`: one permission ask under a bundle-declared
/// session permission mode.
#[derive(Clone, Debug)]
pub struct PermissionApproveInput {
    /// Session whose tool call triggered the ask (a subagent session for
    /// subagent asks).
    pub session: SessionId,
    /// Root of the session tree that owns the permission mode.
    pub root_session: SessionId,
    /// Stable id of the agent bound to `session`, when known.
    pub agent: Option<AgentName>,
    /// Bundle-local mode id (the `id` of the `permission_modes:` entry).
    pub mode: String,
    /// Permission action being requested.
    pub action: Action,
    /// Subject of the ask.
    pub resource: Resource,
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
    /// Root of `session`'s spawn tree (the request chain); equals `session`
    /// for a root session.
    pub root_session: SessionId,
    /// Stable id of the agent bound to `session`, when known.
    pub agent: Option<AgentName>,
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

/// Class of a pre-stream provider failure, as reported to `model.fallback`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelFailureClass {
    /// Transport failure, HTTP 429, or HTTP 5xx
    /// ([`ProviderError::is_retryable_before_stream`]).
    Retryable,
    /// No provider route claims the model.
    UnknownModel,
    /// Expired credentials, or HTTP 401/403.
    Auth,
    /// Another HTTP 4xx, or a route that cannot serve this request.
    InvalidRequest,
    /// Anything else (decode, JSON, provider error frame, other statuses).
    Other,
}

impl ModelFailureClass {
    /// Classify a provider error returned before any stream existed.
    #[must_use]
    pub fn of(error: &ProviderError) -> Self {
        if error.is_retryable_before_stream() {
            return Self::Retryable;
        }
        match error {
            ProviderError::UnknownModel(_) => Self::UnknownModel,
            ProviderError::AuthExpired { .. }
            | ProviderError::HttpStatus {
                status: 401 | 403, ..
            } => Self::Auth,
            ProviderError::HttpStatus { status, .. } if (400..500).contains(status) => {
                Self::InvalidRequest
            }
            ProviderError::Incompatible(_) => Self::InvalidRequest,
            _ => Self::Other,
        }
    }

    /// Wire spelling (`retryable`, `unknown_model`, `auth`,
    /// `invalid_request`, `other`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::UnknownModel => "unknown_model",
            Self::Auth => "auth",
            Self::InvalidRequest => "invalid_request",
            Self::Other => "other",
        }
    }
}

/// Input to `model_fallback`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFallbackInput {
    /// Session making the completion.
    pub session: SessionId,
    /// Root of `session`'s spawn tree; equals `session` for a root.
    pub root_session: SessionId,
    /// Stable id of the agent bound to `session`, when known.
    pub agent: Option<AgentName>,
    /// Assistant message being prepared.
    pub message: MessageId,
    /// Model whose attempt just failed.
    pub model: ModelRef,
    /// Class of the failure.
    pub error_class: ModelFailureClass,
    /// Display text of the failure.
    pub error_message: String,
    /// 1-based count of failed attempts so far in this round.
    pub attempt: u32,
    /// Models already attempted in this round, in order (all failed).
    pub tried: Vec<ModelRef>,
}

/// Outcome of `model_fallback`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelFallbackOutcome {
    /// Attempt the completion again on `model`.
    Retry {
        /// Next model to try.
        model: ModelRef,
    },
    /// No opinion: let the next hook decide, or surface the failure.
    GiveUp,
}

/// Native tool result shape passed through after-hooks.
#[derive(Clone, Debug, PartialEq)]
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

/// Why the engine is about to compact context (input to `compaction_before`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// The transcript is over its resolved window threshold. Compaction must
    /// not be blocked: sending the request un-compacted risks context
    /// overflow, so a `Skip` is demoted to a warning.
    Overflow,
    /// Pre-emptive compaction while still under the threshold. A `Skip` may be
    /// honored.
    Proactive,
}

/// Engine-facing outcome of `compaction_before`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionDecision {
    /// Run the built-in compaction unchanged.
    Proceed,
    /// Request that compaction be skipped, with a reason for the warning.
    Skip {
        /// Human-visible reason the hook asked to skip.
        reason: String,
    },
    /// Run compaction, with summarizer calls using these instructions instead
    /// of the built-in summary template.
    Replace {
        /// Instructions for the summarizer prompt.
        instructions: String,
    },
}

/// Input to `compaction_before`.
pub struct CompactionBeforeInput {
    /// Session whose context is about to be folded.
    pub session: SessionId,
    /// Why compaction is running.
    pub trigger: CompactionTrigger,
    /// Estimated token occupancy of the transcript about to be compacted.
    pub messages_token_estimate: usize,
}

/// Input to `compaction_after`.
pub struct CompactionAfterInput {
    /// Session whose context was folded.
    pub session: SessionId,
    /// Estimated token size of the summary that was committed.
    pub summary_tokens: usize,
}

/// Input to `session_start` and `session_end`.
pub struct SessionLifecycleInput {
    /// Session that was created or closed.
    pub session: SessionId,
}

/// Input to `agent_spawn`.
pub struct AgentSpawnInput {
    /// Session of the parent (the roster-owning team root).
    pub parent: SessionId,
    /// Session of the freshly registered child.
    pub child: SessionId,
}

/// Engine-facing outcome of dispatching `goal.evaluate` to a plugin provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalEvaluateReply {
    /// A provider returned a well-formed verdict object.
    Verdict {
        /// Whether the goal condition is satisfied by the transcript.
        met: bool,
        /// Short reason for logs and the next directive.
        reason: String,
    },
    /// A provider replied, but the verdict object was malformed. Drivers must
    /// count this as not-met so a broken evaluator still consumes an
    /// iteration of the cap instead of erroring the run or looping forever.
    Malformed,
}
