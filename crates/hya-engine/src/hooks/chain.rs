use std::sync::Arc;

use super::*;

/// Ordered composition of hook dispatchers captured for one runtime binding.
///
/// Rewrite hooks feed each output into the next dispatcher. Guard vetoes and
/// compaction skips stop the chain immediately; observation hooks fan out to
/// every dispatcher. Evaluator hooks try capable dispatchers in order and only
/// fail after every provider failed.
pub struct HookChain {
    dispatchers: Vec<Arc<dyn HookDispatcher>>,
}

impl HookChain {
    /// Create a chain in deterministic dispatch order.
    #[must_use]
    pub fn new(dispatchers: Vec<Arc<dyn HookDispatcher>>) -> Self {
        Self { dispatchers }
    }

    /// Whether the chain contains no dispatchers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dispatchers.is_empty()
    }
}

#[async_trait]
impl HookDispatcher for HookChain {
    fn dispatch_event(&self, envelope: &Envelope) {
        for dispatcher in &self.dispatchers {
            dispatcher.dispatch_event(envelope);
        }
    }

    fn is_healthy(&self) -> bool {
        self.dispatchers
            .iter()
            .all(|dispatcher| dispatcher.is_healthy())
    }

    fn permission_semantic_identity_v1(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut bytes = b"hya.hook.permission-chain/v1".to_vec();
        for dispatcher in &self.dispatchers {
            bytes.extend_from_slice(&dispatcher.permission_semantic_identity_v1()?);
        }
        Some(Sha256::digest(bytes).into())
    }

    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        for dispatcher in &self.dispatchers {
            if let Some(decision) = dispatcher.permission_ask(session, action, resource).await {
                return Some(decision);
            }
        }
        None
    }

    async fn permission_approve(&self, input: PermissionApproveInput) -> Option<Decision> {
        for dispatcher in &self.dispatchers {
            if let Some(decision) = dispatcher.permission_approve(input.clone()).await {
                return Some(decision);
            }
        }
        None
    }

    async fn command_execute_before(
        &self,
        mut input: CommandExecuteBeforeInput,
    ) -> CommandExecuteBeforeOutcome {
        for dispatcher in &self.dispatchers {
            let CommandExecuteBeforeOutcome::Continue { text } = dispatcher
                .command_execute_before(CommandExecuteBeforeInput {
                    session: input.session,
                    command: input.command.clone(),
                    arguments: input.arguments.clone(),
                    text: input.text,
                })
                .await;
            input.text = text;
        }
        CommandExecuteBeforeOutcome::Continue { text: input.text }
    }

    async fn text_complete(&self, mut input: TextCompleteInput) -> TextCompleteOutcome {
        for dispatcher in &self.dispatchers {
            let TextCompleteOutcome::Continue { text } = dispatcher
                .text_complete(TextCompleteInput {
                    session: input.session,
                    message: input.message,
                    part: input.part,
                    text: input.text,
                })
                .await;
            input.text = text;
        }
        TextCompleteOutcome::Continue { text: input.text }
    }

    async fn message_user_before(
        &self,
        mut input: MessageUserBeforeInput,
    ) -> MessageUserBeforeOutcome {
        for dispatcher in &self.dispatchers {
            let MessageUserBeforeOutcome::Continue { text } = dispatcher
                .message_user_before(MessageUserBeforeInput {
                    session: input.session,
                    text: input.text,
                })
                .await;
            input.text = text;
        }
        MessageUserBeforeOutcome::Continue { text: input.text }
    }

    async fn chat_params(&self, mut input: ChatParamsInput) -> ChatParamsOutcome {
        for dispatcher in &self.dispatchers {
            let ChatParamsOutcome::Continue { request } = dispatcher
                .chat_params(ChatParamsInput {
                    session: input.session,
                    root_session: input.root_session,
                    agent: input.agent.clone(),
                    message: input.message,
                    request: input.request,
                })
                .await;
            input.request = request;
        }
        ChatParamsOutcome::Continue {
            request: input.request,
        }
    }

    async fn tool_execute_before(
        &self,
        mut input: ToolExecuteBeforeInput,
    ) -> ToolExecuteBeforeOutcome {
        for dispatcher in &self.dispatchers {
            match dispatcher
                .tool_execute_before(ToolExecuteBeforeInput {
                    session: input.session,
                    message: input.message,
                    call: input.call,
                    tool: input.tool.clone(),
                    input: input.input,
                })
                .await
            {
                ToolExecuteBeforeOutcome::Continue { input: next } => input.input = next,
                veto @ ToolExecuteBeforeOutcome::Veto { .. } => return veto,
            }
        }
        ToolExecuteBeforeOutcome::Continue { input: input.input }
    }

    async fn tool_execute_after(
        &self,
        mut input: ToolExecuteAfterInput,
    ) -> ToolExecuteAfterOutcome {
        for dispatcher in &self.dispatchers {
            let ToolExecuteAfterOutcome::Continue { result } = dispatcher
                .tool_execute_after(ToolExecuteAfterInput {
                    session: input.session,
                    message: input.message,
                    call: input.call,
                    tool: input.tool.clone(),
                    input: input.input.clone(),
                    result: input.result,
                })
                .await;
            input.result = result;
        }
        ToolExecuteAfterOutcome::Continue {
            result: input.result,
        }
    }

    async fn model_fallback(&self, input: ModelFallbackInput) -> ModelFallbackOutcome {
        for dispatcher in &self.dispatchers {
            if let retry @ ModelFallbackOutcome::Retry { .. } =
                dispatcher.model_fallback(input.clone()).await
            {
                return retry;
            }
        }
        ModelFallbackOutcome::GiveUp
    }

    async fn compaction_before(&self, input: CompactionBeforeInput) -> CompactionDecision {
        let mut replacement = None;
        for dispatcher in &self.dispatchers {
            match dispatcher
                .compaction_before(CompactionBeforeInput {
                    session: input.session,
                    trigger: input.trigger,
                    messages_token_estimate: input.messages_token_estimate,
                })
                .await
            {
                CompactionDecision::Proceed => {}
                CompactionDecision::Replace { instructions } => replacement = Some(instructions),
                skip @ CompactionDecision::Skip { .. } => return skip,
            }
        }
        replacement.map_or(CompactionDecision::Proceed, |instructions| {
            CompactionDecision::Replace { instructions }
        })
    }

    async fn compaction_after(&self, input: CompactionAfterInput) {
        for dispatcher in &self.dispatchers {
            dispatcher
                .compaction_after(CompactionAfterInput {
                    session: input.session,
                    summary_tokens: input.summary_tokens,
                })
                .await;
        }
    }

    async fn session_start(&self, input: SessionLifecycleInput) {
        for dispatcher in &self.dispatchers {
            dispatcher
                .session_start(SessionLifecycleInput {
                    session: input.session,
                })
                .await;
        }
    }

    async fn session_end(&self, input: SessionLifecycleInput) {
        for dispatcher in &self.dispatchers {
            dispatcher
                .session_end(SessionLifecycleInput {
                    session: input.session,
                })
                .await;
        }
    }

    async fn agent_spawn(&self, input: AgentSpawnInput) {
        for dispatcher in &self.dispatchers {
            dispatcher
                .agent_spawn(AgentSpawnInput {
                    parent: input.parent,
                    child: input.child,
                })
                .await;
        }
    }

    fn has_goal_evaluate(&self) -> bool {
        self.dispatchers
            .iter()
            .any(|dispatcher| dispatcher.has_goal_evaluate())
    }

    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<GoalEvaluateReply, CoreError> {
        let mut last_error = None;
        for dispatcher in self
            .dispatchers
            .iter()
            .rev()
            .filter(|dispatcher| dispatcher.has_goal_evaluate())
        {
            match dispatcher.goal_evaluate(condition, transcript).await {
                Ok(reply) => return Ok(reply),
                Err(error) => last_error = Some(error),
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
        for dispatcher in self.dispatchers.iter().rev() {
            match dispatcher.loop_verify(target, transcript).await {
                Ok(reply) => return Ok(reply),
                Err(error) => last_error = Some(error),
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
        for dispatcher in self.dispatchers.iter().rev() {
            match dispatcher
                .loop_plan(target, history, last, planner_notes)
                .await
            {
                Ok(reply) => return Ok(reply),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .unwrap_or_else(|| CoreError::Invalid("loop.planner hook not registered".to_string())))
    }

    async fn loop_should_stop(&self, target: &str, transcript: &str) -> Option<String> {
        for dispatcher in self.dispatchers.iter().rev() {
            if let Some(reason) = dispatcher.loop_should_stop(target, transcript).await {
                return Some(reason);
            }
        }
        None
    }
}
