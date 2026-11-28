//! The plugin host as one [`NativeHook`] chain entry: converts the ABI hook
//! payloads to wire frames, folds each interception hook through plugins in
//! load order, and applies fixed failure behavior (guards fail safe,
//! enrichment fails open).

use async_trait::async_trait;
use hya_engine::{HookPoint, NativeHook};
use hya_proto::{Envelope, ModelRef, SessionId};
use hya_runtime_abi::{
    HostError, NativeAgentSpawnInput, NativeChatParamsInput, NativeCommandBeforeInput,
    NativeCompactionAfterInput, NativeCompactionBeforeInput, NativeCompactionDecision,
    NativeCompactionTrigger, NativeMessageUserBeforeInput, NativeModelFallbackInput,
    NativePermissionApproveInput, NativeSessionLifecycleInput, NativeTextCompleteInput,
    NativeToolAfterInput, NativeToolAfterOutcome, NativeToolBeforeInput, NativeToolBeforeOutcome,
    NativeToolResult,
};
use hya_tool::{Action, Decision, Resource};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use hya_plugin::PreparedPlugin;
use hya_plugin::messages::{
    AgentSpawnParams, ChatParamsOutcomeWire, ChatParamsParams, CommandBeforeOutcomeWire,
    CommandExecuteBeforeParams, CompactionAfterParams, CompactionBeforeOutcomeWire,
    CompactionBeforeParams, HookId, HookName, MessageUserBeforeOutcomeWire,
    MessageUserBeforeParams, ModelFailureClassWire, ModelFallbackErrorWire,
    ModelFallbackOutcomeWire, ModelFallbackParams, PermissionApproveParams, SessionLifecycleParams,
    TextCompleteOutcomeWire, TextCompleteParams, ToolAfterOutcomeWire, ToolBeforeOutcomeWire,
    ToolExecuteAfterParams, ToolExecuteBeforeParams, WireCompletionRequest, WireToolResult,
};

use super::PluginHostAdapter;

const GUARD_FAILED_SAFE: &str = "guard failed safe";

impl PluginHostAdapter {
    /// Core points and declared points the plugins registered, in load order.
    pub(super) fn registered_points(plugins: &[PreparedPlugin]) -> (Vec<HookPoint>, Vec<String>) {
        let mut points = Vec::new();
        let mut declared = Vec::new();
        for id in plugins.iter().flat_map(PreparedPlugin::hook_ids) {
            match id {
                HookId::Core(name) => {
                    if let Some(point) = HookPoint::from_name(name.as_str())
                        && !points.contains(&point)
                    {
                        points.push(point);
                    }
                }
                HookId::Declared(name) => {
                    if !declared.contains(name) {
                        declared.push(name.clone());
                    }
                }
            }
        }
        (points, declared)
    }
}

#[async_trait]
impl NativeHook for PluginHostAdapter {
    fn id(&self) -> &str {
        "plugin-host"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &self.points
    }

    fn declared_points(&self) -> &[String] {
        &self.declared
    }

    fn is_healthy(&self) -> bool {
        self.host.is_healthy()
    }

    fn on_event(&self, envelope: &Envelope) {
        self.host.fan_out_event(envelope);
    }

    fn permission_identity(&self) -> Option<[u8; 32]> {
        self.host.permission_semantic_identity_v1()
    }

    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Result<Option<Decision>, HostError> {
        Ok(self.host.permission_ask(session, action, resource).await)
    }

    async fn permission_approve(
        &self,
        input: NativePermissionApproveInput,
    ) -> Result<Option<Decision>, HostError> {
        Ok(self
            .host
            .permission_approve(PermissionApproveParams {
                session: input.session,
                root_session: input.root_session,
                agent: input.agent,
                mode: input.mode,
                action: input.action,
                resource: (&input.resource).into(),
            })
            .await)
    }

    async fn command_execute_before(
        &self,
        input: NativeCommandBeforeInput,
    ) -> Result<String, HostError> {
        let mut text = input.text;
        for conn in self.with_hook(HookName::CommandExecuteBefore) {
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
        Ok(text)
    }

    async fn text_complete(&self, input: NativeTextCompleteInput) -> Result<String, HostError> {
        let mut text = input.text;
        for conn in self.with_hook(HookName::TextComplete) {
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
        Ok(text)
    }

    async fn message_user_before(
        &self,
        input: NativeMessageUserBeforeInput,
    ) -> Result<String, HostError> {
        let mut text = input.text;
        for conn in self.with_hook(HookName::MessageUserBefore) {
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
        Ok(text)
    }

    async fn chat_params(&self, input: NativeChatParamsInput) -> Result<Value, HostError> {
        let mut request: WireCompletionRequest = serde_json::from_value(input.params)
            .map_err(|error| HostError::InvalidInput(format!("chat.params request: {error}")))?;
        for conn in self.with_hook(HookName::ChatParams) {
            let params = ChatParamsParams {
                session: input.session,
                root_session: Some(input.root_session),
                agent: input.agent.clone(),
                message: input.message,
                request: request.clone(),
            };
            if let Some(ChatParamsOutcomeWire::Continue { request: next }) =
                enrich(conn, HookName::ChatParams, &params).await
            {
                // A reply without a reasoning effort keeps the current one.
                let reasoning = next.reasoning.clone().or(request.reasoning.take());
                request = WireCompletionRequest { reasoning, ..next };
            }
        }
        serde_json::to_value(request).map_err(|error| HostError::Internal(error.to_string()))
    }

    async fn tool_execute_before(
        &self,
        input: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, HostError> {
        let mut current = input.input;
        for conn in self.with_hook(HookName::ToolExecuteBefore) {
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
                    return Ok(NativeToolBeforeOutcome::Veto { reason });
                }
                Err(failed) => {
                    return Ok(NativeToolBeforeOutcome::Veto {
                        reason: if conn.is_activation() {
                            format!("{GUARD_FAILED_SAFE}: {failed}")
                        } else {
                            format!("{GUARD_FAILED_SAFE}: {} ({failed})", conn.id())
                        },
                    });
                }
            }
        }
        Ok(NativeToolBeforeOutcome::Continue { input: current })
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        let mut result = match input.result {
            NativeToolResult::Ok { output, time_ms } => WireToolResult::Ok { output, time_ms },
            NativeToolResult::Err { message } => WireToolResult::Err { message },
        };
        for conn in self.with_hook(HookName::ToolExecuteAfter) {
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
        Ok(NativeToolAfterOutcome::Continue {
            result: match result {
                WireToolResult::Ok { output, time_ms } => NativeToolResult::Ok { output, time_ms },
                WireToolResult::Err { message } => NativeToolResult::Err { message },
            },
        })
    }

    async fn model_fallback(
        &self,
        input: NativeModelFallbackInput,
    ) -> Result<Option<ModelRef>, HostError> {
        let class: ModelFailureClassWire =
            serde_json::from_value(Value::String(input.error_class.clone()))
                .unwrap_or(ModelFailureClassWire::Other);
        let params = ModelFallbackParams {
            session: input.session,
            root_session: input.root_session,
            agent: input.agent,
            message: input.message,
            model: input.model,
            error: ModelFallbackErrorWire {
                class,
                message: input.error_message,
            },
            attempt: input.attempt,
            tried: input.tried,
        };
        for conn in self.with_hook(HookName::ModelFallback) {
            match conn
                .call_hook::<_, ModelFallbackOutcomeWire>(HookName::ModelFallback, &params)
                .await
            {
                Ok(ModelFallbackOutcomeWire::Retry { model })
                    if !model.as_str().trim().is_empty() =>
                {
                    return Ok(Some(model));
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
                    // Fail-open: a broken hook reads as give_up and the next
                    // plugin still gets the consult.
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::ModelFallback.as_str(),
                        "model.fallback hook failed; treating it as give_up: {failed}"
                    );
                }
            }
        }
        Ok(None)
    }

    async fn compaction_before(
        &self,
        input: NativeCompactionBeforeInput,
    ) -> Result<NativeCompactionDecision, HostError> {
        let trigger = match input.trigger {
            NativeCompactionTrigger::Overflow => {
                hya_plugin::messages::CompactionTriggerWire::Overflow
            }
            NativeCompactionTrigger::Proactive => {
                hya_plugin::messages::CompactionTriggerWire::Proactive
            }
        };
        for conn in self.with_hook(HookName::CompactionBefore) {
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
                    return Ok(NativeCompactionDecision::Skip { reason });
                }
                Ok(CompactionBeforeOutcomeWire::Replace { instructions }) => {
                    return Ok(NativeCompactionDecision::Replace { instructions });
                }
                Err(failed) => {
                    // Fail-open: compaction guards against context overflow,
                    // so a broken hook must never block it.
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = HookName::CompactionBefore.as_str(),
                        "compaction.before hook failed; proceeding with built-in compaction: {failed}"
                    );
                }
            }
        }
        Ok(NativeCompactionDecision::Proceed)
    }

    async fn compaction_after(&self, input: NativeCompactionAfterInput) -> Result<(), HostError> {
        let params = CompactionAfterParams {
            session: input.session,
            summary_tokens: u64::try_from(input.summary_tokens).unwrap_or(u64::MAX),
        };
        for conn in self.with_hook(HookName::CompactionAfter) {
            conn.notify_hook(HookName::CompactionAfter, &params).await;
        }
        Ok(())
    }

    async fn session_start(&self, input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in self.with_hook(HookName::SessionStart) {
            conn.notify_hook(HookName::SessionStart, &params).await;
        }
        Ok(())
    }

    async fn session_end(&self, input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        let params = SessionLifecycleParams {
            session: input.session,
        };
        for conn in self.with_hook(HookName::SessionEnd) {
            conn.notify_hook(HookName::SessionEnd, &params).await;
        }
        Ok(())
    }

    async fn agent_spawn(&self, input: NativeAgentSpawnInput) -> Result<(), HostError> {
        let params = AgentSpawnParams {
            parent: input.parent,
            child: input.child,
        };
        for conn in self.with_hook(HookName::AgentSpawn) {
            conn.notify_hook(HookName::AgentSpawn, &params).await;
        }
        Ok(())
    }

    /// The first plugin (in load order) that answers wins; a failing plugin
    /// is skipped so a broken provider never decides by itself.
    async fn invoke_declared(&self, point: &str, input: Value) -> Result<Option<Value>, HostError> {
        for conn in self.plugins.iter().filter(|conn| conn.declares(point)) {
            match conn.call_declared(point, input.clone()).await {
                Ok(Value::Null) => {}
                Ok(reply) => return Ok(Some(reply)),
                Err(failed) => {
                    tracing::warn!(
                        plugin = %conn.id(),
                        hook = point,
                        "declared hook failed; trying the next provider: {failed}"
                    );
                }
            }
        }
        Ok(None)
    }
}

impl PluginHostAdapter {
    fn with_hook(&self, hook: HookName) -> impl Iterator<Item = &PreparedPlugin> {
        self.plugins.iter().filter(move |conn| conn.has_hook(hook))
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
