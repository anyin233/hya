//! `impl HookDispatcher for PluginHost`: converts the engine's native payloads
//! to wire frames, folds each interception hook through the plugins in load
//! order, and applies per-hook posture on failure (guards fail safe, enrichment
//! fails open).

use async_trait::async_trait;
use hya_core::hooks::{
    AgentSpawnInput, ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput,
    CommandExecuteBeforeOutcome, CompactionAfterInput, CompactionBeforeInput, CompactionDecision,
    CompactionTrigger, HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome,
    SessionLifecycleInput, TextCompleteInput, TextCompleteOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use hya_proto::Envelope;
use hya_provider::{CompletionRequest, ReasoningEffort};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::host::{PluginConn, PluginHost};
use crate::messages::{
    AgentSpawnParams, ChatParamsOutcomeWire, ChatParamsParams, CommandBeforeOutcomeWire,
    CommandExecuteBeforeParams, CompactionAfterParams, CompactionBeforeOutcomeWire,
    CompactionBeforeParams, HookName, HookPosture, MessageUserBeforeOutcomeWire,
    MessageUserBeforeParams, SessionLifecycleParams, TextCompleteOutcomeWire, TextCompleteParams,
    ToolAfterOutcomeWire, ToolBeforeOutcomeWire, ToolExecuteAfterParams, ToolExecuteBeforeParams,
    WireCompletionRequest, WireToolResult,
};

const GUARD_FAILED_SAFE: &str = "guard failed safe";

#[async_trait]
impl HookDispatcher for PluginHost {
    fn dispatch_event(&self, envelope: &Envelope) {
        self.fan_out_event(envelope);
    }

    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        let mut text = input.text;
        for conn in self.plugins() {
            if conn.posture(HookName::CommandExecuteBefore).is_none() {
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
        for conn in self.plugins() {
            if conn.posture(HookName::TextComplete).is_none() {
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
        for conn in self.plugins() {
            if conn.posture(HookName::MessageUserBefore).is_none() {
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
        for conn in self.plugins() {
            if conn.posture(HookName::ChatParams).is_none() {
                continue;
            }
            let params = ChatParamsParams {
                session: input.session,
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
        for conn in self.plugins() {
            let Some(posture) = conn.posture(HookName::ToolExecuteBefore) else {
                continue;
            };
            let params = ToolExecuteBeforeParams {
                session: input.session,
                message: input.message,
                call: input.call,
                tool: input.tool.clone(),
                input: current.clone(),
            };
            match call_outcome::<ToolBeforeOutcomeWire>(conn, HookName::ToolExecuteBefore, &params)
                .await
            {
                Ok(ToolBeforeOutcomeWire::Continue { input: next }) => current = next,
                Ok(ToolBeforeOutcomeWire::Veto { reason }) => {
                    return ToolExecuteBeforeOutcome::Veto { reason };
                }
                Err(failed) => {
                    if posture == HookPosture::Safe {
                        return ToolExecuteBeforeOutcome::Veto {
                            reason: format!("{GUARD_FAILED_SAFE}: {} ({failed})", conn.id),
                        };
                    }
                }
            }
        }
        ToolExecuteBeforeOutcome::Continue { input: current }
    }

    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        let mut result = outcome_to_wire(input.result);
        for conn in self.plugins() {
            if conn.posture(HookName::ToolExecuteAfter).is_none() {
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

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        let trigger = match input.trigger {
            CompactionTrigger::Overflow => crate::messages::CompactionTriggerWire::Overflow,
            CompactionTrigger::Proactive => crate::messages::CompactionTriggerWire::Proactive,
        };
        for conn in self.plugins() {
            if conn.posture(HookName::CompactionBefore).is_none() {
                continue;
            }
            let params = CompactionBeforeParams {
                session: input.session,
                trigger,
                messages_token_estimate: u64::try_from(input.messages_token_estimate)
                    .unwrap_or(u64::MAX),
            };
            match call_outcome::<CompactionBeforeOutcomeWire>(
                conn,
                HookName::CompactionBefore,
                &params,
            )
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
                    // Unconditionally fail-open, posture notwithstanding:
                    // compaction guards against context overflow, so a broken
                    // hook must never block it.
                    tracing::warn!(
                        plugin = %conn.id,
                        hook = HookName::CompactionBefore.as_str(),
                        "compaction.before hook failed; proceeding with built-in compaction: {failed}"
                    );
                }
            }
        }
        CompactionDecision::Proceed
    }

    async fn compaction_after(&self, input: CompactionAfterInput) {
        for conn in self.plugins() {
            if conn.posture(HookName::CompactionAfter).is_none() {
                continue;
            }
            let params = CompactionAfterParams {
                session: input.session,
                summary_tokens: u64::try_from(input.summary_tokens).unwrap_or(u64::MAX),
            };
            notify(conn, HookName::CompactionAfter, &params).await;
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in self.plugins() {
            if conn.posture(HookName::SessionStart).is_none() {
                continue;
            }
            notify(conn, HookName::SessionStart, &params).await;
        }
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in self.plugins() {
            if conn.posture(HookName::SessionEnd).is_none() {
                continue;
            }
            notify(conn, HookName::SessionEnd, &params).await;
        }
    }

    async fn agent_spawn(&self, input: AgentSpawnInput) {
        let params = AgentSpawnParams {
            parent: input.parent,
            child: input.child,
        };
        for conn in self.plugins() {
            if conn.posture(HookName::AgentSpawn).is_none() {
                continue;
            }
            notify(conn, HookName::AgentSpawn, &params).await;
        }
    }
}

async fn enrich<P, O>(conn: &PluginConn, hook: HookName, params: &P) -> Option<O>
where
    P: Serialize,
    O: DeserializeOwned,
{
    match call_outcome(conn, hook, params).await {
        Ok(outcome) => Some(outcome),
        Err(failed) => {
            // Enrichment hooks fail open, but never silently: the pipeline
            // continues with the prior payload and the failure is logged.
            tracing::warn!(
                plugin = %conn.id,
                hook = hook.as_str(),
                "hook failed; continuing with prior payload: {failed}"
            );
            None
        }
    }
}

/// Fire a notification-style hook (no decisive reply) and log any failure.
///
/// Used by the observation points (`compaction.after`, `session.start`,
/// `session.end`, `agent.spawn`), which must never affect the main flow.
async fn notify<P: Serialize>(conn: &PluginConn, hook: HookName, params: &P) {
    let value = match serde_json::to_value(params) {
        Ok(value) => value,
        Err(failed) => {
            tracing::warn!(
                plugin = %conn.id,
                hook = hook.as_str(),
                "hook params failed to serialize: {failed}"
            );
            return;
        }
    };
    if let Err(failed) = conn.call_hook(hook, value).await {
        tracing::warn!(
            plugin = %conn.id,
            hook = hook.as_str(),
            "notification hook failed: {failed}"
        );
    }
}

async fn call_outcome<O>(
    conn: &PluginConn,
    hook: HookName,
    params: &impl Serialize,
) -> Result<O, String>
where
    O: DeserializeOwned,
{
    let value = serde_json::to_value(params).map_err(|e| e.to_string())?;
    let reply = conn
        .call_hook(hook, value)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value::<O>(reply).map_err(|e| e.to_string())
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

pub(crate) fn outcome_to_wire(outcome: ToolOutcomeNative) -> WireToolResult {
    match outcome {
        ToolOutcomeNative::Ok { output, time_ms } => WireToolResult::Ok { output, time_ms },
        ToolOutcomeNative::Err { message } => WireToolResult::Err { message },
    }
}

pub(crate) fn wire_to_outcome(wire: WireToolResult) -> ToolOutcomeNative {
    match wire {
        WireToolResult::Ok { output, time_ms } => ToolOutcomeNative::Ok { output, time_ms },
        WireToolResult::Err { message } => ToolOutcomeNative::Err { message },
    }
}
