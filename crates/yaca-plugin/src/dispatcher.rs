//! `impl HookDispatcher for PluginHost`: converts the engine's native payloads
//! to wire frames, folds each interception hook through the plugins in load
//! order, and applies per-hook posture on failure (guards fail safe, enrichment
//! fails open).

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use yaca_core::hooks::{
    ChatParamsInput, ChatParamsOutcome, CommandExecuteBeforeInput, CommandExecuteBeforeOutcome,
    HookDispatcher, MessageUserBeforeInput, MessageUserBeforeOutcome, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use yaca_proto::Envelope;
use yaca_provider::{CompletionRequest, ReasoningEffort};

use crate::host::{PluginConn, PluginHost};
use crate::messages::{
    ChatParamsOutcomeWire, ChatParamsParams, CommandBeforeOutcomeWire, CommandExecuteBeforeParams,
    HookName, HookPosture, MessageUserBeforeOutcomeWire, MessageUserBeforeParams,
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
}

async fn enrich<P, O>(conn: &PluginConn, hook: HookName, params: &P) -> Option<O>
where
    P: Serialize,
    O: DeserializeOwned,
{
    call_outcome(conn, hook, params).await.ok()
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
