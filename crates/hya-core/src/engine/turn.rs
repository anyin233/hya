use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{
    CompactionStrategy, Event, FinishReason, Message, MessageId, Role, SessionId, TokenUsage,
    ToolCallId,
};
use hya_store::ActorClaim;
use hya_tool::{Action, AgentDef, Mode, PermissionPlane, ResolvedTool, Rule, ToolCtx, ToolError};
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{
    AgentSpec, FixedSystemAgent, SessionEngine, agent_roster, agent_with_bound_skills,
    agent_with_guidance_layer, authorize_tool_call, effective_agent_for_binding_with_sidecar_tools,
    fixed_system_agent, session_workdir, summarize_options_from_definition,
};
use crate::error::CoreError;
use crate::hooks::{
    ChatParamsInput, ChatParamsOutcome, HookDispatcher, ToolExecuteAfterInput,
    ToolExecuteAfterOutcome, ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
    activation_hook_for, scope_activation_hooks,
};
use crate::runtime_registry::CompiledResourceView;
use crate::sidecar::{SidecarEnvironment, SidecarHandle, SidecarStart};
use crate::{AgentResourcePolicy, TurnBinding};

mod messages;

use messages::{projection_to_messages, request_from_messages};

/// Range endpoints for a compaction that folded the entire input window.
///
/// Native provider compact is handed the whole window, unlike the local
/// summarizer which folds only the prefix before the retained recent messages.
/// `None` for an empty window, which cannot trip the threshold anyway.
fn whole_window_range(messages: &[Message]) -> Option<(MessageId, MessageId, u32)> {
    let first = messages.first()?;
    let last = messages.last()?;
    Some((
        first.id(),
        last.id(),
        u32::try_from(messages.len()).unwrap_or(u32::MAX),
    ))
}

struct TurnExecution<'a> {
    binding: &'a TurnBinding,
    resources: &'a CompiledResourceView,
    agents: &'a Arc<[AgentDef]>,
    cancel: &'a CancellationToken,
    external_dirs: &'a [PathBuf],
    actor_claim: Option<&'a ActorClaim>,
    /// Immutable triggering-turn guidance scoped into child SpawnerPlane.
    guidance: Option<Arc<str>>,
}

struct ToolHookContext<'a> {
    session: SessionId,
    message: MessageId,
    call: ToolCallId,
    tool: &'a str,
}

async fn apply_tool_execute_before_hooks(
    global: Option<&Arc<dyn HookDispatcher>>,
    activation: Option<&Arc<dyn HookDispatcher>>,
    context: &ToolHookContext<'_>,
    mut input: serde_json::Value,
) -> Result<serde_json::Value, String> {
    for hooks in [global, activation].into_iter().flatten() {
        match hooks
            .tool_execute_before(ToolExecuteBeforeInput {
                session: context.session,
                message: context.message,
                call: context.call,
                tool: context.tool.to_string(),
                input,
            })
            .await
        {
            ToolExecuteBeforeOutcome::Continue { input: next } => input = next,
            ToolExecuteBeforeOutcome::Veto { reason } => return Err(reason),
        }
    }
    Ok(input)
}

async fn apply_tool_execute_after_hooks(
    global: Option<&Arc<dyn HookDispatcher>>,
    activation: Option<&Arc<dyn HookDispatcher>>,
    context: &ToolHookContext<'_>,
    input: serde_json::Value,
    mut result: ToolOutcomeNative,
) -> ToolOutcomeNative {
    for hooks in [global, activation].into_iter().flatten() {
        let ToolExecuteAfterOutcome::Continue { result: next } = hooks
            .tool_execute_after(ToolExecuteAfterInput {
                session: context.session,
                message: context.message,
                call: context.call,
                tool: context.tool.to_string(),
                input: input.clone(),
                result,
            })
            .await;
        result = next;
    }
    result
}

enum TurnActivation {
    Root,
    Bound(TurnBinding),
    Resolved {
        binding: TurnBinding,
        agents: Arc<[AgentDef]>,
        resources: AgentResourcePolicy,
        sidecar_tools: Arc<[ResolvedTool]>,
    },
}

async fn start_root_sidecar(
    environment: Option<&Arc<dyn SidecarEnvironment>>,
    binding: &TurnBinding,
    stable_id: &str,
    cancel: &CancellationToken,
) -> Result<(Option<Box<dyn SidecarHandle>>, Arc<[ResolvedTool]>), CoreError> {
    let Some(environment) = environment else {
        return Ok((None, Arc::from([])));
    };
    let Some(factory) = environment.factory_for(binding, stable_id)? else {
        return Ok((None, Arc::from([])));
    };
    let mut handle = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(CoreError::Cancelled),
        result = factory.start(SidecarStart::transient()) => result?,
    };
    let ready = tokio::select! {
        result = handle.ready() => result,
        _ = cancel.cancelled() => Err(CoreError::Cancelled),
    };
    if let Err(error) = ready {
        let _ = handle.terminate().await;
        return Err(error);
    }
    if handle
        .loss_token()
        .is_some_and(|loss_token| loss_token.is_cancelled())
    {
        let _ = handle.terminate().await;
        return Err(CoreError::Cancelled);
    }
    let tools = handle.tool_bindings();
    Ok((Some(handle), tools))
}

async fn terminate_sidecar(handle: &mut Option<Box<dyn SidecarHandle>>) {
    if let Some(mut handle) = handle.take() {
        let _ = handle.terminate().await;
    }
}

async fn shutdown_sidecar(handle: &mut Option<Box<dyn SidecarHandle>>) -> Result<(), CoreError> {
    if let Some(mut handle) = handle.take() {
        handle.shutdown().await
    } else {
        Ok(())
    }
}

impl SessionEngine {
    /// Run one model/tool turn for `session` until stop or cancel.
    pub async fn run_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], None),
            None,
            TurnActivation::Root,
        )
        .await
    }

    /// Run a turn with temporary ExternalDirectory allow rules for `external_dirs`.
    pub async fn run_turn_with_external_dirs(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
        external_dirs: &[PathBuf],
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (external_dirs, None),
            None,
            TurnActivation::Root,
        )
        .await
    }

    /// Run a turn with optional external directories and request-scoped guidance.
    ///
    /// `guidance` is pre-rendered by the caller and composed once after Bundle
    /// agent_base resolution (and before skill prompt material). Absence is an
    /// empty layer. Existing [`Self::run_turn`] / [`Self::run_turn_with_external_dirs`]
    /// callers stay source-compatible with no guidance.
    pub async fn run_turn_with_external_dirs_and_guidance(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
        external_dirs: &[PathBuf],
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (external_dirs, guidance),
            None,
            TurnActivation::Root,
        )
        .await
    }

    pub(crate) async fn run_bound_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        binding: TurnBinding,
        cancel: CancellationToken,
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], guidance),
            None,
            TurnActivation::Bound(binding),
        )
        .await
    }

    pub(crate) async fn run_bound_turn_for_actor(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        binding: TurnBinding,
        claim: &ActorClaim,
        cancel: CancellationToken,
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], guidance),
            Some(claim),
            TurnActivation::Bound(binding),
        )
        .await
    }

    pub(crate) async fn run_resolved_turn_with_sidecar_tools(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        resolved: (
            TurnBinding,
            Arc<[AgentDef]>,
            AgentResourcePolicy,
            Arc<[ResolvedTool]>,
        ),
        cancel: CancellationToken,
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        let (binding, agents, resources, sidecar_tools) = resolved;
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], guidance),
            None,
            TurnActivation::Resolved {
                binding,
                agents,
                resources,
                sidecar_tools,
            },
        )
        .await
    }

    pub(crate) async fn run_resolved_turn_with_sidecar_tools_for_actor(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        resolved: (
            TurnBinding,
            Arc<[AgentDef]>,
            AgentResourcePolicy,
            Arc<[ResolvedTool]>,
        ),
        claim: &ActorClaim,
        cancel: CancellationToken,
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        let (binding, agents, resources, sidecar_tools) = resolved;
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], guidance),
            Some(claim),
            TurnActivation::Resolved {
                binding,
                agents,
                resources,
                sidecar_tools,
            },
        )
        .await
    }

    async fn run_turn_with_external_dirs_and_claim(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
        request_context: (&[PathBuf], Option<Arc<str>>),
        actor_claim: Option<&ActorClaim>,
        activation: TurnActivation,
    ) -> Result<FinishReason, CoreError> {
        let (external_dirs, guidance) = request_context;
        self.validate_actor_claim(actor_claim).await?;
        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let (binding, resolved, root_sidecar_tools, mut sidecar_handle) = match activation {
            TurnActivation::Root => {
                let binding = self.bind_root_runtime(&workdir).await?;
                let stable_id = projection
                    .session
                    .agent
                    .as_ref()
                    .unwrap_or(&agent.name)
                    .as_str()
                    .to_string();
                let (sidecar_handle, sidecar_tools) = start_root_sidecar(
                    self.sidecar_environment.as_ref(),
                    &binding,
                    &stable_id,
                    &cancel,
                )
                .await?;
                (binding, None, sidecar_tools, sidecar_handle)
            }
            TurnActivation::Bound(binding) => (binding, None, Arc::from([]), None),
            TurnActivation::Resolved {
                binding,
                agents,
                resources,
                sidecar_tools,
            } => (
                binding,
                Some((agents, resources, sidecar_tools)),
                Arc::from([]),
                None,
            ),
        };
        let sidecar_hooks = sidecar_handle
            .as_ref()
            .and_then(|handle| handle.hook_dispatcher());
        let sidecar_loss = sidecar_handle
            .as_ref()
            .and_then(|handle| handle.loss_token());
        let post_ack = async {
            let guidance_text = guidance.as_deref();
            let prepared: Result<_, CoreError> = match resolved {
                Some((agents, policy, sidecar_tools)) => {
                    binding
                        .compile_agent_resources_with_sidecar_tools(&policy, &sidecar_tools)
                        .map_err(CoreError::from)
                        .map(|resources| {
                            // Resolved activation reuses caller-owned agent_base; optional
                            // inherited guidance composed once, then skills.
                            let agent = agent_with_guidance_layer(agent.clone(), guidance_text);
                            (
                                agent_with_bound_skills(agent, resources.as_ref()),
                                agents,
                                resources,
                            )
                        })
                }
                None => {
                    let stable_id = projection
                        .session
                        .agent
                        .as_ref()
                        .unwrap_or(&agent.name)
                        .as_str();
                    effective_agent_for_binding_with_sidecar_tools(
                        agent,
                        stable_id,
                        &binding,
                        guidance_text,
                        &root_sidecar_tools,
                    )
                    .and_then(|(agent, resources)| {
                        agent_roster(&binding, stable_id).map(|agents| (agent, agents, resources))
                    })
                }
            };
            let (agent, agents, resources) = prepared?;
            let message = MessageId::new();
            self.emit_for_actor(
                actor_claim,
                session,
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                },
            )
            .await?;
            self.emit_for_actor(
                actor_claim,
                session,
                Event::TurnBindingRecorded {
                    session,
                    message,
                    generation: binding.generation(),
                },
            )
            .await?;

            let execution = TurnExecution {
                binding: &binding,
                resources: &resources,
                agents: &agents,
                cancel: &cancel,
                external_dirs,
                actor_claim,
                // Same Arc for nested spawn scope; no re-discovery.
                guidance: guidance.clone(),
            };
            let (outcome, sidecar_lost) = match sidecar_loss {
                Some(loss_token) => {
                    tokio::select! {
                        biased;
                        _ = loss_token.cancelled() => (Err(CoreError::Cancelled), true),
                        outcome = self.run_turn_rounds(session, message, &agent, execution) => (outcome, false),
                    }
                }
                None => (
                    self.run_turn_rounds(session, message, &agent, execution)
                        .await,
                    false,
                ),
            };
            if sidecar_lost
                && let Ok(projection) = self.store.read_projection(session).await
                && projection
                    .session
                    .messages
                    .iter()
                    .any(|entry| entry.id == message && entry.finish.is_none())
            {
                let _ = self
                    .emit_for_actor(
                        actor_claim,
                        session,
                        Event::MessageFinished {
                            session,
                            message,
                            role: Role::Assistant,
                            finish: FinishReason::Cancelled,
                            tokens: None,
                        },
                    )
                    .await;
            }
            if outcome.is_err() && !matches!(&outcome, Err(CoreError::Cancelled)) {
                // A provider/tool error after MessageStarted must still close the assistant
                // message, else UI clients (e.g. the hya TUI) wait forever for a finish event.
                let _ = self
                    .emit_for_actor(
                        actor_claim,
                        session,
                        Event::MessageFinished {
                            session,
                            message,
                            role: Role::Assistant,
                            finish: FinishReason::Error,
                            tokens: None,
                        },
                    )
                    .await;
            }
            outcome
        };
        let outcome = if let Some(hooks) = sidecar_hooks {
            scope_activation_hooks(session, hooks, post_ack).await
        } else {
            post_ack.await
        };
        let cleanup_result = if matches!(&outcome, Ok(FinishReason::Stop | FinishReason::Length)) {
            shutdown_sidecar(&mut sidecar_handle).await
        } else {
            terminate_sidecar(&mut sidecar_handle).await;
            Ok(())
        };
        // A completed top-level (depth-0) turn ends the "run": release its per-run
        // subagent budget so long-lived root sessions do not leak budget entries and
        // the next top-level turn starts with a fresh budget.
        if self.governor.is_some()
            && let Ok((root, 0)) = self.session_lineage(session).await
        {
            self.finalize_root_spawn_admissions(root).await?;
        }
        cleanup_result?;
        outcome
    }

    async fn run_turn_rounds(
        &self,
        session: SessionId,
        message: MessageId,
        agent: &AgentSpec,
        execution: TurnExecution<'_>,
    ) -> Result<FinishReason, CoreError> {
        let TurnExecution {
            binding,
            resources,
            agents,
            cancel,
            external_dirs,
            actor_claim,
            guidance,
        } = execution;
        let mut rounds: u32 = 0;
        let mut total_tokens = None;
        // Depth in the subagent tree, derived from the parent chain. Subagents use
        // the general live provider-stream class; the root uses the independent
        // reserved class so root progress never waits behind background work.
        let depth = match &self.governor {
            Some(_) => self
                .session_lineage(session)
                .await
                .map(|(_, d)| d)
                .unwrap_or(0),
            None => 0,
        };
        loop {
            self.validate_actor_claim(actor_claim).await?;
            if activation_hook_for(session).is_some_and(|hooks| !hooks.is_healthy()) {
                return Err(CoreError::Cancelled);
            }
            if cancel.is_cancelled() {
                self.emit_for_actor(
                    actor_claim,
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::Cancelled,
                        tokens: None,
                    },
                )
                .await?;
                return Ok(FinishReason::Cancelled);
            }

            let mut projection = self.store.read_projection(session).await?;
            let mut messages = projection_to_messages(agent, &projection);
            // Context protection: prefer provider `/responses/compact` when the
            // route supports it; otherwise fall back to the local model summarizer.
            // Active route for this turn. Its advertised context window scales the
            // compaction threshold, so resolve it before deciding.
            let model = projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| agent.model.clone());
            let resolved_threshold = crate::compaction::resolved_threshold(
                &self.compaction,
                self.providers.capabilities(&model).map(|c| c.max_context),
            );
            // One running token count for the whole reduction sequence. It starts
            // from the provider-measured value when available, then tracks
            // request-local edits by delta — re-measuring after an edit would
            // return the stale pre-edit number and hide the saving.
            let mut tokens = crate::compaction::tokens_in_use(&messages);
            let over_threshold = |tokens: usize, messages: &[_]| {
                messages.len() > self.compaction.keep_recent && tokens > resolved_threshold
            };

            // Cheapest reduction first: drop stale tool outputs. Only if that is
            // not enough do we pay a summarizer call and lose whole turns to prose.
            if over_threshold(tokens, &messages) {
                let estimate_before = crate::compaction::estimate_tokens(&messages);
                let evicted = crate::compaction::evict_stale_tool_outputs(
                    &mut messages,
                    self.compaction.keep_recent,
                );
                if evicted > 0 {
                    let saved = estimate_before
                        .saturating_sub(crate::compaction::estimate_tokens(&messages));
                    let before = tokens;
                    tokens = tokens.saturating_sub(saved);
                    if !over_threshold(tokens, &messages) {
                        self.emit_for_actor(
                            actor_claim,
                            session,
                            Event::ContextEvicted {
                                session,
                                evicted_parts: evicted,
                                tokens_before: u64::try_from(before).unwrap_or(u64::MAX),
                                tokens_after: u64::try_from(tokens).unwrap_or(u64::MAX),
                                threshold: u64::try_from(resolved_threshold).unwrap_or(u64::MAX),
                            },
                        )
                        .await?;
                    }
                }
            }
            if over_threshold(tokens, &messages) {
                // Snapshot what tripped the threshold before the transcript is
                // replaced, so the ContextCompacted record explains why it ran.
                let input_tokens_est = u64::try_from(tokens).unwrap_or(u64::MAX);
                let threshold = u64::try_from(resolved_threshold).unwrap_or(u64::MAX);
                // Exact-resolve fixed Compaction once before any compact provider
                // call (native or local). Missing definition fails closed here.
                // Reuse the turn's captured binding; never re-bind or open a second catalog.
                let definition = fixed_system_agent(binding, FixedSystemAgent::Compaction)?;
                let compaction_prompt = definition.prompt.as_deref();
                match self
                    .providers
                    .compact_if_supported(&model, &messages, compaction_prompt)
                    .await
                {
                    Ok(Some(window)) => {
                        let body = hya_provider::format_responses_compact_system(&window.items);
                        // Native compact folds the whole input window it was given.
                        let folded = whole_window_range(&messages);
                        // Persist so subsequent rounds re-inject the compact window
                        // and drop pre-marker history via HYA_COMPACTED_CONTEXT.
                        let injected = match actor_claim {
                            Some(claim) => {
                                self.inject_system_message_for_actor(claim, session, body)
                                    .await
                            }
                            None => self.inject_system_message(session, body).await,
                        };
                        if let Ok(marker) = injected {
                            if let Some((from_message, to_message, folded_count)) = folded {
                                self.emit_for_actor(
                                    actor_claim,
                                    session,
                                    Event::ContextCompacted {
                                        session,
                                        message: marker,
                                        strategy: CompactionStrategy::Native,
                                        from_message,
                                        to_message,
                                        folded_count,
                                        input_tokens_est,
                                        threshold,
                                    },
                                )
                                .await?;
                            }
                            projection = self.store.read_projection(session).await?;
                            messages = projection_to_messages(agent, &projection);
                        }
                    }
                    Ok(None) | Err(_) => {
                        if let Some(summarizer) = &self.summarizer {
                            // Local fallback reuses the same exact-resolved definition
                            // (Bundle model/reasoning overrides apply here only).
                            let options = summarize_options_from_definition(definition);
                            // Provider failures stay soft (prior behavior); missing
                            // definition already failed closed above.
                            if let Ok(Some(plan)) = crate::compaction::fold_prefix(
                                &messages,
                                &self.compaction,
                                summarizer.as_ref(),
                                options,
                            )
                            .await
                            {
                                // Persist the local summary behind the same marker the
                                // native path uses. Without this the summary died with
                                // the request and every later round re-summarized the
                                // same history.
                                let body = format!(
                                    "{}\n{}",
                                    hya_provider::COMPACT_CONTEXT_MARKER,
                                    plan.summary
                                );
                                let injected = match actor_claim {
                                    Some(claim) => {
                                        self.inject_system_message_for_actor(claim, session, body)
                                            .await
                                    }
                                    None => self.inject_system_message(session, body).await,
                                };
                                if let Ok(marker) = injected {
                                    self.emit_for_actor(
                                        actor_claim,
                                        session,
                                        Event::ContextCompacted {
                                            session,
                                            message: marker,
                                            strategy: CompactionStrategy::LocalSummarizer,
                                            from_message: plan.from_message,
                                            to_message: plan.to_message,
                                            folded_count: plan.folded_count,
                                            input_tokens_est,
                                            threshold,
                                        },
                                    )
                                    .await?;
                                    projection = self.store.read_projection(session).await?;
                                    messages = projection_to_messages(agent, &projection);
                                }
                            }
                        }
                    }
                }
            } else if let Some(summarizer) = &self.summarizer {
                // Under threshold, compact_with is a no-op; no fixed definition required.
                if let Ok(compacted) = crate::compaction::compact_with(
                    messages.clone(),
                    &self.compaction,
                    summarizer.as_ref(),
                    crate::compaction::SummarizeOptions::default(),
                )
                .await
                {
                    messages = compacted;
                }
            }
            let request = request_from_messages(agent, &projection, messages, resources);
            let request = if let Some(hooks) = &self.hooks {
                match hooks
                    .chat_params(ChatParamsInput {
                        session,
                        message,
                        request,
                    })
                    .await
                {
                    ChatParamsOutcome::Continue { request } => request,
                }
            } else {
                request
            };
            // Hold a live provider-stream permit ONLY around provider streaming.
            // It is dropped before tool execution, so a member blocked in the
            // `task` tool (awaiting its children) holds no permit. These classes
            // bound execution only; durable admission/order remains authoritative.
            let stream_permit = match (&self.governor, depth > 0) {
                (Some(gov), true) => gov.acquire_general_stream().await,
                (Some(gov), false) => gov.acquire_reserved_stream().await,
                (None, _) => None,
            };
            self.validate_actor_claim(actor_claim).await?;
            let stream = self.providers.stream(request, session, message).await?;
            let step = rounds;
            self.emit_for_actor(
                actor_claim,
                session,
                Event::StepStarted {
                    session,
                    message,
                    step,
                },
            )
            .await?;
            let stream_round = self
                .collect_stream_round(session, message, stream, actor_claim)
                .await?;
            add_tokens(&mut total_tokens, stream_round.tokens);
            self.emit_for_actor(
                actor_claim,
                session,
                Event::StepFinished {
                    session,
                    message,
                    step,
                    finish: stream_round.finish,
                },
            )
            .await?;
            // Release the streaming slot before running tools (which may spawn and
            // await child subagents that need permits of their own).
            drop(stream_permit);

            if stream_round.tool_calls.is_empty() {
                self.emit_for_actor(
                    actor_claim,
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: stream_round.finish,
                        tokens: total_tokens,
                    },
                )
                .await?;
                return Ok(stream_round.finish);
            }

            for mut tc in stream_round.tool_calls {
                self.validate_actor_claim(actor_claim).await?;
                let activation_hooks = activation_hook_for(session);
                let hook_context = ToolHookContext {
                    session,
                    message,
                    call: tc.call,
                    tool: &tc.name,
                };
                if self.hooks.is_some() || activation_hooks.is_some() {
                    let input = apply_tool_execute_before_hooks(
                        self.hooks.as_ref(),
                        activation_hooks.as_ref(),
                        &hook_context,
                        std::mem::take(&mut tc.input),
                    )
                    .await;
                    if activation_hooks
                        .as_ref()
                        .is_some_and(|hooks| !hooks.is_healthy())
                    {
                        return Err(CoreError::Cancelled);
                    }
                    match input {
                        Ok(input) => tc.input = input,
                        Err(reason) => {
                            let message_text = format!("blocked by plugin: {reason}");
                            self.emit_for_actor(
                                actor_claim,
                                session,
                                Event::ToolError {
                                    session,
                                    message,
                                    part: tc.part,
                                    call: tc.call,
                                    value: Some(tool_error_message_value("blocked", &message_text)),
                                    message_text,
                                },
                            )
                            .await?;
                            continue;
                        }
                    }
                }
                let input_for_after =
                    (self.hooks.is_some() || activation_hooks.is_some()).then(|| tc.input.clone());
                let started = std::time::Instant::now();
                let result = match resources.resolve_tool(&tc.name) {
                    Some(resolved) => match authorize_tool_call(
                        &resolved,
                        &tc.input,
                        permission_for_session(&self.permission, session, external_dirs),
                        message,
                        tc.call,
                    )
                    .await
                    {
                        Ok(permission) => {
                            let ctx = ToolCtx {
                                permission,
                                interaction: self.interaction.for_session(session),
                                spawner: self
                                    .spawner
                                    .for_binding(binding)
                                    .for_session_with_agents_and_guidance(
                                        session,
                                        agents.clone(),
                                        guidance.clone(),
                                    ),
                                operation: hya_tool::ToolOperation::from_tool_call(tc.call)
                                    .with_actor_claim(actor_claim.copied()),
                                mailbox: self
                                    .mailbox
                                    .for_session_with_actor(session, actor_claim.copied()),
                                session: Some(session),
                                parent_session: projection.session.parent,
                                todo: self.todo.clone(),
                                skills: resources.skill_plane(),
                                agents: agents.clone(),
                                websearch: self.websearch.clone(),
                                lsp: self.lsp.clone(),
                                formatter: self.formatter.clone(),
                                workdir: binding.workdir().to_path_buf(),
                                cancel: cancel.clone(),
                            };
                            // Permission and plugin hooks can await. Recheck at the
                            // actual dispatch boundary so takeover cannot turn a
                            // previously valid resident into an unfenced launch.
                            self.validate_actor_claim(actor_claim).await?;
                            resolved.tool.execute(&ctx, tc.input).await
                        }
                        Err(error) => Err(error),
                    },
                    None => Err(ToolError::Other(format!("unknown tool: {}", tc.name))),
                };
                let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                if actor_claim.is_some()
                    && matches!(&result, Err(ToolError::Cancelled))
                    && cancel.is_cancelled()
                {
                    return Err(CoreError::Cancelled);
                }
                self.validate_actor_claim(actor_claim).await?;
                let result = if self.hooks.is_some() || activation_hooks.is_some() {
                    let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
                    let mut native = match &result {
                        Ok(output) => ToolOutcomeNative::Ok {
                            output: output.clone(),
                            time_ms,
                        },
                        Err(e) => ToolOutcomeNative::Err {
                            message: e.to_string(),
                        },
                    };
                    native = apply_tool_execute_after_hooks(
                        self.hooks.as_ref(),
                        activation_hooks.as_ref(),
                        &hook_context,
                        input_for_after.unwrap_or_default(),
                        native,
                    )
                    .await;
                    if activation_hooks
                        .as_ref()
                        .is_some_and(|hooks| !hooks.is_healthy())
                    {
                        return Err(CoreError::Cancelled);
                    }
                    if was_permission_err {
                        result
                    } else {
                        match native {
                            ToolOutcomeNative::Ok { output, .. } => Ok(output),
                            ToolOutcomeNative::Err { message } => Err(ToolError::Other(message)),
                        }
                    }
                } else {
                    result
                };
                let event = match result {
                    Ok(output) => Event::ToolResult {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        // Cap every tool (builtin/MCP/plugin) so a single oversized
                        // result cannot blow the next model context window.
                        output: hya_tool::cap_tool_output(output),
                        time_ms,
                    },
                    Err(e) => Event::ToolError {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        value: Some(tool_error_value(&e)),
                        message_text: e.to_string(),
                    },
                };
                self.emit_for_actor(actor_claim, session, event).await?;
            }

            rounds += 1;
        }
    }
}

fn add_tokens(target: &mut Option<TokenUsage>, update: Option<TokenUsage>) {
    if let Some(update) = update {
        let current = target.get_or_insert_with(TokenUsage::default);
        current.input = current.input.saturating_add(update.input);
        current.output = current.output.saturating_add(update.output);
        current.reasoning = current.reasoning.saturating_add(update.reasoning);
        current.cache_read = current.cache_read.saturating_add(update.cache_read);
        current.cache_write = current.cache_write.saturating_add(update.cache_write);
    }
}

fn permission_for_session(
    permission: &PermissionPlane,
    session: SessionId,
    external_dirs: &[PathBuf],
) -> PermissionPlane {
    let permission = permission.for_session(session);
    let rules = external_dirs
        .iter()
        .map(|dir| {
            Rule::new(
                Action::ExternalDirectory,
                dir.join("*").to_string_lossy().replace('\\', "/"),
                Mode::Allow,
            )
        })
        .collect();
    permission.with_snapshot_rules(rules)
}
