use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{
    CompactionStrategy, Event, FinishReason, Message, MessageId, ModelRef, PartId, Role, SessionId,
    TokenUsage, ToolCallId,
};
use hya_provider::{CompletionRequest, EventStream, ProviderError};
use hya_store::ActorClaim;
use hya_tool::{Action, AgentDef, Mode, PermissionPlane, ResolvedTool, Rule, ToolCtx, ToolError};
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

use super::shell::BashArtifactGuard;
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
use crate::workflow::WorkflowTurnRoute;
use crate::{AgentResourcePolicy, TurnBinding};

mod messages;

use super::spill::ArtifactEvictionSink;
use crate::agent_catalog::AgentDefinition;
pub use messages::advertise_tool;
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
    /// Explicit model selected for this one request, if any.
    ///
    /// It outranks Session-tree temporary defaults but is never copied into
    /// child/resident contexts.
    explicit_model: Option<&'a ModelRef>,
    /// Whether this activation resolves Session/file defaults freshly.
    apply_default_overlays: bool,
    /// Request-local Workflow route, absent for ordinary Agent turns.
    workflow_route: Option<&'a WorkflowTurnRoute>,
}

/// Request-local context shared by one governed turn activation.
pub(crate) struct TurnRequestContext<'a> {
    cancel: CancellationToken,
    external_dirs: &'a [PathBuf],
    guidance: Option<Arc<str>>,
    actor_claim: Option<&'a ActorClaim>,
    explicit_model: Option<ModelRef>,
    workflow_route: Option<WorkflowTurnRoute>,
}

impl<'a> TurnRequestContext<'a> {
    /// Build one request-local turn context.
    pub(crate) fn new(
        cancel: CancellationToken,
        external_dirs: &'a [PathBuf],
        guidance: Option<Arc<str>>,
        actor_claim: Option<&'a ActorClaim>,
        workflow_route: Option<WorkflowTurnRoute>,
    ) -> Self {
        Self {
            cancel,
            external_dirs,
            guidance,
            actor_claim,
            explicit_model: None,
            workflow_route,
        }
    }
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
fn workflow_provider_failure_class(error: &ProviderError) -> hya_proto::WorkflowRouteFailureClass {
    use hya_proto::WorkflowRouteFailureClass as Class;
    match error {
        ProviderError::Transport(_) => Class::Transport,
        ProviderError::HttpStatus { status, .. } => {
            if *status == 429 {
                Class::RateLimited
            } else if (500..=599).contains(status) {
                Class::Server
            } else {
                Class::Http
            }
        }
        ProviderError::UnknownModel(_) => Class::UnknownModel,
        ProviderError::AuthExpired { .. } => Class::Auth,
        ProviderError::Incompatible(_) => Class::Incompatible,
        ProviderError::Decode(_) => Class::Decode,
        ProviderError::Json(_) | ProviderError::Http(_) => Class::Http,
    }
}

fn workflow_failure_class(
    error: &CoreError,
    cancel: &CancellationToken,
) -> hya_proto::WorkflowRouteFailureClass {
    match error {
        CoreError::Provider(error) => workflow_provider_failure_class(error),
        CoreError::Cancelled if cancel.is_cancelled() => {
            hya_proto::WorkflowRouteFailureClass::Cancelled
        }
        CoreError::Cancelled => hya_proto::WorkflowRouteFailureClass::Aborted,
        _ => hya_proto::WorkflowRouteFailureClass::Internal,
    }
}

impl SessionEngine {
    /// Open the completion stream for `request`, walking the configured
    /// cross-model fallback plane while no event stream exists yet.
    ///
    /// Each candidate re-enters [`ProviderRouter::stream`], so preflight and
    /// reasoning-stripping keep applying per route. A retryable pre-stream
    /// failure (`is_retryable_before_stream`) or an unrouted candidate
    /// (`UnknownModel`) advances to the next chain entry; any other error —
    /// and everything that happens after an [`EventStream`] was returned —
    /// stops selection immediately. STRICT NO-REPLAY: once a stream is
    /// returned this never switches models or replays, so mid-stream errors
    /// surface exactly once, unchanged.
    async fn stream_with_model_fallback(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        // Candidate order for this turn. An unconfigured plane yields exactly
        // one candidate — the preferred model — reproducing today's direct
        // router call; each configured chain starts with its own key.
        let candidates: &[ModelRef] = match self.model_fallback_chain(&request.model) {
            Some(chain) => chain,
            None => std::slice::from_ref(&request.model),
        };
        debug_assert_eq!(candidates.first(), Some(&request.model));
        for (index, candidate) in candidates.iter().enumerate() {
            let mut attempt = request.clone();
            attempt.model = candidate.clone();
            attempt.reasoning = messages::reasoning_for_model(&attempt.model, request.reasoning);
            match self
                .provider_router()
                .stream(attempt, session, message)
                .await
            {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    let advance = (error.is_retryable_before_stream()
                        || matches!(error, ProviderError::UnknownModel(_)))
                        && index + 1 < candidates.len();
                    if !advance {
                        return Err(error);
                    }
                    tracing::warn!(
                        from = %candidate,
                        to = %candidates[index + 1],
                        error = %error,
                        "pre-stream provider failure; advancing cross-model fallback chain"
                    );
                }
            }
        }
        // Unreachable: `advance` requires a successor candidate, so the final
        // iteration always returns `Ok` or `Err` above.
        unreachable!("cross-model fallback iteration must terminate")
    }
    /// Open one Workflow-assigned stream, starting at admission's candidate.
    ///
    /// The route remains request-local. Only pre-stream retryable failures can
    /// advance it, and a returned stream is never replayed on another model.
    async fn stream_with_workflow_route(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
        route: &WorkflowTurnRoute,
    ) -> Result<EventStream, ProviderError> {
        let candidates = &route.route().candidates;
        let mut pending_failure = None;
        for index in route.route().selected_index..candidates.len() {
            let candidate = &candidates[index];
            route.begin_attempt(index);
            let mut attempt = request.clone();
            attempt.model = candidate.model.clone();
            attempt.reasoning = Some(candidate.reasoning);
            match self
                .provider_router()
                .stream(attempt, session, message)
                .await
            {
                Ok(stream) => {
                    route.selected(index, pending_failure);
                    return Ok(stream);
                }
                Err(error) => {
                    let failure = workflow_provider_failure_class(&error);
                    pending_failure = Some(failure);
                    let advance = (error.is_retryable_before_stream()
                        || matches!(error, ProviderError::UnknownModel(_)))
                        && index + 1 < candidates.len();
                    route.record_failure(index, failure);
                    if !advance {
                        return Err(error);
                    }
                    tracing::warn!(
                        from = %candidate.model,
                        to = %candidates[index + 1].model,
                        error = %error,
                        "pre-stream Workflow route failure; advancing declared candidate"
                    );
                }
            }
        }
        Err(ProviderError::UnknownModel(
            "Workflow route has no candidate".to_string(),
        ))
    }

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
            TurnActivation::Root,
            TurnRequestContext::new(cancel, &[], None, None, None),
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
            TurnActivation::Root,
            TurnRequestContext::new(cancel, external_dirs, None, None, None),
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
        explicit_model: Option<ModelRef>,
    ) -> Result<FinishReason, CoreError> {
        let mut request = TurnRequestContext::new(cancel, external_dirs, guidance, None, None);
        request.explicit_model = explicit_model;
        self.run_turn_with_external_dirs_and_claim(session, agent, TurnActivation::Root, request)
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
            TurnActivation::Bound(binding),
            TurnRequestContext::new(cancel, &[], guidance, None, None),
        )
        .await
    }

    /// Run a bound turn with one request-local Workflow route.
    pub(crate) async fn run_bound_turn_with_workflow_route(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        binding: TurnBinding,
        request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            TurnActivation::Bound(binding),
            request,
        )
        .await
    }
    /// Run an actor-fenced bound turn with one request-local Workflow route.
    pub(crate) async fn run_bound_turn_for_actor_with_workflow_route(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        binding: TurnBinding,
        request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            TurnActivation::Bound(binding),
            request,
        )
        .await
    }
    /// Run a resolved turn with one request-local Workflow route.
    pub(crate) async fn run_resolved_turn_with_sidecar_tools_and_workflow_route(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        resolved: (
            TurnBinding,
            Arc<[AgentDef]>,
            AgentResourcePolicy,
            Arc<[ResolvedTool]>,
        ),
        request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        let (binding, agents, resources, sidecar_tools) = resolved;
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            TurnActivation::Resolved {
                binding,
                agents,
                resources,
                sidecar_tools,
            },
            request,
        )
        .await
    }
    /// Run an actor-fenced resolved turn with one request-local Workflow route.
    pub(crate) async fn run_resolved_turn_with_sidecar_tools_for_actor_and_workflow_route(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        resolved: (
            TurnBinding,
            Arc<[AgentDef]>,
            AgentResourcePolicy,
            Arc<[ResolvedTool]>,
        ),
        request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        let (binding, agents, resources, sidecar_tools) = resolved;
        self.run_turn_with_external_dirs_and_claim(
            session,
            agent,
            TurnActivation::Resolved {
                binding,
                agents,
                resources,
                sidecar_tools,
            },
            request,
        )
        .await
    }

    async fn run_turn_with_external_dirs_and_claim(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        activation: TurnActivation,
        request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        let TurnRequestContext {
            cancel,
            external_dirs,
            guidance,
            actor_claim,
            workflow_route,
            explicit_model,
        } = request;
        self.validate_actor_claim(actor_claim).await?;
        let projection = self.store.read_projection(session).await?;
        let workdir = session_workdir(agent, &projection);
        let (binding, resolved, root_sidecar_tools, mut sidecar_handle, apply_default_overlays) =
            match activation {
                TurnActivation::Root => {
                    let binding = self.bind_session_runtime(session, &workdir).await?;
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
                    (binding, None, sidecar_tools, sidecar_handle, true)
                }
                TurnActivation::Bound(binding) => (binding, None, Arc::from([]), None, false),
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
                    false,
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
                apply_default_overlays,
                workflow_route: workflow_route.as_ref(),
                explicit_model: explicit_model.as_ref(),
            };
            let outcome = match sidecar_loss {
                Some(loss_token) => {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => Ok(FinishReason::Cancelled),
                        _ = loss_token.cancelled() => Err(CoreError::Cancelled),
                        outcome = self.run_turn_rounds(session, message, &agent, execution) => outcome,
                    }
                }
                None => {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => Ok(FinishReason::Cancelled),
                        outcome = self.run_turn_rounds(session, message, &agent, execution) => outcome,
                    }
                }
            };
            if let Some(route) = workflow_route.as_ref() {
                let failure = match &outcome {
                    Ok(FinishReason::Cancelled) => Some(if cancel.is_cancelled() {
                        hya_proto::WorkflowRouteFailureClass::Cancelled
                    } else {
                        hya_proto::WorkflowRouteFailureClass::Aborted
                    }),
                    Err(error) => Some(workflow_failure_class(error, &cancel)),
                    Ok(_) => None,
                };
                if let Some(failure) = failure {
                    route.finalize(Some(failure)).await?;
                }
            }
            if matches!(
                &outcome,
                Ok(FinishReason::Cancelled) | Err(CoreError::Cancelled)
            ) && let Ok(projection) = self.store.read_projection(session).await
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
        // subagent budget so long-lived root sessions do not leak budget entries.
        // Descendant force-archive happens on terminal quiescence instead
        // (resident supervisor), so engine-driven synthesis turns on the root
        // session do not tear down a still-running team.
        if self.governor.is_some()
            && let Ok((root, 0)) = self.session_lineage(session).await
        {
            self.finalize_root_spawn_admissions(root).await?;
        }
        cleanup_result?;
        outcome
    }

    /// Summarize options for one folding attempt, anchored on the transcript as
    /// it stands right now.
    ///
    /// Rebuilt per rung rather than shared, because `previous_summary` reads the
    /// transcript and by the time the ladder escalates the transcript is no
    /// longer the one the earlier rung saw.
    fn folding_options(
        &self,
        definition: &AgentDefinition<'_>,
        binding: &TurnBinding,
        messages: &[Message],
    ) -> crate::compaction::SummarizeOptions {
        let options = summarize_options_from_definition(
            definition,
            &self.model_categories,
            binding.agent_model_preference(definition.stable_id),
            &|candidate| self.provider_router().resolve(candidate).is_some(),
        );
        // Anchor on whatever summary the session already carries, and give the
        // call room for the full section template.
        crate::compaction::SummarizeOptions {
            previous_summary: crate::compaction::previous_summary(messages),
            max_output_tokens: Some(self.compaction.summary_max_tokens),
            ..options
        }
    }

    /// Re-read the transcript after a rung replaced it, and re-count it.
    ///
    /// The count is estimated rather than measured on purpose. A provider's
    /// reported usage describes the prompt it was sent, and after compaction that
    /// prompt no longer exists; trusting the stale anchor would report the
    /// pre-compaction size and escalate the ladder straight past the rung that
    /// had just worked.
    async fn reload_after_compaction(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        model: &ModelRef,
    ) -> Result<(hya_proto::Projection, Vec<Message>, usize), CoreError> {
        let projection = self.store.read_projection(session).await?;
        let messages = projection_to_messages(agent, &projection, model);
        let tokens = self.token_accounting.estimate(&messages);
        Ok((projection, messages, tokens))
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
            apply_default_overlays,
            external_dirs,
            actor_claim,
            guidance,
            workflow_route,
            explicit_model,
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
        // Steer mailbox (fix for the unread-mail blindness): unread mail is
        // surfaced inside tool results, mid-turn, without breaking the model's
        // chain of thought. Snapshot the durable backlog once, then follow the
        // live bus so no per-tool projection replay is needed.
        let mut steer = self.steer_mailbox_snapshot(session).await;
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
            let stable_id = projection
                .session
                .agent
                .as_ref()
                .unwrap_or(&agent.name)
                .as_str();
            // One explicit request wins for this turn only. Fresh root
            // activations then use the captured Session override, user-file
            // model, and authored direct/category policy before falling back
            // to the persisted Session model. Bound/Resolved child work keeps
            // its already-resolved AgentSpec model and never reapplies root
            // defaults over an inline or Workflow choice.
            let session_model = apply_default_overlays
                .then(|| binding.session_agent_model(stable_id).cloned())
                .flatten();
            let configured_model = apply_default_overlays
                .then(|| binding.configured_agent_model(stable_id).cloned())
                .flatten();
            let authored_model = apply_default_overlays
                .then(|| {
                    binding
                        .agent_catalog()
                        .resolve(stable_id)
                        .and_then(|definition| {
                            crate::category::resolve_configured_agent_model(
                                &definition.model_policy,
                                &self.model_categories,
                                &|candidate| self.provider_router().resolve(candidate).is_some(),
                            )
                        })
                })
                .flatten();
            let model = explicit_model
                .cloned()
                .or(session_model)
                .or(configured_model)
                .or(authored_model)
                .or_else(|| projection.session.model.clone())
                .unwrap_or_else(|| agent.model.clone());
            let mut messages = projection_to_messages(agent, &projection, &model);
            // Active route for this turn. Its advertised context window scales
            // the compaction threshold, so resolve it before deciding.
            let capabilities = self.provider_router().capabilities(&model);
            let resolved_threshold = crate::compaction::resolved_threshold(
                &self.compaction,
                capabilities.as_ref().map(|c| c.max_context),
            );
            // Routes advertise usage support they do not always honour, so the
            // claim is an input to the accounting decision, not the decision.
            let usage_reporting = capabilities.as_ref().is_some_and(|c| c.usage_reporting);
            // One running token count for the whole reduction sequence. It starts
            // from the provider-measured value when that is believable, then tracks
            // request-local edits by delta — re-measuring after an edit would
            // return the stale pre-edit number and hide the saving.
            let initial_count = self
                .token_accounting
                .tokens_in_use(&messages, usage_reporting);
            let mut tokens = initial_count.tokens;
            let token_source = initial_count.source;
            let over_threshold = |tokens: usize, messages: &[_]| {
                messages.len() > self.compaction.keep_recent && tokens > resolved_threshold
            };

            // Reduction ladder: the five built-in mechanisms (oh-my-pi parity),
            // walked in the configured order — `compaction.method_order`. The
            // walk stops at the first rung that brings the transcript under the
            // threshold, so a turn never pays for a model call that spilling
            // alone would have avoided, and an unavailable or failed rung
            // (unsupported route, no summarizer wired) advances to the next.
            // Escalation matters in the other direction too: a native compact
            // that succeeded but left the transcript over threshold used to end
            // the sequence, sending the request out still over the window it
            // was trying to fit.
            let spill = ArtifactEvictionSink::new(&session_workdir(agent, &projection));
            // Resolved on first use, so a turn that an earlier rung rescued
            // never requires the fixed Compaction agent to exist.
            let mut compaction_agent: Option<AgentDefinition<'_>> = None;

            for rung in self.compaction.method_order {
                if !over_threshold(tokens, &messages) {
                    break;
                }
                // Snapshot what tripped the threshold before this rung edits the
                // transcript, so each record explains why that rung ran.
                let input_tokens_est = u64::try_from(tokens).unwrap_or(u64::MAX);
                let threshold = u64::try_from(resolved_threshold).unwrap_or(u64::MAX);

                match rung {
                    crate::compaction::CompactionRung::SpillToolOutputs => {
                        let estimate_before = self.token_accounting.estimate(&messages);
                        let evicted = crate::compaction::evict_stale_tool_outputs(
                            &mut messages,
                            self.compaction.keep_recent,
                            Some(&spill),
                        );
                        if evicted == 0 {
                            continue;
                        }
                        let saved = estimate_before
                            .saturating_sub(self.token_accounting.estimate(&messages));
                        tokens = tokens.saturating_sub(saved);
                        // Record the saving whether or not it sufficed. A partial
                        // reduction that still needed a summary is real work, and
                        // hiding it made the ledger disagree with the transcript.
                        self.emit_for_actor(
                            actor_claim,
                            session,
                            Event::ContextEvicted {
                                session,
                                evicted_parts: evicted,
                                tokens_before: input_tokens_est,
                                tokens_after: u64::try_from(tokens).unwrap_or(u64::MAX),
                                threshold,
                            },
                        )
                        .await?;
                    }
                    crate::compaction::CompactionRung::ProviderCompact => {
                        // Exact-resolve the fixed Compaction agent before any
                        // compact provider call. Missing definition fails closed.
                        // Reuse the turn's captured binding; never re-bind or open
                        // a second catalog.
                        let definition = match &compaction_agent {
                            Some(definition) => definition,
                            None => compaction_agent
                                .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                        };
                        let options = self.folding_options(definition, binding, &messages);
                        let compaction_model =
                            options.model.clone().unwrap_or_else(|| model.clone());
                        let Ok(Some(window)) = self
                            .provider_router()
                            .compact_if_supported(&compaction_model, &messages, definition.prompt)
                            .await
                        else {
                            continue;
                        };
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
                        let Ok(marker) = injected else {
                            continue;
                        };
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
                        (projection, messages, tokens) =
                            self.reload_after_compaction(session, agent, &model).await?;
                    }
                    crate::compaction::CompactionRung::SnapCompact => {
                        // Local and deterministic: no model call, no capability
                        // gate. Folds the same prefix a summary would into the
                        // dense archive, so the ladder's model-free rung works
                        // even where no summarizer is wired at all.
                        if messages.len() <= self.compaction.keep_recent {
                            continue;
                        }
                        let split = messages.len() - self.compaction.keep_recent;
                        let (Some(from), Some(to)) = (messages.first(), messages.get(split - 1))
                        else {
                            continue;
                        };
                        let archive = crate::compaction::snapcompact_archive(&messages[..split]);
                        let body = format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, archive);
                        let injected = match actor_claim {
                            Some(claim) => {
                                self.inject_system_message_for_actor(claim, session, body)
                                    .await
                            }
                            None => self.inject_system_message(session, body).await,
                        };
                        let Ok(marker) = injected else {
                            continue;
                        };
                        self.emit_for_actor(
                            actor_claim,
                            session,
                            Event::ContextCompacted {
                                session,
                                message: marker,
                                strategy: CompactionStrategy::SnapCompact,
                                from_message: from.id(),
                                to_message: to.id(),
                                folded_count: u32::try_from(split).unwrap_or(u32::MAX),
                                input_tokens_est,
                                threshold,
                            },
                        )
                        .await?;
                        (projection, messages, tokens) =
                            self.reload_after_compaction(session, agent, &model).await?;
                    }
                    crate::compaction::CompactionRung::Handoff => {
                        let Some(summarizer) = &self.summarizer else {
                            continue;
                        };
                        let definition = match &compaction_agent {
                            Some(definition) => definition,
                            None => compaction_agent
                                .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                        };
                        // The handoff call sees the whole transcript verbatim;
                        // the fold range still leaves the recent tail in place.
                        let Ok(Some(plan)) = crate::compaction::plan_handoff(
                            &messages,
                            &self.compaction,
                            summarizer.as_ref(),
                            self.folding_options(definition, binding, &messages),
                        )
                        .await
                        else {
                            continue;
                        };
                        let body =
                            format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, plan.summary);
                        let injected = match actor_claim {
                            Some(claim) => {
                                self.inject_system_message_for_actor(claim, session, body)
                                    .await
                            }
                            None => self.inject_system_message(session, body).await,
                        };
                        let Ok(marker) = injected else {
                            continue;
                        };
                        self.emit_for_actor(
                            actor_claim,
                            session,
                            Event::ContextCompacted {
                                session,
                                message: marker,
                                strategy: CompactionStrategy::Handoff,
                                from_message: plan.from_message,
                                to_message: plan.to_message,
                                folded_count: plan.folded_count,
                                input_tokens_est,
                                threshold,
                            },
                        )
                        .await?;
                        (projection, messages, tokens) =
                            self.reload_after_compaction(session, agent, &model).await?;
                    }
                    crate::compaction::CompactionRung::Summarize => {
                        let Some(summarizer) = &self.summarizer else {
                            continue;
                        };
                        let definition = match &compaction_agent {
                            Some(definition) => definition,
                            None => compaction_agent
                                .insert(fixed_system_agent(binding, FixedSystemAgent::Compaction)?),
                        };
                        // Provider failures stay soft; a missing definition has
                        // already failed closed by the time we are here.
                        let Ok(Some(plan)) = crate::compaction::fold_prefix(
                            &messages,
                            &self.compaction,
                            summarizer.as_ref(),
                            self.folding_options(definition, binding, &messages),
                        )
                        .await
                        else {
                            continue;
                        };
                        // Persist the local summary behind the same marker the
                        // native path uses. Without this the summary died with the
                        // request and every later round re-summarized the same
                        // history.
                        let body =
                            format!("{}\n{}", hya_provider::COMPACT_CONTEXT_MARKER, plan.summary);
                        let injected = match actor_claim {
                            Some(claim) => {
                                self.inject_system_message_for_actor(claim, session, body)
                                    .await
                            }
                            None => self.inject_system_message(session, body).await,
                        };
                        let Ok(marker) = injected else {
                            continue;
                        };
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
                        (projection, messages, tokens) =
                            self.reload_after_compaction(session, agent, &model).await?;
                    }
                }
            }
            // The wire surface of token accounting: one report per round,
            // recorded after the ladder so the figure is the occupancy this
            // request actually carries.
            self.emit_for_actor(
                actor_claim,
                session,
                Event::ContextStatus {
                    session,
                    tokens: u64::try_from(tokens).unwrap_or(u64::MAX),
                    source: token_source,
                    mode: self.token_accounting.mode(),
                    threshold: u64::try_from(resolved_threshold).unwrap_or(u64::MAX),
                },
            )
            .await?;
            let request = request_from_messages(agent, messages, resources, &model, depth);
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
            let stream = if let Some(route) = workflow_route {
                match self
                    .stream_with_workflow_route(request, session, message, route)
                    .await
                {
                    Ok(stream) => stream,
                    Err(error) => {
                        route
                            .finalize(Some(workflow_provider_failure_class(&error)))
                            .await?;
                        return Err(error.into());
                    }
                }
            } else {
                self.stream_with_model_fallback(request, session, message)
                    .await?
            };
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
            let stream_round = match self
                .collect_stream_round(session, message, stream, actor_claim)
                .await
            {
                Ok(stream_round) => {
                    if let Some(route) = workflow_route {
                        route.finalize(None).await?;
                    }
                    stream_round
                }
                Err(error) => {
                    if let Some(route) = workflow_route {
                        route
                            .finalize(Some(workflow_failure_class(&error, cancel)))
                            .await?;
                    }
                    return Err(error);
                }
            };
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
                let (result, result_policy) = match resources.resolve_tool(&tc.name) {
                    Some(resolved) => {
                        let result_policy = resolved.tool.result_policy();
                        let result = match authorize_tool_call(
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
                                    workflows: self
                                        .workflows
                                        .for_binding(binding)
                                        .for_session_with_agents(session, agents.clone()),
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
                                    lifecycle: self.lifecycle.for_session(session),
                                    session: Some(session),
                                    parent_session: projection.session.parent,
                                    todo: self.todo.clone(),
                                    skills: resources.skill_plane(),
                                    artifacts: self.artifacts.clone(),
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
                                self.execute_tool_maybe_backgrounded(
                                    Arc::clone(&resolved.tool),
                                    ctx,
                                    tc.input,
                                    &tc.name,
                                    ToolCallSite {
                                        session,
                                        message,
                                        part: tc.part,
                                        call: tc.call,
                                    },
                                )
                                .await
                            }
                            Err(error) => Err(error),
                        };
                        (result, result_policy)
                    }
                    None => (
                        Err(ToolError::Other(format!("unknown tool: {}", tc.name))),
                        hya_tool::ToolResultPolicy::Default,
                    ),
                };
                let mut artifact_guard = BashArtifactGuard::capture(
                    tc.name.as_str(),
                    result.as_ref().ok(),
                    binding.workdir(),
                );
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
                let (event, retains_artifact) = match result {
                    Ok(mut output) => {
                        artifact_guard.remove_unowned_path(&mut output);
                        if !artifact_guard.retained_by(&output) {
                            artifact_guard.discard()?;
                        }
                        // Cap every tool (builtin/MCP/plugin) after hooks and
                        // immediately before durable Event publication. Whatever
                        // the cap drops is preserved as an artifact first, so a
                        // truncated result stays reachable without re-running the
                        // tool that produced it.
                        let mut output = hya_tool::cap_tool_output_spilling(
                            output,
                            result_policy,
                            &self.artifacts.store(binding.workdir()),
                            tc.name.as_str(),
                        );
                        // Steer (ADR-0016 follow-up): unread team mail rides the
                        // tail of this tool result so a long turn stays aware
                        // of new messages without breaking the model's flow.
                        if let Some(notice) = steer.drain(self).await? {
                            append_steer_notice(&mut output, &notice);
                        }
                        let retains_artifact = artifact_guard.retained_by(&output);
                        (
                            Event::ToolResult {
                                session,
                                message,
                                part: tc.part,
                                call: tc.call,
                                output,
                                time_ms,
                            },
                            retains_artifact,
                        )
                    }
                    Err(e) => {
                        artifact_guard.discard()?;
                        (
                            Event::ToolError {
                                session,
                                message,
                                part: tc.part,
                                call: tc.call,
                                value: Some(tool_error_value(&e)),
                                message_text: e.to_string(),
                            },
                            false,
                        )
                    }
                };
                self.emit_for_actor(actor_claim, session, event).await?;
                if retains_artifact {
                    artifact_guard.disarm();
                } else {
                    artifact_guard.discard()?;
                }
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

/// Splice a steer notice into a tool result's model-facing text.
fn append_steer_notice(output: &mut serde_json::Value, notice: &str) {
    let Some(object) = output.as_object_mut() else {
        return;
    };
    if let Some(serde_json::Value::String(text)) = object.get("output") {
        let updated = format!("{text}{notice}");
        object.insert("output".to_string(), serde_json::Value::String(updated));
    } else {
        object.insert(
            "steer".to_string(),
            serde_json::Value::String(notice.trim_start().to_string()),
        );
    }
}

/// Execute one tool call, moving long-running `mcp__` calls to the background
/// when a budget is configured.
///
/// Past the budget the turn receives an early "backgrounded" result marker and
/// the abandoned future is detached into a watcher; when the call eventually
/// settles, [`SessionEngine::finish_background_mcp`] records the real outcome
/// and steers a reclaim prompt into the session.
///
/// Where a tool call lives in the transcript: used by the background watcher
/// to write the real outcome back onto the originating tool part.
struct ToolCallSite {
    session: SessionId,
    message: MessageId,
    part: PartId,
    call: ToolCallId,
}

/// Identity of one backgrounded MCP call, detached from the turn loop.
struct BackgroundCall {
    site: ToolCallSite,
    job: String,
    tool_name: String,
    started: std::time::Instant,
}

impl SessionEngine {
    async fn execute_tool_maybe_backgrounded(
        &self,
        tool: Arc<dyn hya_tool::Tool>,
        ctx: ToolCtx,
        input: serde_json::Value,
        tool_name: &str,
        site: ToolCallSite,
    ) -> Result<serde_json::Value, ToolError> {
        let budget = self
            .mcp_background_after
            .filter(|_| tool_name.starts_with("mcp__"));
        let Some(budget) = budget else {
            return tool.execute(&ctx, input).await;
        };
        let mut fut = Box::pin(async move { tool.execute(&ctx, input).await });
        let mut deadline = std::pin::pin!(tokio::time::sleep(budget));
        tokio::select! {
            result = &mut fut => result,
            _ = &mut deadline => {
                let job = format!(
                    "mcpbg-{}",
                    self.background_job_seq.fetch_add(1, Ordering::SeqCst),
                );
                let marker = backgrounded_marker(tool_name, &job, budget);
                let engine = self.clone();
                let finished = BackgroundCall {
                    site,
                    job,
                    tool_name: tool_name.to_string(),
                    started: std::time::Instant::now(),
                };
                tracing::info!(session=%finished.site.session, tool=%finished.tool_name, job=%finished.job, "mcp call backgrounded");
                tokio::spawn(async move {
                    let outcome = fut.await;
                    engine.finish_background_mcp(finished, outcome).await;
                });
                Ok(marker)
            }
        }
    }

    async fn finish_background_mcp(
        self,
        finished: BackgroundCall,
        outcome: Result<serde_json::Value, ToolError>,
    ) {
        let BackgroundCall {
            site,
            job,
            tool_name,
            started,
        } = finished;
        let ToolCallSite {
            session,
            message,
            part,
            call,
        } = site;
        let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let committed: Result<(), crate::error::CoreError> = match outcome {
            Ok(mut output) => {
                let text = output
                    .get("output")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| output.to_string(), str::to_owned);
                if let Some(object) = output.as_object_mut() {
                    let mut metadata = object
                        .get("metadata")
                        .and_then(serde_json::Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    metadata.insert("background_result".to_string(), serde_json::json!(job));
                    object.insert("metadata".to_string(), serde_json::Value::Object(metadata));
                }
                let prompt = format!(
                    "[background job {job} completed: {tool_name}]\n{text}\n\
                     Reclaim this result: incorporate it into your work and continue from it."
                );
                // Admission lands before the marker event so an observer that
                // drives a reclaim turn on the marker always sees the prompt.
                async {
                    self.admit_user_prompt(session, prompt).await?;
                    self.emit(
                        session,
                        Event::ToolResult {
                            session,
                            message,
                            part,
                            call,
                            output,
                            time_ms,
                        },
                    )
                    .await?;
                    Ok(())
                }
                .await
            }
            Err(ToolError::Cancelled) => {
                tracing::info!(session=%session, tool=%tool_name, %job, "backgrounded mcp call cancelled");
                return;
            }
            Err(error) => {
                let prompt = format!(
                    "[background job {job} failed: {tool_name}] {error}\n\
                     Continue without this result, or retry the tool."
                );
                let mut value = tool_error_value(&error);
                if let Some(object) = value.as_object_mut() {
                    object.insert("background_failed".to_string(), serde_json::json!(job));
                }
                async {
                    self.admit_user_prompt(session, prompt).await?;
                    self.emit(
                        session,
                        Event::ToolError {
                            session,
                            message,
                            part,
                            call,
                            value: Some(value),
                            message_text: error.to_string(),
                        },
                    )
                    .await?;
                    Ok(())
                }
                .await
            }
        };
        if let Err(error) = committed {
            tracing::warn!(session=%session, tool=%tool_name, %job, %error, "mcp background completion failed to commit");
        }
    }
}

/// Model-facing marker returned in place of a backgrounded call's result.
fn backgrounded_marker(
    tool_name: &str,
    job: &str,
    budget: std::time::Duration,
) -> serde_json::Value {
    serde_json::json!({
        "title": "",
        "output": format!(
            "[backgrounded] {tool_name} is still running and has moved to the background as \
             job {job} (foreground budget {}ms exceeded). Continue with other work now; when \
             the job completes, its result will arrive as a new user prompt - reclaim it there.",
            budget.as_millis()
        ),
        "metadata": { "backgrounded": true, "job": job, "tool": tool_name },
    })
}
