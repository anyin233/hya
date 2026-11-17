use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Role, SessionId, TokenUsage,
};
use hya_provider::{CompletionRequest, EventStream, ProviderError};
use hya_store::ActorClaim;
use hya_tool::{Action, AgentDef, Mode, PermissionPlane, ResolvedTool, Rule};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::turn_end::unstarted_answer;
use super::turn_gate::{TurnLease, scope_held_turn};
use super::{
    AgentSpec, SessionEngine, agent_roster, agent_with_bound_skills, agent_with_guidance_layer,
    effective_agent_for_binding_with_sidecar_tools, session_workdir,
    summarize_options_from_definition,
};
use crate::error::CoreError;
use crate::hooks::{
    ChatParamsInput, ChatParamsOutcome, HookChain, HookDispatcher, ModelFailureClass,
    ModelFallbackInput, ModelFallbackOutcome, activation_hook_for, replace_activation_hooks,
    scope_activation_hooks, scope_optional_activation_hooks,
};
use crate::kernel::Rewriter;
use crate::runtime_registry::CompiledResourceView;
use crate::sidecar::{SidecarEnvironment, SidecarHandle, SidecarStart};
use crate::workflow::WorkflowTurnRoute;
use crate::{AgentResourcePolicy, TurnBinding};

mod ladder;
pub(super) mod messages;
mod tool_gate;

use crate::agent_catalog::AgentDefinition;
pub use messages::advertise_tool;
use messages::{
    AttachmentData, attachment_blobs, filtered_tool_schemas, projection_to_messages,
    prompt_digests, system_prompt,
};

/// Human-answer tools cannot be useful once their host receiver is gone.
pub(super) fn tool_available(name: &str, interaction_available: bool) -> bool {
    interaction_available || !["ask_user", "plan_exit"].contains(&name)
}

struct TurnExecution<'a> {
    binding: TurnBinding,
    resources: Arc<CompiledResourceView>,
    agents: Arc<[AgentDef]>,
    cancel: &'a CancellationToken,
    external_dirs: &'a [PathBuf],
    /// Workspace roots and grant scope resolved at this turn's start
    /// (ADR-0024, ADR-0026); every tool call of the turn carries the roots in
    /// `ToolCtx::roots` and remembers scoped grants under the grant scope.
    workspace: Arc<super::roots::SessionWorkspace>,
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
    /// Round-boundary rebind inputs, present for Root activations only.
    rebind: Option<TurnRebindInputs<'a>>,
}

/// What a Root activation retains so a round-boundary rebind can recompile.
struct TurnRebindInputs<'a> {
    /// The uncomposed base spec. Prompt heat re-materializes from this so the
    /// agent_base, guidance, and skill layers are appended once, not stacked
    /// per round.
    base_agent: &'a AgentSpec,
    /// Sidecar tools captured at activation; recompiled into every fresh
    /// resource view so a rebind never drops them.
    sidecar_tools: Arc<[ResolvedTool]>,
    sidecar_hooks: Option<Arc<dyn HookDispatcher>>,
}

/// Request-local context shared by one governed turn activation.
pub(crate) struct TurnRequestContext<'a> {
    cancel: CancellationToken,
    external_dirs: &'a [PathBuf],
    guidance: Option<Arc<str>>,
    actor_claim: Option<&'a ActorClaim>,
    explicit_model: Option<ModelRef>,
    workflow_route: Option<WorkflowTurnRoute>,
    /// Turn claim already taken by the caller (resident wakes claim under the
    /// team lock). `None` claims — queueing behind an active turn — on entry.
    lease: Option<TurnLease>,
    /// The prompt this turn's caller admitted for it. Only a turn that owns
    /// its prompt answers it when it ends before its assistant message starts.
    admitted_prompt: Option<MessageId>,
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
            lease: None,
            admitted_prompt: None,
        }
    }

    /// Run under a turn claim the caller already holds for this session.
    pub(crate) fn with_lease(mut self, lease: TurnLease) -> Self {
        self.lease = Some(lease);
        self
    }

    pub(crate) fn with_admitted_prompt(mut self, prompt: Option<MessageId>) -> Self {
        self.admitted_prompt = prompt;
        self
    }
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

/// Most provider attempts one model-selection round may make, counting the
/// configured chain and every `model.fallback` pick.
const MODEL_FALLBACK_MAX_ATTEMPTS: usize = 8;

/// Hook-facing identity of the turn asking for a completion.
struct RequestLineage<'a> {
    /// Stable id of the session's bound agent.
    agent: &'a str,
    /// Spawn-tree root, resolved on first use (see `request_root_session`).
    root_session_cache: &'a mut Option<SessionId>,
}

impl SessionEngine {
    /// Resolve the spawn-tree root that hooks report as `root_session`, once
    /// per activation (`cache`). A lineage read failure falls back to
    /// `session` itself: hook context is advisory and never fails the turn.
    async fn request_root_session(
        &self,
        session: SessionId,
        cache: &mut Option<SessionId>,
    ) -> SessionId {
        if let Some(root) = *cache {
            return root;
        }
        let root = self
            .session_lineage(session)
            .await
            .map_or(session, |(root, _)| root);
        *cache = Some(root);
        root
    }

    /// Open the completion stream for `request`, walking the configured
    /// cross-model fallback plane while no event stream exists yet, then the
    /// `model.fallback` hook.
    ///
    /// Each candidate re-enters [`ProviderRouter::stream`], so preflight and
    /// reasoning-stripping keep applying per route. A retryable pre-stream
    /// failure (`is_retryable_before_stream`) or an unrouted candidate
    /// (`UnknownModel`) advances to the next chain entry. When the chain
    /// cannot advance (any error class), the active hooks' `model.fallback`
    /// may name the next model; a model already tried this round is refused,
    /// and a round makes at most [`MODEL_FALLBACK_MAX_ATTEMPTS`] attempts.
    /// STRICT NO-REPLAY: once a stream is returned this never switches models
    /// or replays, so mid-stream errors surface exactly once, unchanged.
    ///
    /// Returns the stream with the model that actually serves it (the
    /// candidate whose attempt opened the stream), for usage attribution.
    async fn stream_with_model_fallback(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
        lineage: RequestLineage<'_>,
    ) -> Result<(EventStream, ModelRef), ProviderError> {
        // Candidate order for this turn. An unconfigured plane yields exactly
        // one candidate — the preferred model — reproducing today's direct
        // router call; each configured chain starts with its own key.
        let candidates: &[ModelRef] = match self.model_fallback_chain(&request.model) {
            Some(chain) => chain,
            None => std::slice::from_ref(&request.model),
        };
        debug_assert_eq!(candidates.first(), Some(&request.model));
        let mut tried: Vec<ModelRef> = Vec::new();
        let mut last_failure = None;
        for (index, candidate) in candidates.iter().enumerate() {
            tried.push(candidate.clone());
            match self
                .stream_model_candidate(&request, candidate, session, message)
                .await
            {
                Ok(stream) => return Ok((stream, candidate.clone())),
                Err(error) => {
                    let next = candidates.get(index + 1).filter(|_| {
                        error.is_retryable_before_stream()
                            || matches!(error, ProviderError::UnknownModel(_))
                    });
                    let Some(next) = next else {
                        last_failure = Some((candidate.clone(), error));
                        break;
                    };
                    tracing::warn!(
                        from = %candidate,
                        to = %next,
                        error = %error,
                        "pre-stream provider failure; advancing cross-model fallback chain"
                    );
                }
            }
        }
        let Some((mut failed, mut error)) = last_failure else {
            // Unreachable: every iteration returns `Ok`, advances to an
            // existing successor, or records the failure and stops.
            return Err(ProviderError::UnknownModel(request.model.to_string()));
        };
        let Some(hooks) = self.active_hook_dispatcher(session) else {
            return Err(error);
        };
        while tried.len() < MODEL_FALLBACK_MAX_ATTEMPTS {
            let root_session = self
                .request_root_session(session, lineage.root_session_cache)
                .await;
            let outcome = hooks
                .model_fallback(ModelFallbackInput {
                    session,
                    root_session,
                    agent: Some(AgentName::new(lineage.agent)),
                    message,
                    model: failed.clone(),
                    error_class: ModelFailureClass::of(&error),
                    error_message: error.to_string(),
                    attempt: u32::try_from(tried.len()).unwrap_or(u32::MAX),
                    tried: tried.clone(),
                })
                .await;
            let ModelFallbackOutcome::Retry { model } = outcome else {
                break;
            };
            if tried.contains(&model) {
                tracing::warn!(
                    model = %model,
                    "model.fallback chose a model already tried this round; giving up"
                );
                break;
            }
            tracing::warn!(
                from = %failed,
                to = %model,
                error = %error,
                "pre-stream provider failure; model.fallback hook chose the next model"
            );
            tried.push(model.clone());
            match self
                .stream_model_candidate(&request, &model, session, message)
                .await
            {
                Ok(stream) => return Ok((stream, model)),
                Err(next_error) => {
                    failed = model;
                    error = next_error;
                }
            }
        }
        Err(error)
    }

    fn check_request_context(&self, request: &CompletionRequest) -> Result<(), ProviderError> {
        let router = self.provider_router();
        let Some(capabilities) = router.capabilities(&request.model) else {
            return Ok(()); // The router reports unknown models through its normal path.
        };
        let tokens = self
            .token_accounting
            .estimate_request(request, router.reasoning_replay_policy(&request.model));
        let required = tokens.saturating_add(request.max_output_tokens.unwrap_or(0) as usize);
        if capabilities.max_context > 0 && required > capabilities.max_context as usize {
            return Err(ProviderError::Incompatible(format!(
                "final request for {} exceeds context window: estimated {} input tokens + {} output tokens > {} tokens after context reduction",
                request.model,
                tokens,
                request.max_output_tokens.unwrap_or(0),
                capabilities.max_context
            )));
        }
        Ok(())
    }

    /// One router attempt of `request` on `model`, with that model's own
    /// reasoning variant.
    async fn stream_model_candidate(
        &self,
        request: &CompletionRequest,
        model: &ModelRef,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let mut attempt = request.clone();
        attempt.model = model.clone();
        attempt.reasoning = messages::reasoning_for_model(&attempt.model, request.reasoning);
        self.check_request_context(&attempt)?;
        self.provider_router()
            .stream(attempt, session, message)
            .await
    }
    /// Open one Workflow-assigned stream, starting at admission's candidate.
    ///
    /// The route remains request-local. Only pre-stream retryable failures can
    /// advance it, and a returned stream is never replayed on another model.
    /// Returns the stream with the selected candidate's model.
    async fn stream_with_workflow_route(
        &self,
        request: CompletionRequest,
        session: SessionId,
        message: MessageId,
        route: &WorkflowTurnRoute,
    ) -> Result<(EventStream, ModelRef), ProviderError> {
        let candidates = &route.route().candidates;
        let mut pending_failure = None;
        for index in route.route().selected_index..candidates.len() {
            let candidate = &candidates[index];
            route.begin_attempt(index);
            let mut attempt = request.clone();
            attempt.model = candidate.model.clone();
            attempt.reasoning = Some(candidate.reasoning);
            self.check_request_context(&attempt)?;
            match self
                .provider_router()
                .stream(attempt, session, message)
                .await
            {
                Ok(stream) => {
                    route.selected(index, pending_failure);
                    return Ok((stream, candidate.model.clone()));
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
        self.ensure_external_turn_allowed(session).await?;
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
        self.ensure_external_turn_allowed(session).await?;
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
    /// empty layer. `admitted_prompt` is the prompt the caller admitted for
    /// this turn: the turn then owns it and answers it with an assistant
    /// start/finish pair however it ends, even refused or stopped before its
    /// message starts. Without it a turn that never started answers nothing.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_turn_with_external_dirs_and_guidance(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
        external_dirs: &[PathBuf],
        guidance: Option<Arc<str>>,
        explicit_model: Option<ModelRef>,
        admitted_prompt: Option<MessageId>,
    ) -> Result<FinishReason, CoreError> {
        self.ensure_external_turn_allowed(session).await?;
        let mut request = TurnRequestContext::new(cancel, external_dirs, guidance, None, None);
        request.explicit_model = explicit_model;
        request.admitted_prompt = admitted_prompt;
        self.run_turn_with_external_dirs_and_claim(session, agent, TurnActivation::Root, request)
            .await
    }

    /// Run an admitted external turn under a lease reserved before admission.
    /// Restore startup uses this to acknowledge admission only after excluding
    /// recovered main work. The lease identifies the session and lasts through
    /// the turn's own message closure.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_admitted_turn(
        &self,
        lease: TurnLease,
        agent: &AgentSpec,
        cancel: CancellationToken,
        external_dirs: &[PathBuf],
        guidance: Option<Arc<str>>,
        explicit_model: Option<ModelRef>,
        admitted_prompt: MessageId,
    ) -> Result<FinishReason, CoreError> {
        let session = lease.session();
        self.ensure_external_turn_allowed(session).await?;
        let mut request = TurnRequestContext::new(cancel, external_dirs, guidance, None, None)
            .with_admitted_prompt(Some(admitted_prompt))
            .with_lease(lease);
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

    /// The single turn choke point: claim the session's one active turn
    /// (single-active-turn invariant), then run it. A caller-held lease is
    /// used as-is; otherwise the claim queues behind an active turn and a
    /// cancel (or drain) while queued ends the request as `Cancelled`,
    /// answering the prompt it admitted, if any, without executing the turn.
    async fn run_turn_with_external_dirs_and_claim(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        activation: TurnActivation,
        mut request: TurnRequestContext<'_>,
    ) -> Result<FinishReason, CoreError> {
        let message = MessageId::new();
        let lease = match request.lease.take() {
            Some(lease) if lease.session() == session => lease,
            Some(_) => {
                return Err(CoreError::Invalid(
                    "turn lease belongs to another session".to_string(),
                ));
            }
            None => match self.begin_turn(session, &request.cancel).await? {
                Some(lease) => lease,
                None => {
                    if request.admitted_prompt.is_some() {
                        let cause = if request.cancel.is_cancelled() {
                            Some(hya_proto::FinishCause::UserCancel)
                        } else {
                            self.draining()
                        };
                        self.emit_batch_for_actor(
                            request.actor_claim,
                            session,
                            unstarted_answer(
                                session,
                                message,
                                FinishReason::Cancelled,
                                cause,
                                None,
                            ),
                        )
                        .await?;
                    }
                    return Ok(FinishReason::Cancelled);
                }
            },
        };
        lease.set_message(message, request.admitted_prompt.is_some());
        // The turn's effective cancel: the caller's token, or the engine
        // (user abort via `cancel_turn`, process drain).
        let caller_cancel = request.cancel.clone();
        request.cancel = lease.bind_cancel(&caller_cancel);
        let actor_claim = request.actor_claim;
        let cancel = request.cancel.clone();
        let workflow_route = request.workflow_route.clone();
        let observer = self.turn_gate.observer();
        if let Some(observer) = observer.as_ref() {
            observer.turn_started(session);
        }
        let mut outcome = scope_held_turn(
            session,
            self.run_claimed_turn(session, message, agent, activation, request),
        )
        .await;
        let cause = self.turn_gate.cancel_cause(session).or_else(|| {
            caller_cancel
                .is_cancelled()
                .then_some(hya_proto::FinishCause::UserCancel)
        });
        if matches!(outcome, Err(CoreError::Cancelled))
            && cause == Some(hya_proto::FinishCause::Shutdown)
        {
            outcome = Ok(FinishReason::Cancelled);
        }
        match &outcome {
            Ok(FinishReason::Cancelled) | Err(CoreError::Cancelled) => {
                self.close_turn_message(
                    actor_claim,
                    session,
                    message,
                    FinishReason::Cancelled,
                    cause,
                )
                .await?;
            }
            Err(error) => {
                self.fail_turn_message(actor_claim, session, message, error)
                    .await?
            }
            Ok(_) => {}
        }
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
        if let Err(error) = &outcome
            && !matches!(error, CoreError::Cancelled)
        {
            // A lead that failed (provider/runtime error, never a cancel)
            // stays live and resumable; its team is told to wrap up. The
            // observer hears it before the lease release can deliver any
            // deferred wake (quiescence synthesis) to the failed lead.
            if let Some(observer) = observer.as_ref() {
                observer.turn_failed(session);
            }
            self.broadcast_leader_failed(session, error).await;
        }
        drop(lease);
        outcome
    }

    /// Continue a stored session whose recorded agent left the catalog (for
    /// example a renamed built-in) as the general-purpose task agent.
    ///
    /// Records the switch as `AgentSwitched` so the projection and every client
    /// show the agent that actually runs. Returns whether it switched; a
    /// catalog without the task agent keeps the recorded id, and the turn then
    /// fails with `AgentDefinitionMissing` naming it.
    pub(crate) async fn fall_back_from_missing_agent(
        &self,
        session: SessionId,
        recorded: Option<&AgentName>,
        binding: &TurnBinding,
    ) -> Result<bool, CoreError> {
        let Some(recorded) = recorded else {
            return Ok(false);
        };
        if binding.resolve_agent(recorded.as_str()).is_some()
            || binding.resolve_agent(crate::TASK_AGENT_ID).is_none()
        {
            return Ok(false);
        }
        tracing::warn!(
            %session,
            recorded_agent = %recorded,
            fallback = crate::TASK_AGENT_ID,
            "stored session agent is not in the catalog; continuing as the task agent"
        );
        self.emit(
            session,
            Event::AgentSwitched {
                session,
                agent: AgentName::new(crate::TASK_AGENT_ID),
            },
        )
        .await?;
        Ok(true)
    }

    async fn run_claimed_turn(
        &self,
        session: SessionId,
        message: MessageId,
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
            lease: _,
            admitted_prompt: _,
        } = request;
        self.validate_actor_claim(actor_claim).await?;
        let mut projection = self.store.read_projection_shared(session).await?;
        let workdir = session_workdir(agent, &projection);
        // Uncomposed base spec, captured before the prepared agent shadows it
        // below; round-boundary rebinds re-materialize from this.
        let base_agent = agent;
        let (binding, resolved, root_sidecar_tools, mut sidecar_handle, apply_default_overlays) =
            match activation {
                TurnActivation::Root => {
                    let binding = tokio::select! {
                        biased;
                        () = cancel.cancelled() => return Ok(FinishReason::Cancelled),
                        binding = self.bind_session_runtime(session, &workdir) => binding?,
                    };
                    if self
                        .fall_back_from_missing_agent(
                            session,
                            projection.session.agent.as_ref(),
                            &binding,
                        )
                        .await?
                    {
                        projection = self.store.read_projection_shared(session).await?;
                    }
                    let stable_id = projection
                        .session
                        .agent
                        .as_ref()
                        .unwrap_or(&agent.name)
                        .as_str();
                    let (sidecar_handle, sidecar_tools) = start_root_sidecar(
                        self.sidecar_environment.as_ref(),
                        &binding,
                        stable_id,
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
        // Read fresh per turn so a Project edit applies to the next turn.
        let workspace = Arc::new(
            self.session_workspace(session, &projection, binding.workdir())
                .await,
        );
        let sidecar_hooks = sidecar_handle
            .as_ref()
            .and_then(|handle| handle.hook_dispatcher())
            .map(crate::bundle_hooks::restricted_sidecar_hooks);
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&agent.name)
            .as_str();
        self.capture_session_bundle_hooks(session, &binding, stable_id)
            .await;
        let mut bound_hooks = binding.bundle_hooks_for_agent(stable_id);
        if let Some(sidecar_hooks) = sidecar_hooks.clone() {
            bound_hooks.push(sidecar_hooks);
        }
        // Bound/Resolved activations (subagents, resident members) run inside
        // a scope their caller opened for this session with the member's
        // activation-sidecar hooks. The turn's own scope replaces that one,
        // so carry the inherited hooks along instead of silently dropping
        // them; a Root activation owns its sidecar above.
        if !apply_default_overlays
            && !bound_hooks.is_empty()
            && let Some(inherited) = activation_hook_for(session)
        {
            bound_hooks.push(inherited);
        }
        let activation_hooks = (!bound_hooks.is_empty())
            .then(|| Arc::new(HookChain::new(bound_hooks)) as Arc<dyn HookDispatcher>);
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
            // `list_agents` shows the user's runtime/configured Agent efforts.
            let agents = self.annotate_agent_efforts(agents, Some(&binding)).await?;
            let stable_id = projection
                .session
                .agent
                .as_ref()
                .unwrap_or(&agent.name)
                .clone();
            let requested_model = self.requested_turn_model(
                &binding,
                stable_id.as_str(),
                apply_default_overlays,
                explicit_model.as_ref(),
                &projection,
                &agent,
            );
            drop(projection);
            let message_guard = self.turn_gate.message_guard(session).await;
            if cancel.is_cancelled() {
                return Ok(FinishReason::Cancelled);
            }
            self.emit_for_actor(
                actor_claim,
                session,
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: Some(stable_id),
                    model: Some(requested_model),
                },
            )
            .await?;
            drop(message_guard);

            let execution = TurnExecution {
                binding,
                resources,
                agents,
                cancel: &cancel,
                external_dirs,
                workspace: Arc::clone(&workspace),
                actor_claim,
                // Same Arc for nested spawn scope; no re-discovery.
                guidance: guidance.clone(),
                apply_default_overlays,
                workflow_route: workflow_route.as_ref(),
                explicit_model: explicit_model.as_ref(),
                rebind: apply_default_overlays.then_some(TurnRebindInputs {
                    base_agent,
                    sidecar_tools: Arc::clone(&root_sidecar_tools),
                    sidecar_hooks,
                }),
            };
            let mut rounds = Box::pin(self.run_turn_rounds(session, message, &agent, execution));
            if let Some(loss_token) = sidecar_loss {
                tokio::select! {
                    biased;
                    () = loss_token.cancelled() => {
                        cancel.cancel();
                        rounds.await?;
                        Err(CoreError::Cancelled)
                    }
                    outcome = &mut rounds => outcome,
                }
            } else {
                rounds.await
            }
        };
        let outcome = if apply_default_overlays {
            scope_optional_activation_hooks(session, activation_hooks, post_ack).await
        } else if let Some(hooks) = activation_hooks {
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

    /// Record the turn's runtime generation and prompt digests.
    async fn emit_turn_binding(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        message: MessageId,
        binding: &TurnBinding,
        (system_sha256, tools_sha256): &(String, String),
    ) -> Result<(), CoreError> {
        self.emit_for_actor(
            actor_claim,
            session,
            Event::TurnBindingRecorded {
                session,
                message,
                generation: binding.generation(),
                system_sha256: system_sha256.clone(),
                tools_sha256: tools_sha256.clone(),
            },
        )
        .await
    }

    /// Re-read the transcript after a rung replaced it, and re-count it.
    ///
    /// Outputs spilled earlier stay spilled: their placeholders are re-applied
    /// before counting, as at the top of every round.
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
        sticky: &crate::compaction::StickyEvictions,
        reasoning_policy: hya_provider::ReasoningReplayPolicy,
    ) -> Result<(Vec<Message>, usize), CoreError> {
        let projection = self.store.read_projection_shared(session).await?;
        let attachments = self.attachment_data(session, &projection).await?;
        let mut messages = projection_to_messages(agent, &projection, model, &attachments);
        crate::compaction::apply_sticky_evictions(&mut messages, sticky);
        let tokens = self
            .token_accounting
            .estimate_with_reasoning_policy(&messages, reasoning_policy);
        Ok((messages, tokens))
    }

    /// Load the prompt images the next request sends (see
    /// [`crate::attachments`]) from the session blob table as `data:` URLs.
    async fn attachment_data(
        &self,
        session: SessionId,
        projection: &hya_proto::Projection,
    ) -> Result<AttachmentData, CoreError> {
        let mut data = AttachmentData::new();
        for (blob, mime) in attachment_blobs(projection) {
            if data.contains_key(&blob) {
                continue;
            }
            if let Some(bytes) = self.store.file_blob(session, &blob).await? {
                data.insert(blob, crate::attachments::data_url(&mime, &bytes));
            }
        }
        Ok(data)
    }

    /// Model a Root turn of `session` would request first, before
    /// `chat.params`, fallback, or routing (the same resolution the turn
    /// uses: session and configured agent model preferences, the agent's
    /// authored model, then the session binding).
    ///
    /// # Errors
    /// Projection read or runtime binding failures.
    pub async fn root_turn_model(
        &self,
        session: SessionId,
        agent: &AgentSpec,
    ) -> Result<ModelRef, CoreError> {
        let projection = self.store.read_projection_shared(session).await?;
        let workdir = session_workdir(agent, &projection);
        let binding = self.bind_session_runtime(session, &workdir).await?;
        let stable_id = projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&agent.name)
            .as_str()
            .to_string();
        Ok(self.requested_turn_model(&binding, &stable_id, true, None, &projection, agent))
    }

    /// Model a turn round requests before `chat.params`, fallback, or routing.
    ///
    /// One explicit request wins for this turn only. Fresh root activations
    /// then use the captured Session override, user-file model, and authored
    /// direct/category policy before falling back to the persisted Session
    /// model. Bound/Resolved child work keeps its already-resolved AgentSpec
    /// model and never reapplies root defaults over an inline or Workflow
    /// choice. `MessageStarted` records the first round's answer so the
    /// transcript can attribute the message before any round is served.
    fn requested_turn_model(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
        apply_default_overlays: bool,
        explicit_model: Option<&ModelRef>,
        projection: &hya_proto::Projection,
        agent: &AgentSpec,
    ) -> ModelRef {
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
        explicit_model
            .cloned()
            .or(session_model)
            .or(configured_model)
            .or(authored_model)
            .or_else(|| projection.session.model.clone())
            .unwrap_or_else(|| agent.model.clone())
    }

    /// Drive the streaming rounds of one turn activation.
    ///
    /// Round-boundary dynamic rebinding (Root turns only): at the top of every
    /// round after the first, a Root activation re-runs
    /// `bind_session_runtime`. When the fresh binding carries a different
    /// runtime generation, the round swaps in the fresh binding, a freshly
    /// compiled resource view (retaining the activation's sidecar tools), a
    /// refreshed agent roster, and a re-materialized agent prompt — so tools,
    /// hooks, skills, and prompts published mid-turn take effect at the next
    /// model call without rebuilding the transcript context. Any rebind or
    /// recompilation failure is logged and ignored: the turn keeps the current
    /// generation and never dies from a rebind failure (fail-open). Bound and
    /// Resolved activations (subagents, Workflow members) pin their
    /// activation-time binding for the whole turn and never rebind.
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
            workspace,
            actor_claim,
            guidance,
            workflow_route,
            explicit_model,
            rebind,
        } = execution;
        // Owned rebind state. The loop reads it through shared locals so an
        // activation-time generation and a round-swapped generation behave
        // identically; a successful rebind republishes the owned state and
        // re-points the shared locals at it.
        let mut live_binding = binding;
        let mut live_resources = resources;
        let mut live_agents = agents;
        // Active agent spec for the current round. Root rounds swap it at a
        // round boundary (prompt heat); Bound/Resolved rounds keep the
        // activation-time composition.
        let mut live_agent = agent.clone();
        let mut binding = &live_binding;
        let mut resources = &*live_resources;
        let mut agents = &live_agents;
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
        // Digests of the system prompt and tool list, recorded with the
        // binding at turn start and again whenever a round's differ.
        let tools = self.offered_tools(resources, depth);
        let mut recorded_digests =
            prompt_digests(&system_prompt(&live_agent, &tools, depth), &tools)?;
        self.emit_turn_binding(actor_claim, session, message, binding, &recorded_digests)
            .await?;
        // Steer mailbox (fix for the unread-mail blindness): unread mail is
        // surfaced inside tool results, mid-turn, without breaking the model's
        // chain of thought. Snapshot the durable backlog once, then follow the
        // live bus so no per-tool projection replay is needed.
        let steer_policy = Some(
            crate::ChannelPolicy::from_binding(binding)?.snapshot_for(live_agent.name.as_str()),
        );
        let mut steer = self
            .steer_mailbox_snapshot_with_policy(session, steer_policy)
            .await;
        // Root of this session's spawn tree, resolved on first hook use: the
        // parent chain is immutable, so one walk serves every round.
        let mut root_session_cache = None;
        // Set when a `report` of this turn is accepted: the turn ends right
        // after that tool round, and a second `report` is rejected.
        let report_latch = hya_tool::ReportLatch::new();
        // Engine handle for nested tool calls, cloned once per turn on first use.
        let mut nested_engine: Option<Arc<SessionEngine>> = None;
        loop {
            self.validate_actor_claim(actor_claim).await?;
            if activation_hook_for(session).is_some_and(|hooks| !hooks.is_healthy()) {
                return Err(CoreError::Cancelled);
            }
            if cancel.is_cancelled() {
                return Ok(FinishReason::Cancelled);
            }
            let projection = self.store.read_projection_shared(session).await?;
            let stable_id = projection
                .session
                .agent
                .as_ref()
                .unwrap_or(&live_agent.name)
                .as_str()
                .to_string();
            // Round-boundary dynamic rebind (Root turns only, never the first
            // round). The bind already short-circuits on an unchanged registry
            // generation, so the generation comparison here decides whether the
            // round swaps in a new runtime at all.
            if rounds > 0
                && apply_default_overlays
                && let Some(inputs) = rebind.as_ref()
            {
                let workdir = session_workdir(&live_agent, &projection);
                // Skills are discovered into the registry explicitly (they are
                // not part of the bind itself), so a workdir skill change must
                // refresh them before the bind for the new generation to
                // carry it. Fail-open like every other rebind step.
                if let Err(error) = self.refresh_runtime(|candidate| {
                    candidate.refresh_skills(&workdir);
                    Ok(())
                }) {
                    tracing::warn!(
                        session = %session,
                        error = %error,
                        "round rebind could not refresh skills; keeping current skills"
                    );
                }
                match self.bind_session_runtime(session, &workdir).await {
                    Ok(fresh) if fresh.generation() != binding.generation() => {
                        // Mirror the Root activation compile path: agent_base
                        // resolution, guidance layer, skills prompt, sidecar
                        // tools retained, plus a fresh spawn roster.
                        let rematerialized = effective_agent_for_binding_with_sidecar_tools(
                            inputs.base_agent,
                            stable_id.as_str(),
                            &fresh,
                            guidance.as_deref(),
                            &inputs.sidecar_tools,
                        )
                        .and_then(|(materialized, compiled)| {
                            agent_roster(&fresh, stable_id.as_str())
                                .map(|roster| (materialized, compiled, roster))
                        });
                        match rematerialized {
                            Ok((materialized, compiled, roster)) => {
                                tracing::info!(
                                    session = %session,
                                    generation = ?fresh.generation(),
                                    "round rebind applied a new runtime generation"
                                );
                                live_binding = fresh;
                                live_agent = materialized;
                                live_resources = compiled;
                                live_agents = self
                                    .annotate_agent_efforts(roster, Some(&live_binding))
                                    .await?;
                                binding = &live_binding;
                                resources = &live_resources;
                                agents = &live_agents;
                                let mut hooks = binding.bundle_hooks_for_agent(stable_id.as_str());
                                if let Some(sidecar_hooks) = &inputs.sidecar_hooks {
                                    hooks.push(Arc::clone(sidecar_hooks));
                                }
                                let hooks = (!hooks.is_empty()).then(|| {
                                    Arc::new(HookChain::new(hooks)) as Arc<dyn HookDispatcher>
                                });
                                replace_activation_hooks(session, hooks);
                                let channel_policy = crate::ChannelPolicy::from_binding(binding)?
                                    .snapshot_for(live_agent.name.as_str());
                                steer.set_policy(channel_policy);
                                self.update_session_channel_policy(session, channel_policy);
                            }
                            Err(error) => {
                                tracing::warn!(
                                    session = %session,
                                    %error,
                                    "round rebind failed to recompile resources; keeping the current runtime generation"
                                );
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            session = %session,
                            %error,
                            "round rebind failed; keeping the current runtime generation"
                        );
                    }
                }
            }
            let model = self.requested_turn_model(
                binding,
                &stable_id,
                apply_default_overlays,
                explicit_model,
                &projection,
                agent,
            );
            let parent_session = projection.session.parent;
            let mut ladder = ladder::CompactionLadder {
                engine: self,
                session,
                agent: &live_agent,
                model: &model,
                binding,
                resources,
                depth,
                actor_claim,
                messages: Vec::new(),
            };
            // The ladder re-reads the transcript after each rung it commits,
            // so whether it rewrote needs no second read here.
            ladder.before_round(&projection).await?;
            let messages = ladder.messages;
            drop(projection);
            // Resolved every round, so a preference saved mid-session applies
            // to the next request without a restart. `live_agent.reasoning` is
            // the Agent's authored effort; the user's per-Agent choices outrank it.
            let effort = self
                .effective_effort(
                    &model,
                    Some(&stable_id),
                    live_agent.reasoning,
                    Some(binding),
                )
                .await?
                .effort;
            let tools = self.offered_tools(resources, depth);
            let system = system_prompt(&live_agent, &tools, depth);
            let digests = prompt_digests(&system, &tools)?;
            if digests != recorded_digests {
                self.emit_turn_binding(actor_claim, session, message, binding, &digests)
                    .await?;
                recorded_digests = digests;
            }
            let request =
                crate::kernel::request::from_messages(messages, &system, tools, &model, effort);
            let request = if let Some(hooks) = self.active_hook_dispatcher(session) {
                let root_session = self
                    .request_root_session(session, &mut root_session_cache)
                    .await;
                match hooks
                    .chat_params(ChatParamsInput {
                        session,
                        root_session,
                        agent: Some(AgentName::new(stable_id.as_str())),
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
            let mut round_retries = 0;
            let stream_round = loop {
                self.validate_actor_claim(actor_claim).await?;
                let open_stream = async {
                    Ok::<_, CoreError>(if let Some(route) = workflow_route {
                        match self
                            .stream_with_workflow_route(request.clone(), session, message, route)
                            .await
                        {
                            Ok(opened) => opened,
                            Err(error) => {
                                if let Some(route) = workflow_route {
                                    route
                                        .finalize(Some(workflow_provider_failure_class(&error)))
                                        .await?;
                                }
                                return Err(error.into());
                            }
                        }
                    } else {
                        self.stream_with_model_fallback(
                            request.clone(),
                            session,
                            message,
                            RequestLineage {
                                agent: stable_id.as_str(),
                                root_session_cache: &mut root_session_cache,
                            },
                        )
                        .await?
                    })
                };
                let (stream, served_model) = tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Ok(FinishReason::Cancelled),
                    result = open_stream => result?,
                };
                let step = rounds + round_retries;
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
                match self
                    .collect_stream_round(
                        session,
                        message,
                        stream,
                        actor_claim,
                        step,
                        &served_model,
                        cancel,
                    )
                    .await
                {
                    Ok(stream_round) => {
                        if let Some(route) = workflow_route {
                            route.finalize(None).await?;
                        }
                        break stream_round;
                    }
                    Err(failure) => {
                        self.emit_for_actor(
                            actor_claim,
                            session,
                            Event::StepFinished {
                                session,
                                message,
                                step,
                                finish: FinishReason::Error,
                            },
                        )
                        .await?;
                        let retryable = round_retries < MAX_ROUND_RETRIES
                            && !failure.saw_tool_call
                            && !failure.saw_text
                            && is_retryable_round_error(&failure.error);
                        if !retryable {
                            if let Some(route) = workflow_route {
                                route
                                    .finalize(Some(workflow_failure_class(&failure.error, cancel)))
                                    .await?;
                            }
                            return Err(failure.error);
                        }
                        round_retries += 1;
                        tracing::warn!(
                            %session,
                            retry = round_retries,
                            "provider round failed before text or tool output; retrying"
                        );
                        let delay = std::time::Duration::from_secs(1 << (round_retries - 1))
                            + std::time::Duration::from_millis(50 * u64::from(round_retries));
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Err(CoreError::Cancelled),
                            () = tokio::time::sleep(delay) => {}
                        }
                    }
                }
            };
            // Failed attempts consume step numbers so subsequent tool rounds do
            // not reuse a step already recorded in the event log; `rounds` is
            // now the step of the attempt that succeeded.
            rounds += round_retries;
            if let Some(tokens) = stream_round.tokens {
                total_tokens
                    .get_or_insert_with(TokenUsage::default)
                    .add(tokens);
            }
            self.emit_for_actor(
                actor_claim,
                session,
                Event::StepFinished {
                    session,
                    message,
                    step: rounds,
                    finish: stream_round.finish,
                },
            )
            .await?;
            // Release the streaming slot before running tools (which may spawn and
            // await child subagents that need permits of their own).
            drop(stream_permit);

            if stream_round.finish == FinishReason::Cancelled {
                return Ok(FinishReason::Cancelled);
            }
            if stream_round.tool_calls.is_empty() {
                self.close_turn_message_with_tokens(
                    actor_claim,
                    session,
                    message,
                    stream_round.finish,
                    None,
                    total_tokens,
                )
                .await?;
                return Ok(stream_round.finish);
            }

            let emit = super::stream_round::TurnEmit {
                engine: self,
                actor_claim,
            };
            let provider = self.provider_router();
            let scope = crate::kernel::Scope {
                store: &self.store,
                emit: &emit,
                provider: &provider,
                model: &model,
                session,
                message,
            };
            let mut gate = tool_gate::TurnToolGate {
                engine: self,
                session,
                message,
                binding,
                resources: &live_resources,
                agents,
                agent: &live_agent,
                depth,
                external_dirs,
                workspace: &workspace,
                actor_claim,
                guidance: &guidance,
                cancel,
                report_latch: &report_latch,
                nested_engine: &mut nested_engine,
                steer: &mut steer,
                parent_session,
                offered: &request.tools,
            };
            // Calls a cancel leaves unrun stay open; the turn end closes them.
            crate::kernel::execute_calls(&scope, &mut gate, stream_round.tool_calls, cancel)
                .await?;
            if cancel.is_cancelled() {
                return Ok(FinishReason::Cancelled);
            }

            // An accepted `report` ends the member's episode: the round's
            // other tool calls have completed and are recorded; no further
            // model call is made.
            if report_latch.is_set() {
                self.close_turn_message_with_tokens(
                    actor_claim,
                    session,
                    message,
                    FinishReason::Stop,
                    None,
                    total_tokens,
                )
                .await?;
                return Ok(FinishReason::Stop);
            }

            rounds += 1;
        }
    }
}

/// Maximum additional provider opens for one engine round.
const MAX_ROUND_RETRIES: u32 = 2;

/// Mid-stream retries are limited to link failures that are safe to replay before
/// any assistant text or tool request was delivered. Decode is deliberately
/// narrow: only the known truncated response-body diagnostics are included.
fn is_retryable_round_error(error: &CoreError) -> bool {
    let CoreError::Provider(error) = error else {
        return false;
    };
    match error.as_ref() {
        ProviderError::Transport(_) => true,
        ProviderError::HttpStatus { status, .. } => *status == 429 || (500..=599).contains(status),
        ProviderError::Decode(message) => {
            let message = message.to_ascii_lowercase();
            message == "error decoding response body"
                || message.contains("unexpected eof")
                || message.contains("unexpected end of file")
        }
        _ => false,
    }
}

impl SessionEngine {
    /// Tool schemas a full-mode request offers: the agent's filtered tools
    /// plus the kernel's `rewrite`, which the kernel loop intercepts by name.
    fn offered_tools(
        &self,
        resources: &CompiledResourceView,
        depth: u32,
    ) -> Vec<hya_proto::ToolSchema> {
        let mut tools = filtered_tool_schemas(
            resources,
            self.interaction.is_available(),
            depth,
            self.exclusive_tool.as_deref(),
        );
        tools.push(crate::kernel::tools::rewrite_schema());
        tools
    }

    /// Permission plane for one tool call of `agent`, before authorization.
    ///
    /// Reads the tree's permission mode at every call, so a switch applies to
    /// the next check even mid-turn.
    pub(super) async fn tool_call_permission(
        &self,
        binding: &TurnBinding,
        session: SessionId,
        agent: &AgentName,
        external_dirs: &[PathBuf],
        grant_scope: &hya_tool::GrantScope,
        activation_hooks: Option<&Arc<dyn HookDispatcher>>,
    ) -> Result<PermissionPlane, CoreError> {
        let mode_plane = self
            .mode_permission_plane(binding, session, Some(agent))
            .await?;
        let mut permission = permission_for_session(&mode_plane, session, external_dirs)
            .with_grant_scope(grant_scope.clone());
        if let Some(hooks) = activation_hooks {
            permission = permission.prepend_interceptor(Arc::new(
                crate::bundle_hooks::BundlePermissionInterceptor::new(Arc::clone(hooks)),
            ));
        }
        Ok(permission)
    }
}

fn permission_for_session(
    permission: &PermissionPlane,
    session: SessionId,
    external_dirs: &[PathBuf],
) -> PermissionPlane {
    let permission = permission.for_session(session);
    // `ExternalDirectory` asks name canonical directories (ADR-0026), so the
    // attached directories are canonicalized to meet them. These are
    // configured-style glob rules: they cover the attached tree.
    let rules = external_dirs
        .iter()
        .map(|dir| {
            let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.clone());
            Rule::new(
                Action::ExternalDirectory,
                dir.join("*").to_string_lossy().replace('\\', "/"),
                Mode::Allow,
            )
        })
        .collect();
    permission.with_snapshot_rules(rules)
}
