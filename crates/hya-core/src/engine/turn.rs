use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{Event, FinishReason, MessageId, Role, SessionId, TokenUsage};
use hya_store::ActorClaim;
use hya_tool::{Action, AgentDef, Mode, PermissionPlane, Rule, ToolCtx, ToolError};
use tokio_util::sync::CancellationToken;

use super::tool_error::{tool_error_message_value, tool_error_value};
use super::{
    AgentSpec, FixedSystemAgent, SessionEngine, agent_roster, agent_with_bound_skills,
    agent_with_guidance_layer, authorize_tool_call, effective_agent_for_binding,
    fixed_system_agent, session_workdir, summarize_options_from_definition,
};
use crate::error::CoreError;
use crate::hooks::{
    ChatParamsInput, ChatParamsOutcome, ToolExecuteAfterInput, ToolExecuteAfterOutcome,
    ToolExecuteBeforeInput, ToolExecuteBeforeOutcome, ToolOutcomeNative,
};
use crate::runtime_registry::CompiledResourceView;
use crate::{AgentResourcePolicy, TurnBinding};

mod messages;

use messages::{projection_to_messages, request_from_messages};

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

impl SessionEngine {
    pub async fn run_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(session, agent, cancel, (&[], None), None, None)
            .await
    }

    pub(crate) async fn run_turn_for_actor(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        claim: &ActorClaim,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], None),
            Some(claim),
            None,
        )
        .await
    }

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
            None,
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
            None,
        )
        .await
    }

    pub(crate) async fn run_resolved_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        agents: Arc<[AgentDef]>,
        resources: AgentResourcePolicy,
        cancel: CancellationToken,
        guidance: Option<Arc<str>>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            cancel,
            (&[], guidance),
            None,
            Some((agents, resources)),
        )
        .await
    }

    pub(crate) async fn run_resolved_turn_for_actor(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        resolved: (Arc<[AgentDef]>, AgentResourcePolicy),
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
            Some(resolved),
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
        resolved: Option<(Arc<[AgentDef]>, AgentResourcePolicy)>,
    ) -> Result<FinishReason, CoreError> {
        let (external_dirs, guidance) = request_context;
        self.validate_actor_claim(actor_claim).await?;
        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let binding = self.runtime.bind_turn(&workdir)?;
        let guidance_text = guidance.as_deref();
        let (agent, agents, resources) = match resolved {
            Some((agents, policy)) => {
                let resources = binding.compile_agent_resources(&policy)?;
                // Resolved activation reuses caller-owned agent_base; optional
                // inherited guidance composed once, then skills.
                let agent = agent_with_guidance_layer(agent.clone(), guidance_text);
                (
                    agent_with_bound_skills(agent, resources.as_ref()),
                    agents,
                    resources,
                )
            }
            None => {
                let stable_id = projection
                    .session
                    .agent
                    .as_ref()
                    .unwrap_or(&agent.name)
                    .as_str();
                let (agent, resources) =
                    effective_agent_for_binding(agent, stable_id, &binding, guidance_text)?;
                let agents = agent_roster(&binding, stable_id)?;
                (agent, agents, resources)
            }
        };
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

        let outcome = self
            .run_turn_rounds(
                session,
                message,
                &agent,
                TurnExecution {
                    binding: &binding,
                    resources: &resources,
                    agents: &agents,
                    cancel: &cancel,
                    external_dirs,
                    actor_claim,
                    // Same Arc for nested spawn scope; no re-discovery.
                    guidance: guidance.clone(),
                },
            )
            .await;
        if outcome.is_err() {
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
        // A completed top-level (depth-0) turn ends the "run": release its per-run
        // subagent budget so long-lived root sessions do not leak budget entries and
        // the next top-level turn starts with a fresh budget.
        if self.governor.is_some()
            && let Ok((root, 0)) = self.session_lineage(session).await
        {
            self.finalize_root_spawn_admissions(root).await?;
        }
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
        // Depth in the subagent tree, derived from the parent chain. Only subagents
        // (depth > 0) are subject to the streaming-concurrency semaphore; the
        // interactive lead (depth 0) never waits behind background subagents.
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
            if crate::compaction::needs_compaction(&messages, &self.compaction) {
                // Exact-resolve fixed Compaction once before any compact provider
                // call (native or local). Missing definition fails closed here.
                // Reuse the turn's captured binding; never re-bind or open a second catalog.
                let definition = fixed_system_agent(binding, FixedSystemAgent::Compaction)?;
                let compaction_prompt = definition.prompt.as_deref();
                // Native compact resolves the active session provider/model route.
                let model = projection
                    .session
                    .model
                    .clone()
                    .unwrap_or_else(|| agent.model.clone());
                match self
                    .providers
                    .compact_if_supported(&model, &messages, compaction_prompt)
                    .await
                {
                    Ok(Some(window)) => {
                        let body = hya_provider::format_responses_compact_system(&window.items);
                        // Persist so subsequent rounds re-inject the compact window
                        // and drop pre-marker history via HYA_COMPACTED_CONTEXT.
                        let injected = match actor_claim {
                            Some(claim) => {
                                self.inject_system_message_for_actor(claim, session, body)
                                    .await
                            }
                            None => self.inject_system_message(session, body).await,
                        };
                        if injected.is_ok() {
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
                            if let Ok(compacted) = crate::compaction::compact_with(
                                messages.clone(),
                                &self.compaction,
                                summarizer.as_ref(),
                                options,
                            )
                            .await
                            {
                                messages = compacted;
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
            // Hold a global streaming permit ONLY around provider streaming, and only
            // for subagents. Acquired here and dropped before tool execution, so a
            // member blocked in the `task` tool (awaiting its children) holds no
            // permit — guaranteeing nested spawns can always make progress.
            let stream_permit = match (depth > 0, &self.governor) {
                (true, Some(gov)) => gov.acquire_stream().await,
                _ => None,
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
                if let Some(hooks) = &self.hooks {
                    let input = std::mem::take(&mut tc.input);
                    match hooks
                        .tool_execute_before(ToolExecuteBeforeInput {
                            session,
                            message,
                            call: tc.call,
                            tool: tc.name.clone(),
                            input,
                        })
                        .await
                    {
                        ToolExecuteBeforeOutcome::Continue { input } => tc.input = input,
                        ToolExecuteBeforeOutcome::Veto { reason } => {
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
                let input_for_after = self.hooks.as_ref().map(|_| tc.input.clone());
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
                                spawner: self.spawner.for_session_with_agents_and_guidance(
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
                self.validate_actor_claim(actor_claim).await?;
                let result = if let Some(hooks) = &self.hooks {
                    let was_permission_err = matches!(&result, Err(ToolError::Permission(_)));
                    let native = match &result {
                        Ok(output) => ToolOutcomeNative::Ok {
                            output: output.clone(),
                            time_ms,
                        },
                        Err(e) => ToolOutcomeNative::Err {
                            message: e.to_string(),
                        },
                    };
                    let ToolExecuteAfterOutcome::Continue { result: rewritten } = hooks
                        .tool_execute_after(ToolExecuteAfterInput {
                            session,
                            message,
                            call: tc.call,
                            tool: tc.name.clone(),
                            input: input_for_after.unwrap_or_default(),
                            result: native,
                        })
                        .await;
                    if was_permission_err {
                        result
                    } else {
                        match rewritten {
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
