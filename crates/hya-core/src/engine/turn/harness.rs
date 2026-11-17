//! The full mode's [`Harness`]: what a full-mode turn adds around the kernel
//! loop. Before each round it re-validates the actor claim, rebinds a Root
//! activation to a new runtime generation, resolves the round's model, and
//! walks the compaction ladder; the request carries the agent's prompt,
//! tools, and effort through `chat.params`; streams open through model
//! fallback or the Workflow route under a governor permit; a round that
//! failed before any output is retried; an accepted `report` ends the turn.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{
    AgentName, FinishReason, MessageId, ModelRef, Projection, SessionId, TokenUsage, ToolSchema,
};
use hya_provider::{CompletionRequest, EventStream, ProviderError};
use hya_store::ActorClaim;
use hya_tool::{AgentDef, ReportLatch};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use super::super::roots::SessionWorkspace;
use super::super::steer::SteerMailbox;
use super::super::stream_round::TurnEmit;
use super::super::{
    AgentSpec, SessionEngine, agent_roster, effective_agent_for_binding_with_sidecar_tools,
    session_workdir,
};
use super::ladder::CompactionLadder;
use super::messages::{AttachmentData, prompt_digests, system_prompt};
use super::tool_gate::TurnToolGate;
use super::{
    RequestLineage, TurnExecution, TurnRebindInputs, workflow_failure_class,
    workflow_provider_failure_class,
};
use crate::TurnBinding;
use crate::error::CoreError;
use crate::hooks::{
    ChatParamsInput, ChatParamsOutcome, HookChain, HookDispatcher, activation_hook_for,
    replace_activation_hooks,
};
use crate::kernel::{self, Failure, Harness, Rewriter, Round, ToolCall, ToolGate};
use crate::runtime_registry::CompiledResourceView;
use crate::workflow::WorkflowTurnRoute;

/// Maximum additional provider opens for one engine round.
const MAX_ROUND_RETRIES: u32 = 2;

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
    /// hooks, skills, and prompts published mid-turn take effect at the next
    /// model call without rebuilding the transcript context. Any rebind or
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
        let tools = self.offered_tools(&resources, depth);
        let recorded_digests = prompt_digests(&system_prompt(agent, &tools, depth), &tools)?;
        self.emit_turn_binding(actor_claim, session, message, &binding, &recorded_digests)
            .await?;
        // Steer mailbox (fix for the unread-mail blindness): unread mail is
        // surfaced inside tool results, mid-turn, without breaking the model's
        // chain of thought. Snapshot the durable backlog once, then follow the
        // live bus so no per-tool projection replay is needed.
        let steer_policy =
            crate::ChannelPolicy::from_binding(&binding)?.snapshot_for(agent.name.as_str());
        let steer = self
            .steer_mailbox_snapshot_with_policy(session, Some(steer_policy))
            .await;
        let emit = TurnEmit {
            engine: self,
            actor_claim,
        };
        let provider = self.provider_router();
        let scope = kernel::Scope {
            store: &self.store,
            emit: &emit,
            complete: Some(self),
            provider: &provider,
            session,
            message,
        };
        let mut harness = TurnHarness {
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
            workflow_route,
            guidance,
            external_dirs,
            workspace,
            actor_claim,
            cancel,
            depth,
            steer,
            recorded_digests,
            report_latch: ReportLatch::new(),
            nested_engine: None,
            root_session_cache: None,
            rounds: 0,
            stable_id: String::new(),
            model: agent.model.clone(),
            attachments: AttachmentData::new(),
            parent_session: None,
            offered: Vec::new(),
            permit: None,
            retries: 0,
            total_tokens: None,
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

/// One full-mode turn's state across the kernel's rounds.
struct TurnHarness<'a> {
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
    workflow_route: Option<&'a WorkflowTurnRoute>,
    guidance: Option<Arc<str>>,
    external_dirs: &'a [PathBuf],
    workspace: Arc<SessionWorkspace>,
    actor_claim: Option<&'a ActorClaim>,
    cancel: &'a CancellationToken,
    depth: u32,
    steer: SteerMailbox,
    /// Prompt digests last recorded in `TurnBindingRecorded`.
    recorded_digests: (String, String),
    /// Set when a `report` of this turn is accepted: the turn ends right
    /// after that tool round, and a second `report` is rejected.
    report_latch: ReportLatch,
    /// Engine handle for nested tool calls, cloned once per turn on first use.
    nested_engine: Option<Arc<SessionEngine>>,
    /// Root of this session's spawn tree, resolved on first hook use.
    root_session_cache: Option<SessionId>,
    /// Rounds collected so far.
    rounds: u32,
    // Resolved per round in `before_round`.
    stable_id: String,
    model: ModelRef,
    attachments: AttachmentData,
    parent_session: Option<SessionId>,
    /// Tool schemas the round's request offered.
    offered: Vec<ToolSchema>,
    /// Live provider-stream permit, held only while a round streams: a member
    /// blocked in the `task` tool (awaiting its children) holds none.
    permit: Option<OwnedSemaphorePermit>,
    /// Retries of the current round.
    retries: u32,
    total_tokens: Option<TokenUsage>,
}

impl TurnHarness<'_> {
    /// Swap in a new runtime generation when the registry has one.
    async fn rebind(&mut self, projection: &Projection) -> Result<(), CoreError> {
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
        let fresh = match engine.bind_session_runtime(session, &workdir).await {
            Ok(fresh) if fresh.generation() != self.binding.generation() => fresh,
            Ok(_) => return Ok(()),
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
            hooks.push(Arc::clone(sidecar_hooks));
        }
        self.agents = engine.annotate_agent_efforts(roster, Some(&fresh)).await?;
        self.binding = fresh;
        self.live_agent = materialized;
        self.resources = compiled;
        let hooks =
            (!hooks.is_empty()).then(|| Arc::new(HookChain::new(hooks)) as Arc<dyn HookDispatcher>);
        replace_activation_hooks(session, hooks);
        let channel_policy = crate::ChannelPolicy::from_binding(&self.binding)?
            .snapshot_for(self.live_agent.name.as_str());
        self.steer.set_policy(channel_policy);
        engine.update_session_channel_policy(session, channel_policy);
        Ok(())
    }
}

/// Before each round: re-validate the actor claim, rebind (Root, after the
/// first round), resolve the round's model and attachments, then walk the
/// compaction ladder.
#[async_trait]
impl Rewriter for TurnHarness<'_> {
    async fn before_round(&mut self, projection: &Projection) -> Result<bool, CoreError> {
        let engine = self.engine;
        engine.validate_actor_claim(self.actor_claim).await?;
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
            self.rebind(projection).await?;
        }
        self.model = engine.requested_turn_model(
            &self.binding,
            &self.stable_id,
            self.apply_default_overlays,
            self.explicit_model,
            projection,
            self.agent,
        );
        self.parent_session = projection.session.parent;
        self.attachments = engine.attachment_data(self.session, projection).await?;
        CompactionLadder {
            engine,
            session: self.session,
            agent: &self.live_agent,
            model: &self.model,
            binding: &self.binding,
            resources: &self.resources,
            depth: self.depth,
            actor_claim: self.actor_claim,
            attachments: &self.attachments,
        }
        .walk(projection)
        .await
    }
}

#[async_trait]
impl ToolGate for TurnHarness<'_> {
    async fn call(&mut self, call: ToolCall) -> Result<(), CoreError> {
        TurnToolGate {
            engine: self.engine,
            session: self.session,
            message: self.message,
            binding: &self.binding,
            resources: &self.resources,
            agents: &self.agents,
            agent: &self.live_agent,
            depth: self.depth,
            external_dirs: self.external_dirs,
            workspace: &self.workspace,
            actor_claim: self.actor_claim,
            guidance: &self.guidance,
            cancel: self.cancel,
            report_latch: &self.report_latch,
            nested_engine: &mut self.nested_engine,
            steer: &mut self.steer,
            parent_session: self.parent_session,
            offered: &self.offered,
        }
        .call(call)
        .await
    }

    /// Unrun calls stay open; the turn end closes them as `CANCELLED`.
    async fn cancelled(&mut self, _calls: Vec<ToolCall>) -> Result<(), CoreError> {
        Ok(())
    }
}

#[async_trait]
impl Harness for TurnHarness<'_> {
    async fn request(&mut self, projection: &Projection) -> Result<CompletionRequest, CoreError> {
        let engine = self.engine;
        let session = self.session;
        // Resolved every round, so a preference saved mid-session applies to
        // the next request without a restart. `live_agent.reasoning` is the
        // Agent's authored effort; the user's per-Agent choices outrank it.
        let effort = engine
            .effective_effort(
                &self.model,
                Some(&self.stable_id),
                self.live_agent.reasoning,
                Some(&self.binding),
            )
            .await?
            .effort;
        let tools = engine.offered_tools(&self.resources, self.depth);
        let system = system_prompt(&self.live_agent, &tools, self.depth);
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
            projection,
            &system,
            tools,
            &self.model,
            &self.live_agent.name,
            &self.attachments,
            effort,
        );
        let request = if let Some(hooks) = engine.active_hook_dispatcher(session) {
            let root_session = engine
                .request_root_session(session, &mut self.root_session_cache)
                .await;
            match hooks
                .chat_params(ChatParamsInput {
                    session,
                    root_session,
                    agent: Some(AgentName::new(self.stable_id.as_str())),
                    message: self.message,
                    request,
                })
                .await
            {
                ChatParamsOutcome::Continue { request } => request,
            }
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
        if self.permit.is_none() {
            // These classes bound execution only; durable admission/order
            // remains authoritative.
            self.permit = match (&engine.governor, self.depth > 0) {
                (Some(gov), true) => gov.acquire_general_stream().await,
                (Some(gov), false) => gov.acquire_reserved_stream().await,
                (None, _) => None,
            };
        }
        engine.validate_actor_claim(self.actor_claim).await?;
        let cancel = self.cancel;
        let workflow_route = self.workflow_route;
        let lineage = RequestLineage {
            agent: self.stable_id.as_str(),
            root_session_cache: &mut self.root_session_cache,
        };
        let open = async {
            match workflow_route {
                Some(route) => match engine
                    .stream_with_workflow_route(request, session, message, route)
                    .await
                {
                    Ok(opened) => Ok(opened),
                    Err(error) => {
                        route
                            .finalize(Some(workflow_provider_failure_class(&error)))
                            .await?;
                        Err(error.into())
                    }
                },
                None => Ok(engine
                    .stream_with_model_fallback(request, session, message, lineage)
                    .await?),
            }
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(None),
            opened = open => opened.map(Some),
        }
    }

    async fn failed(&mut self, failure: &Failure) -> Result<bool, CoreError> {
        let retryable = self.retries < MAX_ROUND_RETRIES
            && !failure.saw_tool_call
            && !failure.saw_text
            && is_retryable_round_error(&failure.error);
        if !retryable {
            if let Some(route) = self.workflow_route {
                route
                    .finalize(Some(workflow_failure_class(&failure.error, self.cancel)))
                    .await?;
            }
            return Ok(false);
        }
        self.retries += 1;
        tracing::warn!(
            session = %self.session,
            retry = self.retries,
            "provider round failed before text or tool output; retrying"
        );
        let delay = std::time::Duration::from_secs(1 << (self.retries - 1))
            + std::time::Duration::from_millis(50 * u64::from(self.retries));
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(CoreError::Cancelled),
            () = tokio::time::sleep(delay) => Ok(true),
        }
    }

    async fn collected(&mut self, round: &Round) -> Result<(), CoreError> {
        if let Some(route) = self.workflow_route {
            route.finalize(None).await?;
        }
        if let Some(tokens) = round.tokens {
            self.total_tokens
                .get_or_insert_with(TokenUsage::default)
                .add(tokens);
        }
        // Release the streaming slot before running tools (which may spawn
        // and await child subagents that need permits of their own).
        self.permit = None;
        self.retries = 0;
        self.rounds += 1;
        Ok(())
    }

    fn done(&self) -> bool {
        // An accepted `report` ends the member's episode: the round's other
        // tool calls have completed and are recorded.
        self.report_latch.is_set()
    }
}

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
