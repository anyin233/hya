//! The engine state and round callbacks that drive the context kernel.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_context::rewrite::RewriteCtx;
use hya_proto::{
    ActorClaim, AgentName, Event, FinishReason, MessageId, ModelRef, PartProjection, Projection,
    SessionId, TokenUsage, ToolSchema,
};
use hya_provider::{CompletionRequest, EventStream, ProviderRetryClass, ProviderRouter};
use hya_tool::AgentDef;
use tokio_util::sync::CancellationToken;

use super::super::roots::SessionWorkspace;
use super::super::stream_round::TurnEmit;
use super::super::{
    AgentSpec, SessionEngine, agent_roster, effective_agent_for_binding_with_sidecar_tools,
    session_workdir,
};
use super::hooks::{self as turn_hooks, OpenRequest, TurnHook};
use super::messages::{AttachmentData, prompt_digests, system_prompt};
use super::rewrite_host::{HostErrors, TurnLog, TurnModels, TurnTranscript, model_window};
use super::tool_gate::{ToolState, TurnToolGate};
use super::{RequestLineage, TurnExecution, TurnRebindInputs};
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{
    HookChain, HookPoint, NativeChatParamsInput, activation_hook_for, apply_chat_params,
    chat_params_value, replace_activation_hooks,
};
use crate::kernel::{self, Failure, Harness, Round, ToolCall, ToolGate, View};
use crate::prompt::{PromptEnv, discover_context_files, render_environment_and_context, today};
use crate::runtime_registry::CompiledResourceView;

impl SessionEngine {
    /// Drive the streaming rounds of one turn activation on the kernel loop,
    /// then finish the assistant message, unless the turn was cancelled (the
    /// turn end closes it then).
    ///
    /// Round-boundary dynamic rebinding (Root turns only): at the top of every
    /// round after the first, a Root activation re-runs
    /// `bind_session_runtime`. When the fresh binding carries a different
    /// runtime generation, the round swaps in the fresh binding, a freshly
    /// compiled resource view (retaining the activation's sidecar tools), a
    /// refreshed agent roster, and a re-materialized agent prompt — so tools,
    /// hooks, skills, and agent definitions published mid-turn take effect at
    /// the next model call. The directory skill index stays in the initial
    /// context. Any rebind or
    /// recompilation failure is logged and ignored: the turn keeps the current
    /// generation and never dies from a rebind failure (fail-open). Bound and
    /// Resolved activations (subagents, Workflow members) pin their
    /// activation-time binding for the whole turn and never rebind.
    pub(super) async fn run_turn_rounds(
        &self,
        session: SessionId,
        message: MessageId,
        agent: &AgentSpec,
        execution: TurnExecution<'_>,
    ) -> Result<FinishReason, CoreError> {
        let TurnExecution {
            router,
            binding,
            resources,
            agents,
            cancel,
            apply_default_overlays,
            external_dirs,
            workspace,
            actor_claim,
            guidance,
            explicit_route,
            ran_known_tool,
            explicit_model,
            rebind,
            effort_fallback,
        } = execution;
        // Depth in the subagent tree, derived from the parent chain. Subagents use
        // the general live provider-stream class; the root uses the independent
        // reserved class so root progress never waits behind background work.
        let projection = self.store.read_projection_shared(session).await?;
        let depth = match (&self.governor, projection.session.parent) {
            (Some(_), Some(parent)) => self
                .session_lineage(parent)
                .await
                .map(|(_, depth)| depth + 1)
                .unwrap_or(0),
            _ => 0,
        };
        let mut events = Vec::with_capacity(2);
        let workdir = session_workdir(agent, &projection);
        let initial_context = match &projection.session.initial_context {
            Some(text) => text.clone(),
            None => {
                let env = PromptEnv {
                    platform: std::env::consts::OS.to_string(),
                    date: today(),
                };
                let files = if self.pure_context {
                    Vec::new()
                } else {
                    discover_context_files(&workdir)
                };
                let mut text = render_environment_and_context(&env, &files);
                if let Some(skills) = resources.directory_skills_prompt_section() {
                    text.push('\n');
                    text.push_str(&skills);
                }
                events.push(Event::InitialContextRecorded {
                    session,
                    text: text.clone(),
                });
                text
            }
        };
        // The first round's model: the tool list depends on it.
        let initial_stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&agent.name)
            .as_str()
            .to_string();
        let model = self.requested_turn_model(
            &router,
            apply_default_overlays.then_some(&binding),
            &initial_stable_id,
            explicit_model,
            &projection,
            agent,
        );
        drop(projection);
        // Digests of the system prompt and tool list, recorded with the
        // binding at turn start and again whenever a round's differ.
        let tools = self
            .offered_tools(session, Some(&agent.name), &resources, depth, &model)
            .await;
        let system = system_prompt(
            agent,
            &workdir,
            Some(&initial_context),
            &tools,
            depth,
            self.use_tool_family_prompts,
        );
        let recorded_digests = prompt_digests(&system, &tools)?;
        events.push(Event::TurnBindingRecorded {
            session,
            message,
            generation: binding.generation(),
            system_sha256: recorded_digests.0.clone(),
            tools_sha256: recorded_digests.1.clone(),
        });
        self.emit_batch_for_actor(actor_claim, session, events)
            .await?;
        // Keep the session's channel policy synced with the admitted turn binding
        // so the `hya/agent-channels` `UnreadMailSteerHook` sees the exact
        // binding-pinned policy on every tool call.
        let steer_policy = crate::channel_policy::from_binding(&binding)
            .map_err(CoreError::bundle)?
            .snapshot_for(agent.name.as_str());
        self.update_session_channel_policy(session, steer_policy);
        let emit = TurnEmit {
            engine: self,
            actor_claim,
        };
        let provider = Arc::clone(&router);
        let scope = kernel::Scope {
            watches: &self.workspace_watches,
            store: &self.store,
            emit: &emit,
            complete: Some(self),
            provider: &provider,
            session,
            message,
        };
        let mut harness = EngineHarness {
            engine: self,
            session,
            message,
            agent,
            binding,
            resources,
            agents,
            live_agent: agent.clone(),
            rebind,
            apply_default_overlays,
            explicit_model,
            entries: turn_hooks::entries(self, actor_claim, explicit_route, cancel, depth),
            ran_known_tool,
            guidance,
            external_dirs,
            workspace,
            actor_claim,
            cancel,
            depth,
            defer_steer: false,
            recorded_digests,
            tool_state: ToolState::default(),
            nested_engine: None,
            root_session_cache: None,
            rounds: 0,
            router,
            stable_id: String::new(),
            model,
            attachments: AttachmentData::new(),
            parent_session: None,
            offered: Vec::new(),
            retries: 0,
            serving_model: None,
            reasoning_tokens: None,
            empty_turns: 0,
            output_tokens: None,
            last_finish: None,
            total_tokens: None,
            effort_fallback,
        };
        let finish = kernel::drive(&scope, &mut harness, cancel).await?;
        if finish != FinishReason::Cancelled {
            self.close_turn_message_with_tokens(
                actor_claim,
                session,
                message,
                finish,
                None,
                harness.total_tokens,
            )
            .await?;
        }
        Ok(finish)
    }
}

/// One turn's state across the kernel's rounds.
struct EngineHarness<'a> {
    ran_known_tool: Option<&'a mut bool>,
    engine: &'a SessionEngine,
    session: SessionId,
    message: MessageId,
    /// The activation's agent spec; the round's model falls back to its model.
    agent: &'a AgentSpec,
    // The live runtime; a round-boundary rebind replaces these four.
    binding: TurnBinding,
    resources: Arc<CompiledResourceView>,
    agents: Arc<[AgentDef]>,
    live_agent: AgentSpec,
    /// Present for Root activations only.
    rebind: Option<TurnRebindInputs<'a>>,
    apply_default_overlays: bool,
    explicit_model: Option<&'a ModelRef>,
    entries: Vec<Box<dyn TurnHook + 'a>>,
    guidance: Option<Arc<str>>,
    external_dirs: &'a [PathBuf],
    workspace: Arc<SessionWorkspace>,
    actor_claim: Option<&'a ActorClaim>,
    cancel: &'a CancellationToken,
    depth: u32,
    defer_steer: bool,
    /// Prompt digests last recorded in `TurnBindingRecorded`.
    recorded_digests: (String, String),
    tool_state: ToolState,
    /// Engine handle for nested tool calls, cloned once per turn on first use.
    nested_engine: Option<Arc<SessionEngine>>,
    /// Root of this session's spawn tree, resolved on first hook use.
    root_session_cache: Option<SessionId>,
    /// Rounds collected so far.
    rounds: u32,
    // Retained from round preparation through retries and served-route accounting.
    router: Arc<ProviderRouter>,
    // Resolved per round in `before_round`.
    stable_id: String,
    model: ModelRef,
    attachments: AttachmentData,
    parent_session: Option<SessionId>,
    /// Tool schemas the round's request offered.
    offered: Vec<ToolSchema>,
    /// Retries of the current round.
    retries: usize,
    serving_model: Option<ModelRef>,
    reasoning_tokens: Option<u64>,
    empty_turns: usize,
    output_tokens: Option<u64>,
    last_finish: Option<FinishReason>,
    total_tokens: Option<TokenUsage>,
    effort_fallback: super::super::TurnEffortFallback,
}

impl EngineHarness<'_> {
    /// Swap in a new runtime generation when the registry has one.
    async fn rebind(
        &mut self,
        projection: &Projection,
        provider_changed: bool,
    ) -> Result<(), CoreError> {
        let Some(inputs) = &self.rebind else {
            return Ok(());
        };
        let engine = self.engine;
        let session = self.session;
        let workdir = session_workdir(&self.live_agent, projection);
        // Skills are discovered into the registry explicitly (they are not
        // part of the bind itself), so a workdir skill change must refresh
        // them before the bind for the new generation to carry it. Fail-open
        // like every other rebind step.
        if let Err(error) = engine.refresh_runtime(|candidate| {
            candidate.refresh_skills(&workdir);
            Ok(())
        }) {
            tracing::warn!(
                session = %session,
                error = %error,
                "round rebind could not refresh skills; keeping current skills"
            );
        }
        let fresh = match engine
            .bind_session_runtime_with_router(session, &workdir, &self.router)
            .await
        {
            Ok(fresh) if fresh.generation() != self.binding.generation() => fresh,
            Ok(fresh) => {
                if provider_changed {
                    self.binding = fresh;
                }
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    session = %session,
                    %error,
                    "round rebind failed; keeping the current runtime generation"
                );
                return Ok(());
            }
        };
        // Mirror the Root activation compile path: agent_base resolution,
        // guidance layer, skills prompt, sidecar tools retained, plus a fresh
        // spawn roster.
        let stable_id = self.stable_id.as_str();
        let rematerialized = effective_agent_for_binding_with_sidecar_tools(
            inputs.base_agent,
            stable_id,
            &fresh,
            self.guidance.as_deref(),
            &inputs.sidecar_tools,
        )
        .and_then(|(materialized, compiled)| {
            agent_roster(&fresh, stable_id).map(|roster| (materialized, compiled, roster))
        });
        let (materialized, compiled, roster) = match rematerialized {
            Ok(rematerialized) => rematerialized,
            Err(error) => {
                tracing::warn!(
                    session = %session,
                    %error,
                    "round rebind failed to recompile resources; keeping the current runtime generation"
                );
                return Ok(());
            }
        };
        tracing::info!(
            session = %session,
            generation = ?fresh.generation(),
            "round rebind applied a new runtime generation"
        );
        let mut hooks = fresh.bundle_hooks_for_agent(stable_id);
        if let Some(sidecar_hooks) = &inputs.sidecar_hooks {
            hooks.extend(sidecar_hooks.entries().iter().cloned());
        }
        self.agents = engine
            .annotate_agent_efforts_with(
                roster,
                Some(&fresh),
                Some((session, &mut self.effort_fallback)),
            )
            .await?;
        self.binding = fresh;
        self.live_agent = materialized;
        self.resources = compiled;
        let hooks = (!hooks.is_empty()).then(|| Arc::new(HookChain::new(hooks)));
        replace_activation_hooks(session, hooks);
        let channel_policy = crate::channel_policy::from_binding(&self.binding)
            .map_err(CoreError::bundle)?
            .snapshot_for(self.live_agent.name.as_str());
        engine.update_session_channel_policy(session, channel_policy);
        Ok(())
    }
}

#[async_trait]
impl ToolGate for EngineHarness<'_> {
    async fn call(&mut self, call: ToolCall) -> Result<(), CoreError> {
        let mut gate = TurnToolGate {
            engine: self.engine,
            session: self.session,
            message: self.message,
            binding: &self.binding,
            resources: &self.resources,
            agents: &self.agents,
            agent: &self.live_agent,
            external_dirs: self.external_dirs,
            workspace: &self.workspace,
            actor_claim: self.actor_claim,
            guidance: &self.guidance,
            cancel: self.cancel,
            state: &mut self.tool_state,
            nested_engine: &mut self.nested_engine,
            defer_steer: self.defer_steer,
            parent_session: self.parent_session,
            offered: &self.offered,
        };
        gate.call(call).await?;
        self.defer_steer = gate.defer_steer;
        Ok(())
    }

    /// Unrun calls stay open; the turn end closes them as `CANCELLED`.
    async fn cancelled(&mut self, _calls: Vec<ToolCall>) -> Result<(), CoreError> {
        Ok(())
    }
}

#[async_trait]
impl Harness for EngineHarness<'_> {
    /// Prepare the runtime and reduce context before the next model call.
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError> {
        self.defer_steer = false;
        let engine = self.engine;
        for entry in &mut self.entries {
            entry.before_round().await?;
        }
        if activation_hook_for(self.session).is_some_and(|hooks| !hooks.is_healthy()) {
            return Err(CoreError::Cancelled);
        }
        self.stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&self.live_agent.name)
            .as_str()
            .to_string();
        if self.rounds > 0 {
            let router = engine.provider_router();
            let provider_changed = !Arc::ptr_eq(&self.router, &router);
            self.router = router;
            self.rebind(projection, provider_changed).await?;
        }
        self.model = engine.requested_turn_model(
            &self.router,
            self.apply_default_overlays.then_some(&self.binding),
            &self.stable_id,
            self.explicit_model,
            projection,
            self.agent,
        );
        self.parent_session = projection.session.parent;
        self.attachments = engine.attachment_data(self.session, projection).await?;
        // What the system prompt and tools cost: a request with no messages.
        let tools = engine
            .offered_tools(
                self.session,
                Some(&self.live_agent.name),
                &self.resources,
                self.depth,
                &self.model,
            )
            .await;
        let workdir = session_workdir(&self.live_agent, projection);
        let overhead_tokens = engine.token_accounting.estimate_request(
            &[],
            Some(&system_prompt(
                &self.live_agent,
                &workdir,
                projection.session.initial_context.as_deref(),
                &tools,
                self.depth,
                engine.use_tool_family_prompts,
            )),
            &tools,
            self.router.reasoning_replay_policy(&self.model),
        );
        let errors = HostErrors::default();
        let config = engine.rewriter_config();
        let log = TurnLog {
            engine,
            actor_claim: self.actor_claim,
            errors: &errors,
        };
        let transcript = TurnTranscript {
            agent: &self.live_agent,
            model: &self.model,
            attachments: &self.attachments,
        };
        let models = TurnModels {
            router: &self.router,
            errors: &errors,
        };
        let cx = RewriteCtx {
            session: self.session,
            message: self.message,
            model: &self.model,
            projection,
            window: model_window(&self.router, &self.model),
            overhead_tokens,
            pressure: None,
            workdir: &workdir,
            config: &config,
            log: &log,
            transcript: &transcript,
            models: Some(&models),
            hooks: Some(engine),
        };
        kernel::before_round(cx, &engine.rewriters)
            .await
            .map_err(|error| errors.into_error(error))
    }

    async fn request(&mut self, view: &View<'_>) -> Result<CompletionRequest, CoreError> {
        let engine = self.engine;
        let session = self.session;
        // Resolved every round, so a preference saved mid-session applies to
        // the next request without a restart. `live_agent.reasoning` is the
        // Agent's authored effort; the user's per-Agent choices outrank it.
        let effort = engine
            .effective_effort_with(
                &self.router,
                &self.model,
                Some(&self.stable_id),
                self.live_agent.reasoning,
                Some(&self.binding),
                Some((session, &mut self.effort_fallback)),
            )
            .await?
            .effort;
        let tools = engine
            .offered_tools(
                session,
                Some(&self.live_agent.name),
                &self.resources,
                self.depth,
                &self.model,
            )
            .await;
        let system = system_prompt(
            &self.live_agent,
            &session_workdir(&self.live_agent, view.projection),
            view.projection.session.initial_context.as_deref(),
            &tools,
            self.depth,
            engine.use_tool_family_prompts,
        );
        let digests = prompt_digests(&system, &tools)?;
        if digests != self.recorded_digests {
            engine
                .emit_turn_binding(
                    self.actor_claim,
                    session,
                    self.message,
                    &self.binding,
                    &digests,
                )
                .await?;
            self.recorded_digests = digests;
        }
        let request = kernel::request::request(
            view,
            &system,
            tools,
            &self.model,
            &self.live_agent.name,
            &self.attachments,
            effort,
        );
        let request = if let Some(hooks) = engine
            .active_hook_dispatcher(session)
            .filter(|hooks| hooks.handles(HookPoint::ChatParams))
        {
            let root_session = engine
                .request_root_session(session, &mut self.root_session_cache)
                .await;
            let params = hooks
                .chat_params(NativeChatParamsInput {
                    session,
                    root_session,
                    agent: Some(AgentName::new(self.stable_id.as_str())),
                    message: self.message,
                    params: chat_params_value(&request),
                })
                .await;
            let mut request = request;
            apply_chat_params(&mut request, params);
            request
        } else {
            request
        };
        self.offered = request.tools.clone();
        Ok(request)
    }

    async fn open(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<Option<(EventStream, ModelRef)>, CoreError> {
        let engine = self.engine;
        let (session, message) = (self.session, self.message);
        for entry in &mut self.entries {
            entry.before_open().await?;
        }
        let cancel = self.cancel;
        let input = OpenRequest {
            router: &self.router,
            request,
            session,
            message,
        };
        let open = async {
            for entry in &mut self.entries {
                if let Some(opened) = entry.open(&input).await? {
                    return Ok(opened);
                }
            }
            if let Some(model) = &self.serving_model {
                return Ok((
                    engine
                        .stream_model_candidate(&self.router, request, model, session, message)
                        .await?,
                    model.clone(),
                ));
            }
            Ok(engine
                .stream_with_model_fallback(
                    &self.router,
                    request,
                    session,
                    message,
                    RequestLineage {
                        agent: self.stable_id.as_str(),
                        root_session_cache: &mut self.root_session_cache,
                    },
                )
                .await?)
        };
        let opened = tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(None),
            opened = open => opened,
        };
        match opened {
            Ok((stream, model)) => {
                self.serving_model = Some(model.clone());
                Ok(Some((stream, model)))
            }
            Err(_) if cancel.is_cancelled() => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn failed(&mut self, failure: &Failure) -> Result<bool, CoreError> {
        if self.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let deadline = hya_tool::deadline::current();
        let mut retry = self
            .engine
            .round_retry_waits
            .get(self.retries)
            .copied()
            .filter(|&wait| {
                !failure.saw_tool_call
                    && is_retryable_round_error(failure)
                    && !self.cancel.is_cancelled()
                    && deadline
                        .is_none_or(|d| d.remaining().saturating_sub(d.final_window()) > wait)
            });
        for entry in &mut self.entries {
            entry.failed(failure, &mut retry)?;
        }
        if self.cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        if let Some(wait) = retry {
            if failure.step_started {
                let session = self.session;
                let message = self.message;
                let discarded: Vec<_> = self
                    .engine
                    .store
                    .with_projection(session, |projection| {
                        projection
                            .session
                            .messages
                            .iter()
                            .find(|row| row.id == message)
                            .and_then(|row| row.parts.get(row.last_step_start?..))
                            .unwrap_or_default()
                            .iter()
                            .map(|part| Event::PartDeleted {
                                session,
                                message,
                                part: part.id(),
                            })
                            .collect()
                    })
                    .await?;
                if !discarded.is_empty() {
                    self.engine
                        .emit_batch_for_actor(self.actor_claim, session, discarded)
                        .await?;
                }
            }
            self.retries += 1;
            tracing::warn!(name: "round_retry", session = %self.session, retry = self.retries, error = %failure.error,
                "provider round failed; waiting {wait:?} before retry");
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => {}
                () = tokio::time::sleep(wait) => {}
            }
            if self.cancel.is_cancelled() {
                return Err(CoreError::Cancelled);
            }
            if deadline.is_none_or(|d| d.stage() < hya_tool::deadline::Stage::Final) {
                return Ok(true);
            }
            for entry in &mut self.entries {
                entry.failed(failure, &mut None)?;
            }
        }
        Ok(false)
    }

    async fn collected(&mut self, round: &Round) -> Result<(), CoreError> {
        if let Some(ran) = self.ran_known_tool.as_deref_mut()
            && !*ran
        {
            *ran = round.tool_calls.iter().any(|call| {
                [kernel::tools::REWRITE, kernel::tools::TODO].contains(&call.name.as_str())
                    || self.resources.resolve_tool(&call.name).is_some()
            });
        }
        for entry in &mut self.entries {
            entry.collected()?;
        }
        self.serving_model = None;
        self.reasoning_tokens = round.tokens.map(|tokens| tokens.reasoning);
        self.last_finish = Some(round.finish);
        self.output_tokens = round.tokens.map(|tokens| tokens.output);
        if !round.tool_calls.is_empty() {
            self.empty_turns = 0;
        }
        if let Some(tokens) = round.tokens {
            self.total_tokens
                .get_or_insert_with(TokenUsage::default)
                .add(tokens);
        }
        self.retries = 0;
        self.rounds += 1;
        Ok(())
    }

    async fn before_end(&mut self) -> Result<bool, CoreError> {
        let projection = self
            .engine
            .store
            .read_projection_shared(self.session)
            .await?;
        let parts = projection
            .session
            .messages
            .iter()
            .find(|row| row.id == self.message)
            .and_then(|row| row.parts.get(row.last_step_start.unwrap_or(0)..))
            .unwrap_or_default();
        if parts.iter().any(
            |part| matches!(part, PartProjection::Text { text, .. } if !text.trim().is_empty()),
        ) {
            self.empty_turns = 0;
            return Ok(false);
        }
        let has_reasoning = parts
            .iter()
            .any(|part| matches!(part, PartProjection::Reasoning { text, .. } if !text.trim().is_empty()))
            || self.reasoning_tokens.is_some_and(|n| n > 0);
        if (!has_reasoning && self.last_finish != Some(FinishReason::Length))
            || self.empty_turns >= kernel::EMPTY_TURN_NOTICES
            || self.cancel.is_cancelled()
            || hya_tool::deadline::current()
                .is_some_and(|d| d.stage() >= hya_tool::deadline::Stage::Final)
        {
            return Ok(false);
        }
        self.empty_turns += 1;
        let events = kernel::empty_turn_events(self.session, parts, self.output_tokens);
        self.engine
            .emit_batch_for_actor(self.actor_claim, self.session, events)
            .await?;
        Ok(true)
    }

    fn done(&self) -> bool {
        self.tool_state.done
    }

    fn deadline_delivery_tool(&self) -> Result<Option<&str>, CoreError> {
        turn_hooks::delivery_tool(
            self.engine,
            &self.binding,
            &self.live_agent,
            &self.resources,
            self.parent_session,
            &self.offered,
        )
    }

    fn served_route(
        &self,
        model: &ModelRef,
    ) -> Option<(hya_proto::context::ReasoningReplayPolicy, bool)> {
        Some((
            self.router.reasoning_replay_policy(model),
            self.router.sends_tool_images(model),
        ))
    }
}

fn is_retryable_round_error(failure: &Failure) -> bool {
    let CoreError::Provider(error) = &failure.error else {
        return false;
    };
    match error.retry_class() {
        ProviderRetryClass::Transient => true,
        ProviderRetryClass::InterruptedBody | ProviderRetryClass::TruncatedBody => {
            !failure.saw_text
        }
        ProviderRetryClass::Terminal => false,
    }
}
