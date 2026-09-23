//! Filter process hook dispatch to one agent's declared hook references.

use crate::error::CoreError;
use crate::hooks::*;
use crate::loop_mode::{PlannerOutput, VerifierVerdict};
use async_trait::async_trait;
use hya_proto::Envelope;
use hya_proto::SessionId;
use hya_tool::{Action, Decision, PermissionInterceptor, Resource};
use std::collections::BTreeSet;
use std::sync::Arc;

pub(crate) struct ScopedBundleHooks {
    dispatcher: Arc<dyn HookDispatcher>,
    names: Option<BTreeSet<String>>,
    _owner: Option<Arc<dyn crate::RuntimeSourceOwner>>,
}

impl ScopedBundleHooks {
    pub(crate) fn new(dispatcher: Arc<dyn HookDispatcher>, references: &[String]) -> Self {
        Self {
            dispatcher,
            names: Some(
                references
                    .iter()
                    .filter_map(|name| {
                        name.rsplit_once("/hook/")
                            .map(|(_, local)| local.to_string())
                    })
                    .collect(),
            ),
            _owner: None,
        }
    }
    pub(crate) fn retaining(
        dispatcher: Arc<dyn HookDispatcher>,
        owner: Arc<dyn crate::RuntimeSourceOwner>,
    ) -> Self {
        Self {
            dispatcher,
            names: None,
            _owner: Some(owner),
        }
    }
    fn has(&self, name: &str) -> bool {
        self.names.as_ref().is_none_or(|names| names.contains(name))
    }
}

/// Restrict legacy JS sidecars to their historical three-hook contract.
/// Native bundle processes are scoped separately from declared `hook_refs` and
/// must retain the complete dispatcher surface.
pub(crate) fn restricted_sidecar_hooks(
    dispatcher: Arc<dyn HookDispatcher>,
) -> Arc<dyn HookDispatcher> {
    Arc::new(ScopedBundleHooks::new(
        dispatcher,
        &[
            "sidecar/hook/event".to_string(),
            "sidecar/hook/tool.execute.before".to_string(),
            "sidecar/hook/tool.execute.after".to_string(),
        ],
    ))
}

#[async_trait]
impl HookDispatcher for ScopedBundleHooks {
    fn dispatch_event(&self, envelope: &Envelope) {
        if self.has("event") {
            self.dispatcher.dispatch_event(envelope);
        }
    }
    fn is_healthy(&self) -> bool {
        self.dispatcher.is_healthy()
    }
    fn permission_semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.has("permission.ask")
            .then(|| self.dispatcher.permission_semantic_identity_v1())?
    }
    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        if self.has("permission.ask") {
            self.dispatcher
                .permission_ask(session, action, resource)
                .await
        } else {
            None
        }
    }
    async fn command_execute_before(
        &self,
        input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        if self.has("command.execute.before") {
            self.dispatcher.command_execute_before(input).await
        } else {
            CommandExecuteBeforeOutcome::Continue { text: input.text }
        }
    }
    async fn text_complete(&self, input: TextCompleteInput) -> TextCompleteOutcome {
        if self.has("experimental.text.complete") {
            self.dispatcher.text_complete(input).await
        } else {
            TextCompleteOutcome::Continue { text: input.text }
        }
    }
    async fn message_user_before(&self, input: MessageUserBeforeInput) -> MessageUserBeforeOutcome {
        if self.has("message.user.before") {
            self.dispatcher.message_user_before(input).await
        } else {
            MessageUserBeforeOutcome::Continue { text: input.text }
        }
    }
    async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
        if self.has("chat.params") {
            self.dispatcher.chat_params(input).await
        } else {
            ChatParamsOutcome::Continue {
                request: input.request,
            }
        }
    }
    async fn tool_execute_before(&self, input: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome {
        if self.has("tool.execute.before") {
            self.dispatcher.tool_execute_before(input).await
        } else {
            ToolExecuteBeforeOutcome::Continue { input: input.input }
        }
    }
    async fn tool_execute_after(&self, input: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome {
        if self.has("tool.execute.after") {
            self.dispatcher.tool_execute_after(input).await
        } else {
            ToolExecuteAfterOutcome::Continue {
                result: input.result,
            }
        }
    }
    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        if self.has("model.fallback") {
            self.dispatcher.model_fallback(input).await
        } else {
            ModelFallbackOutcome::GiveUp
        }
    }
    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        if self.has("compaction.before") {
            self.dispatcher.compaction_before(input).await
        } else {
            CompactionDecision::Proceed
        }
    }
    async fn compaction_after(&self, input: CompactionAfterInput) {
        if self.has("compaction.after") {
            self.dispatcher.compaction_after(input).await;
        }
    }
    async fn session_start(&self, input: SessionLifecycleInput) {
        if self.has("session.start") {
            self.dispatcher.session_start(input).await;
        }
    }
    async fn session_end(&self, input: SessionLifecycleInput) {
        if self.has("session.end") {
            self.dispatcher.session_end(input).await;
        }
    }
    async fn agent_spawn(&self, input: AgentSpawnInput) {
        if self.has("agent.spawn") {
            self.dispatcher.agent_spawn(input).await;
        }
    }
    fn has_goal_evaluate(&self) -> bool {
        self.has("goal.evaluate") && self.dispatcher.has_goal_evaluate()
    }
    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        if self.has("goal.evaluate") {
            self.dispatcher.goal_evaluate(condition, transcript).await
        } else {
            Err(CoreError::Invalid("goal.evaluate not selected".into()))
        }
    }
    async fn loop_verify(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<VerifierVerdict, CoreError> {
        if self.has("loop.verifier") {
            self.dispatcher.loop_verify(target, transcript).await
        } else {
            Err(CoreError::Invalid("loop.verifier not selected".into()))
        }
    }
    async fn loop_plan(
        &self,
        target: &str,
        history: &[String],
        last: &VerifierVerdict,
        notes: &str,
    ) -> Result<PlannerOutput, CoreError> {
        if self.has("loop.planner") {
            self.dispatcher
                .loop_plan(target, history, last, notes)
                .await
        } else {
            Err(CoreError::Invalid("loop.planner not selected".into()))
        }
    }
    async fn loop_should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        if self.has("loop.should_stop") {
            self.dispatcher.loop_should_stop(target, transcript).await
        } else {
            None
        }
    }
}

pub(crate) struct BundlePermissionInterceptor {
    dispatcher: Arc<dyn HookDispatcher>,
}

impl BundlePermissionInterceptor {
    pub(crate) fn new(dispatcher: Arc<dyn HookDispatcher>) -> Self {
        Self { dispatcher }
    }
}

#[async_trait]
impl PermissionInterceptor for BundlePermissionInterceptor {
    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.dispatcher.permission_semantic_identity_v1()
    }

    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.dispatcher
            .permission_ask(session, action, resource)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_proto::Envelope;

    struct PermissionHook(Option<Decision>);

    #[async_trait]
    impl HookDispatcher for PermissionHook {
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
        async fn message_user_before(
            &self,
            input: MessageUserBeforeInput,
        ) -> MessageUserBeforeOutcome {
            MessageUserBeforeOutcome::Continue { text: input.text }
        }
        async fn chat_params(&self, input: ChatParamsInput) -> ChatParamsOutcome {
            ChatParamsOutcome::Continue {
                request: input.request,
            }
        }
        async fn tool_execute_before(
            &self,
            input: ToolExecuteBeforeInput,
        ) -> ToolExecuteBeforeOutcome {
            ToolExecuteBeforeOutcome::Continue { input: input.input }
        }
        async fn tool_execute_after(
            &self,
            input: ToolExecuteAfterInput,
        ) -> ToolExecuteAfterOutcome {
            ToolExecuteAfterOutcome::Continue {
                result: input.result,
            }
        }
        async fn permission_ask(
            &self,
            _session: Option<SessionId>,
            _action: Action,
            _resource: &Resource,
        ) -> Option<Decision> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn private_hook_refs_gate_permission_ask() {
        let dispatcher: Arc<dyn HookDispatcher> =
            Arc::new(PermissionHook(Some(Decision::AllowOnce)));
        let hidden = ScopedBundleHooks::new(
            Arc::clone(&dispatcher),
            &["acme/hook/message.user.before".to_string()],
        );
        assert!(
            hidden
                .permission_ask(None, Action::Bash, &Resource::Command("ls".into()))
                .await
                .is_none()
        );
        let selected =
            ScopedBundleHooks::new(dispatcher, &["acme/hook/permission.ask".to_string()]);
        assert!(matches!(
            selected
                .permission_ask(None, Action::Bash, &Resource::Command("ls".into()))
                .await,
            Some(Decision::AllowOnce)
        ));
    }

    #[tokio::test]
    async fn permission_chain_stops_on_first_decision_and_all_defer_returns_none() {
        let resource = Resource::Command("ls".into());
        let chain = HookChain::new(vec![
            Arc::new(PermissionHook(None)),
            Arc::new(PermissionHook(Some(Decision::Reject {
                feedback: Some("no".into()),
            }))),
            Arc::new(PermissionHook(Some(Decision::AllowOnce))),
        ]);
        assert!(matches!(
            chain.permission_ask(None, Action::Bash, &resource).await,
            Some(Decision::Reject { .. })
        ));
        let defer = HookChain::new(vec![
            Arc::new(PermissionHook(None)),
            Arc::new(PermissionHook(None)),
        ]);
        assert!(
            defer
                .permission_ask(None, Action::Bash, &resource)
                .await
                .is_none()
        );
    }
}
