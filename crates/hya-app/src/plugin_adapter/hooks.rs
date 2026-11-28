//! Converts the engine's native hook payloads
//! to wire frames, folds each interception hook through plugins in load
//! order, and applies fixed failure behavior (guards fail safe, enrichment
//! fails open).

use async_trait::async_trait;
use hya_engine::CoreError;
use hya_engine::hooks::{
    AgentSpawnInput, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionDecision,
    CompactionTrigger, EvidenceQuality, GoalEvaluateReply, HookDispatcher, MessageUserBeforeInput,
    MessageUserBeforeOutcome, ModelFailureClass, ModelFallbackInput, ModelFallbackOutcome,
    PlannerOutput, SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome,
    ToolExecuteAfterInput, ToolExecuteAfterOutcome, ToolExecuteBeforeInput,
    ToolExecuteBeforeOutcome, ToolOutcomeNative, VerifierVerdict,
};
use hya_proto::Envelope;
use hya_provider::{CompletionRequest, ReasoningEffort};
use hya_tool::{Action, Decision, Resource};
use serde::Serialize;
use serde::de::DeserializeOwned;

use hya_plugin::PreparedPlugin;
use hya_plugin::messages::{
    AgentSpawnParams, ChatParamsOutcomeWire, ChatParamsParams, CommandBeforeOutcomeWire,
    CommandExecuteBeforeParams, CompactionAfterParams, CompactionBeforeOutcomeWire,
    CompactionBeforeParams, EvidenceQualityWire, GoalEvaluateParams, GoalVerdictWire, HookName,
    LoopPlanParams, LoopShouldStopParams, LoopShouldStopWire, LoopVerifyParams,
    MessageUserBeforeOutcomeWire, MessageUserBeforeParams, ModelFailureClassWire,
    ModelFallbackErrorWire, ModelFallbackOutcomeWire, ModelFallbackParams, PermissionApproveParams,
    PlannerOutputWire, SessionLifecycleParams, TextCompleteOutcomeWire, TextCompleteParams,
    ToolAfterOutcomeWire, ToolBeforeOutcomeWire, ToolExecuteAfterParams, ToolExecuteBeforeParams,
    VerifierVerdictWire, WireCompletionRequest, WireToolResult,
};

use super::PluginHostAdapter;

const GUARD_FAILED_SAFE: &str = "guard failed safe";

#[async_trait]
impl HookDispatcher for PluginHostAdapter {
    fn is_healthy(&self) -> bool {
        self.host.is_healthy()
    }

    fn dispatch_event(&self, envelope: &Envelope) {
        self.host.fan_out_event(envelope);
    }

    fn permission_semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.host.permission_semantic_identity_v1()
    }

    async fn permission_ask(
        &self,
        session: Option<hya_proto::SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.host.permission_ask(session, action, resource).await
    }

    async fn permission_approve(
        &self,
        input: hya_engine::hooks::PermissionApproveInput,
    ) -> Option<Decision> {
        self.host
            .permission_approve(PermissionApproveParams {
                session: input.session,
                root_session: input.root_session,
                agent: input.agent,
                mode: input.mode,
                action: input.action,
                resource: (&input.resource).into(),
            })
            .await
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        let mut text = input.text;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::CommandExecuteBefore) {
                continue;
            }
            let params = CommandExecuteBeforeParams {
                session: input.session,
                command: input.command.clone(),
                arguments: input.arguments.clone(),
                text: text.clone(),
            };
            if let Some(CommandBeforeOutcomeWire::Continue { text: next }) =
                enrich(conn, HookName::CommandExecuteBefore, &params).await
            {
                text = next;
            }
        }
        CommandExecuteBeforeOutcome::Continue { text }
    }

    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        let mut text = input.text;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::TextComplete) {
                continue;
            }
            let params = TextCompleteParams {
                session: input.session,
                message: input.message,
                part: input.part,
                text: text.clone(),
            };
            if let Some(TextCompleteOutcomeWire::Continue { text: next }) =
                enrich(conn, HookName::TextComplete, &params).await
            {
                text = next;
            }
        }
        TextCompleteOutcome::Continue { text }
    }

    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        let mut text = input.text;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::MessageUserBefore) {
                continue;
            }
            let params = MessageUserBeforeParams {
                session: input.session,
                text: text.clone(),
            };
            if let Some(MessageUserBeforeOutcomeWire::Continue { text: next }) =
                enrich(conn, HookName::MessageUserBefore, &params).await
            {
                text = next;
            }
        }
        MessageUserBeforeOutcome::Continue { text }
    }

    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        let mut request = input.request;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::ChatParams) {
                continue;
            }
            let params = ChatParamsParams {
                session: input.session,
                root_session: Some(input.root_session),
                agent: input.agent.clone(),
                message: input.message,
                request: request_to_wire(&request),
            };
            if let Some(ChatParamsOutcomeWire::Continue { request: next }) =
                enrich(conn, HookName::ChatParams, &params).await
            {
                request = wire_to_request(next, &request);
            }
        }
        ChatParamsOutcome::Continue { request }
    }

    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        let mut current = input.input;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::ToolExecuteBefore) {
                continue;
            }
            let params = ToolExecuteBeforeParams {
                session: input.session,
                message: input.message,
                call: input.call,
                tool: input.tool.clone(),
                input: current.clone(),
            };
            match conn
                .call_hook::<_, ToolBeforeOutcomeWire>(HookName::ToolExecuteBefore, &params)
                .await
            {
                Ok(ToolBeforeOutcomeWire::Continue { input: next }) => current = next,
                Ok(ToolBeforeOutcomeWire::Veto { reason }) => {
                    return ToolExecuteBeforeOutcome::Veto { reason };
                }
                Err(failed) => {
                    return ToolExecuteBeforeOutcome::Veto {
                        reason: if conn.is_activation() {
                            format!("{GUARD_FAILED_SAFE}: {failed}")
                        } else {
                            format!("{GUARD_FAILED_SAFE}: {} ({failed})", conn.id())
                        },
                    };
                }
            }
        }
        ToolExecuteBeforeOutcome::Continue { input: current }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        let mut result = outcome_to_wire(input.result);
        for conn in &self.plugins {
            if !conn.has_hook(HookName::ToolExecuteAfter) {
                continue;
            }
            let params = ToolExecuteAfterParams {
                session: input.session,
                message: input.message,
                call: input.call,
                tool: input.tool.clone(),
                input: input.input.clone(),
                result: result.clone(),
            };
            if let Some(ToolAfterOutcomeWire::Continue { result: next }) =
                enrich(conn, HookName::ToolExecuteAfter, &params).await
            {
                result = next;
            }
        }
        ToolExecuteAfterOutcome::Continue {
            result: wire_to_outcome(result),
        }
    }

    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        let params = ModelFallbackParams {
            session: input.session,
            root_session: input.root_session,
            agent: input.agent,
            message: input.message,
            model: input.model,
            error: ModelFallbackErrorWire {
                class: failure_class_to_wire(input.error_class),
                message: input.error_message,
            },
            attempt: input.attempt,
            tried: input.tried,
        };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::ModelFallback) {
                continue;
            }
            match conn
                .call_hook::<_, ModelFallbackOutcomeWire>(HookName::ModelFallback, &params)
                .await
            {
                Ok(ModelFallbackOutcomeWire::Retry { model })
                    if !model.as_str().trim().is_empty() =>
                {
                    return ModelFallbackOutcome::Retry { model };
                }
                Ok(ModelFallbackOutcomeWire::Retry { .. }) => {
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::ModelFallback.as_str(),
                        "model.fallback retry named an empty model; treating it as give_up"
                    );
                }
                Ok(ModelFallbackOutcomeWire::GiveUp) => {}
                Err(failed) => {
                    // Fail-open: a broken hook reads
                    // as give_up and the next plugin still gets the consult.
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::ModelFallback.as_str(),
                        "model.fallback hook failed; treating it as give_up: {failed}"
                    );
                }
            }
        }
        ModelFallbackOutcome::GiveUp
    }

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        let trigger = match input.trigger {
            CompactionTrigger::Overflow => hya_plugin::messages::CompactionTriggerWire::Overflow,
            CompactionTrigger::Proactive => hya_plugin::messages::CompactionTriggerWire::Proactive,
        };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::CompactionBefore) {
                continue;
            }
            let params = CompactionBeforeParams {
                session: input.session,
                trigger,
                messages_token_estimate: u64::try_from(input.messages_token_estimate)
                    .unwrap_or(u64::MAX),
            };
            match conn
                .call_hook::<_, CompactionBeforeOutcomeWire>(HookName::CompactionBefore, &params)
                .await
            {
                Ok(CompactionBeforeOutcomeWire::Proceed) => {}
                Ok(CompactionBeforeOutcomeWire::Skip { reason }) => {
                    return CompactionDecision::Skip { reason };
                }
                Ok(CompactionBeforeOutcomeWire::Replace { instructions }) => {
                    return CompactionDecision::Replace { instructions };
                }
                Err(failed) => {
                    // Fail-open:
                    // compaction guards against context overflow, so a broken
                    // hook must never block it.
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::CompactionBefore.as_str(),
                        "compaction.before hook failed; proceeding with built-in compaction: {failed}"
                    );
                }
            }
        }
        CompactionDecision::Proceed
    }

    async fn compaction_after(&self, input: CompactionAfterInput) {
        for conn in &self.plugins {
            if !conn.has_hook(HookName::CompactionAfter) {
                continue;
            }
            let params = CompactionAfterParams {
                session: input.session,
                summary_tokens: u64::try_from(input.summary_tokens).unwrap_or(u64::MAX),
            };
            conn.notify_hook(HookName::CompactionAfter, &params).await;
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::SessionStart) {
                continue;
            }
            conn.notify_hook(HookName::SessionStart, &params).await;
        }
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::SessionEnd) {
                continue;
            }
            conn.notify_hook(HookName::SessionEnd, &params).await;
        }
    }

    async fn agent_spawn(&self, input: AgentSpawnInput) {
        let params = AgentSpawnParams {
            parent: input.parent,
            child: input.child,
        };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::AgentSpawn) {
                continue;
            }
            conn.notify_hook(HookName::AgentSpawn, &params).await;
        }
    }

    fn has_goal_evaluate(&self) -> bool {
        self.plugins
            .iter()
            .any(|conn| conn.has_hook(HookName::GoalEvaluate))
    }

    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        let value = serde_json::to_value(GoalEvaluateParams {
            condition,
            transcript,
        })
        .map_err(|error| CoreError::Invalid(error.to_string()))?;
        let mut last_failure: Option<String> = None;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::GoalEvaluate) {
                continue;
            }
            match conn
                .call_hook::<_, serde_json::Value>(HookName::GoalEvaluate, value.clone())
                .await
            {
                Ok(reply) => {
                    return match serde_json::from_value::<GoalVerdictWire>(reply) {
                        Ok(verdict) => Ok(GoalEvaluateReply::Verdict {
                            met: verdict.met,
                            reason: verdict.reason,
                        }),
                        // A reply that is not a parseable verdict is decisive:
                        // report `Malformed` so the driver counts not-met
                        // against the iteration cap instead of shopping for
                        // another opinion.
                        Err(_) => Ok(GoalEvaluateReply::Malformed),
                    };
                }
                Err(failed) => {
                    // Evaluators fail open: a broken provider never decides the
                    // run; the next registered one gets the call.
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::GoalEvaluate.as_str(),
                        "goal.evaluate provider failed; trying next in load order: {failed}"
                    );
                    last_failure = Some(failed.to_string());
                }
            }
        }
        Err(CoreError::Invalid(last_failure.unwrap_or_else(|| {
            "goal.evaluate hook not registered".to_string()
        })))
    }

    async fn loop_verify(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<VerifierVerdict, CoreError> {
        let params = LoopVerifyParams { target, transcript };
        let mut last_failure: Option<String> = None;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::LoopVerifier) {
                continue;
            }
            match conn
                .call_hook::<_, VerifierVerdictWire>(HookName::LoopVerifier, &params)
                .await
            {
                Ok(wire) => return Ok(wire_to_verdict(wire)),
                Err(failed) => {
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::LoopVerifier.as_str(),
                        "loop.verifier provider failed; trying next in load order: {failed}"
                    );
                    last_failure = Some(failed);
                }
            }
        }
        Err(CoreError::Invalid(last_failure.unwrap_or_else(|| {
            "loop.verifier hook not registered".to_string()
        })))
    }

    async fn loop_plan(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        planner_notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        let params = LoopPlanParams {
            target,
            history,
            last: verdict_to_wire(last),
            planner_notes,
        };
        let mut last_failure: Option<String> = None;
        for conn in &self.plugins {
            if !conn.has_hook(HookName::LoopPlanner) {
                continue;
            }
            match conn
                .call_hook::<_, PlannerOutputWire>(HookName::LoopPlanner, &params)
                .await
            {
                Ok(wire) => return Ok(wire_to_plan(wire)),
                Err(failed) => {
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::LoopPlanner.as_str(),
                        "loop.planner provider failed; trying next in load order: {failed}"
                    );
                    last_failure = Some(failed);
                }
            }
        }
        Err(CoreError::Invalid(last_failure.unwrap_or_else(|| {
            "loop.planner hook not registered".to_string()
        })))
    }

    async fn loop_should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        let params = LoopShouldStopParams { target, transcript };
        for conn in &self.plugins {
            if !conn.has_hook(HookName::LoopShouldStop) {
                continue;
            }
            match conn
                .call_hook::<_, LoopShouldStopWire>(HookName::LoopShouldStop, &params)
                .await
            {
                Ok(wire) if wire.stop => {
                    let reason = if wire.reason.is_empty() {
                        "no reason given".to_string()
                    } else {
                        wire.reason
                    };
                    return Some(reason);
                }
                // `stop: false` keeps the chain going: a later registered
                // provider may still force the stop.
                Ok(_) => {}
                Err(failed) => {
                    // Fail-open: the consult never decides by failing. A
                    // broken hook logs and yields to the next provider (and
                    // ultimately to the verifier).
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::LoopShouldStop.as_str(),
                        "loop.should_stop provider failed; continuing the loop: {failed}"
                    );
                }
            }
        }
        None
    }
}

fn verdict_to_wire(verdict: &VerifierVerdict) -> VerifierVerdictWire {
    VerifierVerdictWire {
        score: verdict.score,
        satisfied: verdict.satisfied,
        evidence_quality: match verdict.evidence_quality {
            EvidenceQuality::Missing => EvidenceQualityWire::Missing,
            EvidenceQuality::ClaimOnly => EvidenceQualityWire::ClaimOnly,
            EvidenceQuality::Supported => EvidenceQualityWire::Supported,
            EvidenceQuality::Verified => EvidenceQualityWire::Verified,
        },
        critical_gaps: verdict.critical_gaps.clone(),
        iteration_summary: verdict.iteration_summary.clone(),
        reason: verdict.reason.clone(),
    }
}

fn wire_to_verdict(wire: VerifierVerdictWire) -> VerifierVerdict {
    VerifierVerdict {
        score: wire.score,
        satisfied: wire.satisfied,
        evidence_quality: match wire.evidence_quality {
            EvidenceQualityWire::Missing => EvidenceQuality::Missing,
            EvidenceQualityWire::ClaimOnly => EvidenceQuality::ClaimOnly,
            EvidenceQualityWire::Supported => EvidenceQuality::Supported,
            EvidenceQualityWire::Verified => EvidenceQuality::Verified,
        },
        critical_gaps: wire.critical_gaps,
        iteration_summary: wire.iteration_summary,
        reason: wire.reason,
    }
}

fn wire_to_plan(wire: PlannerOutputWire) -> PlannerOutput {
    PlannerOutput {
        directive: wire.directive,
        continuity_brief: wire.continuity_brief,
        planner_notes: wire.planner_notes,
        strategy_change: wire.strategy_change,
        change_note: wire.change_note,
    }
}

async fn enrich<P, O>(conn: &PreparedPlugin, hook: HookName, params: &P) -> Option<O>
where
    P: Serialize + Sync,
    O: DeserializeOwned,
{
    match conn.call_hook(hook, params).await {
        Ok(outcome) => Some(outcome),
        Err(failed) => {
            // Enrichment hooks fail open, but never silently: the pipeline
            // continues with the prior payload and the failure is logged.
            tracing::warn!(
                plugin = %conn.id(),
                hook = hook.as_str(),
                "hook failed; continuing with prior payload: {failed}"
            );
            None
        }
    }
}

fn request_to_wire(request: &CompletionRequest) -> WireCompletionRequest {
    WireCompletionRequest {
        model: request.model.clone(),
        system: request.system.clone(),
        messages: request.messages.clone(),
        tools: request.tools.clone(),
        temperature: request.temperature,
        max_output_tokens: request.max_output_tokens,
        reasoning: request.reasoning.map(|r| r.as_str().to_string()),
        headers: request.headers.clone(),
    }
}

fn wire_to_request(wire: WireCompletionRequest, original: &CompletionRequest) -> CompletionRequest {
    CompletionRequest {
        model: wire.model,
        system: wire.system,
        messages: wire.messages,
        tools: wire.tools,
        temperature: wire.temperature,
        max_output_tokens: wire.max_output_tokens,
        reasoning: wire
            .reasoning
            .as_deref()
            .and_then(ReasoningEffort::parse)
            .or(original.reasoning),
        headers: wire.headers,
    }
}

fn failure_class_to_wire(class: ModelFailureClass) -> ModelFailureClassWire {
    match class {
        ModelFailureClass::Retryable => ModelFailureClassWire::Retryable,
        ModelFailureClass::UnknownModel => ModelFailureClassWire::UnknownModel,
        ModelFailureClass::Auth => ModelFailureClassWire::Auth,
        ModelFailureClass::InvalidRequest => ModelFailureClassWire::InvalidRequest,
        ModelFailureClass::Other => ModelFailureClassWire::Other,
    }
}

fn outcome_to_wire(outcome: ToolOutcomeNative) -> WireToolResult {
    match outcome {
        ToolOutcomeNative::Ok { output, time_ms } => WireToolResult::Ok { output, time_ms },
        ToolOutcomeNative::Err { message } => WireToolResult::Err { message },
    }
}

fn wire_to_outcome(wire: WireToolResult) -> ToolOutcomeNative {
    match wire {
        WireToolResult::Ok { output, time_ms } => ToolOutcomeNative::Ok { output, time_ms },
        WireToolResult::Err { message } => ToolOutcomeNative::Err { message },
    }
}
