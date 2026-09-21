//! Engine-facing hook dispatch: the trait the turn loop calls (implemented
//! out-of-process by `hya-plugin`) plus the native payload/outcome types.

use async_trait::async_trait;
use std::future::Future;
use std::sync::Arc;

use hya_proto::{Envelope, MessageId, PartId, SessionId, ToolCallId};
use hya_provider::CompletionRequest;
use serde_json::Value;

use crate::error::CoreError;
use crate::loop_mode::{PlannerOutput, VerifierVerdict};

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
    /// Consulted before the engine compacts context.
    ///
    /// Implementors should be fail-open by construction: the engine additionally
    /// ignores a [`CompactionDecision::Skip`] on an overflow-forced trigger (see
    /// [`resolve_compaction_decision`]), so a skip can never push a request out
    /// over its window.
    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        let _ = input;
        CompactionDecision::Proceed
    }
    /// Notified after a compaction committed its summary. Best-effort; failures
    /// are the implementor's to log.
    async fn compaction_after(&self, input: CompactionAfterInput) {
        let _ = input;
    }
    /// Notified when a session is created. Best-effort; never fatal.
    async fn session_start(&self, input: SessionLifecycleInput) {
        let _ = input;
    }
    /// Notified when a session is closed (archived or deleted). Best-effort.
    async fn session_end(&self, input: SessionLifecycleInput) {
        let _ = input;
    }
    /// Notified when a subagent is registered under its parent. Best-effort.
    async fn agent_spawn(&self, input: AgentSpawnInput) {
        let _ = input;
    }
    /// Whether any hook provider is registered for `goal.evaluate`.
    ///
    /// Capability probe for goal-mode evaluator selection: callers build a
    /// plugin-backed [`crate::completion::PluginGoalEvaluator`] only when this
    /// returns true, and otherwise fail open to the built-in model evaluator.
    /// Default false.
    fn has_goal_evaluate(&self) -> bool {
        false
    }
    /// Evaluate a goal condition against a transcript through the registered
    /// `goal.evaluate` provider.
    ///
    /// **Contract:** evaluators, not guards — implementors fail open by
    /// trying the next registered provider when one errors, and report a
    /// reply that is not a parseable verdict as
    /// [`GoalEvaluateReply::Malformed`] instead of an error, so the driver
    /// counts it as not-met against the iteration cap.
    ///
    /// # Errors
    /// The default impl reports the hook as not registered. Implementors
    /// return an error only when no registered provider produced any verdict.
    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        let _ = (condition, transcript);
        Err(CoreError::Invalid(
            "goal.evaluate hook not registered".to_string(),
        ))
    }
    /// Grade a loop target against a transcript through the registered
    /// `loop.verifier` provider.
    ///
    /// Like [`Self::goal_evaluate`], this is an evaluator hook: implementors
    /// chain providers in load order and fail open on transport errors.
    ///
    /// # Errors
    /// The default impl reports the hook as not registered.
    async fn loop_verify(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<VerifierVerdict, CoreError> {
        let _ = (target, transcript);
        Err(CoreError::Invalid(
            "loop.verifier hook not registered".to_string(),
        ))
    }
    /// Plan the next loop directive through the registered `loop.planner`
    /// provider.
    ///
    /// # Errors
    /// The default impl reports the hook as not registered.
    async fn loop_plan(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        let _ = (target, history, last, planner_notes);
        Err(CoreError::Invalid(
            "loop.planner hook not registered".to_string(),
        ))
    }
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

/// Why the engine is about to compact context (input to `compaction_before`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// The transcript is over its resolved window threshold. Compaction must
    /// not be blocked: sending the request un-compacted risks context
    /// overflow, so a `Skip` is demoted to a warning (see
    /// [`resolve_compaction_decision`]).
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

/// A `compaction_before` decision resolved against its trigger: what the engine
/// should actually do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompactionResolution {
    /// Run the built-in compaction ladder.
    Proceed,
    /// Run the ladder; summarizer calls use these instructions.
    Replace {
        /// Instructions for the summarizer prompt.
        instructions: String,
    },
    /// Do not compact at all. Only ever produced for proactive triggers.
    Skip {
        /// Human-visible reason, for the audit log.
        reason: String,
    },
}

/// Fold a hook's [`CompactionDecision`] into an executable resolution.
///
/// Fail-open rule: a [`CompactionDecision::Skip`] is honored for
/// [`CompactionTrigger::Proactive`] but demoted to
/// [`CompactionResolution::Proceed`] (with a warning) for
/// [`CompactionTrigger::Overflow`] — an overflow-forced compaction that is
/// skipped would send the request out over the very window it was trying to
/// fit, so no hook may block it. `Proceed` and `Replace` pass through for both
/// triggers.
#[must_use]
pub fn resolve_compaction_decision(
    decision: CompactionDecision,
    trigger: CompactionTrigger,
) -> CompactionResolution {
    match (decision, trigger) {
        (CompactionDecision::Proceed, _) => CompactionResolution::Proceed,
        (CompactionDecision::Replace { instructions }, _) => {
            CompactionResolution::Replace { instructions }
        }
        (CompactionDecision::Skip { reason }, CompactionTrigger::Proactive) => {
            CompactionResolution::Skip { reason }
        }
        (CompactionDecision::Skip { reason }, CompactionTrigger::Overflow) => {
            tracing::warn!(
                %reason,
                "compaction.before skip ignored on overflow-forced compaction; \
                 proceeding with built-in compaction"
            );
            CompactionResolution::Proceed
        }
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn last_verdict() -> VerifierVerdict {
        VerifierVerdict {
            score: 50,
            satisfied: false,
            evidence_quality: crate::loop_mode::EvidenceQuality::ClaimOnly,
            critical_gaps: Vec::new(),
            iteration_summary: String::new(),
            reason: String::new(),
        }
    }

    /// Default trait impls must keep existing implementors compiling and
    /// behave as no-ops: a `NoopHookHost` answers the new injection points
    /// without overriding them.
    #[tokio::test]
    async fn default_hook_impls_are_noops() {
        let session = SessionId::new();
        let host = NoopHookHost;
        assert_eq!(
            host.compaction_before(CompactionBeforeInput {
                session,
                trigger: CompactionTrigger::Overflow,
                messages_token_estimate: 10,
            })
            .await,
            CompactionDecision::Proceed
        );
        host.compaction_after(CompactionAfterInput {
            session,
            summary_tokens: 1,
        })
        .await;
        host.session_start(SessionLifecycleInput { session }).await;
        host.session_end(SessionLifecycleInput { session }).await;
        host.agent_spawn(AgentSpawnInput {
            parent: session,
            child: SessionId::new(),
        })
        .await;
        // Goal/loop evaluator hooks default to unregistered and unprobed, so
        // existing implementors stay fail-open to the built-in evaluators.
        assert!(
            !host.has_goal_evaluate(),
            "default capability probe must be false"
        );
        assert!(
            host.goal_evaluate("condition", "transcript").await.is_err(),
            "default goal_evaluate hook must be unregistered"
        );
        assert!(
            host.loop_verify("target", "transcript").await.is_err(),
            "default loop_verify hook must be unregistered"
        );
        assert!(
            host.loop_plan("target", &[], &last_verdict(), "")
                .await
                .is_err(),
            "default loop_plan hook must be unregistered"
        );
    }

    /// A `Skip` is only ever honored for proactive compaction. On an
    /// overflow-forced trigger it is demoted to `Proceed` (plus a warning),
    /// because skipping a compaction the request depends on risks context
    /// overflow.
    #[test]
    fn skip_is_honored_proactively_but_ignored_on_overflow() {
        let skip = || CompactionDecision::Skip {
            reason: "still fits".to_string(),
        };
        assert_eq!(
            resolve_compaction_decision(skip(), CompactionTrigger::Proactive),
            CompactionResolution::Skip {
                reason: "still fits".to_string()
            }
        );
        assert_eq!(
            resolve_compaction_decision(skip(), CompactionTrigger::Overflow),
            CompactionResolution::Proceed,
            "overflow-forced compaction must never be skipped"
        );
    }

    /// `Proceed` and `Replace` pass through unchanged for both triggers: a
    /// replacement of the summarizer instructions is not a safety decision and
    /// needs no trigger gating.
    #[test]
    fn proceed_and_replace_pass_through_both_triggers() {
        for trigger in [CompactionTrigger::Overflow, CompactionTrigger::Proactive] {
            assert_eq!(
                resolve_compaction_decision(CompactionDecision::Proceed, trigger),
                CompactionResolution::Proceed
            );
            assert_eq!(
                resolve_compaction_decision(
                    CompactionDecision::Replace {
                        instructions: "summarize as bullets".to_string(),
                    },
                    trigger
                ),
                CompactionResolution::Replace {
                    instructions: "summarize as bullets".to_string(),
                }
            );
        }
    }
}
