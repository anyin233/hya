//! Spawn target authorization, child model selection, and bound member preparation.

use super::{
    Episode, EpisodeInit, MainActivation, PreparedResident, ResidentActivation,
    ResidentRuntimeContext, ResidentSpawnContext, ResidentSupervisor, ResolvedResidentRuntime,
    SlotStatus, TaskResidentSpawn, TeamRegistration, await_episode,
};
use crate::TaskSpawnOrigin;
use crate::agent_catalog::AgentDefinition;
use crate::{
    AgentResourcePolicy, AgentSpec, BoundSidecarFactory, CategoryRegistry, CoreError,
    SessionEngine, SidecarEnvironment, TurnBinding, apply_agent_model_preference,
    apply_spawn_model_policy, resolve_dispatch_model,
};
use hya_proto::{
    ActorClaim, AgentName, ChannelKind, Event, MailEndpoint, MemberId, ModelRef, SessionId,
    SubagentMode, scope,
};
use hya_provider::{ProviderCatalogSnapshot, ReasoningEffort};
use hya_tool::extension::ExtensionEvent;
use hya_tool::{AgentDef, MemberOutcome, SpawnError, SpawnMember};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;

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
        .map_err(spawn_error)?;
    let resources = engine
        .agent_resource_policy_for_binding(binding, root_stable)
        .map_err(spawn_error)?;
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
    let providers = ctx.engine.provider_snapshot();
    resolve_spawn_member_with_catalog(ctx, member, providers.catalog())
}

/// Resolve a member with the catalog retained by the caller's spawn operation.
/// Pair it with `ctx.is_servable` from the same provider publication.
pub fn resolve_spawn_member_with_catalog(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
    catalog: &ProviderCatalogSnapshot,
) -> Result<ResolvedSpawnMember, SpawnError> {
    let definition = authorize_spawn_target(ctx.binding, ctx.allowed_agents, ctx.caller, &member)?;
    resolve_authorized_spawn_member(ctx, member, &definition, catalog)
}

/// A missing agent id is the caller's error; any other failure means the
/// roster is unavailable.
fn spawn_error(error: CoreError) -> SpawnError {
    let missing = match &error {
        CoreError::AgentDefinitionMissing { agent_id } => Some(agent_id.clone()),
        CoreError::Bundle(error) => match error.downcast_ref() {
            Some(hya_bundle::BundleError::UnknownAgentId { agent_id }) => Some(agent_id.clone()),
            _ => None,
        },
        _ => None,
    };
    missing.map_or(SpawnError::Unavailable, |agent_id| {
        SpawnError::UnknownAgentId { agent_id }
    })
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
    catalog: &ProviderCatalogSnapshot,
    mut member: SpawnMember,
) -> SpawnMember {
    let model_ids: Vec<String> = catalog
        .models()
        .iter()
        .map(|row| format!("{}/{}", row.provider_id, row.model_id))
        .collect();
    let provider_ids: Vec<String> = catalog
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
    catalog: &ProviderCatalogSnapshot,
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
        let advertised = catalog
            .models()
            .iter()
            .find(|row| {
                base == format!("{}/{}", row.provider_id, row.model_id) || base == row.model_id
            })
            .map(|row| &row.reasoning_variants)
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
    catalog: &ProviderCatalogSnapshot,
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
    let member = resolve_member_dispatch_model(catalog, member);
    let agent =
        apply_spawn_model_policy(agent, definition, &member, ctx.categories, ctx.is_servable);
    let mut agent = apply_spawn_effort(catalog, agent, member.effort.as_deref())?;
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
                    Err(CoreError::CapacityExceeded { .. }) => Err(SpawnError::Overloaded),
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

impl ResidentSupervisor {
    /// Register the team root as the main actor so child mail (and quiescence) wake
    /// it. Idempotent: a second call for the same root is a no-op. The root is
    /// registered on the roster as `main` (transient mode — it is the root, not a
    /// resident subagent) if not already present.
    ///
    /// Callers must supply the already-resolved **team root** `AgentSpec`, can_spawn
    /// roster, and resource policy from the same captured [`TurnBinding`] as the
    /// triggering spawn (exact root stable `AgentName`, not the nested caller).
    /// `guidance` remains optional in-process-only triggering-turn text. Recovery
    /// invents nothing. Later `ensure_main` calls for the same root leave the first
    /// registration intact (first-wins).
    pub async fn ensure_main(
        &self,
        root: SessionId,
        agent: AgentSpec,
        resolved: (TurnBinding, Arc<[AgentDef]>, AgentResourcePolicy),
        actor_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(), CoreError> {
        let (binding, agents, resources) = resolved;
        self.engine
            .ensure_root_registered_for_actor(root, actor_claim)
            .await?;
        self.team_for_with_main(
            root,
            MainActivation {
                agent,
                binding,
                agents,
                resources,
                guidance,
            },
        );
        Ok(())
    }

    /// Spawn a brand-new resident under `parent`: create its session, assign a
    /// stable team-scoped handle, register it (mode = resident), announce it in the
    /// parent tree, and give it an initial wake so it runs its first turn on
    /// `directive`, then idles. Returns the new session + its handle.
    ///
    /// Non-blocking: this returns as soon as the resident is registered and armed;
    /// the caller (parent) does NOT wait for the resident's turn.
    ///
    /// `guidance` is the immutable triggering-turn Arc stored only in-process for
    /// every activation of this slot; it is not written to the resident definition.
    pub async fn spawn_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    directive.clone(),
                    Some(directive),
                    parent_claim,
                    guidance,
                ),
            )
            .await?;
        Ok((session, handle))
    }

    /// [`spawn_resident`](Self::spawn_resident) for a `task` spawn: the handle
    /// prefix is the sanitized `subagent_type` (the resolved agent id the call
    /// named, see [`hya_tool::sanitize_handle_prefix`]) rather than the spec's
    /// agent name, which an inline overlay may replace. The handle becomes
    /// `<parent path>/<subagent_type>-<operator>`; see [`crate::handle_naming`].
    /// The member row on the parent log carries the call's description and id.
    ///
    /// # Errors
    /// Every [`spawn_resident`](Self::spawn_resident) failure.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_resident_typed(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        directive: String,
        task: TaskSpawnOrigin,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String), CoreError> {
        let (session, handle, _) = self
            .spawn_resident_inner(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(
                    directive.clone(),
                    Some(directive),
                    parent_claim,
                    guidance,
                )
                .with_task(task),
            )
            .await?;
        Ok((session, handle))
    }

    /// Create and register one resident without an implicit first turn.
    ///
    /// The caller must deliver work through durable mail after this returns.
    /// `registration_directive` is provenance only and must not contain the work
    /// payload or input values. Returns the committed Session, handle and member
    /// id before initializer hooks complete; the episode runs those before work.
    pub async fn spawn_resident_parked(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        registration_directive: String,
        parent_claim: Option<&ActorClaim>,
        guidance: Option<Arc<str>>,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(
                parent,
                agent,
                resolved,
                ResidentSpawnContext::new(registration_directive, None, parent_claim, guidance),
            )
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        // Return the committed identity before initialization can wait on a
        // plugin. Activation and cancellation then belong to the controller.
        self.register_episodes(root, vec![prepared], None).await?;
        Ok((session, handle, member))
    }

    async fn spawn_resident_inner(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, String, MemberId), CoreError> {
        let (root, prepared, member) = self
            .prepare_resident(parent, agent, resolved, context)
            .await?;
        let session = prepared.session;
        let handle = scope::join_path(&prepared.parent_path, &prepared.leaf);
        let initialized = self.register_episodes(root, vec![prepared], None).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok((session, handle, member))
    }

    /// Register all resolved task members or none, then install every committed
    /// episode under the claim-write boundary before awaiting initialization.
    pub async fn spawn_residents_typed(
        &self,
        parent: SessionId,
        members: Vec<TaskResidentSpawn>,
        main: MainActivation,
        parent_claim: Option<&ActorClaim>,
    ) -> Result<Vec<(SessionId, String)>, CoreError> {
        let (root, _) = self.engine.session_lineage(parent).await?;
        let mut prepared = Vec::with_capacity(members.len());
        let mut identities = Vec::with_capacity(members.len());
        for member in members {
            let (_, registration, _) = self
                .prepare_resident(
                    parent,
                    member.agent,
                    member.runtime,
                    ResidentSpawnContext::new(
                        member.directive.clone(),
                        Some(member.directive),
                        parent_claim,
                        member.guidance,
                    )
                    .with_task(member.task),
                )
                .await?;
            identities.push((
                registration.session,
                scope::join_path(&registration.parent_path, &registration.leaf),
            ));
            prepared.push(registration);
        }
        let initialized = self.register_episodes(root, prepared, Some(main)).await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(identities)
    }

    pub(super) async fn prepare_resident(
        &self,
        parent: SessionId,
        agent: AgentSpec,
        resolved: ResolvedResidentRuntime,
        context: ResidentSpawnContext,
    ) -> Result<(SessionId, PreparedResident, MemberId), CoreError> {
        let ResidentSpawnContext {
            directive,
            initial,
            parent_claim,
            guidance,
            task,
        } = context;
        let prefix = hya_tool::sanitize_handle_prefix(
            task.as_ref()
                .map_or(agent.name.as_str(), |task| task.subagent_type.as_str()),
        );
        let (binding, agents, resources, sidecar_factory) = resolved;
        let (root, parent_depth) = self.engine.session_lineage(parent).await?;
        let session = SessionId::new();
        let parent_projection = self.engine.read_projection_shared(parent).await?;
        let mut events = vec![Event::SessionCreated {
            session,
            parent: Some(parent),
            agent: agent.name.clone(),
            model: agent.model.clone(),
            workdir: agent.workdir.to_string_lossy().into_owned(),
            project: parent_projection.session.project,
            kind: parent_projection.session.kind,
        }];
        drop(parent_projection);
        if parent_depth.saturating_add(1) > crate::MAX_SUBAGENT_DEPTH {
            return Err(CoreError::Invalid("maximum subagent depth exceeded".into()));
        }
        let parent_path = if parent == root {
            scope::ROOT_HANDLE.into()
        } else {
            self.engine.resolve_handle(root, parent).await?
        };
        let reservation = {
            // A commit may release a reservation only after this snapshot has
            // observed its durable identity; stale snapshots cannot reuse it.
            let _write = self.claim_writes.lock().await;
            self.engine.mint_member_leaf(root, &prefix).await?
        };
        let leaf = reservation.leaf.clone();
        let member = MemberId::new();
        let description = task
            .as_ref()
            .map(|task| task.description.trim())
            .filter(|description| !description.is_empty())
            .map_or_else(|| directive.chars().take(80).collect(), ToString::to_string);
        let tool_call = task.as_ref().and_then(|task| task.tool_call);
        events.push(
            ExtensionEvent::MemberSpawned {
                session: parent,
                member,
                child: Some(session),
                subagent_type: agent.name.clone(),
                description,
                depth: parent_depth.saturating_add(1),
                directive,
                tool_call,
            }
            .into_event(),
        );
        Ok((
            root,
            PreparedResident {
                reservation: Some(reservation),
                session,
                parent_path,
                leaf,
                agent,
                runtime: ResidentRuntimeContext::Resolved {
                    binding,
                    agents,
                    resources,
                },
                activation: ResidentActivation {
                    events,
                    parent_claim,
                    initial,
                    guidance,
                    route: None,
                    sidecar_factory,
                },
            },
            member,
        ))
    }

    /// Register an already-created `session` as a resident of team `root`, arm it,
    /// and (when `initial` is set) give it a first wake. Used by
    /// [`spawn_resident`](Self::spawn_resident); also the seam tests drive directly.
    pub async fn register_existing_resident(
        &self,
        root: SessionId,
        session: SessionId,
        handle: String,
        agent: AgentSpec,
        initial: Option<String>,
    ) -> Result<(), CoreError> {
        let binding = self
            .engine
            .bind_session_runtime(session, &agent.workdir)
            .await?;
        let parent_path = self.engine.parent_agent_path(root, session).await;
        let leaf = scope::leaf(&handle).to_string();
        self.register_episode(
            root,
            session,
            parent_path,
            leaf,
            agent,
            ResidentRuntimeContext::Bound(binding),
            ResidentActivation {
                events: Vec::new(),
                parent_claim: None,
                initial,
                guidance: None,
                route: None,
                sidecar_factory: None,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn register_episode(
        &self,
        root: SessionId,
        session: SessionId,
        parent_path: String,
        leaf: String,
        agent: AgentSpec,
        runtime: ResidentRuntimeContext,
        activation: ResidentActivation,
    ) -> Result<(), CoreError> {
        let initialized = self
            .register_episodes(
                root,
                vec![PreparedResident {
                    reservation: None,
                    session,
                    parent_path,
                    leaf,
                    agent,
                    runtime,
                    activation,
                }],
                None,
            )
            .await?;
        let _ = futures::future::join_all(initialized).await;
        Ok(())
    }

    pub(super) fn capacity(&self) -> u64 {
        self.engine
            .governor()
            .map_or(u64::MAX, |governor| governor.limits().per_run_budget)
    }

    pub(super) async fn register_episodes(
        &self,
        root: SessionId,
        members: Vec<PreparedResident>,
        main: Option<MainActivation>,
    ) -> Result<Vec<oneshot::Receiver<Result<(), CoreError>>>, CoreError> {
        let projection = self.engine.read_projection_shared(root).await?;
        let parent_claim = members
            .first()
            .and_then(|member| member.activation.parent_claim);
        let mut registrations = Vec::with_capacity(members.len());
        let mut installs = Vec::with_capacity(members.len());
        let mut reservations = Vec::with_capacity(members.len());
        for member in members {
            let PreparedResident {
                reservation,
                session,
                parent_path,
                leaf,
                agent,
                runtime,
                activation,
            } = member;
            reservations.push(reservation);
            let ResidentActivation {
                mut events,
                parent_claim: _,
                initial,
                guidance,
                route,
                sidecar_factory,
            } = activation;
            let (binding, agents, resources) = match runtime {
                ResidentRuntimeContext::Bound(binding) => (binding, None, None),
                ResidentRuntimeContext::Resolved {
                    binding,
                    agents,
                    resources,
                } => (binding, Some(agents), Some(resources)),
            };
            let handle = scope::join_path(&parent_path, &leaf);
            // The store resolves the unit's group channel under the writer lock;
            // simultaneous first siblings must join the same broadcast channel.
            let group_channel = hya_proto::mint_channel_id(ChannelKind::Group);
            let dm_channel = hya_proto::mint_channel_id(ChannelKind::Dm);
            events.extend([
                ExtensionEvent::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: leaf,
                    parent: Some(parent_path.clone()),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }
                .into_event(),
                // The unit's leader-only broadcast channel: minted with the
                // leader as founding member on first registration, joined
                // by every child thereafter.
                ExtensionEvent::ChannelCreated {
                    session: root,
                    channel: group_channel,
                    kind: ChannelKind::Group,
                    unit: Some(parent_path.clone()),
                    members: vec![parent_path.clone(), handle.clone()],
                }
                .into_event(),
                // The persistent parent-child DM pair: the revival address
                // that outlives archive cycles.
                ExtensionEvent::ChannelCreated {
                    session: root,
                    channel: dm_channel.clone(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec![parent_path.clone(), handle.clone()],
                }
                .into_event(),
            ]);
            let initial = initial.filter(|text| !text.trim().is_empty());
            let pending = initial.is_some();
            if let Some(text) = initial {
                events.push(
                    ExtensionEvent::MailSent {
                        session: root,
                        from: parent_path.clone(),
                        to: MailEndpoint::Channel(dm_channel),
                        kind: hya_proto::MailKind::Message,
                        body: text,
                    }
                    .into_event(),
                );
            }
            let channel_policy = crate::channel_policy::from_binding(&binding)
                .map_err(CoreError::bundle)?
                .snapshot_for(agent.name.as_str());

            let team_proj = hya_tool::extension::team_projection(&projection);
            let parent_session = team_proj
                .roster
                .get(&parent_path)
                .map_or(root, |entry| entry.session);
            registrations.push(crate::store_bridge::EpisodeRegistration {
                actor: session,
                events,
            });
            let episode = Episode {
                handle,
                agent,
                binding,
                agents,
                resources,
                guidance,
                route,
                activation: None,
                sidecar_factory,
                is_main: false,
                status: SlotStatus::Idle,
                pending,
                report: None,
                user_stopped: false,
                synth_pending: false,
                claim: None,
                cancel: CancellationToken::new(),
                ending: None,
                done: watch::channel(None).0,
                cursor: 0,
                last_activity: None,
                last_heartbeat_emit: None,
                notify: Arc::new(Notify::new()),
            };
            installs.push((session, parent_session, episode, channel_policy));
        }
        let archived: std::collections::HashSet<_> =
            hya_tool::extension::team_projection(&projection)
                .archived
                .values()
                .map(|entry| entry.session)
                .collect();
        drop(projection);
        let mut team = if let Some(main) = main {
            let mut teams = self.teams();
            let team = teams
                .entry(root)
                .or_insert_with(|| self.new_team(root, main));
            {
                let state = team.lock();
                if state.root_driver.runs.load(Ordering::Acquire) == 0 {
                    state.root_driver.autonomous.store(true, Ordering::Release);
                }
            }
            TeamRegistration::new(team.clone())
        } else {
            self.team_for(root).await?
        };
        #[cfg(test)]
        {
            let pause = self
                .registration_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some((entered, release)) = pause {
                entered.notify_one();
                release.notified().await;
            }
        }
        let mut write = self.claim_writes.lock().await;
        loop {
            let previous = registrations.iter().find_map(|registration| {
                self.team_with_resident(registration.actor)
                    .and_then(|team| {
                        team.lock()
                            .residents
                            .get(&registration.actor)
                            .map(|slot| (registration.actor, slot.done.subscribe()))
                    })
            });
            let Some((actor, done)) = previous else { break };
            if !archived.contains(&actor) {
                return Err(CoreError::Invalid(
                    "resident already has a live episode".into(),
                ));
            }
            drop(write);
            let _ = await_episode(done).await;
            team = self.team_for(root).await?;
            write = self.claim_writes.lock().await;
        }
        use crate::store_bridge::SessionStoreExt as _;
        let registered = self
            .engine
            .store()
            .register_episodes(
                root,
                self.owner_run_id,
                parent_claim.as_ref(),
                self.capacity(),
                registrations,
            )
            .await?;
        // Committed identities now belong to durable history. Release while
        // claim_writes still excludes name allocation from a stale snapshot.
        drop(reservations);
        let mut initialized_members = Vec::with_capacity(installs.len());
        let mut publications = Vec::with_capacity(installs.len());
        for ((claim, envelopes), (session, parent, mut episode, channel_policy)) in
            registered.into_iter().zip(installs)
        {
            self.engine
                .update_session_channel_policy(session, channel_policy);
            let (ready, initialized) = oneshot::channel();
            let (publication, published) = oneshot::channel();
            let (child_envelopes, parent_envelopes): (Vec<_>, Vec<_>) = envelopes
                .into_iter()
                .partition(|envelope| envelope.event.session() == Some(session));
            let init = EpisodeInit {
                binding: episode.binding.clone(),
                agent: episode.agent.name.clone(),
                parent,
                envelopes: child_envelopes,
                published,
                ready,
            };
            episode.claim = Some(claim);
            episode.cancel = team.cancel.child_token();
            team.install_episode(session, episode, Some(init));
            initialized_members.push(initialized);
            publications.push((parent_envelopes, publication));
        }
        for (envelopes, _) in &publications {
            for envelope in envelopes {
                self.engine.publish_envelope(envelope.clone());
            }
        }
        for (_, publication) in publications {
            let _ = publication.send(());
        }
        drop(write);
        // Commit is the batch's success boundary. Each episode owns initializer
        // errors and its ending; none can retract these committed identities.
        Ok(initialized_members)
    }
}
