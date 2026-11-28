//! Adapter bridging ABI v2 [`hya_runtime_abi::NativeHook`] instances into
//! [`crate::hooks::HookDispatcher`].

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{Envelope, ModelRef, SessionId};
use hya_runtime_abi::{
    HookPoint, NativeChatParamsInput, NativeCommandBeforeInput, NativeCompactionAfterInput,
    NativeCompactionBeforeInput, NativeCompactionDecision, NativeCompactionTrigger,
    NativeGoalEvaluateVerdict, NativeHook, NativeLoopPlanInput, NativeMessageUserBeforeInput,
    NativeModelFallbackInput, NativePermissionApproveInput, NativeTextCompleteInput,
    NativeToolAfterInput, NativeToolAfterOutcome, NativeToolBeforeInput, NativeToolBeforeOutcome,
    NativeToolResult, ToolVisibilityInput, ToolVisibilityOutcome,
};
use hya_tool::{Action, Decision, Resource};

use super::{
    AgentSpawnInput, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionDecision,
    CompactionTrigger, GoalEvaluateReply, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, ModelFallbackInput, ModelFallbackOutcome, PermissionApproveInput,
    SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use crate::error::CoreError;
use crate::loop_mode::{EvidenceQuality, PlannerOutput, VerifierVerdict};

/// [`HookDispatcher`] backed by one or more ABI v2 [`NativeHook`] registrations.
pub struct NativeHookDispatcher {
    hooks: Vec<Arc<dyn NativeHook>>,
}

impl NativeHookDispatcher {
    /// Wrap `hooks` in registration order.
    #[must_use]
    pub fn new(hooks: Vec<Arc<dyn NativeHook>>) -> Self {
        Self { hooks }
    }

    /// Return `true` when no native hooks are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    fn matching(&self, point: HookPoint) -> impl Iterator<Item = &Arc<dyn NativeHook>> {
        self.hooks
            .iter()
            .filter(move |hook| hook.hook_points().contains(&point))
    }
}

/// Apply a single [`ToolVisibilityOutcome`] to a mutable visibility accumulator.
pub(crate) fn fold_visibility_outcome(
    remaining: &mut Vec<String>,
    hidden: &mut Vec<String>,
    had_allow_only: &mut bool,
    outcome: ToolVisibilityOutcome,
) {
    match outcome {
        ToolVisibilityOutcome::Preserve => {}
        ToolVisibilityOutcome::Hide { tools } => {
            remaining.retain(|name| !tools.contains(name));
            for tool in tools {
                if !hidden.contains(&tool) {
                    hidden.push(tool);
                }
            }
        }
        ToolVisibilityOutcome::AllowOnly { tools } => {
            remaining.retain(|name| tools.contains(name));
            *had_allow_only = true;
        }
    }
}

/// Finalize the folded visibility accumulator into one [`ToolVisibilityOutcome`].
pub(crate) fn finalize_visibility_outcome(
    remaining: Vec<String>,
    hidden: Vec<String>,
    had_allow_only: bool,
) -> ToolVisibilityOutcome {
    if had_allow_only {
        ToolVisibilityOutcome::AllowOnly { tools: remaining }
    } else if !hidden.is_empty() {
        ToolVisibilityOutcome::Hide { tools: hidden }
    } else {
        ToolVisibilityOutcome::Preserve
    }
}

#[async_trait]
impl HookDispatcher for NativeHookDispatcher {
    fn dispatch_event(&self, envelope: &Envelope) {
        let targets: Vec<Arc<dyn NativeHook>> =
            self.matching(HookPoint::EventObserve).cloned().collect();
        if targets.is_empty() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let envelope = envelope.clone();
        handle.spawn(async move {
            for hook in targets {
                if let Err(error) = hook.on_event(&envelope).await {
                    tracing::warn!(hook_id = hook.id(), %error, "native event hook failed");
                }
            }
        });
    }

    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        for hook in self.matching(HookPoint::PermissionAsk) {
            match hook.permission_ask(session, action, resource).await {
                Ok(Some(decision)) => return Some(decision),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native permission.ask hook failed");
                }
            }
        }
        None
    }

    async fn permission_approve(&self, input: PermissionApproveInput) -> Option<Decision> {
        for hook in self.matching(HookPoint::PermissionApprove) {
            let native_input = NativePermissionApproveInput {
                session: input.session,
                root_session: input.root_session,
                agent: input.agent.clone(),
                mode: input.mode.clone(),
                action: input.action,
                resource: input.resource.clone(),
            };
            match hook.permission_approve(native_input).await {
                Ok(Some(decision)) => return Some(decision),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native permission.approve hook failed");
                }
            }
        }
        None
    }

    async fn tool_visibility(&self, input: ToolVisibilityInput) -> ToolVisibilityOutcome {
        let mut remaining = input.candidate_tools.clone();
        let mut hidden = Vec::new();
        let mut had_allow_only = false;
        for hook in self.matching(HookPoint::ToolVisibility) {
            let step_input = ToolVisibilityInput {
                session: input.session,
                agent: input.agent.clone(),
                candidate_tools: remaining.clone(),
                interactive: input.interactive,
            };
            match hook.tool_visibility(step_input).await {
                Ok(outcome) => fold_visibility_outcome(
                    &mut remaining,
                    &mut hidden,
                    &mut had_allow_only,
                    outcome,
                ),
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native tool.visibility hook failed");
                }
            }
        }
        finalize_visibility_outcome(remaining, hidden, had_allow_only)
    }

    async fn command_execute_before(
        &self,
        mut input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        for hook in self.matching(HookPoint::CommandExecuteBefore) {
            match hook
                .command_execute_before(NativeCommandBeforeInput {
                    session: input.session,
                    command: input.command.clone(),
                    arguments: input.arguments.clone(),
                    text: input.text.clone(),
                })
                .await
            {
                Ok(next) => input.text = next,
                Err(error) => {
                    tracing::warn!(
                        hook_id = hook.id(),
                        %error,
                        "native command.execute.before hook failed"
                    );
                }
            }
        }
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, mut input: TextCompleteInput) -> TextCompleteOutcome {
        for hook in self.matching(HookPoint::TextComplete) {
            match hook
                .text_complete(NativeTextCompleteInput {
                    session: input.session,
                    message: input.message,
                    part: input.part,
                    text: input.text.clone(),
                })
                .await
            {
                Ok(next) => input.text = next,
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native text.complete hook failed");
                }
            }
        }
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        mut input: MessageUserBeforeInput,
    ) -> MessageUserBeforeOutcome {
        for hook in self.matching(HookPoint::MessageUserBefore) {
            match hook
                .message_user_before(NativeMessageUserBeforeInput {
                    session: input.session,
                    text: input.text.clone(),
                })
                .await
            {
                Ok(next) => input.text = next,
                Err(error) => {
                    tracing::warn!(
                        hook_id = hook.id(),
                        %error,
                        "native message.user.before hook failed"
                    );
                }
            }
        }
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, mut input: ChatParamsInput) -> ChatParamsOutcome {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct ChatParamsWire {
            model: hya_proto::ModelRef,
            system: Option<String>,
            messages: Vec<hya_proto::Message>,
            tools: Vec<hya_proto::ToolSchema>,
            temperature: Option<f32>,
            max_output_tokens: Option<u32>,
            reasoning: Option<String>,
            headers: std::collections::BTreeMap<String, String>,
        }
        for hook in self.matching(HookPoint::ChatParams) {
            let wire = ChatParamsWire {
                model: input.request.model.clone(),
                system: input.request.system.clone(),
                messages: input.request.messages.clone(),
                tools: input.request.tools.clone(),
                temperature: input.request.temperature,
                max_output_tokens: input.request.max_output_tokens,
                reasoning: input.request.reasoning.map(|r| r.as_str().to_string()),
                headers: input.request.headers.clone(),
            };
            let Ok(params) = serde_json::to_value(&wire) else {
                continue;
            };
            match hook
                .chat_params(NativeChatParamsInput {
                    session: input.session,
                    root_session: input.root_session,
                    agent: input.agent.clone(),
                    message: input.message,
                    params,
                })
                .await
            {
                Ok(updated) => match serde_json::from_value::<ChatParamsWire>(updated) {
                    Ok(next) => {
                        input.request = hya_provider::CompletionRequest {
                            model: next.model,
                            system: next.system,
                            messages: next.messages,
                            tools: next.tools,
                            temperature: next.temperature,
                            max_output_tokens: next.max_output_tokens,
                            reasoning: next
                                .reasoning
                                .as_deref()
                                .and_then(hya_provider::ReasoningEffort::parse)
                                .or(input.request.reasoning),
                            headers: next.headers,
                        };
                    }
                    Err(error) => {
                        tracing::warn!(
                            hook_id = hook.id(),
                            %error,
                            "native chat.params returned invalid CompletionRequest"
                        );
                    }
                },
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native chat.params hook failed");
                }
            }
        }
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(
        &self,
        mut input: ToolExecuteBeforeInput,
    ) -> ToolExecuteBeforeOutcome {
        for hook in self.matching(HookPoint::ToolExecuteBefore) {
            match hook
                .tool_execute_before(NativeToolBeforeInput {
                    session: input.session,
                    message: input.message,
                    call: input.call,
                    tool: input.tool.clone(),
                    input: input.input.clone(),
                })
                .await
            {
                Ok(NativeToolBeforeOutcome::Continue { input: next }) => input.input = next,
                Ok(NativeToolBeforeOutcome::Veto { reason }) => {
                    return ToolExecuteBeforeOutcome::Veto { reason };
                }
                Err(error) => {
                    return ToolExecuteBeforeOutcome::Veto {
                        reason: format!("native hook `{}` failed: {error}", hook.id()),
                    };
                }
            }
        }
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(
        &self,
        mut input: ToolExecuteAfterInput,
    ) -> ToolExecuteAfterOutcome {
        for hook in self.matching(HookPoint::ToolExecuteAfter) {
            let native_result = match &input.result {
                ToolOutcomeNative::Ok { output, time_ms } => NativeToolResult::Ok {
                    output: output.clone(),
                    time_ms: *time_ms,
                },
                ToolOutcomeNative::Err { message } => NativeToolResult::Err {
                    message: message.clone(),
                },
            };
            match hook
                .tool_execute_after(NativeToolAfterInput {
                    session: input.session,
                    message: input.message,
                    call: input.call,
                    tool: input.tool.clone(),
                    input: input.input.clone(),
                    result: native_result,
                })
                .await
            {
                Ok(NativeToolAfterOutcome::Continue { result }) => {
                    input.result = match result {
                        NativeToolResult::Ok { output, time_ms } => {
                            ToolOutcomeNative::Ok { output, time_ms }
                        }
                        NativeToolResult::Err { message } => ToolOutcomeNative::Err { message },
                    };
                }
                Err(error) => {
                    tracing::warn!(
                        hook_id = hook.id(),
                        %error,
                        "native tool.execute.after hook failed"
                    );
                }
            }
        }
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        for hook in self.matching(HookPoint::ModelFallback) {
            let native_input = NativeModelFallbackInput {
                session: input.session,
                root_session: input.root_session,
                agent: input.agent.clone(),
                message: input.message,
                model: input.model.clone(),
                error_class: input.error_class.as_str().to_string(),
                error_message: input.error_message.clone(),
                attempt: input.attempt,
                tried: input.tried.clone(),
            };
            match hook.model_fallback(native_input).await {
                Ok(Some(model)) => return ModelFallbackOutcome::Retry { model },
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(hook_id = hook.id(), %error, "native model.fallback hook failed");
                }
            }
        }
        ModelFallbackOutcome::GiveUp
    }

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        let mut replacement = None;
        let trigger = match input.trigger {
            CompactionTrigger::Overflow => NativeCompactionTrigger::Overflow,
            CompactionTrigger::Proactive => NativeCompactionTrigger::Proactive,
        };
        for hook in self.matching(HookPoint::CompactionBefore) {
            match hook
                .compaction_before(NativeCompactionBeforeInput {
                    session: input.session,
                    trigger,
                    messages_token_estimate: input.messages_token_estimate,
                })
                .await
            {
                Ok(NativeCompactionDecision::Proceed) => {}
                Ok(NativeCompactionDecision::Skip { reason }) => {
                    return CompactionDecision::Skip { reason };
                }
                Ok(NativeCompactionDecision::Replace { instructions }) => {
                    replacement = Some(instructions);
                }
                Err(error) => {
                    tracing::warn!(
                        hook_id = hook.id(),
                        %error,
                        "native compaction.before hook failed"
                    );
                }
            }
        }
        replacement.map_or(CompactionDecision::Proceed, |instructions| {
            CompactionDecision::Replace { instructions }
        })
    }

    async fn compaction_after(&self, input: CompactionAfterInput) {
        for hook in self.matching(HookPoint::CompactionAfter) {
            if let Err(error) = hook
                .compaction_after(NativeCompactionAfterInput {
                    session: input.session,
                    summary_tokens: input.summary_tokens,
                })
                .await
            {
                tracing::warn!(hook_id = hook.id(), %error, "native compaction.after hook failed");
            }
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        for hook in self.matching(HookPoint::SessionStart) {
            if let Err(error) = hook
                .session_start(hya_runtime_abi::NativeSessionLifecycleInput {
                    session: input.session,
                })
                .await
            {
                tracing::warn!(hook_id = hook.id(), %error, "native session.start hook failed");
            }
        }
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        for hook in self.matching(HookPoint::SessionEnd) {
            if let Err(error) = hook
                .session_end(hya_runtime_abi::NativeSessionLifecycleInput {
                    session: input.session,
                })
                .await
            {
                tracing::warn!(hook_id = hook.id(), %error, "native session.end hook failed");
            }
        }
    }

    async fn agent_spawn(&self, input: AgentSpawnInput) {
        for hook in self.matching(HookPoint::AgentSpawn) {
            if let Err(error) = hook
                .agent_spawn(hya_runtime_abi::NativeAgentSpawnInput {
                    parent: input.parent,
                    child: input.child,
                })
                .await
            {
                tracing::warn!(hook_id = hook.id(), %error, "native agent.spawn hook failed");
            }
        }
    }

    fn has_goal_evaluate(&self) -> bool {
        self.matching(HookPoint::GoalEvaluate).next().is_some()
    }

    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        let mut last_error = None;
        for hook in self.matching(HookPoint::GoalEvaluate) {
            match hook.goal_evaluate(condition, transcript).await {
                Ok(Some(NativeGoalEvaluateVerdict::Verdict { met, reason })) => {
                    return Ok(GoalEvaluateReply::Verdict { met, reason });
                }
                Ok(Some(NativeGoalEvaluateVerdict::Malformed)) => {
                    return Ok(GoalEvaluateReply::Malformed);
                }
                Ok(None) => {}
                Err(error) => last_error = Some(CoreError::Invalid(error.to_string())),
            }
        }
        Err(last_error
            .unwrap_or_else(|| CoreError::Invalid("goal.evaluate hook not registered".to_string())))
    }

    async fn loop_verify(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<VerifierVerdict, CoreError> {
        let mut last_error = None;
        for hook in self.matching(HookPoint::LoopVerify) {
            match hook.loop_verify(target, transcript).await {
                Ok(Some(verdict)) => {
                    let evidence_quality = match verdict.evidence_quality.as_str() {
                        "missing" => EvidenceQuality::Missing,
                        "claim_only" => EvidenceQuality::ClaimOnly,
                        "supported" => EvidenceQuality::Supported,
                        "verified" => EvidenceQuality::Verified,
                        _ => EvidenceQuality::Missing,
                    };
                    return Ok(VerifierVerdict {
                        score: verdict.score,
                        satisfied: verdict.satisfied,
                        evidence_quality,
                        critical_gaps: verdict.critical_gaps,
                        iteration_summary: verdict.iteration_summary,
                        reason: verdict.reason,
                    });
                }
                Ok(None) => {}
                Err(error) => last_error = Some(CoreError::Invalid(error.to_string())),
            }
        }
        Err(last_error
            .unwrap_or_else(|| CoreError::Invalid("loop.verifier hook not registered".to_string())))
    }

    async fn loop_plan(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        let mut last_error = None;
        let evidence_quality = match last.evidence_quality {
            EvidenceQuality::Missing => "missing",
            EvidenceQuality::ClaimOnly => "claim_only",
            EvidenceQuality::Supported => "supported",
            EvidenceQuality::Verified => "verified",
        };
        for hook in self.matching(HookPoint::LoopPlan) {
            let input = NativeLoopPlanInput {
                target: target.to_string(),
                history: history.to_vec(),
                last: hya_runtime_abi::NativeVerifierVerdict {
                    score: last.score,
                    satisfied: last.satisfied,
                    evidence_quality: evidence_quality.to_string(),
                    critical_gaps: last.critical_gaps.clone(),
                    iteration_summary: last.iteration_summary.clone(),
                    reason: last.reason.clone(),
                },
                planner_notes: planner_notes.to_string(),
            };
            match hook.loop_plan(input).await {
                Ok(Some(plan)) => {
                    return Ok(PlannerOutput {
                        directive: plan.directive,
                        continuity_brief: plan.continuity_brief,
                        planner_notes: plan.planner_notes,
                        strategy_change: plan.strategy_change,
                        change_note: plan.change_note,
                    });
                }
                Ok(None) => {}
                Err(error) => last_error = Some(CoreError::Invalid(error.to_string())),
            }
        }
        Err(last_error
            .unwrap_or_else(|| CoreError::Invalid("loop.planner hook not registered".to_string())))
    }

    async fn loop_should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        for hook in self.matching(HookPoint::LoopShouldStop) {
            match hook.loop_should_stop(target, transcript).await {
                Ok(Some(reason)) => return Some(reason),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        hook_id = hook.id(),
                        %error,
                        "native loop.should_stop hook failed"
                    );
                }
            }
        }
        None
    }
}

#[allow(dead_code)]
fn _assert_model_ref_import(_: &ModelRef) {}
