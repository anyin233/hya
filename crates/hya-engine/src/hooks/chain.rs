use std::collections::BTreeSet;
use std::sync::Arc;

use hya_proto::{Envelope, ModelRef, SessionId};
use hya_runtime_abi::{HostError, NativeTurnAdmitInput, NativeTurnAdmitOutcome};
use hya_tool::{Action, Decision, Resource};
use serde_json::Value;

use super::*;

/// One hook source in a [`HookChain`], with its scope.
#[derive(Clone)]
pub struct HookEntry {
    hook: Arc<dyn NativeHook>,
    /// Hook names (`tool.execute.before`, `hya/goal:evaluate`, …) this entry
    /// admits; `None` admits every point the hook implements.
    admits: Option<Arc<BTreeSet<String>>>,
    events: bool,
    _owner: Option<Arc<dyn crate::RuntimeSourceOwner>>,
}

impl HookEntry {
    /// An entry admitting every point `hook` implements.
    #[must_use]
    pub fn new(hook: Arc<dyn NativeHook>) -> Self {
        Self {
            hook,
            admits: None,
            events: true,
            _owner: None,
        }
    }

    /// Restrict the entry to the hook references an agent declares
    /// (`<bundle>/hook/<name>`).
    #[must_use]
    pub fn scoped(hook: Arc<dyn NativeHook>, references: &[String]) -> Self {
        Self {
            admits: Some(Arc::new(
                references
                    .iter()
                    .filter_map(|name| {
                        name.rsplit_once("/hook/")
                            .map(|(_, local)| local.to_string())
                    })
                    .collect(),
            )),
            ..Self::new(hook)
        }
    }

    /// Keep `owner` (the source process handle) alive with the entry.
    #[must_use]
    pub fn retaining(hook: Arc<dyn NativeHook>, owner: Arc<dyn crate::RuntimeSourceOwner>) -> Self {
        Self {
            _owner: Some(owner),
            ..Self::new(hook)
        }
    }

    /// Narrow the entry to the names it already admits that `references`
    /// (`<bundle>/hook/<name>`) also admit.
    #[must_use]
    pub fn restrict(self, references: &[String]) -> Self {
        let narrowed = Self::scoped(Arc::clone(&self.hook), references)
            .admits
            .map(|names| match &self.admits {
                Some(current) => Arc::new(names.intersection(current).cloned().collect()),
                None => names,
            });
        Self {
            admits: narrowed,
            ..self
        }
    }

    /// The same entry without live events (they reach the source elsewhere).
    #[must_use]
    pub fn without_events(mut self) -> Self {
        self.events = false;
        self
    }

    /// The hook source.
    #[must_use]
    pub fn hook(&self) -> &Arc<dyn NativeHook> {
        &self.hook
    }

    fn admits(&self, name: &str) -> bool {
        self.admits
            .as_ref()
            .is_none_or(|names| names.contains(name))
    }

    fn handles(&self, point: HookPoint) -> bool {
        self.admits(point.name()) && self.hook.hook_points().contains(&point)
    }

    fn declares(&self, point: &str) -> bool {
        self.admits(point) && self.hook.declared_points().iter().any(|p| p == point)
    }
}

/// Ordered hook entries dispatched for one binding.
///
/// Rewrite hooks feed each output into the next entry. A veto or a
/// compaction skip stops the chain; observers fan out to every entry. A
/// failing entry is logged and skipped. Bundle-declared points are answered by
/// the latest entry that answers.
#[derive(Clone, Default)]
pub struct HookChain {
    entries: Vec<HookEntry>,
}

fn warn(hook: &dyn NativeHook, point: &str, error: &HostError) {
    tracing::warn!(hook_id = hook.id(), point, %error, "hook failed; skipping it");
}

impl HookChain {
    /// A chain in dispatch order.
    #[must_use]
    pub fn new(entries: Vec<HookEntry>) -> Self {
        Self { entries }
    }

    /// A chain of unscoped entries.
    #[must_use]
    pub fn of_hooks(hooks: impl IntoIterator<Item = Arc<dyn NativeHook>>) -> Self {
        Self::new(hooks.into_iter().map(HookEntry::new).collect())
    }

    /// Concatenate chains in order.
    #[must_use]
    pub fn concat<'a>(chains: impl IntoIterator<Item = &'a HookChain>) -> Self {
        Self::new(
            chains
                .into_iter()
                .flat_map(|chain| chain.entries.iter().cloned())
                .collect(),
        )
    }

    /// Append an entry.
    pub fn push(&mut self, entry: HookEntry) {
        self.entries.push(entry);
    }

    /// The entries in dispatch order.
    #[must_use]
    pub fn entries(&self) -> &[HookEntry] {
        &self.entries
    }

    /// Whether the chain has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn matching(&self, point: HookPoint) -> impl Iterator<Item = &Arc<dyn NativeHook>> {
        self.entries
            .iter()
            .filter(move |entry| entry.handles(point))
            .map(HookEntry::hook)
    }

    /// Whether any entry implements `point`.
    #[must_use]
    pub fn handles(&self, point: HookPoint) -> bool {
        self.matching(point).next().is_some()
    }

    /// Deliver a live envelope to every `event` observer, in publish order.
    pub fn dispatch_event(&self, envelope: &Envelope) {
        for entry in &self.entries {
            if entry.events && entry.handles(HookPoint::EventObserve) {
                entry.hook.on_event(envelope);
            }
        }
    }

    /// Whether every entry's source can still serve calls.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.entries.iter().all(|entry| entry.hook.is_healthy())
    }

    /// Identity of the chain's `permission.ask` decisions; `None` unless every
    /// entry admits and pins `permission.ask`.
    #[must_use]
    pub fn permission_identity(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut bytes = b"hya.hook.permission-chain/v1".to_vec();
        for entry in &self.entries {
            if !entry.admits(HookPoint::PermissionAsk.name()) {
                return None;
            }
            bytes.extend_from_slice(&entry.hook.permission_identity()?);
        }
        Some(Sha256::digest(bytes).into())
    }

    /// `permission.ask`: the first decision wins.
    pub async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        for hook in self.matching(HookPoint::PermissionAsk) {
            match hook.permission_ask(session, action, resource).await {
                Ok(Some(decision)) => return Some(decision),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "permission.ask", &error),
            }
        }
        None
    }

    /// `permission.approve`: the first decision wins.
    pub async fn permission_approve(
        &self,
        input: NativePermissionApproveInput,
    ) -> Option<Decision> {
        for hook in self.matching(HookPoint::PermissionApprove) {
            match hook.permission_approve(input.clone()).await {
                Ok(Some(decision)) => return Some(decision),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "permission.approve", &error),
            }
        }
        None
    }

    /// `tool.visibility`: hides accumulate, `allow_only` intersects.
    pub async fn tool_visibility(&self, input: ToolVisibilityInput) -> ToolVisibilityOutcome {
        let mut remaining = input.candidate_tools.clone();
        let mut hidden: Vec<String> = Vec::new();
        let mut allow_only = false;
        for hook in self.matching(HookPoint::ToolVisibility) {
            let step = ToolVisibilityInput {
                candidate_tools: remaining.clone(),
                ..input.clone()
            };
            match hook.tool_visibility(step).await {
                Ok(ToolVisibilityOutcome::Preserve) => {}
                Ok(ToolVisibilityOutcome::Hide { tools }) => {
                    remaining.retain(|name| !tools.contains(name));
                    for tool in tools {
                        if !hidden.contains(&tool) {
                            hidden.push(tool);
                        }
                    }
                }
                Ok(ToolVisibilityOutcome::AllowOnly { tools }) => {
                    remaining.retain(|name| tools.contains(name));
                    allow_only = true;
                }
                Err(error) => warn(hook.as_ref(), "tool.visibility", &error),
            }
        }
        if allow_only {
            ToolVisibilityOutcome::AllowOnly { tools: remaining }
        } else if !hidden.is_empty() {
            ToolVisibilityOutcome::Hide { tools: hidden }
        } else {
            ToolVisibilityOutcome::Preserve
        }
    }

    /// `command.execute.before`: rewrites the command text.
    pub async fn command_execute_before(&self, mut input: NativeCommandBeforeInput) -> String {
        for hook in self.matching(HookPoint::CommandExecuteBefore) {
            match hook.command_execute_before(input.clone()).await {
                Ok(text) => input.text = text,
                Err(error) => warn(hook.as_ref(), "command.execute.before", &error),
            }
        }
        input.text
    }

    /// `experimental.text.complete`: rewrites completed assistant text.
    pub async fn text_complete(&self, mut input: NativeTextCompleteInput) -> String {
        for hook in self.matching(HookPoint::TextComplete) {
            match hook.text_complete(input.clone()).await {
                Ok(text) => input.text = text,
                Err(error) => warn(hook.as_ref(), "text.complete", &error),
            }
        }
        input.text
    }

    /// `message.user.before`: rewrites user text before admission.
    pub async fn message_user_before(&self, mut input: NativeMessageUserBeforeInput) -> String {
        for hook in self.matching(HookPoint::MessageUserBefore) {
            match hook.message_user_before(input.clone()).await {
                Ok(text) => input.text = text,
                Err(error) => warn(hook.as_ref(), "message.user.before", &error),
            }
        }
        input.text
    }

    /// `turn.admit`: wait for readiness in order, then admit with accumulated steering.
    pub async fn turn_admit(&self, input: NativeTurnAdmitInput) -> Result<Vec<String>, HostError> {
        let mut steering = Vec::new();
        for hook in self.matching(HookPoint::TurnAdmit) {
            match hook.turn_admit(input.clone()).await? {
                NativeTurnAdmitOutcome::Admit => {}
                NativeTurnAdmitOutcome::Defer { until } => {
                    until.await.map_err(|_| {
                        HostError::Unavailable(format!(
                            "admission hook `{}` dropped readiness",
                            hook.id()
                        ))
                    })?;
                }
                NativeTurnAdmitOutcome::Refuse { reason } => {
                    return Err(HostError::InvalidInput(reason));
                }
                NativeTurnAdmitOutcome::Steer { message } => steering.push(message),
            }
        }
        Ok(steering)
    }

    /// `chat.params`: rewrites the request payload ([`chat_params_value`]).
    pub async fn chat_params(&self, mut input: NativeChatParamsInput) -> Value {
        for hook in self.matching(HookPoint::ChatParams) {
            match hook.chat_params(input.clone()).await {
                Ok(params) => input.params = params,
                Err(error) => warn(hook.as_ref(), "chat.params", &error),
            }
        }
        input.params
    }

    /// `tool.execute.before`: rewrites arguments; the first veto wins.
    pub async fn tool_execute_before(
        &self,
        mut input: NativeToolBeforeInput,
    ) -> NativeToolBeforeOutcome {
        for hook in self.matching(HookPoint::ToolExecuteBefore) {
            match hook.tool_execute_before(input.clone()).await {
                Ok(NativeToolBeforeOutcome::Continue { input: next }) => input.input = next,
                Ok(veto @ NativeToolBeforeOutcome::Veto { .. }) => return veto,
                Err(error) => warn(hook.as_ref(), "tool.execute.before", &error),
            }
        }
        NativeToolBeforeOutcome::Continue { input: input.input }
    }

    /// `tool.execute.after`: rewrites the tool result.
    pub async fn tool_execute_after(&self, mut input: NativeToolAfterInput) -> NativeToolResult {
        for hook in self.matching(HookPoint::ToolExecuteAfter) {
            match hook.tool_execute_after(input.clone()).await {
                Ok(NativeToolAfterOutcome::Continue { result }) => input.result = result,
                Err(error) => warn(hook.as_ref(), "tool.execute.after", &error),
            }
        }
        input.result
    }

    /// `model.fallback`: the first named model wins.
    pub async fn model_fallback(&self, input: NativeModelFallbackInput) -> Option<ModelRef> {
        for hook in self.matching(HookPoint::ModelFallback) {
            match hook.model_fallback(input.clone()).await {
                Ok(Some(model)) => return Some(model),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "model.fallback", &error),
            }
        }
        None
    }

    /// `compaction.before`: a skip stops the chain; the last replacement wins.
    pub async fn compaction_before(
        &self,
        input: NativeCompactionBeforeInput,
    ) -> NativeCompactionDecision {
        let mut replacement = None;
        for hook in self.matching(HookPoint::CompactionBefore) {
            match hook.compaction_before(input.clone()).await {
                Ok(NativeCompactionDecision::Proceed) => {}
                Ok(NativeCompactionDecision::Replace { instructions }) => {
                    replacement = Some(instructions);
                }
                Ok(skip @ NativeCompactionDecision::Skip { .. }) => return skip,
                Err(error) => warn(hook.as_ref(), "compaction.before", &error),
            }
        }
        replacement.map_or(NativeCompactionDecision::Proceed, |instructions| {
            NativeCompactionDecision::Replace { instructions }
        })
    }

    /// `compaction.after` observers.
    pub async fn compaction_after(&self, input: NativeCompactionAfterInput) {
        for hook in self.matching(HookPoint::CompactionAfter) {
            if let Err(error) = hook.compaction_after(input.clone()).await {
                warn(hook.as_ref(), "compaction.after", &error);
            }
        }
    }

    /// `session.start` observers.
    pub async fn session_start(&self, session: SessionId) {
        for hook in self.matching(HookPoint::SessionStart) {
            if let Err(error) = hook
                .session_start(NativeSessionLifecycleInput { session })
                .await
            {
                warn(hook.as_ref(), "session.start", &error);
            }
        }
    }

    /// `session.end` observers.
    pub async fn session_end(&self, session: SessionId) {
        for hook in self.matching(HookPoint::SessionEnd) {
            if let Err(error) = hook
                .session_end(NativeSessionLifecycleInput { session })
                .await
            {
                warn(hook.as_ref(), "session.end", &error);
            }
        }
    }

    /// `agent.spawn` observers.
    pub async fn agent_spawn(&self, input: NativeAgentSpawnInput) {
        for hook in self.matching(HookPoint::AgentSpawn) {
            if let Err(error) = hook.agent_spawn(input.clone()).await {
                warn(hook.as_ref(), "agent.spawn", &error);
            }
        }
    }

    /// `model.stream.permit`: every entry's guard, held together.
    pub async fn model_stream_permit(
        &self,
        input: NativeStreamPermitInput,
    ) -> Vec<NativeStreamPermit> {
        let mut permits = Vec::new();
        for hook in self.matching(HookPoint::ModelStreamPermit) {
            match hook.model_stream_permit(input.clone()).await {
                Ok(Some(permit)) => permits.push(permit),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "model.stream.permit", &error),
            }
        }
        permits
    }

    /// `turn.stop` observers, awaited in order.
    pub async fn turn_stop(&self, input: NativeTurnStopInput) {
        for hook in self.matching(HookPoint::TurnStop) {
            if let Err(error) = hook.turn_stop(input.clone()).await {
                warn(hook.as_ref(), "turn.stop", &error);
            }
        }
    }

    /// `session.close.before`: latest entry first; the first error refuses.
    ///
    /// # Errors
    /// The refusing hook's error.
    pub async fn session_close(&self, input: NativeSessionCloseInput) -> Result<(), HostError> {
        let hooks: Vec<_> = self.matching(HookPoint::SessionClose).collect();
        for hook in hooks.into_iter().rev() {
            hook.session_close(input.clone()).await?;
        }
        Ok(())
    }

    /// `run.headless`: every entry's pending-work line, in order.
    pub async fn headless_run(&self, input: NativeHeadlessRunInput) -> Vec<String> {
        let mut lines = Vec::new();
        for hook in self.matching(HookPoint::HeadlessRun) {
            match hook.headless_run(input.clone()).await {
                Ok(Some(line)) => lines.push(line),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "run.headless", &error),
            }
        }
        lines
    }

    /// `session.restore`: every entry's restore text, in order.
    pub async fn session_restore(&self, session: SessionId) -> Vec<String> {
        let mut texts = Vec::new();
        for hook in self.matching(HookPoint::SessionRestore) {
            match hook
                .session_restore(NativeSessionRestoreInput { session })
                .await
            {
                Ok(Some(text)) => texts.push(text),
                Ok(None) => {}
                Err(error) => warn(hook.as_ref(), "session.restore", &error),
            }
        }
        texts
    }

    /// `runtime.restored` observers.
    pub async fn runtime_restored(&self) {
        for hook in self.matching(HookPoint::RuntimeRestored) {
            if let Err(error) = hook.runtime_restored().await {
                warn(hook.as_ref(), "runtime.restored", &error);
            }
        }
    }

    /// Whether any entry implements the bundle-declared point `point`.
    #[must_use]
    pub fn declares(&self, point: &str) -> bool {
        self.entries.iter().any(|entry| entry.declares(point))
    }

    /// Dispatch the bundle-declared point `point`: the latest entry that
    /// answers wins; `None` when no entry answered.
    pub async fn invoke_declared(&self, point: &str, input: Value) -> Option<Value> {
        for entry in self
            .entries
            .iter()
            .rev()
            .filter(|entry| entry.declares(point))
        {
            match entry.hook.invoke_declared(point, input.clone()).await {
                Ok(Some(reply)) => return Some(reply),
                Ok(None) => {}
                Err(error) => warn(entry.hook.as_ref(), point, &error),
            }
        }
        None
    }
}
