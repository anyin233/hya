//! Engine-facing dispatch and native payloads for app-owned plugin adapters.

use async_trait::async_trait;
use hya_proto::{Envelope, SessionId};
use hya_tool::{Action, Decision, Resource};

use crate::error::CoreError;
use crate::loop_mode::{PlannerOutput, VerifierVerdict};

mod activation;
mod chain;
mod payloads;

pub(crate) use activation::{
    activation_hook_for, replace_activation_hooks, scope_activation_hooks,
    scope_optional_activation_hooks,
};
pub use chain::HookChain;
pub use payloads::*;

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
    /// Stable permission-hook identity for permission-plane pinning.
    fn permission_semantic_identity_v1(&self) -> Option<[u8; 32]> {
        None
    }
    /// Decide an ask-mode permission or defer to the next hook/user prompt.
    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        let _ = (session, action, resource);
        None
    }
    /// Ask the approver of a bundle-declared session permission mode.
    ///
    /// Consulted only after every `permission.ask` interceptor deferred, and
    /// only on the dispatcher of the bundle that declared the active mode.
    /// `None` (defer, error, timeout, or no registered hook) falls through to
    /// the normal user ask. The default defers.
    async fn permission_approve(&self, input: PermissionApproveInput) -> Option<Decision> {
        let _ = input;
        None
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
    /// Choose the next model after a provider failed before any stream
    /// existed (`model.fallback`).
    ///
    /// Consulted only once the engine's configured cross-model chain can no
    /// longer advance, and never after a stream was returned. Fail-open by
    /// construction: implementors report errors, timeouts, and malformed
    /// replies as [`ModelFallbackOutcome::GiveUp`]. The default gives up.
    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        let _ = input;
        ModelFallbackOutcome::GiveUp
    }
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
    /// Consult the registered `loop.should_stop` provider chain after one
    /// loop iteration: `Some(reason)` forces the loop to stop with that
    /// reason (surfaced as `GateOutcome::Stop { reason:
    /// "loop.should_stop: <reason>" }`), `None` lets the gate continue to
    /// the verifier.
    ///
    /// **Contract:** fail-open by construction — implementors treat transport
    /// errors and malformed replies as `None` (with a warning), so a broken
    /// hook can never decide the loop by itself. A stop forced here is a
    /// **legitimate stop**: v1 records it as
    /// [`crate::completion::RunOutcome::Achieved`] with the prefixed reason,
    /// exactly like a satisfied deterministic predicate; it is *not* modeled
    /// as an engine-suppressed success.
    async fn loop_should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        let _ = (target, transcript);
        None
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
mod tests;
