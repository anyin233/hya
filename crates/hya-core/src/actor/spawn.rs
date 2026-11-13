//! Spawn target authorization, child model selection, and bound member preparation.

use super::MainActivation;
use crate::TaskSpawnOrigin;
use crate::agent_catalog::AgentDefinition;
use crate::{
    AgentResourcePolicy, AgentSpec, BoundSidecarFactory, CategoryRegistry, CoreError,
    SessionEngine, SidecarEnvironment, TurnBinding, apply_agent_model_preference,
    apply_spawn_model_policy, resolve_dispatch_model,
};
use hya_proto::{AgentName, ModelRef, SessionId};
use hya_provider::ReasoningEffort;
use hya_store::StoreError;
use hya_tool::{MemberOutcome, SpawnError, SpawnMember};
use std::sync::Arc;

/// Fully authorized and resolved member, ready for atomic resident registration.
pub struct ResolvedSpawnMember {
    /// Resolved request including dispatch model and effort.
    pub request: SpawnMember,
    /// Stable catalog identity authorized for this caller.
    pub authorized_target: AgentName,
    /// Effective child specification.
    pub agent: AgentSpec,
    /// Captured request runtime.
    pub binding: TurnBinding,
    /// Child-visible agent roster.
    pub agents: Arc<[hya_tool::AgentDef]>,
    /// Child resource selection.
    pub resources: AgentResourcePolicy,
    /// Activation factory bound to the selected capabilities.
    pub sidecar_factory: Option<Arc<dyn BoundSidecarFactory>>,
    /// Immutable guidance Arc cloned from the spawn request (not from disk).
    pub guidance: Option<Arc<str>>,
}

/// Resolve exact team root + root definition/roster/resource policy before durable
/// spawn admission. Fail-closed; no parent-fallback and no catalog/base synthesis.
pub async fn resolve_main_activation_context(
    engine: &SessionEngine,
    binding: &TurnBinding,
    base: &AgentSpec,
    parent: SessionId,
    guidance: Option<Arc<str>>,
) -> Result<MainActivation, SpawnError> {
    let (root, _) = engine
        .session_lineage(parent)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_projection = engine
        .read_projection_shared(root)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_agent_name = root_projection
        .session
        .agent
        .as_ref()
        .ok_or(SpawnError::Unavailable)?;
    let root_stable = root_agent_name.as_str();
    // Exact catalog lookup for the root stable id; map missing definition through
    // the existing typed UnknownAgentId seam (no general/base fallback).
    if binding.resolve_agent(root_stable).is_none() {
        return Err(SpawnError::UnknownAgentId {
            agent_id: root_stable.to_string(),
        });
    }
    let agent = engine
        .agent_spec_for_binding(binding, base, root_stable)
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = engine
        .agent_roster_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    let resources = engine
        .agent_resource_policy_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    Ok(MainActivation {
        agent,
        binding: binding.clone(),
        agents,
        resources,
        guidance,
    })
}

/// Captured caller configuration used to prepare one task member.
pub struct ResolveSpawnMemberCtx<'a> {
    /// Runtime supplying the current model catalog.
    pub engine: &'a SessionEngine,
    /// Captured request runtime.
    pub binding: &'a TurnBinding,
    /// Process base specification.
    pub base: &'a AgentSpec,
    /// Stable caller identity.
    pub caller: &'a str,
    /// Caller-visible spawn targets.
    pub allowed_agents: &'a [hya_tool::AgentDef],
    /// Configured model categories.
    pub categories: &'a CategoryRegistry,
    /// Captured provider routing predicate.
    pub is_servable: &'a dyn Fn(&ModelRef) -> bool,
    /// Request-scoped immutable guidance.
    pub guidance: Option<Arc<str>>,
    /// Application-owned activation resolver.
    pub sidecar_environment: &'a dyn SidecarEnvironment,
}

/// Authorize a target and resolve its child runtime without durable effects.
pub fn resolve_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
) -> Result<ResolvedSpawnMember, SpawnError> {
    let definition = authorize_spawn_target(ctx.binding, ctx.allowed_agents, ctx.caller, &member)?;
    resolve_authorized_spawn_member(ctx, member, &definition)
}

/// Resolve a member's caller-supplied model request against the live catalog.
///
/// Branch 1 (exact valid id) and branch 2 (first substring match, bare
/// vendor ids excluded) replace the request with the resolved id; anything
/// else clears it so [`apply_spawn_model_policy`] falls through to the
/// definition/preference chain. A `#effort` suffix on the request is split
/// off before matching and becomes the member's `effort` unless the call
/// also passed an explicit `effort`, which wins.
fn resolve_member_dispatch_model(
    ctx: &ResolveSpawnMemberCtx<'_>,
    mut member: SpawnMember,
) -> SpawnMember {
    let catalog = ctx.engine.provider_catalog();
    let model_ids: Vec<String> = catalog
        .iter()
        .map(|row| format!("{}/{}", row.provider_id, row.model_id))
        .collect();
    let provider_ids: Vec<String> = ctx
        .engine
        .provider_catalog_snapshot()
        .providers()
        .iter()
        .map(|state| state.provider_id.clone())
        .collect();
    let requested = member
        .model
        .clone()
        .or_else(|| {
            member
                .inline_agent
                .as_ref()
                .and_then(|inline| inline.model.clone())
        })
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let Some(requested) = requested else {
        return member;
    };
    let (requested, suffix) = match requested.rsplit_once('#') {
        Some((base, effort)) => (base.to_owned(), Some(effort.to_owned())),
        None => (requested, None),
    };
    if member
        .effort
        .as_deref()
        .is_none_or(|effort| effort.trim().is_empty())
    {
        member.effort = suffix.filter(|effort| !effort.trim().is_empty());
    }
    let resolved = resolve_dispatch_model(&requested, &model_ids, &provider_ids)
        .map(|model| model.to_string());
    member.model = resolved.clone();
    if let Some(inline) = member.inline_agent.as_mut() {
        inline.model = resolved;
    }
    member
}

/// Apply a spawn-time `effort` as the child model's `#effort` suffix.
///
/// The suffix is the effort resolver's highest layer, so it outranks the
/// Agent's configured/remembered default and authored policy. An unknown label,
/// or one the child's catalog row does not advertise, is rejected (`none` is
/// always allowed). An absent or empty `effort` leaves the Agent untouched.
fn apply_spawn_effort(
    engine: &SessionEngine,
    mut agent: AgentSpec,
    effort: Option<&str>,
) -> Result<AgentSpec, SpawnError> {
    let Some(label) = effort.map(str::trim).filter(|label| !label.is_empty()) else {
        return Ok(agent);
    };
    let base = agent
        .model
        .as_str()
        .rsplit_once('#')
        .map_or(agent.model.as_str(), |(base, _)| base)
        .to_owned();
    let invalid = || SpawnError::InvalidEffort {
        effort: label.to_owned(),
        model: base.clone(),
    };
    let parsed = ReasoningEffort::parse(label).ok_or_else(invalid)?;
    if parsed != ReasoningEffort::Off {
        let advertised = engine
            .provider_catalog()
            .into_iter()
            .find(|row| {
                base == format!("{}/{}", row.provider_id, row.model_id) || base == row.model_id
            })
            .map(|row| row.reasoning_variants)
            .filter(|variants| !variants.is_empty());
        if let Some(variants) = advertised
            && !variants
                .iter()
                .any(|variant| ReasoningEffort::parse(variant) == Some(parsed))
        {
            return Err(invalid());
        }
    }
    agent.model = ModelRef::new(format!("{base}#{label}"));
    Ok(agent)
}

fn resolve_authorized_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
    definition: &AgentDefinition<'_>,
) -> Result<ResolvedSpawnMember, SpawnError> {
    validate_unsupported_inline_agent_fields(&member)?;
    let authorized_target = hya_proto::AgentName::new(definition.stable_id);
    let sidecar_factory = ctx
        .sidecar_environment
        .factory_for(ctx.binding, authorized_target.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let agent = ctx
        .engine
        .agent_spec_for_binding(ctx.binding, ctx.base, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = ctx
        .engine
        .agent_roster_for_binding(ctx.binding, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;
    let resources = ctx
        .engine
        .agent_resource_policy_for_binding(ctx.binding, definition.stable_id)
        .map_err(|_| SpawnError::Unavailable)?;

    let agent = apply_agent_model_preference(
        agent,
        definition,
        ctx.binding.agent_model_preference(definition.stable_id),
        ctx.is_servable,
    );
    // Dispatch-time model resolution: a caller-supplied model id is resolved
    // against the live catalog (exact -> substring, bare vendor ids never
    // substring-dispatch). Unresolvable requests defer to the user's
    // configured chain instead of overriding it verbatim.
    let member = resolve_member_dispatch_model(ctx, member);
    let agent =
        apply_spawn_model_policy(agent, definition, &member, ctx.categories, ctx.is_servable);
    let mut agent = apply_spawn_effort(ctx.engine, agent, member.effort.as_deref())?;
    if let Some(inline) = member.inline_agent.as_ref() {
        if !inline.prompt.trim().is_empty() {
            agent.system_prompt = inline.prompt.clone();
        }
        if !inline.name.trim().is_empty() {
            agent.name = AgentName::new(&inline.name);
        }
    }

    Ok(ResolvedSpawnMember {
        request: member,
        authorized_target,
        agent,
        binding: ctx.binding.clone(),
        agents,
        resources,
        sidecar_factory,
        guidance: ctx.guidance.clone(),
    })
}

fn authorize_spawn_target<'a>(
    binding: &'a TurnBinding,
    allowed_agents: &[hya_tool::AgentDef],
    caller: &str,
    member: &SpawnMember,
) -> Result<AgentDefinition<'a>, SpawnError> {
    let requested = member.subagent_type.trim();
    let requested = if requested.is_empty() {
        crate::TASK_AGENT_ID
    } else {
        requested
    };
    let definition =
        binding
            .resolve_agent(requested)
            .ok_or_else(|| SpawnError::UnknownAgentId {
                agent_id: requested.to_string(),
            })?;
    if !allowed_agents
        .iter()
        .any(|allowed| allowed.name == definition.stable_id)
    {
        return Err(SpawnError::AgentSpawnNotAllowed {
            caller: caller.to_string(),
            agent_id: definition.stable_id.to_string(),
        });
    }
    Ok(definition)
}

fn validate_unsupported_inline_agent_fields(member: &SpawnMember) -> Result<(), SpawnError> {
    if member
        .inline_agent
        .as_ref()
        .is_some_and(|inline| inline.description.is_some())
    {
        return Err(SpawnError::UnsupportedInlineAgentField {
            field: "description",
        });
    }
    Ok(())
}

impl super::ResidentSupervisor {
    /// Check depth and cancellation, then own registration and its reply until completion.
    /// The host joins `tasks` before draining the supervisor.
    pub async fn dispatch_spawn(
        self: &Arc<Self>,
        req: hya_tool::SpawnRequest,
        resolved: Vec<ResolvedSpawnMember>,
        main_activation: MainActivation,
        stop: tokio_util::sync::CancellationToken,
        tasks: &tokio_util::task::TaskTracker,
    ) {
        match self.engine.session_lineage(req.parent).await {
            Ok((_, depth)) if depth.saturating_add(1) > crate::MAX_SUBAGENT_DEPTH => {
                let _ = req.reply.send(Err(SpawnError::Overloaded));
                return;
            }
            Err(error) => {
                eprintln!("hya: failed to resolve spawn lineage ({error})");
                let _ = req.reply.send(Err(SpawnError::Unavailable));
                return;
            }
            Ok(_) => {}
        }
        if req.cancel.is_cancelled() {
            let _ = req.reply.send(Err(SpawnError::Cancelled));
            return;
        }
        let actor_claim = req.operation.actor_claim();
        let source_tool_call = req.operation.source_tool_call_id();
        let resident_supervisor = self.clone();
        tasks.spawn(async move {
            let cancel = req.cancel.clone();
            let execution = async move {
                let main = main_activation;
                let models = resolved
                    .iter()
                    .map(|member| member.agent.model.to_string())
                    .collect::<Vec<_>>();
                let members = resolved
                    .into_iter()
                    .map(|resolved| crate::resident::TaskResidentSpawn {
                        agent: resolved.agent,
                        runtime: (
                            resolved.binding,
                            resolved.agents,
                            resolved.resources,
                            resolved.sidecar_factory,
                        ),
                        directive: resolved.request.prompt,
                        task: TaskSpawnOrigin {
                            subagent_type: resolved.authorized_target.as_str().into(),
                            description: resolved.request.description,
                            tool_call: Some(source_tool_call),
                        },
                        guidance: resolved.guidance,
                    })
                    .collect();
                let result = resident_supervisor
                    .spawn_residents_typed(req.parent, members, main, actor_claim.as_ref())
                    .await;
                let outcome = match result {
                    Ok(identities) => Ok(identities
                        .into_iter()
                        .zip(models)
                        .map(|((session, handle), model)| MemberOutcome {
                            member: handle.clone(),
                            session: session.to_string(),
                            status: "running".into(),
                            summary: format!(
                                "Resident {handle} is live; results arrive as its report."
                            ),
                            model: Some(model),
                        })
                        .collect()),
                    Err(CoreError::Store(StoreError::CapacityExceeded { .. })) => {
                        Err(SpawnError::Overloaded)
                    }
                    Err(CoreError::Cancelled) => Err(SpawnError::Cancelled),
                    Err(error) => {
                        eprintln!("hya: atomic spawn registration failed ({error})");
                        Err(SpawnError::Unavailable)
                    }
                };
                let _ = req.reply.send(outcome);
            };
            tokio::pin!(execution);
            tokio::select! {
                biased;
                () = stop.cancelled() => {
                    cancel.cancel();
                    execution.await;
                }
                () = &mut execution => {}
            }
        });
    }
}
